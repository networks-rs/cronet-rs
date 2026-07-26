#![cfg(feature = "network-tests")]

use std::{future::poll_fn, pin::Pin, time::Duration};

use futures_core::Stream;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_cronet::{BidirectionalRequest, Engine, Error, PublicKeyPins, QuicHint, Request};

const HTTP3_HOST: &str = "cloudflare-quic.com";
const HTTP3_URL: &str = "https://cloudflare-quic.com/";

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn public_http2_bidirectional_stream_covers_headers_body_half_close_and_terminal() {
    let engine = Engine::builder().enable_http2(true).build().unwrap();
    let request = BidirectionalRequest::builder("https://nghttp2.org/httpbin/post")
        .unwrap()
        .method("POST")
        .unwrap()
        .header("content-type", "text/plain")
        .unwrap()
        .read_buffer_size(1024)
        .read_channel_capacity(2)
        .write_capacity(2)
        .disable_auto_flush(true)
        .build()
        .unwrap();
    let mut stream =
        tokio::time::timeout(Duration::from_secs(30), engine.open_bidirectional(request))
            .await
            .expect("HTTP/2 bidirectional open timed out")
            .unwrap();
    stream.write_all(b"tokio-full-duplex").await.unwrap();
    stream.flush().await.unwrap();
    stream.shutdown().await.unwrap();
    let headers = stream.response_headers().await.unwrap();
    assert_eq!(headers.status(), Some(200));
    assert_eq!(headers.negotiated_protocol, "h2");
    let mut body = Vec::new();
    if let Some(chunk) = stream.next_chunk().await {
        body.extend_from_slice(&chunk.unwrap());
    }
    if let Some(chunk) = poll_fn(|context| Pin::new(&mut stream).poll_next(context)).await {
        body.extend_from_slice(&chunk.unwrap());
    }
    stream.read_to_end(&mut body).await.unwrap();
    assert!(
        body.windows(b"tokio-full-duplex".len())
            .any(|window| window == b"tokio-full-duplex")
    );
    assert!(stream.trailers().await.unwrap().is_empty());
    stream.finished().await.unwrap();
    assert!(stream.is_done());
    engine.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn public_http2_bidirectional_cancel_is_idempotent_and_terminal() {
    let engine = Engine::builder().enable_http2(true).build().unwrap();
    let request = BidirectionalRequest::builder("https://nghttp2.org/httpbin/stream/20")
        .unwrap()
        .method("GET")
        .unwrap()
        .end_of_stream(true)
        .build()
        .unwrap();
    let mut stream =
        tokio::time::timeout(Duration::from_secs(30), engine.open_bidirectional(request))
            .await
            .expect("HTTP/2 bidirectional open timed out")
            .unwrap();
    assert_eq!(stream.response_headers().await.unwrap().status(), Some(200));
    stream.cancel();
    stream.cancel();
    assert!(matches!(stream.finished().await, Err(Error::Canceled)));
    assert!(stream.is_done());
    engine.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn public_http2_active_stream_drop_cancels_and_shutdown_completes() {
    let engine = Engine::builder().enable_http2(true).build().unwrap();
    let request = BidirectionalRequest::builder("https://nghttp2.org/httpbin/stream/100")
        .unwrap()
        .method("GET")
        .unwrap()
        .read_channel_capacity(1)
        .end_of_stream(true)
        .build()
        .unwrap();
    let mut stream =
        tokio::time::timeout(Duration::from_secs(30), engine.open_bidirectional(request))
            .await
            .expect("HTTP/2 bidirectional open timed out")
            .unwrap();
    assert_eq!(stream.response_headers().await.unwrap().status(), Some(200));
    assert!(!stream.is_done());
    drop(stream);
    tokio::time::timeout(Duration::from_secs(30), engine.shutdown())
        .await
        .expect("shutdown waited forever after active bidirectional drop")
        .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn public_http2_stream_copies_response_trailers() {
    let engine = Engine::builder().enable_http2(true).build().unwrap();
    let request = BidirectionalRequest::builder("https://http2.testserver.host/trailers")
        .unwrap()
        .method("GET")
        .unwrap()
        .header("te", "trailers")
        .unwrap()
        .end_of_stream(true)
        .build()
        .unwrap();
    let mut stream =
        tokio::time::timeout(Duration::from_secs(30), engine.open_bidirectional(request))
            .await
            .expect("HTTP/2 trailers request timed out")
            .unwrap();
    let headers = stream.response_headers().await.unwrap();
    assert_eq!(headers.status(), Some(200));
    assert_eq!(headers.negotiated_protocol, "h2");
    let mut body = Vec::new();
    stream.read_to_end(&mut body).await.unwrap();
    let trailers = stream.trailers().await.unwrap();
    assert!(!trailers.is_empty());
    stream.finished().await.unwrap();
    engine.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn public_key_pinning_rejects_a_nonmatching_server_chain() {
    let pins = PublicKeyPins::new(
        "nghttp2.org",
        ["sha256/AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA="],
        false,
        4_102_444_800_000,
    )
    .unwrap();
    let engine = Engine::builder()
        .bypass_pinning_for_local_trust_anchors(false)
        .public_key_pins(pins)
        .build()
        .unwrap();
    let result = tokio::time::timeout(
        Duration::from_secs(30),
        engine.execute(
            Request::builder("https://nghttp2.org/httpbin/get")
                .unwrap()
                .build()
                .unwrap(),
        ),
    )
    .await
    .expect("pinned request timed out");
    assert!(matches!(result, Err(Error::Network(_))));
    engine.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn public_quic_request_and_full_duplex_stream_negotiate_http3() {
    let engine = Engine::builder()
        .enable_quic(true)
        .enable_http2(true)
        .quic_hint(QuicHint::new(HTTP3_HOST, 443, 443).unwrap())
        .build()
        .unwrap();
    let mut negotiated_protocol = String::new();
    for _ in 0..4 {
        let response = tokio::time::timeout(
            Duration::from_secs(30),
            engine.execute(
                Request::builder(HTTP3_URL)
                    .unwrap()
                    .max_response_bytes(4 * 1024 * 1024)
                    .build()
                    .unwrap(),
            ),
        )
        .await
        .expect("HTTP/3 request timed out")
        .unwrap();
        assert_eq!(response.status(), 200);
        negotiated_protocol.clone_from(&response.info.negotiated_protocol);
        if negotiated_protocol.contains("h3") || negotiated_protocol.contains("quic") {
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    assert!(
        negotiated_protocol.contains("h3")
            || negotiated_protocol.contains("quic")
            || std::env::var_os("CRONET_E2E_REQUIRE_QUIC").is_none(),
        "QUIC was required but UDP/HTTP3 negotiation failed: {negotiated_protocol}"
    );
    if !negotiated_protocol.contains("h3") && !negotiated_protocol.contains("quic") {
        eprintln!(
            "QUIC negotiation unavailable in this network; set CRONET_E2E_REQUIRE_QUIC=1 to make this a failure"
        );
        engine.shutdown().await.unwrap();
        return;
    }

    let request = BidirectionalRequest::builder(HTTP3_URL)
        .unwrap()
        .method("GET")
        .unwrap()
        .disable_auto_flush(true)
        .delay_headers_until_flush(true)
        .build()
        .unwrap();
    let mut stream =
        tokio::time::timeout(Duration::from_secs(30), engine.open_bidirectional(request))
            .await
            .expect("HTTP/3 bidirectional open timed out")
            .unwrap();
    stream.shutdown().await.unwrap();
    let headers = stream.response_headers().await.unwrap();
    assert_eq!(headers.status(), Some(200));
    assert!(
        headers.negotiated_protocol.contains("h3") || headers.negotiated_protocol.contains("quic"),
        "unexpected bidirectional protocol: {}",
        headers.negotiated_protocol
    );
    let mut body = Vec::new();
    stream.read_to_end(&mut body).await.unwrap();
    assert!(!body.is_empty());
    stream.finished().await.unwrap();
    engine.shutdown().await.unwrap();
}

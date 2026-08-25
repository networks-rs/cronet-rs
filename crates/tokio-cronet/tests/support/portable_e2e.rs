//! Runtime-portable E2E scenarios shared by desktop, Android, iOS, and OHOS.
//!
//! Keep platform-specific launch code out of this module. Every function below
//! runs against the real Cronet library and a loopback HTTP server, so the same
//! ownership and callback graph is exercised on every runtime platform.

use std::{
    collections::HashMap,
    future::{Future, poll_fn},
    io::{self, BufRead, BufReader, Cursor, Read, SeekFrom, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    task::{Context, Poll},
    thread,
    time::Duration,
};

use futures_core::Stream;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncSeek, AsyncWriteExt, ReadBuf};
use tokio_cronet::{
    BidirectionalRequest, CacheMode, Engine, Error, FinishedReason, Idempotency, Priority,
    PublicKeyPins, QuicHint, RedirectAction, Request, RequestFinishedInfo, RequestStatus,
};

const TIMEOUT: Duration = Duration::from_secs(10);

#[allow(dead_code)] // Mobile runners call the suite; native_smoke exposes each case separately.
pub async fn run_all() {
    request_api_and_tokio_io().await;
    request_controls_and_terminal_callbacks().await;
    callback_and_transport_failures_are_typed().await;
    pending_upload_and_rewind_cancellation_are_safe().await;
    cancellation_and_shutdown_races_are_safe().await;
    bidirectional_configuration_and_failure_are_safe().await;
    #[cfg(feature = "sse")]
    sse_event_source_covers_events_reconnect_and_cancel().await;
    #[cfg(feature = "ws")]
    websocket_echo_and_close_are_terminal().await;
    #[cfg(feature = "nqe")]
    nqe_observes_localhost_after_testing_override().await;
    #[cfg(feature = "network-binding")]
    network_binding_round_trips_engine_handle().await;
    engine_drop_with_active_work_is_process_safe().await;
}

#[allow(clippy::too_many_lines)]
pub async fn request_api_and_tokio_io() {
    let server = TestServer::start();
    let pin = PublicKeyPins::new(
        "unused.invalid",
        ["sha256/AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA="],
        false,
        i64::MAX,
    )
    .unwrap();
    let engine = Engine::builder()
        .user_agent("cronet-rs/portable-e2e")
        .accept_language("zh-CN,en")
        .enable_quic(true)
        .enable_http2(true)
        .enable_brotli(true)
        .cache_mode(CacheMode::Disabled)
        .bypass_pinning_for_local_trust_anchors(true)
        .experimental_options("{}")
        .quic_hint(QuicHint::new("unused.invalid", 443, 443).unwrap())
        .public_key_pins(pin)
        .build()
        .unwrap();
    assert!(!engine.version().unwrap().is_empty());
    assert!(!engine.default_user_agent().unwrap().is_empty());

    #[cfg(target_os = "android")]
    {
        let android_engine = Engine::builder()
            .network_thread_priority(0)
            .build()
            .unwrap();
        android_engine.shutdown().await.unwrap();
    }

    let mut finished_events = engine.subscribe_finished();
    let request = engine
        .request(server.url("/inspect"))
        .unwrap()
        .method("POST")
        .unwrap()
        .header("x-cronet-test", "request-api")
        .unwrap()
        .annotation("portable=request-api")
        .unwrap()
        .body("static-upload")
        .disable_cache(true)
        .priority(Priority::Highest)
        .idempotency(Idempotency::Idempotent)
        .allow_direct_executor(true)
        .read_buffer_size(3)
        .body_channel_capacity(1)
        .max_response_bytes(4096)
        .build()
        .unwrap();
    let response = engine.execute(request).await.unwrap();
    assert_eq!(response.status(), 200);
    assert!(
        String::from_utf8_lossy(response.body())
            .contains("POST|cronet-rs/portable-e2e|zh-CN,en|request-api|static-upload")
    );
    assert_eq!(response.info.status_text, "OK");
    assert!(response.info.url.ends_with("/inspect"));
    assert_eq!(response.info.url_chain.len(), 1);
    assert!(response.info.received_byte_count >= 0);
    assert_eq!(response.finished.reason, FinishedReason::Succeeded);
    assert_eq!(response.finished.annotations, ["portable=request-api"]);
    assert!(
        response
            .finished
            .response
            .as_ref()
            .expect("final response metadata")
            .received_byte_count
            >= i64::try_from(response.body().len()).expect("E2E body length fits i64")
    );
    assert_eq!(response.metrics(), &response.finished.metrics);
    assert_metrics_are_consistent(&response.finished);
    assert_eq!(response.clone().into_body(), response.body());
    let event = recv_annotation(&mut finished_events, "portable=request-api").await;
    assert_eq!(event.reason, FinishedReason::Succeeded);

    let empty = engine
        .execute(
            Request::builder(server.url("/empty"))
                .unwrap()
                .max_response_bytes(0)
                .build()
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(empty.status(), 204);
    assert!(empty.body().is_empty());

    let duplicate_headers = engine
        .execute(
            Request::builder(server.url("/duplicate-headers"))
                .unwrap()
                .build()
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        duplicate_headers
            .info
            .headers
            .iter()
            .filter(|header| header.name().eq_ignore_ascii_case("set-cookie"))
            .count(),
        2
    );

    // Exercise every priority and idempotency representation through native
    // initialization, not merely through builder-only unit tests.
    for (priority, idempotency) in [
        (Priority::Idle, Idempotency::Default),
        (Priority::Lowest, Idempotency::Idempotent),
        (Priority::Low, Idempotency::NotIdempotent),
        (Priority::Medium, Idempotency::Default),
        (Priority::Highest, Idempotency::Idempotent),
    ] {
        let response = engine
            .execute(
                Request::builder(server.url("/ok"))
                    .unwrap()
                    .priority(priority)
                    .idempotency(idempotency)
                    .build()
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.into_body(), b"ok");
    }

    let (mut upload_writer, upload_reader) = tokio::io::duplex(64);
    tokio::spawn(async move {
        upload_writer.write_all(b"known-stream").await.unwrap();
        upload_writer.shutdown().await.unwrap();
    });
    let known = Request::builder(server.url("/echo"))
        .unwrap()
        .body_stream(upload_reader, Some(12))
        .build()
        .unwrap();
    assert_eq!(engine.execute(known).await.unwrap().body(), b"known-stream");

    let (mut upload_writer, upload_reader) = tokio::io::duplex(64);
    tokio::spawn(async move {
        upload_writer.write_all(b"chunked-stream").await.unwrap();
        upload_writer.shutdown().await.unwrap();
    });
    let chunked = Request::builder(server.url("/echo"))
        .unwrap()
        .body_stream(upload_reader, None)
        .build()
        .unwrap();
    assert_eq!(
        engine.execute(chunked).await.unwrap().body(),
        b"chunked-stream"
    );

    let rewindable = Request::builder(server.url("/redirect307"))
        .unwrap()
        .rewindable_body_stream(Cursor::new(b"rewound-upload".to_vec()), Some(14))
        .build()
        .unwrap();
    let response = engine.execute(rewindable).await.unwrap();
    assert_eq!(response.status(), 201);
    assert_eq!(response.body(), b"rewound-upload");
    assert_eq!(response.info.url_chain.len(), 2);

    let automatic = Request::builder(server.url("/redirect"))
        .unwrap()
        .follow_redirects(true)
        .build()
        .unwrap();
    assert_eq!(
        engine.execute(automatic).await.unwrap().body(),
        b"redirected"
    );

    let asynchronous = Request::builder(server.url("/redirect"))
        .unwrap()
        .redirect_handler(|redirect| async move {
            assert_eq!(redirect.response.status_code, 302);
            assert!(redirect.location.ends_with("/final"));
            tokio::task::yield_now().await;
            RedirectAction::Follow
        })
        .build()
        .unwrap();
    assert_eq!(
        engine.execute(asynchronous).await.unwrap().body(),
        b"redirected"
    );

    let rejected = Request::builder(server.url("/redirect"))
        .unwrap()
        .follow_redirects(false)
        .build()
        .unwrap();
    assert!(matches!(
        engine.send(rejected).await,
        Err(Error::Redirect { .. })
    ));

    let async_rejected = Request::builder(server.url("/redirect"))
        .unwrap()
        .redirect_handler(|_| async { RedirectAction::Cancel })
        .build()
        .unwrap();
    assert!(matches!(
        engine.send(async_rejected).await,
        Err(Error::Redirect { .. })
    ));

    let pending = engine
        .start(
            Request::builder(server.url("/chunks"))
                .unwrap()
                .read_buffer_size(3)
                .body_channel_capacity(1)
                .build()
                .unwrap(),
        )
        .unwrap();
    let pending_handle = pending.handle();
    assert_ne!(
        pending.request_status().await.unwrap(),
        RequestStatus::Invalid
    );
    assert_ne!(
        pending_handle.status().await.unwrap(),
        RequestStatus::Invalid
    );
    let response = pending.await.unwrap();
    assert_eq!(response.status(), 200);
    assert!(!response.is_done());
    assert_ne!(
        response.request_status().await.unwrap(),
        RequestStatus::Invalid
    );
    let response_handle = response.handle();
    let (info, mut body) = response.into_parts();
    assert_eq!(info.status_code, 200);
    assert_ne!(body.request_status().await.unwrap(), RequestStatus::Invalid);
    assert!(!body.handle().is_done());
    let first = body.next_chunk().await.unwrap().unwrap();
    let mut streamed = first.to_vec();
    while let Some(chunk) = poll_fn(|context| Pin::new(&mut body).poll_next(context)).await {
        streamed.extend_from_slice(&chunk.unwrap());
    }
    assert_eq!(streamed, b"onetwothree");
    let finished = body.finished().await.unwrap();
    assert_eq!(finished.reason, FinishedReason::Succeeded);
    assert!(body.is_done());
    assert!(response_handle.is_done());
    assert_eq!(
        response_handle.status().await.unwrap(),
        RequestStatus::Invalid
    );

    let mut async_read = engine
        .send(
            Request::builder(server.url("/chunks"))
                .unwrap()
                .build()
                .unwrap(),
        )
        .await
        .unwrap();
    let mut bytes = Vec::new();
    async_read.body.read_to_end(&mut bytes).await.unwrap();
    assert_eq!(bytes, b"onetwothree");
    assert_eq!(
        async_read.body.finished().await.unwrap().reason,
        FinishedReason::Succeeded
    );

    engine.shutdown().await.unwrap();
}

#[allow(clippy::too_many_lines)]
pub async fn request_controls_and_terminal_callbacks() {
    let server = TestServer::start();
    let engine = Engine::builder().build().unwrap();
    let mut events = engine.subscribe_finished();
    let mut second_events = engine.subscribe_finished();

    let pending = annotated_pending(&engine, &server, "pending-cancel");
    let handle = pending.handle();
    pending.cancel();
    assert_canceled(pending).await;
    wait_done(&handle).await;
    assert_eq!(handle.status().await.unwrap(), RequestStatus::Invalid);
    assert_eq!(
        recv_annotation(&mut events, "portable=pending-cancel")
            .await
            .reason,
        FinishedReason::Canceled
    );
    assert_eq!(
        recv_annotation(&mut second_events, "portable=pending-cancel")
            .await
            .reason,
        FinishedReason::Canceled
    );
    drop(second_events);

    let pending = annotated_pending(&engine, &server, "handle-cancel");
    let handle = pending.handle();
    let mut cancelers = Vec::new();
    for _ in 0..8 {
        let handle = handle.clone();
        cancelers.push(tokio::spawn(async move {
            handle.cancel();
            handle.cancel();
        }));
    }
    for canceler in cancelers {
        canceler.await.unwrap();
    }
    assert_canceled(pending).await;
    wait_done(&handle).await;
    assert_eq!(
        recv_annotation(&mut events, "portable=handle-cancel")
            .await
            .reason,
        FinishedReason::Canceled
    );

    let response = engine
        .send(
            Request::builder(server.url("/slow-body"))
                .unwrap()
                .annotation("portable=response-cancel")
                .unwrap()
                .read_buffer_size(1)
                .body_channel_capacity(1)
                .build()
                .unwrap(),
        )
        .await
        .unwrap();
    let handle = response.handle();
    response.cancel();
    let (_, mut body) = response.into_parts();
    assert_eq!(
        timeout("streaming response cancel finished", body.finished())
            .await
            .unwrap()
            .reason,
        FinishedReason::Canceled
    );
    drop(body);
    wait_done(&handle).await;
    assert_eq!(
        recv_annotation(&mut events, "portable=response-cancel")
            .await
            .reason,
        FinishedReason::Canceled
    );

    let mut response = engine
        .send(
            Request::builder(server.url("/slow-body"))
                .unwrap()
                .annotation("portable=body-cancel")
                .unwrap()
                .read_buffer_size(1)
                .body_channel_capacity(1)
                .build()
                .unwrap(),
        )
        .await
        .unwrap();
    let handle = response.body.handle();
    response.body.cancel();
    assert_eq!(
        timeout("response body cancel finished", response.body.finished())
            .await
            .unwrap()
            .reason,
        FinishedReason::Canceled
    );
    drop(response);
    wait_done(&handle).await;
    assert_eq!(
        recv_annotation(&mut events, "portable=body-cancel")
            .await
            .reason,
        FinishedReason::Canceled
    );

    let pending = annotated_pending(&engine, &server, "pending-drop");
    let handle = pending.handle();
    drop(pending);
    wait_done(&handle).await;
    assert_eq!(
        recv_annotation(&mut events, "portable=pending-drop")
            .await
            .reason,
        FinishedReason::Canceled
    );

    let response = engine
        .send(
            Request::builder(server.url("/slow-body"))
                .unwrap()
                .annotation("portable=response-drop")
                .unwrap()
                .read_buffer_size(1)
                .body_channel_capacity(1)
                .build()
                .unwrap(),
        )
        .await
        .unwrap();
    let handle = response.handle();
    drop(response);
    wait_done(&handle).await;
    assert_eq!(
        recv_annotation(&mut events, "portable=response-drop")
            .await
            .reason,
        FinishedReason::Canceled
    );

    let entered = Arc::new(AtomicBool::new(false));
    let dropped = Arc::new(AtomicBool::new(false));
    let handler_entered = entered.clone();
    let handler_dropped = dropped.clone();
    let pending = engine
        .start(
            Request::builder(server.url("/redirect"))
                .unwrap()
                .redirect_handler(move |_| {
                    PendingRedirect::new(handler_entered.clone(), handler_dropped.clone())
                })
                .build()
                .unwrap(),
        )
        .unwrap();
    timeout("redirect handler entered", async {
        while !entered.load(Ordering::Acquire) {
            tokio::task::yield_now().await;
        }
    })
    .await;
    pending.cancel();
    assert_canceled(pending).await;
    timeout("redirect handler dropped", async {
        while !dropped.load(Ordering::Acquire) {
            tokio::task::yield_now().await;
        }
    })
    .await;

    engine.shutdown().await.unwrap();
}

#[allow(clippy::too_many_lines)]
pub async fn callback_and_transport_failures_are_typed() {
    let server = TestServer::start();

    let outside_runtime = std::thread::spawn(|| Engine::builder().build())
        .join()
        .unwrap();
    assert!(matches!(outside_runtime, Err(Error::TokioRuntimeRequired)));

    assert!(Engine::builder().experimental_options("{").build().is_err());

    let engine = Engine::builder().build().unwrap();
    let invalid_method = Request::builder(server.url("/ok"))
        .unwrap()
        .method("invalid method")
        .unwrap()
        .build()
        .unwrap();
    assert!(matches!(
        engine.send(invalid_method).await,
        Err(Error::Cronet(_))
    ));
    let invalid_header = Request::builder(server.url("/ok"))
        .unwrap()
        .header("x-invalid\nname", "value")
        .unwrap()
        .build()
        .unwrap();
    assert!(matches!(
        engine.send(invalid_header).await,
        Err(Error::Cronet(_))
    ));

    let too_large = Request::builder(server.url("/large"))
        .unwrap()
        .read_buffer_size(4)
        .max_response_bytes(5)
        .build()
        .unwrap();
    match engine.execute(too_large).await {
        Err(Error::ResponseTooLarge { limit: 5 }) => {}
        result => panic!("unexpected response-limit result: {result:?}"),
    }
    engine.shutdown().await.unwrap();

    let engine = Engine::builder().build().unwrap();
    let short_upload = Request::builder(server.url("/echo"))
        .unwrap()
        .body_stream(Cursor::new(b"short".to_vec()), Some(8))
        .build()
        .unwrap();
    match engine.execute(short_upload).await {
        Err(Error::Upload(_)) => {}
        result => panic!("unexpected short-upload result: {result:?}"),
    }
    engine.shutdown().await.unwrap();

    let engine = Engine::builder().build().unwrap();
    let read_error = Request::builder(server.url("/echo"))
        .unwrap()
        .body_stream(FailingRead, Some(1))
        .build()
        .unwrap();
    match engine.execute(read_error).await.unwrap_err() {
        Error::Upload(message) => assert!(message.contains("injected read failure")),
        error => panic!("unexpected upload read error: {error:?}"),
    }
    engine.shutdown().await.unwrap();

    let engine = Engine::builder().build().unwrap();
    let rewind_error = Request::builder(server.url("/redirect307"))
        .unwrap()
        .rewindable_body_stream(FailingRewind::new(b"rewind"), Some(6))
        .build()
        .unwrap();
    match engine.execute(rewind_error).await.unwrap_err() {
        Error::Upload(message) => assert!(message.contains("injected seek failure")),
        error => panic!("unexpected upload rewind error: {error:?}"),
    }
    engine.shutdown().await.unwrap();

    let engine = Engine::builder().build().unwrap();
    let handler_panic = Request::builder(server.url("/redirect"))
        .unwrap()
        .redirect_handler(|_| async {
            panic!("injected redirect handler panic");
        })
        .build()
        .unwrap();
    match engine.execute(handler_panic).await.unwrap_err() {
        Error::TokioTask(message) => assert!(message.contains("redirect handler did not complete")),
        error => panic!("unexpected redirect handler error: {error:?}"),
    }
    engine.shutdown().await.unwrap();

    let engine = Engine::builder().build().unwrap();
    let mut events = engine.subscribe_finished();
    let truncated = Request::builder(server.url("/truncated"))
        .unwrap()
        .annotation("portable=truncated")
        .unwrap()
        .build()
        .unwrap();
    assert!(matches!(
        engine.execute(truncated).await,
        Err(Error::Network(_))
    ));
    let event = recv_annotation(&mut events, "portable=truncated").await;
    assert_eq!(event.reason, FinishedReason::Failed);
    let error = event.error.expect("truncated request network error");
    assert!(!error.message.is_empty());
    assert_ne!(error.internal_error_code, 0);
    engine.shutdown().await.unwrap();

    let engine = Engine::builder().build().unwrap();
    let mut events = engine.subscribe_finished();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let refused = listener.local_addr().unwrap();
    drop(listener);
    let refused = Request::builder(format!("http://{refused}/refused"))
        .unwrap()
        .annotation("portable=refused")
        .unwrap()
        .build()
        .unwrap();
    assert!(matches!(
        engine.execute(refused).await,
        Err(Error::Network(_))
    ));
    let event = recv_annotation(&mut events, "portable=refused").await;
    assert_eq!(event.reason, FinishedReason::Failed);
    assert!(event.error.is_some());

    engine.shutdown().await.unwrap();
}

pub async fn pending_upload_and_rewind_cancellation_are_safe() {
    let server = TestServer::start();
    let engine = Engine::builder().build().unwrap();

    let read_polled = Arc::new(AtomicBool::new(false));
    let pending = engine
        .start(
            Request::builder(server.url("/echo"))
                .unwrap()
                .body_stream(PendingRead(read_polled.clone()), Some(8))
                .build()
                .unwrap(),
        )
        .unwrap();
    timeout("pending upload read entered", async {
        while !read_polled.load(Ordering::Acquire) {
            tokio::task::yield_now().await;
        }
    })
    .await;
    pending.cancel();
    assert_canceled(pending).await;

    let rewind_polled = Arc::new(AtomicBool::new(false));
    let mut pending = engine
        .start(
            Request::builder(server.url("/redirect307"))
                .unwrap()
                .rewindable_body_stream(
                    PendingRewind {
                        polled: rewind_polled.clone(),
                        body: b"rewind-me",
                        cursor: 0,
                    },
                    Some(9),
                )
                .build()
                .unwrap(),
        )
        .unwrap();
    tokio::select! {
        () = async {
            while !rewind_polled.load(Ordering::Acquire) {
                tokio::task::yield_now().await;
            }
        } => {}
        result = &mut pending => panic!("request finished before rewind became pending: {result:?}"),
        () = tokio::time::sleep(TIMEOUT) => panic!("Cronet did not request an upload rewind"),
    }
    pending.cancel();
    assert_canceled(pending).await;

    timeout("pending upload engine shutdown", engine.shutdown())
        .await
        .unwrap();
}

pub async fn cancellation_and_shutdown_races_are_safe() {
    let server = TestServer::start();
    let engine = Engine::builder().build().unwrap();
    let mut pending = Vec::new();
    for index in 0..16 {
        pending.push(
            engine
                .start(
                    Request::builder(server.url("/slow-headers"))
                        .unwrap()
                        .annotation(format!("portable=shutdown-{index}"))
                        .unwrap()
                        .build()
                        .unwrap(),
                )
                .unwrap(),
        );
    }

    let mut shutdowns = Vec::new();
    for _ in 0..8 {
        let engine = engine.clone();
        shutdowns.push(tokio::spawn(async move { engine.shutdown().await }));
    }
    for request in pending {
        assert_canceled(request).await;
    }
    for shutdown in shutdowns {
        timeout("concurrent engine shutdown", shutdown)
            .await
            .expect("join concurrent shutdown task")
            .unwrap();
    }

    engine.shutdown().await.unwrap();
    assert!(matches!(engine.version(), Err(Error::EngineShutdown)));
    assert!(matches!(
        engine.default_user_agent(),
        Err(Error::EngineShutdown)
    ));
    assert!(matches!(
        engine.stop_net_log().await,
        Err(Error::EngineShutdown)
    ));
    assert!(matches!(
        engine.start(
            Request::builder(server.url("/ok"))
                .unwrap()
                .build()
                .unwrap()
        ),
        Err(Error::EngineShutdown)
    ));
    assert!(matches!(
        engine
            .send(
                Request::builder(server.url("/ok"))
                    .unwrap()
                    .build()
                    .unwrap()
            )
            .await,
        Err(Error::EngineShutdown)
    ));
    assert!(matches!(
        engine
            .execute(
                Request::builder(server.url("/ok"))
                    .unwrap()
                    .build()
                    .unwrap()
            )
            .await,
        Err(Error::EngineShutdown)
    ));
}

#[cfg(feature = "sse")]
pub async fn sse_event_source_covers_events_reconnect_and_cancel() {
    let server = TestServer::start();
    let engine = Engine::builder().build().unwrap();
    let mut events = timeout(
        "sse open",
        tokio_cronet::EventSource::builder(server.url("/sse"))
            .unwrap()
            .open(&engine),
    )
    .await
    .unwrap();
    let first = timeout("sse first event", events.next_event())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(first.event, "greeting");
    assert_eq!(first.data, "hello\nstream");
    assert_eq!(first.id.as_deref(), Some("1"));
    let second = timeout("sse second event", events.next_event())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(second.event, "message");
    assert_eq!(second.data, "world");
    events.cancel();
    assert_eq!(events.last_event_id(), Some("1"));
    let _ = events.handle();
    let _ = events.is_done();
    drop(events);

    let mut reconnect = timeout(
        "sse reconnect open",
        engine
            .event_source(server.url("/sse-id"))
            .unwrap()
            .auto_reconnect(true)
            .retry(Duration::from_millis(20))
            .last_event_id("1")
            .max_response_bytes(usize::MAX)
            .header("x-cronet-test", "sse")
            .unwrap()
            .open(&engine),
    )
    .await
    .unwrap();
    let event = timeout("sse reconnect event", reconnect.next_event())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(event.id.as_deref(), Some("2"));
    assert_eq!(event.data, "second");
    reconnect.cancel();
    engine.shutdown().await.unwrap();
}

#[cfg(feature = "ws")]
pub async fn websocket_echo_and_close_are_terminal() {
    let server = TestServer::start();
    let engine = Engine::builder().build().unwrap();
    assert!(tokio_cronet::WebSocket::builder(format!("ws://{}/ws", server.address())).is_ok());
    let builder = engine
        .websocket(format!("ws://{}/ws", server.address()))
        .unwrap()
        .protocol("echo")
        .unwrap()
        .origin("http://127.0.0.1")
        .unwrap()
        .header("x-cronet-test", "ws")
        .unwrap();
    let mut socket = timeout("websocket open", engine.open_websocket(builder))
        .await
        .unwrap();
    timeout("websocket send text", socket.send_text("hello"))
        .await
        .unwrap();
    let message = timeout("websocket echo", socket.next_message())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(message, tokio_cronet::WsMessage::Text("hello".into()));
    timeout("websocket send binary", socket.send_binary(b"bin".to_vec()))
        .await
        .unwrap();
    let message = timeout("websocket binary echo", socket.next_message())
        .await
        .unwrap()
        .unwrap();
    match message {
        tokio_cronet::WsMessage::Binary(payload) => assert_eq!(&payload[..], b"bin"),
        other => panic!("expected binary echo, got {other:?}"),
    }
    let _ = socket.protocol();
    let _ = socket.is_done();
    timeout("websocket close", socket.close(1000, "bye"))
        .await
        .unwrap();
    timeout("websocket terminal", async {
        while !socket.is_done() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    socket.cancel();
    drop(socket);
    engine.shutdown().await.unwrap();
}

#[cfg(feature = "nqe")]
pub async fn nqe_observes_localhost_after_testing_override() {
    let server = TestServer::start();
    let engine = Engine::builder()
        .enable_network_quality_estimator(true)
        .build()
        .unwrap();
    engine
        .configure_network_quality_estimator_for_testing(true, true, true)
        .unwrap();
    let mut updates = engine.subscribe_network_quality().unwrap();
    for _ in 0..3 {
        let _ = engine
            .execute(
                Request::builder(server.url("/ok"))
                    .unwrap()
                    .build()
                    .unwrap(),
            )
            .await
            .unwrap();
    }
    let _ = timeout("nqe update", updates.changed()).await;
    let quality = engine.network_quality().unwrap();
    let _ = engine.effective_connection_type().unwrap();
    let _ = engine.http_rtt().unwrap();
    let _ = engine.transport_rtt().unwrap();
    let _ = engine.downstream_throughput_kbps().unwrap();
    let _ = quality;
    engine.shutdown().await.unwrap();
}

#[cfg(feature = "network-binding")]
pub async fn network_binding_round_trips_engine_handle() {
    let server = TestServer::start();
    let engine = Engine::builder().build().unwrap();
    assert_eq!(
        engine.bound_network().unwrap(),
        tokio_cronet::UNBIND_NETWORK_HANDLE
    );
    engine.bind_to_network(42).unwrap();
    assert_eq!(engine.bound_network().unwrap(), 42);
    engine
        .bind_to_network(tokio_cronet::UNBIND_NETWORK_HANDLE)
        .unwrap();
    let response = engine
        .execute(
            Request::builder(server.url("/ok"))
                .unwrap()
                .build()
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    engine.shutdown().await.unwrap();
}

pub async fn engine_drop_with_active_work_is_process_safe() {
    let server = TestServer::start();
    // Repeatedly drop both sides of an active ownership graph. This scenario
    // runs last because EngineInner deliberately completes native shutdown on
    // background threads after the final public handle has gone away.
    for _ in 0..8 {
        let engine = Engine::builder().build().unwrap();
        let request = engine
            .start(
                Request::builder(server.url("/slow-headers"))
                    .unwrap()
                    .build()
                    .unwrap(),
            )
            .unwrap();
        drop(request);
        drop(engine);
    }
    tokio::time::sleep(Duration::from_secs(2)).await;
}

pub async fn bidirectional_configuration_and_failure_are_safe() {
    assert!(
        BidirectionalRequest::builder("https://example.com")
            .unwrap()
            .read_buffer_size(0)
            .build()
            .is_err()
    );
    assert!(
        BidirectionalRequest::builder("https://example.com")
            .unwrap()
            .read_channel_capacity(0)
            .build()
            .is_err()
    );
    assert!(
        BidirectionalRequest::builder("https://example.com")
            .unwrap()
            .write_capacity(0)
            .build()
            .is_err()
    );

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    let engine = Engine::builder().build().unwrap();
    let request = engine
        .bidirectional_request(format!("http://{address}/grpc"))
        .unwrap()
        .method("POST")
        .unwrap()
        .header("content-type", "application/grpc")
        .unwrap()
        .priority(Priority::Low)
        .read_buffer_size(1024)
        .read_channel_capacity(1)
        .write_capacity(1)
        .disable_auto_flush(true)
        .delay_headers_until_flush(true)
        .end_of_stream(true)
        .build()
        .unwrap();
    let result = timeout(
        "bidirectional connection failure",
        engine.open_bidirectional(request),
    )
    .await;
    assert!(result.is_err());
    engine.shutdown().await.unwrap();
}

fn annotated_pending(
    engine: &Engine,
    server: &TestServer,
    name: &str,
) -> tokio_cronet::PendingRequest {
    let pending = engine
        .start(
            Request::builder(server.url("/slow-headers"))
                .unwrap()
                .annotation(format!("portable={name}"))
                .unwrap()
                .build()
                .unwrap(),
        )
        .unwrap();
    assert!(!pending.is_done());
    pending
}

async fn assert_canceled(request: tokio_cronet::PendingRequest) {
    assert!(matches!(
        timeout("pending request cancellation", request).await,
        Err(Error::Canceled)
    ));
}

async fn wait_done(handle: &tokio_cronet::RequestHandle) {
    timeout("request handle terminal state", async {
        while !handle.is_done() {
            tokio::task::yield_now().await;
        }
    })
    .await;
}

async fn recv_annotation(
    events: &mut tokio::sync::broadcast::Receiver<RequestFinishedInfo>,
    annotation: &str,
) -> RequestFinishedInfo {
    timeout("request-finished annotation", async {
        loop {
            let event = events
                .recv()
                .await
                .expect("request-finished broadcast closed");
            if event.annotations.iter().any(|value| value == annotation) {
                return event;
            }
        }
    })
    .await
}

async fn timeout<T>(name: &'static str, future: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(TIMEOUT, future)
        .await
        .unwrap_or_else(|_| panic!("portable Cronet E2E operation timed out: {name}"))
}

fn assert_metrics_are_consistent(info: &RequestFinishedInfo) {
    let metrics = &info.metrics;
    let start = metrics.request_start.expect("request_start metric");
    let end = metrics.request_end.expect("request_end metric");
    assert!(start <= end);
    for (phase_start, phase_end) in [
        (metrics.dns_start, metrics.dns_end),
        (metrics.connect_start, metrics.connect_end),
        (metrics.ssl_start, metrics.ssl_end),
        (metrics.sending_start, metrics.sending_end),
        (metrics.push_start, metrics.push_end),
    ] {
        if let (Some(phase_start), Some(phase_end)) = (phase_start, phase_end) {
            assert!(start <= phase_start);
            assert!(phase_start <= phase_end);
            assert!(phase_end <= end);
        }
    }
    if let Some(response_start) = metrics.response_start {
        assert!(start <= response_start && response_start <= end);
    }
    assert!(metrics.sent_byte_count >= 0);
    assert!(metrics.received_byte_count >= 0);
    assert!(info.response.is_some());
    assert!(info.error.is_none());
}

struct FailingRead;

impl AsyncRead for FailingRead {
    fn poll_read(
        self: Pin<&mut Self>,
        _context: &mut Context<'_>,
        _output: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Poll::Ready(Err(io::Error::other("injected read failure")))
    }
}

struct PendingRead(Arc<AtomicBool>);

impl AsyncRead for PendingRead {
    fn poll_read(
        self: Pin<&mut Self>,
        _context: &mut Context<'_>,
        _output: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        self.0.store(true, Ordering::Release);
        Poll::Pending
    }
}

struct PendingRewind {
    polled: Arc<AtomicBool>,
    body: &'static [u8],
    cursor: usize,
}

impl AsyncRead for PendingRewind {
    fn poll_read(
        mut self: Pin<&mut Self>,
        _context: &mut Context<'_>,
        output: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let count = (self.body.len() - self.cursor).min(output.remaining());
        output.put_slice(&self.body[self.cursor..self.cursor + count]);
        self.cursor += count;
        Poll::Ready(Ok(()))
    }
}

impl AsyncSeek for PendingRewind {
    fn start_seek(self: Pin<&mut Self>, _position: SeekFrom) -> io::Result<()> {
        Ok(())
    }

    fn poll_complete(self: Pin<&mut Self>, _context: &mut Context<'_>) -> Poll<io::Result<u64>> {
        self.polled.store(true, Ordering::Release);
        Poll::Pending
    }
}

struct PendingRedirect {
    entered: Arc<AtomicBool>,
    dropped: Arc<AtomicBool>,
}

impl PendingRedirect {
    fn new(entered: Arc<AtomicBool>, dropped: Arc<AtomicBool>) -> Self {
        Self { entered, dropped }
    }
}

impl Future for PendingRedirect {
    type Output = RedirectAction;

    fn poll(self: Pin<&mut Self>, _context: &mut Context<'_>) -> Poll<Self::Output> {
        self.entered.store(true, Ordering::Release);
        Poll::Pending
    }
}

impl Drop for PendingRedirect {
    fn drop(&mut self) {
        self.dropped.store(true, Ordering::Release);
    }
}

struct FailingRewind {
    body: &'static [u8],
    cursor: usize,
}

impl FailingRewind {
    fn new(body: &'static [u8]) -> Self {
        Self { body, cursor: 0 }
    }
}

impl AsyncRead for FailingRewind {
    fn poll_read(
        mut self: Pin<&mut Self>,
        _context: &mut Context<'_>,
        output: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let count = (self.body.len() - self.cursor).min(output.remaining());
        output.put_slice(&self.body[self.cursor..self.cursor + count]);
        self.cursor += count;
        Poll::Ready(Ok(()))
    }
}

impl AsyncSeek for FailingRewind {
    fn start_seek(self: Pin<&mut Self>, _position: SeekFrom) -> io::Result<()> {
        Ok(())
    }

    fn poll_complete(self: Pin<&mut Self>, _context: &mut Context<'_>) -> Poll<io::Result<u64>> {
        Poll::Ready(Err(io::Error::other("injected seek failure")))
    }
}

struct TestServer {
    address: SocketAddr,
    stopping: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}

impl TestServer {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind portable E2E server");
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let stopping = Arc::new(AtomicBool::new(false));
        let worker_stopping = stopping.clone();
        let worker = thread::spawn(move || {
            while !worker_stopping.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        stream
                            .set_nonblocking(false)
                            .expect("make accepted portable E2E socket blocking");
                        thread::spawn(move || handle_connection(stream));
                    }
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(2));
                    }
                    Err(error) => panic!("portable E2E accept failed: {error}"),
                }
            }
        });
        Self {
            address,
            stopping,
            worker: Some(worker),
        }
    }

    fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.address)
    }

    fn address(&self) -> SocketAddr {
        self.address
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.stopping.store(true, Ordering::Release);
        let _ = TcpStream::connect(self.address);
        if let Some(worker) = self.worker.take() {
            worker.join().unwrap();
        }
    }
}

struct TestRequest {
    method: String,
    path: String,
    headers: HashMap<String, String>,
    body: Vec<u8>,
    body_complete: bool,
}

fn handle_connection(mut stream: TcpStream) {
    let request = match read_request(&stream) {
        Ok(request) => request,
        Err(error) => {
            // Upload-error and cancellation scenarios intentionally close a
            // declared request body early. Return a valid response when the
            // socket is still writable so that a loopback EOF cannot race the
            // binding's more specific terminal error.
            if matches!(
                error.kind(),
                io::ErrorKind::UnexpectedEof
                    | io::ErrorKind::WouldBlock
                    | io::ErrorKind::TimedOut
                    | io::ErrorKind::ConnectionReset
            ) {
                respond(&mut stream, 400, "Bad Request", &[], b"incomplete request");
            } else {
                eprintln!("portable E2E server could not read request: {error}");
            }
            return;
        }
    };
    if !request.body_complete {
        if request.path == "/redirect307" {
            respond(
                &mut stream,
                307,
                "Temporary Redirect",
                &[("Location", "/echo")],
                b"",
            );
        } else {
            respond(&mut stream, 400, "Bad Request", &[], b"incomplete request");
        }
        return;
    }
    match request.path.as_str() {
        "/inspect" => {
            let mut body = format!(
                "{}|{}|{}|{}|",
                request.method,
                header(&request, "user-agent"),
                header(&request, "accept-language"),
                header(&request, "x-cronet-test")
            )
            .into_bytes();
            body.extend_from_slice(&request.body);
            respond(&mut stream, 200, "OK", &[], &body);
        }
        "/echo" => respond(&mut stream, 201, "Created", &[], &request.body),
        "/ok" => respond(&mut stream, 200, "OK", &[], b"ok"),
        "/empty" => respond(&mut stream, 204, "No Content", &[], b""),
        "/duplicate-headers" => respond(
            &mut stream,
            200,
            "OK",
            &[("Set-Cookie", "first=1"), ("Set-Cookie", "second=2")],
            b"headers",
        ),
        "/chunks" => {
            let _ = stream.write_all(
                b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n3\r\none\r\n3\r\ntwo\r\n5\r\nthree\r\n0\r\n\r\n",
            );
            finish(&mut stream);
        }
        "/redirect" => respond(&mut stream, 302, "Found", &[("Location", "/final")], b""),
        "/redirect307" => respond(
            &mut stream,
            307,
            "Temporary Redirect",
            &[("Location", "/echo")],
            b"",
        ),
        "/final" => respond(&mut stream, 200, "OK", &[], b"redirected"),
        "/large" => respond(&mut stream, 200, "OK", &[], b"0123456789"),
        "/slow-headers" => {
            thread::sleep(Duration::from_millis(750));
            respond(&mut stream, 200, "OK", &[], b"slow");
        }
        "/slow-body" => {
            let _ = stream.write_all(
                b"HTTP/1.1 200 OK\r\nContent-Length: 1048576\r\nConnection: close\r\n\r\nx",
            );
            let _ = stream.flush();
            thread::sleep(Duration::from_secs(2));
        }
        "/truncated" => {
            let _ = stream.write_all(
                b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\nConnection: close\r\n\r\nshort",
            );
            finish(&mut stream);
        }
        "/sse" => {
            let _ = stream.write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nConnection: close\r\n\r\n: ping\nevent: greeting\nid: 1\ndata: hello\ndata: stream\n\ndata: world\n\n",
            );
            finish(&mut stream);
        }
        "/sse-id" => {
            let last = header(&request, "last-event-id");
            let body = if last == "1" {
                b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nConnection: close\r\n\r\nid: 2\ndata: second\n\n".as_slice()
            } else {
                b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nConnection: close\r\n\r\nid: 1\ndata: first\n\n".as_slice()
            };
            let _ = stream.write_all(body);
            finish(&mut stream);
        }
        "/ws" => handle_websocket(&mut stream, &request),
        _ => respond(&mut stream, 404, "Not Found", &[], b"missing"),
    }
}

fn header<'a>(request: &'a TestRequest, name: &str) -> &'a str {
    request.headers.get(name).map_or("", String::as_str)
}

fn handle_websocket(stream: &mut TcpStream, request: &TestRequest) {
    let key = header(request, "sec-websocket-key");
    if key.is_empty() {
        respond(stream, 400, "Bad Request", &[], b"missing websocket key");
        return;
    }
    let accept = websocket_accept(key);
    let protocol = header(request, "sec-websocket-protocol")
        .split(',')
        .next()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let _ = write!(
        stream,
        "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n"
    );
    if let Some(protocol) = protocol {
        let _ = write!(stream, "Sec-WebSocket-Protocol: {protocol}\r\n");
    }
    let _ = write!(stream, "\r\n");
    let _ = stream.flush();
    loop {
        let (opcode, payload) = match read_ws_frame(stream) {
            Ok(frame) => frame,
            Err(_) => return,
        };
        match opcode {
            0x1 | 0x2 => {
                if write_ws_frame(stream, opcode, &payload).is_err() {
                    return;
                }
            }
            0x8 => {
                let _ = write_ws_frame(stream, 0x8, &payload);
                return;
            }
            0x9 => {
                let _ = write_ws_frame(stream, 0xA, &payload);
            }
            _ => {}
        }
    }
}

fn websocket_accept(key: &str) -> String {
    let mut handshake = Vec::with_capacity(key.len() + 36);
    handshake.extend_from_slice(key.as_bytes());
    handshake.extend_from_slice(b"258EAFA5-E914-47DA-95CA-C5AB0DC85B11");
    base64_encode(&sha1(&handshake))
}

fn read_ws_frame(stream: &mut TcpStream) -> io::Result<(u8, Vec<u8>)> {
    let mut header = [0u8; 2];
    stream.read_exact(&mut header)?;
    let opcode = header[0] & 0x0f;
    let masked = header[1] & 0x80 != 0;
    let mut len = u64::from(header[1] & 0x7f);
    if len == 126 {
        let mut ext = [0u8; 2];
        stream.read_exact(&mut ext)?;
        len = u64::from(u16::from_be_bytes(ext));
    } else if len == 127 {
        let mut ext = [0u8; 8];
        stream.read_exact(&mut ext)?;
        len = u64::from_be_bytes(ext);
    }
    let mut mask = [0u8; 4];
    if masked {
        stream.read_exact(&mut mask)?;
    }
    let mut payload = vec![0u8; len as usize];
    stream.read_exact(&mut payload)?;
    if masked {
        for (index, byte) in payload.iter_mut().enumerate() {
            *byte ^= mask[index % 4];
        }
    }
    Ok((opcode, payload))
}

fn write_ws_frame(stream: &mut TcpStream, opcode: u8, payload: &[u8]) -> io::Result<()> {
    let mut frame = Vec::with_capacity(payload.len() + 10);
    frame.push(0x80 | opcode);
    if payload.len() < 126 {
        frame.push(payload.len() as u8);
    } else if payload.len() <= u16::MAX as usize {
        frame.push(126);
        frame.extend_from_slice(&(payload.len() as u16).to_be_bytes());
    } else {
        frame.push(127);
        frame.extend_from_slice(&(payload.len() as u64).to_be_bytes());
    }
    frame.extend_from_slice(payload);
    stream.write_all(&frame)?;
    stream.flush()
}

fn sha1(data: &[u8]) -> [u8; 20] {
    let mut message = data.to_vec();
    let bit_len = data.len() as u64 * 8;
    message.push(0x80);
    while message.len() % 64 != 56 {
        message.push(0);
    }
    message.extend_from_slice(&bit_len.to_be_bytes());
    let mut hash: [u32; 5] = [
        0x6745_2301,
        0xEFCD_AB89,
        0x98BA_DCFE,
        0x1032_5476,
        0xC3D2_E1F0,
    ];
    for chunk in message.chunks_exact(64) {
        let mut words = [0u32; 80];
        for (index, word) in words.iter_mut().enumerate().take(16) {
            *word = u32::from_be_bytes(chunk[index * 4..index * 4 + 4].try_into().unwrap());
        }
        for index in 16..80 {
            words[index] =
                (words[index - 3] ^ words[index - 8] ^ words[index - 14] ^ words[index - 16])
                    .rotate_left(1);
        }
        let mut a = hash[0];
        let mut b = hash[1];
        let mut c = hash[2];
        let mut d = hash[3];
        let mut e = hash[4];
        for (index, word) in words.iter().enumerate() {
            let (f, k) = match index {
                0..=19 => ((b & c) | ((!b) & d), 0x5A82_7999),
                20..=39 => (b ^ c ^ d, 0x6ED9_EBA1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1B_BCDC),
                _ => (b ^ c ^ d, 0xCA62_C1D6),
            };
            let temp = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(*word);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = temp;
        }
        hash[0] = hash[0].wrapping_add(a);
        hash[1] = hash[1].wrapping_add(b);
        hash[2] = hash[2].wrapping_add(c);
        hash[3] = hash[3].wrapping_add(d);
        hash[4] = hash[4].wrapping_add(e);
    }
    let mut output = [0u8; 20];
    for (index, value) in hash.iter().enumerate() {
        output[index * 4..index * 4 + 4].copy_from_slice(&value.to_be_bytes());
    }
    output
}

fn base64_encode(data: &[u8]) -> String {
    const TABLE: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut output = String::new();
    let mut index = 0;
    while index + 3 <= data.len() {
        let n = (u32::from(data[index]) << 16)
            | (u32::from(data[index + 1]) << 8)
            | u32::from(data[index + 2]);
        output.push(TABLE[((n >> 18) & 63) as usize] as char);
        output.push(TABLE[((n >> 12) & 63) as usize] as char);
        output.push(TABLE[((n >> 6) & 63) as usize] as char);
        output.push(TABLE[(n & 63) as usize] as char);
        index += 3;
    }
    match data.len() - index {
        1 => {
            let n = u32::from(data[index]) << 16;
            output.push(TABLE[((n >> 18) & 63) as usize] as char);
            output.push(TABLE[((n >> 12) & 63) as usize] as char);
            output.push('=');
            output.push('=');
        }
        2 => {
            let n = (u32::from(data[index]) << 16) | (u32::from(data[index + 1]) << 8);
            output.push(TABLE[((n >> 18) & 63) as usize] as char);
            output.push(TABLE[((n >> 12) & 63) as usize] as char);
            output.push(TABLE[((n >> 6) & 63) as usize] as char);
            output.push('=');
        }
        _ => {}
    }
    output
}

fn read_request(stream: &TcpStream) -> io::Result<TestRequest> {
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut request_line = String::new();
    reader.read_line(&mut request_line)?;
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_owned();
    let path = normalize_request_target(parts.next().unwrap_or_default());
    let mut headers = HashMap::new();
    loop {
        let mut line = String::new();
        reader.read_line(&mut line)?;
        if line == "\r\n" || line.is_empty() {
            break;
        }
        if let Some((name, value)) = line.trim_end().split_once(':') {
            headers.insert(name.to_ascii_lowercase(), value.trim().to_owned());
        }
    }
    if headers
        .get("expect")
        .is_some_and(|value| value.eq_ignore_ascii_case("100-continue"))
    {
        let mut writer = stream.try_clone()?;
        writer.write_all(b"HTTP/1.1 100 Continue\r\n\r\n")?;
        writer.flush()?;
    }
    let (body, body_complete) = if let Some(length) = headers.get("content-length") {
        let length = length.parse::<usize>().map_err(io::Error::other)?;
        let mut body = Vec::with_capacity(length);
        let read = reader
            .by_ref()
            .take(u64::try_from(length).expect("request length fits u64"))
            .read_to_end(&mut body);
        let complete = read.is_ok() && body.len() == length;
        (body, complete)
    } else if headers
        .get("transfer-encoding")
        .is_some_and(|value| value.eq_ignore_ascii_case("chunked"))
    {
        (read_chunked(&mut reader)?, true)
    } else {
        (Vec::new(), true)
    };
    Ok(TestRequest {
        method,
        path,
        headers,
        body,
        body_complete,
    })
}

fn normalize_request_target(target: &str) -> String {
    for scheme in ["http://", "https://"] {
        if let Some(remainder) = target.strip_prefix(scheme) {
            return remainder
                .find('/')
                .map_or_else(|| "/".to_owned(), |index| remainder[index..].to_owned());
        }
    }
    target.to_owned()
}

fn read_chunked(reader: &mut impl BufRead) -> io::Result<Vec<u8>> {
    let mut body = Vec::new();
    loop {
        let mut size = String::new();
        reader.read_line(&mut size)?;
        let size = size.trim_end().split(';').next().unwrap_or_default();
        let size = usize::from_str_radix(size, 16).map_err(io::Error::other)?;
        if size == 0 {
            loop {
                let mut trailer = String::new();
                reader.read_line(&mut trailer)?;
                if trailer == "\r\n" || trailer.is_empty() {
                    return Ok(body);
                }
            }
        }
        let start = body.len();
        body.resize(start + size, 0);
        reader.read_exact(&mut body[start..])?;
        let mut delimiter = [0; 2];
        reader.read_exact(&mut delimiter)?;
        if delimiter != *b"\r\n" {
            return Err(io::Error::other("invalid chunk delimiter"));
        }
    }
}

fn respond(
    stream: &mut TcpStream,
    status: u16,
    reason: &str,
    headers: &[(&str, &str)],
    body: &[u8],
) {
    let _ = write!(
        stream,
        "HTTP/1.1 {status} {reason}\r\nContent-Length: {}\r\nConnection: close\r\n",
        body.len()
    );
    for (name, value) in headers {
        let _ = write!(stream, "{name}: {value}\r\n");
    }
    let _ = stream.write_all(b"\r\n");
    let _ = stream.write_all(body);
    finish(stream);
}

fn finish(stream: &mut TcpStream) {
    let _ = stream.flush();
    // Keep the socket alive briefly after the declared body is flushed. Some
    // platform stacks can otherwise race the loopback FIN with Cronet's final
    // read callback and report ERR_SOCKET_NOT_CONNECTED instead of the
    // binding-level condition the scenario is intended to exercise.
    thread::sleep(Duration::from_millis(50));
}

#[cfg(test)]
mod tests {
    use super::normalize_request_target;

    #[test]
    fn accepts_origin_and_proxy_absolute_request_targets() {
        assert_eq!(normalize_request_target("/redirect307"), "/redirect307");
        assert_eq!(
            normalize_request_target("http://127.0.0.1:8080/redirect307?one=two"),
            "/redirect307?one=two"
        );
        assert_eq!(normalize_request_target("https://example.com"), "/");
    }
}

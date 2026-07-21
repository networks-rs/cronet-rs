#![cfg(feature = "native-tests")]

mod support;

use std::{
    future::poll_fn,
    io::{Cursor, SeekFrom},
    path::PathBuf,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    task::{Context, Poll},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use cronet::{
    CacheMode, Engine, Error, FinishedReason, Idempotency, Priority, PublicKeyPins, QuicHint,
    RedirectAction, Request, RequestStatus,
};
use futures_core::Stream;
use support::TestServer;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncSeek, AsyncWriteExt, ReadBuf};

fn temporary_path(name: &str) -> PathBuf {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is after epoch")
        .as_nanos();
    std::env::temp_dir().join(format!("cronet-rs-{name}-{}-{unique}", std::process::id()))
}

async fn assert_disk_no_http_mode(server: &TestServer) {
    let disk = temporary_path("disk-no-http-cache");
    std::fs::create_dir(&disk).unwrap();
    let engine = Engine::builder()
        .storage_path(&disk)
        .cache_mode(CacheMode::DiskNoHttp {
            max_size: 4 * 1024 * 1024,
        })
        .build()
        .unwrap();
    let network_count = server.cache_requests();
    let path = format!("/cache?disk-no-http={}", std::process::id());
    for _ in 0..2 {
        let response = engine
            .execute(
                Request::builder(server.url(&path))
                    .unwrap()
                    .build()
                    .unwrap(),
            )
            .await
            .unwrap();
        assert!(!response.info.was_cached);
    }
    assert_eq!(server.cache_requests(), network_count + 2);
    engine.shutdown().await.unwrap();
    std::fs::remove_dir_all(&disk).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[allow(clippy::too_many_lines)]
async fn request_api_covers_tokio_io_redirects_metrics_status_and_netlog() {
    let server = TestServer::start();
    let netlog = temporary_path("netlog.json");
    let pin = PublicKeyPins::new(
        "unused.invalid",
        ["sha256/AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA="],
        false,
        i64::MAX,
    )
    .unwrap();
    let engine = Engine::builder()
        .user_agent("cronet-rs/e2e")
        .accept_language("zh-CN,en")
        .enable_quic(true)
        .enable_http2(true)
        .enable_brotli(true)
        .bypass_pinning_for_local_trust_anchors(true)
        .experimental_options("{}")
        .quic_hint(QuicHint::new("unused.invalid", 443, 443).unwrap())
        .public_key_pins(pin)
        .build()
        .unwrap();
    assert!(!engine.version().unwrap().is_empty());
    assert!(!engine.default_user_agent().unwrap().is_empty());
    assert!(engine.start_net_log(&netlog, true).unwrap());

    let get = engine
        .request(server.url("/inspect"))
        .unwrap()
        .header("x-cronet-test", "get")
        .unwrap()
        .priority(Priority::Highest)
        .idempotency(Idempotency::Idempotent)
        .allow_direct_executor(true)
        .build()
        .unwrap();
    let response = engine.execute(get).await.unwrap();
    let inspected = String::from_utf8(response.body().to_vec()).unwrap();
    assert_eq!(response.status(), 200);
    assert!(inspected.starts_with("GET|cronet-rs/e2e|zh-CN,en|get|"));
    assert_eq!(response.info.status_text, "OK");
    assert!(response.info.headers.iter().any(|header| {
        header.name().eq_ignore_ascii_case("x-e2e") && header.value() == "inspect"
    }));

    // Leaving the method unset exercises Cronet's native POST default when an
    // upload provider is present.
    let default_post = engine
        .request(server.url("/inspect"))
        .unwrap()
        .header("x-cronet-test", "static")
        .unwrap()
        .body("static-body")
        .build()
        .unwrap();
    let response = engine.execute(default_post).await.unwrap();
    assert!(
        String::from_utf8(response.body().to_vec())
            .unwrap()
            .ends_with("POST|cronet-rs/e2e|zh-CN,en|static|static-body")
    );

    let (mut upload_writer, upload_reader) = tokio::io::duplex(64);
    tokio::spawn(async move {
        upload_writer.write_all(b"known-stream").await.unwrap();
        upload_writer.shutdown().await.unwrap();
    });
    let known_upload = Request::builder(server.url("/echo"))
        .unwrap()
        .body_stream(upload_reader, Some(12))
        .build()
        .unwrap();
    assert_eq!(
        engine.execute(known_upload).await.unwrap().body(),
        b"known-stream"
    );

    let (mut upload_writer, upload_reader) = tokio::io::duplex(64);
    tokio::spawn(async move {
        upload_writer.write_all(b"chunked-stream").await.unwrap();
        upload_writer.shutdown().await.unwrap();
    });
    let unknown_upload = Request::builder(server.url("/echo"))
        .unwrap()
        .body_stream(upload_reader, None)
        .build()
        .unwrap();
    assert_eq!(
        engine.execute(unknown_upload).await.unwrap().body(),
        b"chunked-stream"
    );

    let rewindable = Request::builder(server.url("/redirect307"))
        .unwrap()
        .rewindable_body_stream(Cursor::new(b"rewound-upload".to_vec()), Some(14))
        .build()
        .unwrap();
    let rewindable = engine.execute(rewindable).await.unwrap();
    assert_eq!(rewindable.status(), 201);
    assert_eq!(rewindable.body(), b"rewound-upload");
    assert_eq!(rewindable.info.url_chain.len(), 2);

    let automatic = Request::builder(server.url("/redirect"))
        .unwrap()
        .build()
        .unwrap();
    let automatic = engine.execute(automatic).await.unwrap();
    assert_eq!(automatic.body(), b"redirected");
    assert_eq!(automatic.info.url_chain.len(), 2);

    let mut async_read_response = engine
        .send(
            Request::builder(server.url("/chunks"))
                .unwrap()
                .build()
                .unwrap(),
        )
        .await
        .unwrap();
    let mut async_read_body = Vec::new();
    async_read_response
        .body
        .read_to_end(&mut async_read_body)
        .await
        .unwrap();
    assert_eq!(async_read_body, b"onetwothree");

    let rejected = Request::builder(server.url("/redirect"))
        .unwrap()
        .follow_redirects(false)
        .build()
        .unwrap();
    match engine.send(rejected).await.unwrap_err() {
        Error::Redirect { location, response } => {
            assert!(location.ends_with("/final"));
            assert_eq!(response.status_code, 302);
            assert!(response.headers.iter().any(|header| {
                header.name().eq_ignore_ascii_case("x-redirect") && header.value() == "yes"
            }));
        }
        error => panic!("unexpected redirect error: {error:?}"),
    }

    let observed_redirect = Arc::new(Mutex::new(None));
    let handler_observation = observed_redirect.clone();
    let asynchronous = Request::builder(server.url("/async-redirect"))
        .unwrap()
        .redirect_handler(move |redirect| {
            let observation = handler_observation.clone();
            async move {
                *observation
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(redirect);
                tokio::time::sleep(Duration::from_millis(20)).await;
                RedirectAction::Follow
            }
        })
        .build()
        .unwrap();
    assert_eq!(
        engine.execute(asynchronous).await.unwrap().body(),
        b"redirected"
    );
    assert_eq!(
        observed_redirect
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .expect("redirect handler observed metadata")
            .response
            .status_code,
        302
    );

    let asynchronous_cancel = Request::builder(server.url("/async-redirect"))
        .unwrap()
        .redirect_handler(|_| async {
            tokio::task::yield_now().await;
            RedirectAction::Cancel
        })
        .build()
        .unwrap();
    assert!(matches!(
        engine.send(asynchronous_cancel).await,
        Err(Error::Redirect { .. })
    ));

    let short_upload = Request::builder(server.url("/echo"))
        .unwrap()
        .body_stream(Cursor::new(b"short".to_vec()), Some(8))
        .build()
        .unwrap();
    assert!(matches!(
        engine.execute(short_upload).await,
        Err(Error::Upload(_))
    ));
    let non_rewindable = Request::builder(server.url("/redirect307"))
        .unwrap()
        .body_stream(Cursor::new(b"one-shot".to_vec()), Some(8))
        .build()
        .unwrap();
    assert!(matches!(
        engine.execute(non_rewindable).await,
        Err(Error::Upload(_))
    ));

    let chunked = Request::builder(server.url("/chunks"))
        .unwrap()
        .read_buffer_size(3)
        .body_channel_capacity(1)
        .build()
        .unwrap();
    let mut response = engine.send(chunked).await.unwrap();
    let mut streamed = Vec::new();
    while let Some(chunk) = poll_fn(|context| Pin::new(&mut response.body).poll_next(context)).await
    {
        streamed.extend_from_slice(&chunk.unwrap());
    }
    assert_eq!(streamed, b"onetwothree");
    assert!(response.body.is_done());

    let too_large = Request::builder(server.url("/large"))
        .unwrap()
        .read_buffer_size(4)
        .max_response_bytes(5)
        .build()
        .unwrap();
    assert!(matches!(
        engine.execute(too_large).await,
        Err(Error::ResponseTooLarge { limit: 5 })
    ));

    let brotli = engine
        .execute(
            Request::builder(server.url("/brotli"))
                .unwrap()
                .build()
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(brotli.body(), b"brotli-decoded-by-cronet");

    let pending = engine
        .start(
            Request::builder(server.url("/slow-headers"))
                .unwrap()
                .build()
                .unwrap(),
        )
        .unwrap();
    let pending_handle = pending.handle();
    let status = tokio::time::timeout(Duration::from_secs(2), pending.request_status())
        .await
        .expect("status query timed out")
        .unwrap();
    assert_ne!(status, RequestStatus::Invalid);
    let slow_response = tokio::select! {
        response = pending => response.unwrap(),
        () = tokio::time::sleep(Duration::from_secs(2)) => panic!("pending request timed out"),
    };
    assert_eq!(slow_response.status(), 200);
    assert!(!pending_handle.is_done() || slow_response.body.is_done());
    drop(slow_response);

    let mut finished_events = engine.subscribe_finished();
    let annotated = Request::builder(server.url("/final"))
        .unwrap()
        .annotation("trace=e2e")
        .unwrap()
        .build()
        .unwrap();
    let annotated = engine.execute(annotated).await.unwrap();
    assert_eq!(annotated.finished.reason, FinishedReason::Succeeded);
    assert_eq!(annotated.finished.annotations, ["trace=e2e"]);
    assert!(annotated.metrics().request_start.is_some());
    assert!(annotated.metrics().request_end.is_some());
    assert!(annotated.finished.response.is_some());
    let event = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let event = finished_events.recv().await.unwrap();
            if event.annotations == ["trace=e2e"] {
                break event;
            }
        }
    })
    .await
    .expect("annotated finished event timed out");
    assert_eq!(event.annotations, ["trace=e2e"]);

    let mut jobs = Vec::new();
    for _ in 0..4 {
        let engine = engine.clone();
        let url = server.url("/final");
        jobs.push(tokio::spawn(async move {
            engine
                .execute(Request::builder(url).unwrap().build().unwrap())
                .await
                .unwrap()
                .body()
                .to_vec()
        }));
    }
    for job in jobs {
        assert_eq!(job.await.unwrap(), b"redirected");
    }

    let cancel_response = engine
        .send(
            Request::builder(server.url("/slow-body"))
                .unwrap()
                .body_channel_capacity(1)
                .build()
                .unwrap(),
        )
        .await
        .unwrap();
    let cancel_handle = cancel_response.handle();
    cancel_handle.cancel();
    drop(cancel_response);
    for _ in 0..100 {
        if cancel_handle.is_done() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(cancel_handle.is_done());

    engine.stop_net_log().await.unwrap();
    assert!(std::fs::metadata(&netlog).unwrap().len() > 0);
    std::fs::remove_file(&netlog).unwrap();
    engine.shutdown().await.unwrap();
    engine.shutdown().await.unwrap();
    assert!(matches!(
        engine.start(
            Request::builder(server.url("/final"))
                .unwrap()
                .build()
                .unwrap()
        ),
        Err(Error::EngineShutdown)
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn memory_and_disk_cache_modes_obey_request_cache_policy() {
    let server = TestServer::start();
    let engine = Engine::builder()
        .cache_mode(CacheMode::InMemory {
            max_size: 4 * 1024 * 1024,
        })
        .build()
        .unwrap();
    let first = engine
        .execute(
            Request::builder(server.url("/cache"))
                .unwrap()
                .build()
                .unwrap(),
        )
        .await
        .unwrap();
    let second = engine
        .execute(
            Request::builder(server.url("/cache"))
                .unwrap()
                .build()
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(first.body(), b"cache-1");
    assert_eq!(second.body(), b"cache-1");
    assert!(second.info.was_cached);
    assert_eq!(server.cache_requests(), 1);
    let uncached = engine
        .execute(
            Request::builder(server.url("/cache"))
                .unwrap()
                .disable_cache(true)
                .build()
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(uncached.body(), b"cache-2");
    assert!(!uncached.info.was_cached);
    engine.shutdown().await.unwrap();

    let disk = temporary_path("disk-cache");
    std::fs::create_dir(&disk).unwrap();
    let engine = Engine::builder()
        .storage_path(&disk)
        .cache_mode(CacheMode::Disk {
            max_size: 4 * 1024 * 1024,
        })
        .build()
        .unwrap();
    let path = format!("/cache?disk={}", std::process::id());
    let first = engine
        .execute(
            Request::builder(server.url(&path))
                .unwrap()
                .build()
                .unwrap(),
        )
        .await
        .unwrap();
    let second = engine
        .execute(
            Request::builder(server.url(&path))
                .unwrap()
                .build()
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(first.status(), 200);
    assert_eq!(second.body(), first.body());
    assert!(second.info.was_cached);
    engine.shutdown().await.unwrap();
    std::fs::remove_dir_all(&disk).unwrap();

    assert_disk_no_http_mode(&server).await;
}

struct UploadGate {
    polled: AtomicBool,
}

struct GatedUpload {
    gate: Arc<UploadGate>,
}

impl AsyncRead for GatedUpload {
    fn poll_read(
        self: Pin<&mut Self>,
        _context: &mut Context<'_>,
        _output: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        self.gate.polled.store(true, Ordering::Release);
        Poll::Pending
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancel_during_pending_tokio_upload_still_completes_native_sink() {
    let server = TestServer::start();
    let engine = Engine::builder().build().unwrap();
    let gate = Arc::new(UploadGate {
        polled: AtomicBool::new(false),
    });
    let pending = engine
        .start(
            Request::builder(server.url("/echo"))
                .unwrap()
                .body_stream(GatedUpload { gate: gate.clone() }, Some(8))
                .build()
                .unwrap(),
        )
        .unwrap();
    let handle = pending.handle();
    tokio::time::timeout(Duration::from_secs(2), async {
        while !gate.polled.load(Ordering::Acquire) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("Cronet did not request upload data");
    handle.cancel();
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(5), pending)
            .await
            .expect("canceled upload did not reach a terminal callback"),
        Err(Error::Canceled)
    ));
    tokio::time::timeout(Duration::from_secs(5), engine.shutdown())
        .await
        .expect("shutdown waited forever for an upload sink callback")
        .unwrap();
}

struct RewindGate {
    polled: AtomicBool,
}

struct GatedRewind {
    gate: Arc<RewindGate>,
    body: &'static [u8],
    cursor: usize,
}

impl AsyncRead for GatedRewind {
    fn poll_read(
        mut self: Pin<&mut Self>,
        _context: &mut Context<'_>,
        output: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let count = (self.body.len() - self.cursor).min(output.remaining());
        output.put_slice(&self.body[self.cursor..self.cursor + count]);
        self.cursor += count;
        Poll::Ready(Ok(()))
    }
}

impl AsyncSeek for GatedRewind {
    fn start_seek(self: Pin<&mut Self>, _position: SeekFrom) -> std::io::Result<()> {
        Ok(())
    }

    fn poll_complete(
        self: Pin<&mut Self>,
        _context: &mut Context<'_>,
    ) -> Poll<std::io::Result<u64>> {
        self.gate.polled.store(true, Ordering::Release);
        Poll::Pending
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancel_during_pending_tokio_rewind_still_completes_native_sink() {
    let server = TestServer::start();
    let engine = Engine::builder().build().unwrap();
    let gate = Arc::new(RewindGate {
        polled: AtomicBool::new(false),
    });
    let mut pending = engine
        .start(
            Request::builder(server.url("/redirect307"))
                .unwrap()
                .rewindable_body_stream(
                    GatedRewind {
                        gate: gate.clone(),
                        body: b"rewind-me",
                        cursor: 0,
                    },
                    Some(9),
                )
                .build()
                .unwrap(),
        )
        .unwrap();
    let handle = pending.handle();
    tokio::select! {
        () = async {
            while !gate.polled.load(Ordering::Acquire) {
                tokio::task::yield_now().await;
            }
        } => {}
        result = &mut pending => panic!("request finished before rewind became pending: {result:?}"),
        () = tokio::time::sleep(Duration::from_secs(2)) => {
            panic!("Cronet did not request an upload rewind")
        }
    }
    handle.cancel();
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(5), pending)
            .await
            .expect("canceled rewind did not reach a terminal callback"),
        Err(Error::Canceled)
    ));
    tokio::time::timeout(Duration::from_secs(5), engine.shutdown())
        .await
        .expect("shutdown waited forever for a rewind sink callback")
        .unwrap();
}

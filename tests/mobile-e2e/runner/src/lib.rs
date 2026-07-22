use std::{
    any::Any,
    io::{Read, Write},
    net::TcpListener,
    panic::{self, AssertUnwindSafe},
    thread,
    time::Duration,
};

#[cfg(target_os = "android")]
use std::ffi::c_void;

use cronet::{BidirectionalRequest, Engine, Error, Request};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Runs the same Tokio/Cronet suite from an Android Activity or iOS app.
#[unsafe(no_mangle)]
pub extern "C" fn cronet_rs_mobile_e2e_run() -> i32 {
    let outcome = panic::catch_unwind(AssertUnwindSafe(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("create the Tokio runtime")
            .block_on(run_all());
    }));
    match outcome {
        Ok(()) => 0,
        Err(payload) => {
            eprintln!(
                "cronet-rs mobile E2E failed: {}",
                panic_message(payload.as_ref())
            );
            1
        }
    }
}

/// JNI entry used by the deliberately tiny Android test application.
#[cfg(target_os = "android")]
#[unsafe(no_mangle)]
pub extern "system" fn Java_io_github_southorange_cronet_e2e_MainActivity_runCronetE2e(
    _environment: *mut c_void,
    _activity: *mut c_void,
) -> i32 {
    cronet_rs_mobile_e2e_run()
}

/// Android invokes JNI_OnLoad only on the final shared object. Forward the VM
/// to statically linked Cronet before the Activity starts the test suite.
#[cfg(all(target_os = "android", feature = "static"))]
#[unsafe(no_mangle)]
pub extern "system" fn JNI_OnLoad(java_vm: *mut c_void, _reserved: *mut c_void) -> i32 {
    // SAFETY: Android supplies its process-wide JavaVM exactly once here.
    unsafe { cronet::android::initialize_java_vm(java_vm) }
}

fn panic_message(payload: &(dyn Any + Send)) -> &str {
    payload
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("unknown panic")
}

async fn run_all() {
    post_streaming_body_metrics_and_limit().await;
    dropping_a_stream_cancels_and_shutdown_completes().await;
    bidirectional_failure_is_async_and_shutdown_safe().await;
}

#[allow(clippy::too_many_lines)]
async fn post_streaming_body_metrics_and_limit() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = Vec::new();
        let mut chunk = [0_u8; 1024];
        let header_end = loop {
            let count = stream.read(&mut chunk).unwrap();
            assert_ne!(count, 0, "client closed before HTTP headers");
            request.extend_from_slice(&chunk[..count]);
            if let Some(index) = request.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
                break index + 4;
            }
        };
        let headers = String::from_utf8_lossy(&request[..header_end]);
        let content_length = headers
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse::<usize>().unwrap())
            })
            .unwrap();
        while request.len() - header_end < content_length {
            let count = stream.read(&mut chunk).unwrap();
            assert_ne!(count, 0, "client closed before HTTP body");
            request.extend_from_slice(&chunk[..count]);
        }
        assert_eq!(
            &request[header_end..header_end + content_length],
            b"tokio upload"
        );
        stream
            .write_all(
                b"HTTP/1.1 201 Created\r\nContent-Length: 8\r\nConnection: close\r\n\r\nstreamed",
            )
            .unwrap();
        drop(stream);

        let (mut stream, _) = listener.accept().unwrap();
        let mut request = Vec::new();
        while !request.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
            let count = stream.read(&mut chunk).unwrap();
            assert_ne!(count, 0, "client closed before limit-test headers");
            request.extend_from_slice(&chunk[..count]);
        }
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: close\r\n\r\ndata")
            .unwrap();
    });

    let engine = Engine::builder()
        .user_agent("cronet-rs/mobile-e2e")
        .build()
        .unwrap();
    let mut finished_events = engine.subscribe_finished();
    let (mut writer, reader) = tokio::io::duplex(64);
    tokio::spawn(async move {
        writer.write_all(b"tokio upload").await.unwrap();
        writer.shutdown().await.unwrap();
    });
    let request = Request::builder(format!("http://{address}/stream-upload"))
        .unwrap()
        .method("POST")
        .unwrap()
        .header("content-type", "application/octet-stream")
        .unwrap()
        .body_stream(reader, Some(12))
        .build()
        .unwrap();
    let mut response = engine.send(request).await.unwrap();
    assert_eq!(response.status(), 201);
    let mut body = Vec::new();
    response.body.read_to_end(&mut body).await.unwrap();
    assert_eq!(body, b"streamed");
    let finished = response.body.finished().await.unwrap();
    assert!(finished.metrics.request_start.is_some());
    assert_eq!(
        finished_events.recv().await.unwrap().reason,
        finished.reason
    );

    let request = Request::builder(format!("http://{address}/too-large"))
        .unwrap()
        .read_buffer_size(4)
        .max_response_bytes(2)
        .build()
        .unwrap();
    assert!(matches!(
        engine.execute(request).await,
        Err(Error::ResponseTooLarge { limit: 2 })
    ));
    engine.shutdown().await.unwrap();
    server.join().unwrap();
}

async fn dropping_a_stream_cancels_and_shutdown_completes() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = Vec::new();
        let mut chunk = [0_u8; 1024];
        while !request.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
            let count = stream.read(&mut chunk).unwrap();
            assert_ne!(count, 0);
            request.extend_from_slice(&chunk[..count]);
        }
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 1048576\r\nConnection: close\r\n\r\nx")
            .unwrap();
        stream.flush().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let _ = stream.read(&mut chunk);
    });

    let engine = Engine::builder().build().unwrap();
    let request = Request::builder(format!("http://{address}/cancel"))
        .unwrap()
        .body_channel_capacity(1)
        .build()
        .unwrap();
    let response = engine.send(request).await.unwrap();
    drop(response);
    tokio::time::timeout(Duration::from_secs(5), engine.shutdown())
        .await
        .expect("shutdown timed out")
        .unwrap();
    server.join().unwrap();
}

async fn bidirectional_failure_is_async_and_shutdown_safe() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);

    let engine = Engine::builder().build().unwrap();
    let request = BidirectionalRequest::builder(format!("http://{address}/grpc"))
        .unwrap()
        .header("content-type", "application/grpc")
        .unwrap()
        .end_of_stream(true)
        .build()
        .unwrap();
    let result = tokio::time::timeout(Duration::from_secs(5), engine.open_bidirectional(request))
        .await
        .expect("bidirectional failure timed out");
    assert!(result.is_err());
    engine.shutdown().await.unwrap();
}

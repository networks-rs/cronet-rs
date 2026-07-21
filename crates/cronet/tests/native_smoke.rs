#![cfg(feature = "native-tests")]

use std::{
    io::{Read, Write},
    net::TcpListener,
    thread,
    time::Duration,
};

use cronet::{BidirectionalRequest, Engine, Error, Request};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::too_many_lines)]
async fn posts_and_enforces_the_response_limit() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = Vec::new();
        let mut chunk = [0_u8; 1024];
        let header_end = loop {
            let count = stream.read(&mut chunk).unwrap();
            assert_ne!(count, 0, "client closed before sending HTTP headers");
            request.extend_from_slice(&chunk[..count]);
            if let Some(index) = request.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
                break index + 4;
            }
        };
        let headers = String::from_utf8_lossy(&request[..header_end]).into_owned();
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
            assert_ne!(count, 0, "client closed before sending the HTTP body");
            request.extend_from_slice(&chunk[..count]);
        }
        assert!(headers.starts_with("POST "));
        assert_eq!(
            &request[header_end..header_end + content_length],
            b"hello cronet"
        );
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok")
            .unwrap();
        drop(stream);

        let (mut stream, _) = listener.accept().unwrap();
        let mut request = Vec::new();
        while !request.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
            let count = stream.read(&mut chunk).unwrap();
            assert_ne!(count, 0, "client closed before sending HTTP headers");
            request.extend_from_slice(&chunk[..count]);
        }
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: close\r\n\r\ndata")
            .unwrap();
        drop(stream);

        let (mut stream, _) = listener.accept().unwrap();
        let mut request = Vec::new();
        let header_end = loop {
            let count = stream.read(&mut chunk).unwrap();
            assert_ne!(count, 0, "client closed before sending HTTP headers");
            request.extend_from_slice(&chunk[..count]);
            if let Some(index) = request.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
                break index + 4;
            }
        };
        while request.len() - header_end < 12 {
            let count = stream.read(&mut chunk).unwrap();
            assert_ne!(count, 0, "client closed before sending streaming body");
            request.extend_from_slice(&chunk[..count]);
        }
        assert_eq!(&request[header_end..header_end + 12], b"async upload");
        stream
            .write_all(
                b"HTTP/1.1 201 Created\r\nContent-Length: 8\r\nConnection: close\r\n\r\nstreamed",
            )
            .unwrap();
    });

    let engine = Engine::builder()
        .user_agent("cronet-rs/test")
        .build()
        .unwrap();
    let mut finished_events = engine.subscribe_finished();
    let request = Request::builder(format!("http://{address}/upload"))
        .unwrap()
        .method("POST")
        .unwrap()
        .header("content-type", "text/plain")
        .unwrap()
        .body(b"hello cronet".to_vec())
        .build()
        .unwrap();
    let mut response = engine.send(request).await.unwrap();
    assert_eq!(response.status(), 200);
    let mut response_body = Vec::new();
    response.body.read_to_end(&mut response_body).await.unwrap();
    assert_eq!(response_body, b"ok");
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

    let (mut writer, reader) = tokio::io::duplex(64);
    tokio::spawn(async move {
        writer.write_all(b"async upload").await.unwrap();
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
    let response = engine.execute(request).await.unwrap();
    assert_eq!(response.status(), 201);
    assert_eq!(response.body(), b"streamed");

    engine.shutdown().await.unwrap();
    server.join().unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dropping_a_stream_cancels_it_and_async_shutdown_completes() {
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bidirectional_stream_failure_is_async_and_shutdown_safe() {
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

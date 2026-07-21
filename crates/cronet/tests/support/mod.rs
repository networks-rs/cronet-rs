#![allow(dead_code)]

use std::{
    collections::HashMap,
    io::{BufRead, BufReader, Read, Write},
    net::{Shutdown, SocketAddr, TcpListener, TcpStream},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread,
    time::Duration,
};

#[derive(Debug)]
pub struct TestServer {
    address: SocketAddr,
    stopping: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
    cache_requests: Arc<AtomicUsize>,
    paths: Arc<Mutex<Vec<String>>>,
}

impl TestServer {
    pub fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind local E2E server");
        listener
            .set_nonblocking(true)
            .expect("make local E2E server nonblocking");
        let address = listener.local_addr().expect("read local E2E address");
        let stopping = Arc::new(AtomicBool::new(false));
        let cache_requests = Arc::new(AtomicUsize::new(0));
        let paths = Arc::new(Mutex::new(Vec::new()));
        let worker_stopping = stopping.clone();
        let worker_cache_requests = cache_requests.clone();
        let worker_paths = paths.clone();
        let worker = thread::spawn(move || {
            while !worker_stopping.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let cache_requests = worker_cache_requests.clone();
                        let paths = worker_paths.clone();
                        thread::spawn(move || handle_connection(stream, &cache_requests, &paths));
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(error) => panic!("local E2E server accept failed: {error}"),
                }
            }
        });
        Self {
            address,
            stopping,
            worker: Some(worker),
            cache_requests,
            paths,
        }
    }

    pub fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.address)
    }

    pub fn address(&self) -> SocketAddr {
        self.address
    }

    pub fn cache_requests(&self) -> usize {
        self.cache_requests.load(Ordering::Acquire)
    }

    pub fn path_count(&self, path: &str) -> usize {
        self.paths
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .filter(|candidate| candidate.as_str() == path)
            .count()
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.stopping.store(true, Ordering::Release);
        let _ = TcpStream::connect(self.address);
        if let Some(worker) = self.worker.take() {
            worker.join().expect("join local E2E server");
        }
    }
}

#[derive(Debug)]
struct TestRequest {
    method: String,
    path: String,
    headers: HashMap<String, String>,
    body: Vec<u8>,
}

fn handle_connection(
    mut stream: TcpStream,
    cache_requests: &AtomicUsize,
    paths: &Mutex<Vec<String>>,
) {
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("set local E2E read timeout");
    let Ok(request) = read_request(&stream) else {
        return;
    };
    paths
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .push(request.path.clone());

    match request.path.as_str() {
        "/inspect" => {
            let user_agent = header(&request, "user-agent");
            let language = header(&request, "accept-language");
            let custom = header(&request, "x-cronet-test");
            let mut body =
                format!("{}|{user_agent}|{language}|{custom}|", request.method).into_bytes();
            body.extend_from_slice(&request.body);
            respond(&mut stream, 200, "OK", &[("X-E2E", "inspect")], &body);
        }
        "/echo" => respond(
            &mut stream,
            201,
            "Created",
            &[("X-E2E", "echo")],
            &request.body,
        ),
        "/chunks" => {
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nX-E2E: chunks\r\nConnection: close\r\n\r\n3\r\none\r\n3\r\ntwo\r\n5\r\nthree\r\n0\r\n\r\n",
                )
                .expect("write chunked E2E response");
            finish_response(&mut stream);
        }
        "/redirect" | "/async-redirect" => respond_empty(
            &mut stream,
            302,
            "Found",
            &[("Location", "/final"), ("X-Redirect", "yes")],
        ),
        "/redirect307" => respond_empty(
            &mut stream,
            307,
            "Temporary Redirect",
            &[("Location", "/echo")],
        ),
        "/final" => respond(&mut stream, 200, "OK", &[("X-E2E", "final")], b"redirected"),
        path if path.starts_with("/cache") => {
            let sequence = cache_requests.fetch_add(1, Ordering::AcqRel) + 1;
            respond(
                &mut stream,
                200,
                "OK",
                &[
                    ("Cache-Control", "public, max-age=3600"),
                    ("X-E2E", "cache"),
                ],
                format!("cache-{sequence}").as_bytes(),
            );
        }
        "/slow-headers" => {
            thread::sleep(Duration::from_millis(500));
            respond(&mut stream, 200, "OK", &[], b"slow");
        }
        "/slow-body" => {
            let _ = stream.write_all(
                b"HTTP/1.1 200 OK\r\nContent-Length: 1048576\r\nConnection: close\r\n\r\nx",
            );
            let _ = stream.flush();
            thread::sleep(Duration::from_secs(2));
        }
        "/large" => respond(&mut stream, 200, "OK", &[], b"0123456789"),
        "/brotli" => {
            let mut compressed = Vec::new();
            {
                let mut compressor = brotli::CompressorWriter::new(&mut compressed, 4096, 5, 22);
                compressor
                    .write_all(b"brotli-decoded-by-cronet")
                    .expect("compress local Brotli response");
            }
            respond(
                &mut stream,
                200,
                "OK",
                &[("Content-Encoding", "br")],
                &compressed,
            );
        }
        _ => respond(&mut stream, 404, "Not Found", &[], b"missing"),
    }
}

fn header<'a>(request: &'a TestRequest, name: &str) -> &'a str {
    request.headers.get(name).map_or("", String::as_str)
}

fn read_request(stream: &TcpStream) -> std::io::Result<TestRequest> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut request_line = String::new();
    reader.read_line(&mut request_line)?;
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_owned();
    let path = parts.next().unwrap_or_default().to_owned();
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
    let body = if let Some(length) = headers.get("content-length") {
        let length = length.parse::<usize>().map_err(std::io::Error::other)?;
        let mut body = vec![0; length];
        reader.read_exact(&mut body)?;
        body
    } else if headers
        .get("transfer-encoding")
        .is_some_and(|value| value.eq_ignore_ascii_case("chunked"))
    {
        read_chunked(&mut reader)?
    } else {
        Vec::new()
    };
    Ok(TestRequest {
        method,
        path,
        headers,
        body,
    })
}

fn read_chunked(reader: &mut impl BufRead) -> std::io::Result<Vec<u8>> {
    let mut body = Vec::new();
    loop {
        let mut size = String::new();
        reader.read_line(&mut size)?;
        let size = size.trim_end().split(';').next().unwrap_or_default();
        let size = usize::from_str_radix(size, 16).map_err(std::io::Error::other)?;
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
            return Err(std::io::Error::other("invalid chunk delimiter"));
        }
    }
}

fn respond_empty(stream: &mut TcpStream, status: u16, reason: &str, headers: &[(&str, &str)]) {
    respond(stream, status, reason, headers, b"");
}

fn respond(
    stream: &mut TcpStream,
    status: u16,
    reason: &str,
    headers: &[(&str, &str)],
    body: &[u8],
) {
    write!(
        stream,
        "HTTP/1.1 {status} {reason}\r\nContent-Length: {}\r\nConnection: close\r\n",
        body.len()
    )
    .expect("write local E2E response headers");
    for (name, value) in headers {
        write!(stream, "{name}: {value}\r\n").expect("write local E2E response header");
    }
    stream
        .write_all(b"\r\n")
        .expect("finish local E2E response headers");
    stream
        .write_all(body)
        .expect("write local E2E response body");
    finish_response(stream);
}

fn finish_response(stream: &mut TcpStream) {
    stream.flush().expect("flush local E2E response");
    stream
        .shutdown(Shutdown::Write)
        .expect("finish local E2E response");
}

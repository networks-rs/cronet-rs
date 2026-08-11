//! Optional buffered HTTPS/1.1 transport backed by `GmSSL`.
//!
//! This module is separate from [`crate::Engine`]: Cronet's C API cannot
//! replace Chromium's internal `BoringSSL` provider. Enable the `gmssl` Cargo
//! feature to use SM2/SM3/SM4 TLS 1.2, TLS 1.3, or TLCP endpoints.

use std::{
    ffi::{CStr, CString, c_char, c_int, c_uchar},
    fmt, fs,
    net::{TcpStream, ToSocketAddrs},
    path::{Path, PathBuf},
    ptr::NonNull,
    sync::Arc,
    time::Duration,
};

use gmssl_rs::{Sm3, X509Cert};
use url::{Host, Url};

use crate::{Error, Header, Request, Result, request::GmSslRequestParts};

const TLS_PROTOCOL_TLCP: c_int = 0x0101;
const TLS_PROTOCOL_1_2: c_int = 0x0303;
const TLS_PROTOCOL_1_3: c_int = 0x0304;
const TLS_RECORD_PLAINTEXT: usize = 16 * 1024;
const MAX_HTTP_HEADER_BYTES: usize = 64 * 1024;
const MAX_HTTP_WIRE_OVERHEAD: usize = 1024 * 1024;
const NATIVE_ERROR_CAPACITY: usize = 256;

/// A protocol and national-cryptography cipher suite supported by `GmSSL`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[non_exhaustive]
pub enum GmSslProtocol {
    /// TLS 1.2 with `TLS_ECDHE_SM4_CBC_SM3`.
    Tls12,
    /// TLS 1.3 with `TLS_SM4_GCM_SM3` (RFC 8998).
    #[default]
    Tls13,
    /// TLCP with `ECC_SM4_CBC_SM3`.
    Tlcp,
}

impl GmSslProtocol {
    const fn native(self) -> c_int {
        match self {
            Self::Tls12 => TLS_PROTOCOL_1_2,
            Self::Tls13 => TLS_PROTOCOL_1_3,
            Self::Tlcp => TLS_PROTOCOL_TLCP,
        }
    }
}

/// A failure from the optional `GmSSL` transport.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum GmSslError {
    InvalidConfiguration(&'static str),
    Certificate(String),
    InvalidUrl(String),
    NameResolution(String),
    Io(String),
    Handshake(String),
    ServerIdentityMismatch,
    StreamingUploadUnsupported,
    Http(String),
}

impl fmt::Display for GmSslError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidConfiguration(message) => formatter.write_str(message),
            Self::Certificate(message) => write!(formatter, "GmSSL certificate error: {message}"),
            Self::InvalidUrl(message) => write!(formatter, "invalid GmSSL request URL: {message}"),
            Self::NameResolution(message) => {
                write!(
                    formatter,
                    "GmSSL endpoint name resolution failed: {message}"
                )
            }
            Self::Io(message) => write!(formatter, "GmSSL transport I/O failed: {message}"),
            Self::Handshake(message) => write!(formatter, "GmSSL handshake failed: {message}"),
            Self::ServerIdentityMismatch => {
                formatter.write_str("GmSSL peer certificate did not match a configured SM3 pin")
            }
            Self::StreamingUploadUnsupported => {
                formatter.write_str("GmSSL transport currently supports only buffered uploads")
            }
            Self::Http(message) => write!(formatter, "GmSSL HTTP/1.1 error: {message}"),
        }
    }
}

impl std::error::Error for GmSslError {}

impl From<GmSslError> for Error {
    fn from(error: GmSslError) -> Self {
        Self::GmSsl(error)
    }
}

/// Configures a reusable `GmSSL` HTTPS client.
#[derive(Debug, Clone)]
pub struct GmSslClientBuilder {
    protocol: GmSslProtocol,
    ca_certificates: Option<PathBuf>,
    server_certificate_pins: Vec<[u8; 32]>,
    client_identity: Option<ClientIdentityBuilder>,
    verify_depth: u8,
    connect_timeout: Duration,
    io_timeout: Duration,
}

impl Default for GmSslClientBuilder {
    fn default() -> Self {
        Self {
            protocol: GmSslProtocol::Tls13,
            ca_certificates: None,
            server_certificate_pins: Vec::new(),
            client_identity: None,
            verify_depth: 4,
            connect_timeout: Duration::from_secs(10),
            io_timeout: Duration::from_secs(30),
        }
    }
}

impl GmSslClientBuilder {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub const fn protocol(mut self, protocol: GmSslProtocol) -> Self {
        self.protocol = protocol;
        self
    }

    /// Sets the trusted PEM CA certificates used by `GmSSL` chain validation.
    #[must_use]
    pub fn ca_certificates(mut self, path: impl Into<PathBuf>) -> Self {
        self.ca_certificates = Some(path.into());
        self
    }

    /// Pins the first certificate in a PEM file by its SM3 DER digest.
    ///
    /// `GmSSL`'s current client API validates certificate chains but does not
    /// validate DNS names. At least one exact leaf-certificate pin is therefore
    /// mandatory and is checked before any HTTP bytes are sent.
    pub fn server_certificate(mut self, path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let pem = fs::read(path).map_err(|error| {
            GmSslError::Certificate(format!("failed to read {}: {error}", path.display()))
        })?;
        let certificate =
            X509Cert::from_pem(&pem).map_err(|error| GmSslError::Certificate(error.to_string()))?;
        self.server_certificate_pins
            .push(Sm3::digest(certificate.as_der()));
        Ok(self)
    }

    /// Adds an already computed SM3 digest of an accepted leaf certificate.
    #[must_use]
    pub fn server_certificate_sm3(mut self, digest: [u8; 32]) -> Self {
        self.server_certificate_pins.push(digest);
        self
    }

    /// Configures an optional client SM2 certificate chain and private key.
    #[must_use]
    pub fn client_identity(
        mut self,
        certificate_chain: impl Into<PathBuf>,
        private_key: impl Into<PathBuf>,
        password: impl Into<String>,
    ) -> Self {
        self.client_identity = Some(ClientIdentityBuilder {
            certificate_chain: certificate_chain.into(),
            private_key: private_key.into(),
            password: password.into(),
        });
        self
    }

    #[must_use]
    pub const fn verify_depth(mut self, depth: u8) -> Self {
        self.verify_depth = depth;
        self
    }

    #[must_use]
    pub const fn connect_timeout(mut self, timeout: Duration) -> Self {
        self.connect_timeout = timeout;
        self
    }

    #[must_use]
    pub const fn io_timeout(mut self, timeout: Duration) -> Self {
        self.io_timeout = timeout;
        self
    }

    pub fn build(self) -> Result<GmSslClient> {
        if !(1..=5).contains(&self.verify_depth) {
            return Err(GmSslError::InvalidConfiguration(
                "GmSSL certificate verify depth must be between 1 and 5",
            )
            .into());
        }
        if self.connect_timeout.is_zero() || self.io_timeout.is_zero() {
            return Err(GmSslError::InvalidConfiguration(
                "GmSSL connect and I/O timeouts must be greater than zero",
            )
            .into());
        }
        let ca_certificates = self.ca_certificates.ok_or_else(|| {
            Error::from(GmSslError::InvalidConfiguration(
                "GmSSL transport requires a PEM CA certificate file",
            ))
        })?;
        ensure_file(&ca_certificates, "GmSSL CA certificate file does not exist")?;
        if self.server_certificate_pins.is_empty() {
            return Err(GmSslError::InvalidConfiguration(
                "GmSSL transport requires at least one server certificate SM3 pin",
            )
            .into());
        }
        let ca_certificates = path_to_cstring(&ca_certificates)?;
        let client_identity = self
            .client_identity
            .map(ClientIdentity::try_from)
            .transpose()?;
        Ok(GmSslClient {
            config: Arc::new(GmSslConfig {
                protocol: self.protocol,
                ca_certificates,
                server_certificate_pins: self.server_certificate_pins,
                client_identity,
                verify_depth: self.verify_depth,
                connect_timeout: self.connect_timeout,
                io_timeout: self.io_timeout,
            }),
        })
    }
}

/// A cloneable buffered HTTPS/1.1 client using `GmSSL` for transport security.
#[derive(Debug, Clone)]
pub struct GmSslClient {
    config: Arc<GmSslConfig>,
}

impl GmSslClient {
    #[must_use]
    pub fn builder() -> GmSslClientBuilder {
        GmSslClientBuilder::new()
    }

    /// Executes one request on a fresh `GmSSL` connection.
    ///
    /// The current transport supports buffered request and response bodies,
    /// HTTP/1.1, and `Connection: close`. Redirects are returned to the caller
    /// and are not followed automatically.
    pub async fn execute(&self, request: Request) -> Result<GmSslResponse> {
        let parts = request.into_gmssl_parts()?;
        let config = self.config.clone();
        tokio::task::spawn_blocking(move || execute_blocking(&config, parts))
            .await
            .map_err(|error| Error::TokioTask(error.to_string()))?
    }
}

/// A buffered HTTP/1.1 response received through `GmSSL`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GmSslResponse {
    pub protocol: GmSslProtocol,
    pub status_code: u16,
    pub reason: String,
    pub headers: Vec<Header>,
    pub body: Vec<u8>,
}

impl GmSslResponse {
    #[must_use]
    pub const fn status(&self) -> u16 {
        self.status_code
    }

    #[must_use]
    pub fn body(&self) -> &[u8] {
        &self.body
    }

    #[must_use]
    pub fn into_body(self) -> Vec<u8> {
        self.body
    }
}

#[derive(Debug, Clone)]
struct ClientIdentityBuilder {
    certificate_chain: PathBuf,
    private_key: PathBuf,
    password: String,
}

#[derive(Debug)]
struct ClientIdentity {
    certificate_chain: CString,
    private_key: CString,
    password: CString,
}

impl TryFrom<ClientIdentityBuilder> for ClientIdentity {
    type Error = Error;

    fn try_from(identity: ClientIdentityBuilder) -> Result<Self> {
        ensure_file(
            &identity.certificate_chain,
            "GmSSL client certificate chain does not exist",
        )?;
        ensure_file(
            &identity.private_key,
            "GmSSL client private key does not exist",
        )?;
        Ok(Self {
            certificate_chain: path_to_cstring(&identity.certificate_chain)?,
            private_key: path_to_cstring(&identity.private_key)?,
            password: CString::new(identity.password).map_err(|_| Error::InvalidString {
                field: "GmSSL client key password",
            })?,
        })
    }
}

#[derive(Debug)]
struct GmSslConfig {
    protocol: GmSslProtocol,
    ca_certificates: CString,
    server_certificate_pins: Vec<[u8; 32]>,
    client_identity: Option<ClientIdentity>,
    verify_depth: u8,
    connect_timeout: Duration,
    io_timeout: Duration,
}

fn execute_blocking(config: &GmSslConfig, parts: GmSslRequestParts) -> Result<GmSslResponse> {
    let prepared = PreparedRequest::new(parts)?;
    let addresses = (prepared.host.as_str(), prepared.port)
        .to_socket_addrs()
        .map_err(|error| GmSslError::NameResolution(error.to_string()))?;
    let mut last_error = None;
    let mut stream = None;
    for address in addresses {
        match TcpStream::connect_timeout(&address, config.connect_timeout) {
            Ok(connected) => {
                stream = Some(connected);
                break;
            }
            Err(error) => last_error = Some(error),
        }
    }
    let stream = stream.ok_or_else(|| {
        GmSslError::Io(last_error.map_or_else(
            || "host resolved to no socket addresses".to_owned(),
            |error| error.to_string(),
        ))
    })?;
    stream
        .set_read_timeout(Some(config.io_timeout))
        .and_then(|()| stream.set_write_timeout(Some(config.io_timeout)))
        .and_then(|()| stream.set_nodelay(true))
        .map_err(|error| GmSslError::Io(error.to_string()))?;

    let mut native = NativeClient::connect(config, &stream)?;
    let peer_digest = native.peer_leaf_sm3()?;
    if !config
        .server_certificate_pins
        .iter()
        .any(|pin| constant_time_eq(pin, &peer_digest))
    {
        return Err(GmSslError::ServerIdentityMismatch.into());
    }

    native.send_all(&prepared.wire)?;
    let response_wire = native.receive_to_end(prepared.max_response_bytes)?;
    parse_response(
        &response_wire,
        &prepared.method,
        prepared.max_response_bytes,
        config.protocol,
    )
}

struct PreparedRequest {
    host: String,
    port: u16,
    method: String,
    wire: Vec<u8>,
    max_response_bytes: usize,
}

impl PreparedRequest {
    fn new(parts: GmSslRequestParts) -> Result<Self> {
        let url =
            Url::parse(&parts.url).map_err(|error| GmSslError::InvalidUrl(error.to_string()))?;
        if url.scheme() != "https" {
            return Err(GmSslError::InvalidUrl(
                "only https:// URLs can use the GmSSL transport".to_owned(),
            )
            .into());
        }
        if url.fragment().is_some() {
            return Err(GmSslError::InvalidUrl(
                "URL fragments are not sent in HTTP requests".to_owned(),
            )
            .into());
        }
        if !url.username().is_empty() || url.password().is_some() {
            return Err(GmSslError::InvalidUrl(
                "embedded URL credentials are not supported".to_owned(),
            )
            .into());
        }
        let host = url
            .host_str()
            .ok_or_else(|| GmSslError::InvalidUrl("URL has no host".to_owned()))?
            .to_owned();
        let port = url
            .port_or_known_default()
            .ok_or_else(|| GmSslError::InvalidUrl("URL has no port".to_owned()))?;
        validate_method(&parts.method)?;
        for header in &parts.headers {
            validate_header(header)?;
            if is_managed_header(header.name()) {
                return Err(GmSslError::Http(format!(
                    "{} is managed by the GmSSL transport",
                    header.name()
                ))
                .into());
            }
        }

        let request_target = url.path().to_owned()
            + url.query().map_or("", |_| "?")
            + url.query().unwrap_or_default();
        let host_header = format_host_header(&url, port);
        let body_length = parts.body.as_ref().map_or(0, bytes::Bytes::len);
        let mut wire = Vec::new();
        write_http_line(&mut wire, &parts.method, &request_target);
        extend_header(&mut wire, "Host", &host_header);
        extend_header(&mut wire, "Connection", "close");
        extend_header(&mut wire, "Accept-Encoding", "identity");
        for header in &parts.headers {
            extend_header(&mut wire, header.name(), header.value());
        }
        if parts.body.is_some() {
            extend_header(&mut wire, "Content-Length", &body_length.to_string());
        }
        wire.extend_from_slice(b"\r\n");
        if let Some(body) = parts.body {
            wire.extend_from_slice(&body);
        }
        Ok(Self {
            host,
            port,
            method: parts.method,
            wire,
            max_response_bytes: parts.max_response_bytes,
        })
    }
}

fn write_http_line(output: &mut Vec<u8>, method: &str, target: &str) {
    output.extend_from_slice(method.as_bytes());
    output.push(b' ');
    output.extend_from_slice(target.as_bytes());
    output.extend_from_slice(b" HTTP/1.1\r\n");
}

fn extend_header(output: &mut Vec<u8>, name: &str, value: &str) {
    output.extend_from_slice(name.as_bytes());
    output.extend_from_slice(b": ");
    output.extend_from_slice(value.as_bytes());
    output.extend_from_slice(b"\r\n");
}

fn format_host_header(url: &Url, port: u16) -> String {
    let host = match url.host().expect("validated URL has a host") {
        Host::Ipv6(address) => format!("[{address}]"),
        host => host.to_string(),
    };
    if port == 443 && url.port().is_none() {
        host
    } else {
        format!("{host}:{port}")
    }
}

fn validate_method(method: &str) -> Result<()> {
    if method.is_empty() || !method.bytes().all(is_http_token_byte) {
        return Err(GmSslError::Http("invalid HTTP method token".to_owned()).into());
    }
    Ok(())
}

fn validate_header(header: &Header) -> Result<()> {
    if header.name().is_empty() || !header.name().bytes().all(is_http_token_byte) {
        return Err(
            GmSslError::Http(format!("invalid HTTP header name: {}", header.name())).into(),
        );
    }
    if header
        .value()
        .bytes()
        .any(|byte| byte == b'\r' || byte == b'\n' || (byte < b' ' && byte != b'\t'))
    {
        return Err(
            GmSslError::Http(format!("invalid HTTP header value for {}", header.name())).into(),
        );
    }
    Ok(())
}

const fn is_http_token_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric()
        || matches!(
            byte,
            b'!' | b'#'
                | b'$'
                | b'%'
                | b'&'
                | b'\''
                | b'*'
                | b'+'
                | b'-'
                | b'.'
                | b'^'
                | b'_'
                | b'`'
                | b'|'
                | b'~'
        )
}

fn is_managed_header(name: &str) -> bool {
    ["host", "connection", "content-length", "transfer-encoding"]
        .iter()
        .any(|managed| name.eq_ignore_ascii_case(managed))
}

fn parse_response(
    wire: &[u8],
    request_method: &str,
    max_response_bytes: usize,
    protocol: GmSslProtocol,
) -> Result<GmSslResponse> {
    if wire.len() > max_response_bytes.saturating_add(MAX_HTTP_WIRE_OVERHEAD) {
        return Err(Error::ResponseTooLarge {
            limit: max_response_bytes,
        });
    }
    let mut raw_headers = [httparse::EMPTY_HEADER; 128];
    let mut parsed = httparse::Response::new(&mut raw_headers);
    let header_length = match parsed
        .parse(wire)
        .map_err(|error| GmSslError::Http(error.to_string()))?
    {
        httparse::Status::Complete(length) => length,
        httparse::Status::Partial => {
            return Err(GmSslError::Http("incomplete response headers".to_owned()).into());
        }
    };
    if header_length > MAX_HTTP_HEADER_BYTES {
        return Err(GmSslError::Http("response headers exceed 64 KiB".to_owned()).into());
    }
    let status_code = parsed
        .code
        .ok_or_else(|| GmSslError::Http("response has no status code".to_owned()))?;
    let reason = parsed.reason.unwrap_or_default().to_owned();
    let mut headers = Vec::with_capacity(parsed.headers.len());
    for header in parsed.headers {
        let value = std::str::from_utf8(header.value)
            .map_err(|_| GmSslError::Http("response header is not UTF-8".to_owned()))?;
        headers.push(Header::new(header.name, value)?);
    }
    let encoded_body = &wire[header_length..];
    let no_body = request_method.eq_ignore_ascii_case("HEAD")
        || (100..200).contains(&status_code)
        || matches!(status_code, 204 | 304);
    let body = if no_body {
        Vec::new()
    } else if is_chunked(&headers)? {
        decode_chunked(encoded_body, max_response_bytes)?
    } else if let Some(length) = content_length(&headers)? {
        if length > max_response_bytes {
            return Err(Error::ResponseTooLarge {
                limit: max_response_bytes,
            });
        }
        if encoded_body.len() < length {
            return Err(GmSslError::Http("response body is truncated".to_owned()).into());
        }
        encoded_body[..length].to_vec()
    } else {
        if encoded_body.len() > max_response_bytes {
            return Err(Error::ResponseTooLarge {
                limit: max_response_bytes,
            });
        }
        encoded_body.to_vec()
    };
    Ok(GmSslResponse {
        protocol,
        status_code,
        reason,
        headers,
        body,
    })
}

fn is_chunked(headers: &[Header]) -> Result<bool> {
    let values = headers
        .iter()
        .filter(|header| header.name().eq_ignore_ascii_case("transfer-encoding"))
        .flat_map(|header| header.value().split(','))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>();
    if values.is_empty() {
        return Ok(false);
    }
    if values.len() == 1 && values[0].eq_ignore_ascii_case("chunked") {
        return Ok(true);
    }
    Err(GmSslError::Http("unsupported Transfer-Encoding".to_owned()).into())
}

fn content_length(headers: &[Header]) -> Result<Option<usize>> {
    let mut parsed = None;
    for header in headers
        .iter()
        .filter(|header| header.name().eq_ignore_ascii_case("content-length"))
    {
        let value = header
            .value()
            .trim()
            .parse::<usize>()
            .map_err(|_| GmSslError::Http("invalid Content-Length".to_owned()))?;
        if parsed.is_some_and(|previous| previous != value) {
            return Err(GmSslError::Http("conflicting Content-Length values".to_owned()).into());
        }
        parsed = Some(value);
    }
    Ok(parsed)
}

fn decode_chunked(input: &[u8], limit: usize) -> Result<Vec<u8>> {
    let mut cursor = 0;
    let mut output = Vec::new();
    loop {
        let line_end = find_bytes(&input[cursor..], b"\r\n")
            .map(|offset| cursor + offset)
            .ok_or_else(|| GmSslError::Http("truncated chunk size".to_owned()))?;
        let size_field = std::str::from_utf8(&input[cursor..line_end])
            .map_err(|_| GmSslError::Http("chunk size is not ASCII".to_owned()))?;
        let size_hex = size_field.split(';').next().unwrap_or_default().trim();
        let size = usize::from_str_radix(size_hex, 16)
            .map_err(|_| GmSslError::Http("invalid chunk size".to_owned()))?;
        cursor = line_end + 2;
        if size == 0 {
            if input.get(cursor..cursor + 2) == Some(b"\r\n")
                || find_bytes(&input[cursor..], b"\r\n\r\n").is_some()
            {
                return Ok(output);
            }
            return Err(GmSslError::Http("truncated chunk trailers".to_owned()).into());
        }
        if output.len().saturating_add(size) > limit {
            return Err(Error::ResponseTooLarge { limit });
        }
        let chunk_end = cursor
            .checked_add(size)
            .ok_or_else(|| GmSslError::Http("chunk size overflow".to_owned()))?;
        if input.get(chunk_end..chunk_end + 2) != Some(b"\r\n") {
            return Err(GmSslError::Http("truncated chunk data".to_owned()).into());
        }
        output.extend_from_slice(&input[cursor..chunk_end]);
        cursor = chunk_end + 2;
    }
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn constant_time_eq(left: &[u8; 32], right: &[u8; 32]) -> bool {
    left.iter()
        .zip(right)
        .fold(0_u8, |difference, (left, right)| {
            difference | (left ^ right)
        })
        == 0
}

fn ensure_file(path: &Path, message: &'static str) -> Result<()> {
    if !path.is_file() {
        return Err(GmSslError::InvalidConfiguration(message).into());
    }
    Ok(())
}

fn path_to_cstring(path: &Path) -> Result<CString> {
    CString::new(path.to_str().ok_or(Error::NonUtf8Path)?).map_err(|_| Error::InvalidString {
        field: "GmSSL certificate path",
    })
}

enum NativeGmSslClient {}

unsafe extern "C" {
    fn cronet_gmssl_client_connect(
        protocol: c_int,
        socket_value: isize,
        ca_certificates: *const c_char,
        verify_depth: c_int,
        client_certificates: *const c_char,
        client_private_key: *const c_char,
        client_key_password: *const c_char,
        error_output: *mut c_char,
        error_capacity: usize,
    ) -> *mut NativeGmSslClient;
    fn cronet_gmssl_client_peer_leaf_sm3(
        client: *mut NativeGmSslClient,
        output: *mut c_uchar,
    ) -> c_int;
    fn cronet_gmssl_client_send(
        client: *mut NativeGmSslClient,
        input: *const c_uchar,
        input_length: usize,
        sent_length: *mut usize,
    ) -> c_int;
    fn cronet_gmssl_client_recv(
        client: *mut NativeGmSslClient,
        output: *mut c_uchar,
        output_capacity: usize,
        received_length: *mut usize,
    ) -> c_int;
    fn cronet_gmssl_client_destroy(client: *mut NativeGmSslClient);
}

struct NativeClient {
    raw: NonNull<NativeGmSslClient>,
}

impl NativeClient {
    fn connect(config: &GmSslConfig, stream: &TcpStream) -> Result<Self> {
        let mut error = [0_u8; NATIVE_ERROR_CAPACITY];
        let (client_certificates, client_private_key, client_key_password) =
            config.client_identity.as_ref().map_or(
                (std::ptr::null(), std::ptr::null(), std::ptr::null()),
                |identity| {
                    (
                        identity.certificate_chain.as_ptr(),
                        identity.private_key.as_ptr(),
                        identity.password.as_ptr(),
                    )
                },
            );
        // SAFETY: all C strings and the live socket outlive the native client;
        // the native constructor copies certificate file contents and state.
        let raw = unsafe {
            cronet_gmssl_client_connect(
                config.protocol.native(),
                raw_socket(stream),
                config.ca_certificates.as_ptr(),
                c_int::from(config.verify_depth),
                client_certificates,
                client_private_key,
                client_key_password,
                error.as_mut_ptr().cast(),
                error.len(),
            )
        };
        NonNull::new(raw).map_or_else(
            || {
                Err(GmSslError::Handshake(
                    CStr::from_bytes_until_nul(&error)
                        .map_or("unknown native error", |message| {
                            message.to_str().unwrap_or("non-UTF-8 native error")
                        })
                        .to_owned(),
                )
                .into())
            },
            |raw| Ok(Self { raw }),
        )
    }

    fn peer_leaf_sm3(&self) -> Result<[u8; 32]> {
        let mut digest = [0_u8; 32];
        // SAFETY: raw is a live GmSSL connection and digest has 32 bytes.
        if unsafe { cronet_gmssl_client_peer_leaf_sm3(self.raw.as_ptr(), digest.as_mut_ptr()) } != 1
        {
            return Err(GmSslError::Handshake(
                "could not read the peer leaf certificate".to_owned(),
            )
            .into());
        }
        Ok(digest)
    }

    fn send_all(&mut self, mut input: &[u8]) -> Result<()> {
        while !input.is_empty() {
            let mut sent = 0;
            // SAFETY: raw is live and input/sent remain valid for the call.
            let result = unsafe {
                cronet_gmssl_client_send(
                    self.raw.as_ptr(),
                    input.as_ptr(),
                    input.len(),
                    &raw mut sent,
                )
            };
            if result != 1 || sent == 0 || sent > input.len() {
                return Err(GmSslError::Io("native TLS send failed".to_owned()).into());
            }
            input = &input[sent..];
        }
        Ok(())
    }

    fn receive_to_end(&mut self, max_response_bytes: usize) -> Result<Vec<u8>> {
        let wire_limit = max_response_bytes.saturating_add(MAX_HTTP_WIRE_OVERHEAD);
        let mut output = Vec::new();
        let mut buffer = [0_u8; TLS_RECORD_PLAINTEXT];
        loop {
            let mut received = 0;
            // SAFETY: raw is live and the output buffer/length are writable.
            let result = unsafe {
                cronet_gmssl_client_recv(
                    self.raw.as_ptr(),
                    buffer.as_mut_ptr(),
                    buffer.len(),
                    &raw mut received,
                )
            };
            if result == 0 {
                return Ok(output);
            }
            if result != 1 || received == 0 || received > buffer.len() {
                return Err(GmSslError::Io("native TLS receive failed".to_owned()).into());
            }
            if output.len().saturating_add(received) > wire_limit {
                return Err(Error::ResponseTooLarge {
                    limit: max_response_bytes,
                });
            }
            output.extend_from_slice(&buffer[..received]);
        }
    }
}

impl Drop for NativeClient {
    fn drop(&mut self) {
        // SAFETY: raw is exclusively owned and destroyed exactly once.
        unsafe { cronet_gmssl_client_destroy(self.raw.as_ptr()) };
    }
}

#[cfg(unix)]
fn raw_socket(stream: &TcpStream) -> isize {
    use std::os::fd::AsRawFd;
    stream.as_raw_fd() as isize
}

#[cfg(windows)]
fn raw_socket(stream: &TcpStream) -> isize {
    use std::os::windows::io::AsRawSocket;
    stream.as_raw_socket() as isize
}

#[cfg(test)]
mod tests {
    use bytes::Bytes;

    use super::*;

    fn parts(url: &str, method: &str, body: Option<&'static [u8]>) -> GmSslRequestParts {
        GmSslRequestParts {
            url: url.to_owned(),
            method: method.to_owned(),
            headers: vec![Header::new("x-test", "one").unwrap()],
            body: body.map(Bytes::from_static),
            max_response_bytes: 1024,
        }
    }

    #[test]
    fn prepares_safe_http11_requests() {
        let prepared = PreparedRequest::new(parts(
            "https://example.com:8443/a%20b?q=1",
            "POST",
            Some(b"hello"),
        ))
        .unwrap();
        assert_eq!(prepared.host, "example.com");
        assert_eq!(prepared.port, 8443);
        assert_eq!(
            prepared.wire,
            b"POST /a%20b?q=1 HTTP/1.1\r\nHost: example.com:8443\r\nConnection: close\r\nAccept-Encoding: identity\r\nx-test: one\r\nContent-Length: 5\r\n\r\nhello"
        );
    }

    #[test]
    fn rejects_request_smuggling_inputs() {
        let mut managed = parts("https://example.com", "GET", None);
        managed.headers = vec![Header::new("Content-Length", "10").unwrap()];
        assert!(PreparedRequest::new(managed).is_err());

        let injected = parts("https://example.com", "GET\r\nX-Bad: yes", None);
        assert!(PreparedRequest::new(injected).is_err());
    }

    #[test]
    fn parses_fixed_and_chunked_responses() {
        let fixed = parse_response(
            b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nX-Test: yes\r\n\r\nhello",
            "GET",
            5,
            GmSslProtocol::Tls12,
        )
        .unwrap();
        assert_eq!(fixed.status(), 200);
        assert_eq!(fixed.body(), b"hello");
        assert_eq!(fixed.protocol, GmSslProtocol::Tls12);

        let chunked = parse_response(
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n3\r\none\r\n3\r\ntwo\r\n0\r\n\r\n",
            "GET",
            6,
            GmSslProtocol::Tlcp,
        )
        .unwrap();
        assert_eq!(chunked.into_body(), b"onetwo");
    }

    #[test]
    fn enforces_decoded_response_limit() {
        let response = b"HTTP/1.1 200 OK\r\nContent-Length: 6\r\n\r\n123456";
        assert!(matches!(
            parse_response(response, "GET", 5, GmSslProtocol::Tls13),
            Err(Error::ResponseTooLarge { limit: 5 })
        ));
    }
}

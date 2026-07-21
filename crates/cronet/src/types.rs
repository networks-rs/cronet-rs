use std::ffi::{CStr, CString, c_char};

use cronet_sys as sys;

use crate::{Error, NetworkError, NetworkErrorCode, Result};

/// An owned HTTP header.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Header {
    name: String,
    value: String,
}

impl Header {
    /// Creates a header after verifying that it can cross the C ABI.
    pub fn new(name: impl Into<String>, value: impl Into<String>) -> Result<Self> {
        let name = name.into();
        let value = value.into();
        validate_string(&name, "header name")?;
        validate_string(&value, "header value")?;
        Ok(Self { name, value })
    }

    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    #[must_use]
    pub fn value(&self) -> &str {
        &self.value
    }

    pub(crate) fn c_name(&self) -> CString {
        CString::new(self.name.as_bytes()).expect("Header::new validated the name")
    }

    pub(crate) fn c_value(&self) -> CString {
        CString::new(self.value.as_bytes()).expect("Header::new validated the value")
    }
}

/// Metadata copied out of Cronet's callback-owned response object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResponseInfo {
    pub url: String,
    pub url_chain: Vec<String>,
    pub status_code: i32,
    pub status_text: String,
    pub headers: Vec<Header>,
    pub was_cached: bool,
    pub negotiated_protocol: String,
    pub proxy_server: String,
    pub received_byte_count: i64,
}

/// Metadata for one redirect before Cronet follows or rejects it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RedirectInfo {
    pub response: ResponseInfo,
    pub location: String,
}

/// The phase in which a request is currently waiting or doing work.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum RequestStatus {
    Invalid,
    Idle,
    WaitingForStalledSocketPool,
    WaitingForAvailableSocket,
    WaitingForDelegate,
    WaitingForCache,
    DownloadingPacFile,
    ResolvingProxyForUrl,
    ResolvingHostInPacFile,
    EstablishingProxyTunnel,
    ResolvingHost,
    Connecting,
    SslHandshake,
    SendingRequest,
    WaitingForResponse,
    ReadingResponse,
    Unknown(i32),
}

impl RequestStatus {
    pub(crate) fn from_raw(value: sys::Cronet_UrlRequestStatusListener_Status) -> Self {
        match value {
            sys::Cronet_UrlRequestStatusListener_Status_INVALID => Self::Invalid,
            sys::Cronet_UrlRequestStatusListener_Status_IDLE => Self::Idle,
            sys::Cronet_UrlRequestStatusListener_Status_WAITING_FOR_STALLED_SOCKET_POOL => {
                Self::WaitingForStalledSocketPool
            }
            sys::Cronet_UrlRequestStatusListener_Status_WAITING_FOR_AVAILABLE_SOCKET => {
                Self::WaitingForAvailableSocket
            }
            sys::Cronet_UrlRequestStatusListener_Status_WAITING_FOR_DELEGATE => {
                Self::WaitingForDelegate
            }
            sys::Cronet_UrlRequestStatusListener_Status_WAITING_FOR_CACHE => Self::WaitingForCache,
            sys::Cronet_UrlRequestStatusListener_Status_DOWNLOADING_PAC_FILE => {
                Self::DownloadingPacFile
            }
            sys::Cronet_UrlRequestStatusListener_Status_RESOLVING_PROXY_FOR_URL => {
                Self::ResolvingProxyForUrl
            }
            sys::Cronet_UrlRequestStatusListener_Status_RESOLVING_HOST_IN_PAC_FILE => {
                Self::ResolvingHostInPacFile
            }
            sys::Cronet_UrlRequestStatusListener_Status_ESTABLISHING_PROXY_TUNNEL => {
                Self::EstablishingProxyTunnel
            }
            sys::Cronet_UrlRequestStatusListener_Status_RESOLVING_HOST => Self::ResolvingHost,
            sys::Cronet_UrlRequestStatusListener_Status_CONNECTING => Self::Connecting,
            sys::Cronet_UrlRequestStatusListener_Status_SSL_HANDSHAKE => Self::SslHandshake,
            sys::Cronet_UrlRequestStatusListener_Status_SENDING_REQUEST => Self::SendingRequest,
            sys::Cronet_UrlRequestStatusListener_Status_WAITING_FOR_RESPONSE => {
                Self::WaitingForResponse
            }
            sys::Cronet_UrlRequestStatusListener_Status_READING_RESPONSE => Self::ReadingResponse,
            other => Self::Unknown(other),
        }
    }
}

/// Why Cronet considers a request finished.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum FinishedReason {
    Succeeded,
    Failed,
    Canceled,
    Unknown(u32),
}

/// Request timing and traffic metrics, copied from Cronet.
///
/// Timestamps are Unix epoch milliseconds. A missing timestamp means that the
/// corresponding phase did not run (for example, DNS on a reused connection).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestMetrics {
    pub request_start: Option<i64>,
    pub dns_start: Option<i64>,
    pub dns_end: Option<i64>,
    pub connect_start: Option<i64>,
    pub connect_end: Option<i64>,
    pub ssl_start: Option<i64>,
    pub ssl_end: Option<i64>,
    pub sending_start: Option<i64>,
    pub sending_end: Option<i64>,
    pub push_start: Option<i64>,
    pub push_end: Option<i64>,
    pub response_start: Option<i64>,
    pub request_end: Option<i64>,
    pub socket_reused: bool,
    pub sent_byte_count: i64,
    pub received_byte_count: i64,
}

/// Final request information delivered after Cronet has completed accounting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestFinishedInfo {
    pub metrics: RequestMetrics,
    pub annotations: Vec<String>,
    pub reason: FinishedReason,
    pub response: Option<ResponseInfo>,
    pub error: Option<NetworkError>,
}

/// A complete buffered HTTP response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Response {
    pub info: ResponseInfo,
    pub body: Vec<u8>,
    pub finished: RequestFinishedInfo,
}

impl Response {
    #[must_use]
    pub fn status(&self) -> i32 {
        self.info.status_code
    }

    #[must_use]
    pub fn body(&self) -> &[u8] {
        &self.body
    }

    #[must_use]
    pub fn into_body(self) -> Vec<u8> {
        self.body
    }

    #[must_use]
    pub fn metrics(&self) -> &RequestMetrics {
        &self.finished.metrics
    }
}

pub(crate) fn validate_string(value: &str, field: &'static str) -> Result<()> {
    CString::new(value.as_bytes())
        .map(|_| ())
        .map_err(|_| Error::InvalidString { field })
}

pub(crate) fn to_cstring(value: &str, field: &'static str) -> Result<CString> {
    CString::new(value.as_bytes()).map_err(|_| Error::InvalidString { field })
}

pub(crate) unsafe fn copy_response_info(raw: sys::Cronet_UrlResponseInfoPtr) -> ResponseInfo {
    if raw.is_null() {
        return ResponseInfo {
            url: String::new(),
            url_chain: Vec::new(),
            status_code: 0,
            status_text: String::new(),
            headers: Vec::new(),
            was_cached: false,
            negotiated_protocol: String::new(),
            proxy_server: String::new(),
            received_byte_count: 0,
        };
    }

    // SAFETY: `raw` is callback-owned and valid for the duration of this
    // function. Every borrowed C string is copied before returning.
    let url = unsafe { copy_c_string(sys::Cronet_UrlResponseInfo_url_get(raw)) };
    // SAFETY: same callback-owned response object as above.
    let chain_len = unsafe { sys::Cronet_UrlResponseInfo_url_chain_size(raw) };
    let mut url_chain = Vec::with_capacity(chain_len as usize);
    for index in 0..chain_len {
        // SAFETY: `index` is bounded by the size returned for this object.
        url_chain
            .push(unsafe { copy_c_string(sys::Cronet_UrlResponseInfo_url_chain_at(raw, index)) });
    }

    // SAFETY: same callback-owned response object as above.
    let header_len = unsafe { sys::Cronet_UrlResponseInfo_all_headers_list_size(raw) };
    let mut headers = Vec::with_capacity(header_len as usize);
    for index in 0..header_len {
        // SAFETY: `index` is bounded by the size returned for this object.
        let header = unsafe { sys::Cronet_UrlResponseInfo_all_headers_list_at(raw, index) };
        if !header.is_null() {
            // SAFETY: nested headers live as long as `raw`; strings are copied.
            let name = unsafe { copy_c_string(sys::Cronet_HttpHeader_name_get(header)) };
            // SAFETY: as above.
            let value = unsafe { copy_c_string(sys::Cronet_HttpHeader_value_get(header)) };
            headers.push(Header { name, value });
        }
    }

    ResponseInfo {
        url,
        url_chain,
        // SAFETY: all getters only inspect the valid callback object.
        status_code: unsafe { sys::Cronet_UrlResponseInfo_http_status_code_get(raw) },
        // SAFETY: returned string is copied immediately.
        status_text: unsafe {
            copy_c_string(sys::Cronet_UrlResponseInfo_http_status_text_get(raw))
        },
        headers,
        // SAFETY: all getters only inspect the valid callback object.
        was_cached: unsafe { sys::Cronet_UrlResponseInfo_was_cached_get(raw) },
        // SAFETY: returned string is copied immediately.
        negotiated_protocol: unsafe {
            copy_c_string(sys::Cronet_UrlResponseInfo_negotiated_protocol_get(raw))
        },
        // SAFETY: returned string is copied immediately.
        proxy_server: unsafe { copy_c_string(sys::Cronet_UrlResponseInfo_proxy_server_get(raw)) },
        // SAFETY: getter only inspects the valid callback object.
        received_byte_count: unsafe { sys::Cronet_UrlResponseInfo_received_byte_count_get(raw) },
    }
}

pub(crate) unsafe fn copy_network_error(raw: sys::Cronet_ErrorPtr) -> NetworkError {
    if raw.is_null() {
        return NetworkError {
            code: NetworkErrorCode::Callback,
            message: "Cronet returned a null error object".to_owned(),
            internal_error_code: 0,
            immediately_retryable: false,
            quic_detailed_error_code: 0,
        };
    }
    // SAFETY: the error is callback-owned and all data is copied immediately.
    unsafe {
        NetworkError {
            code: NetworkErrorCode::from_raw(sys::Cronet_Error_error_code_get(raw)),
            message: copy_c_string(sys::Cronet_Error_message_get(raw)),
            internal_error_code: sys::Cronet_Error_internal_error_code_get(raw),
            immediately_retryable: sys::Cronet_Error_immediately_retryable_get(raw),
            quic_detailed_error_code: sys::Cronet_Error_quic_detailed_error_code_get(raw),
        }
    }
}

pub(crate) unsafe fn copy_finished_info(
    raw: sys::Cronet_RequestFinishedInfoPtr,
    response: sys::Cronet_UrlResponseInfoPtr,
    error: sys::Cronet_ErrorPtr,
    annotations: &[(*mut std::ffi::c_void, String)],
) -> RequestFinishedInfo {
    // A null object is not expected from Cronet, but keeping this copier total
    // avoids dereferencing it if a custom build violates the contract.
    if raw.is_null() {
        return RequestFinishedInfo {
            metrics: empty_metrics(),
            annotations: Vec::new(),
            reason: FinishedReason::Unknown(u32::MAX),
            response: (!response.is_null()).then(|| unsafe { copy_response_info(response) }),
            error: (!error.is_null()).then(|| unsafe { copy_network_error(error) }),
        };
    }

    // SAFETY: all getters inspect callback-owned values for this call only.
    let metrics = unsafe { copy_metrics(sys::Cronet_RequestFinishedInfo_metrics_get(raw)) };
    // SAFETY: the request-finished info is valid for this callback.
    let reason = match unsafe { sys::Cronet_RequestFinishedInfo_finished_reason_get(raw) } {
        sys::Cronet_RequestFinishedInfo_FINISHED_REASON_SUCCEEDED => FinishedReason::Succeeded,
        sys::Cronet_RequestFinishedInfo_FINISHED_REASON_FAILED => FinishedReason::Failed,
        sys::Cronet_RequestFinishedInfo_FINISHED_REASON_CANCELED => FinishedReason::Canceled,
        other => FinishedReason::Unknown(other),
    };
    // SAFETY: size and indexed values belong to the same live object.
    let annotation_count = unsafe { sys::Cronet_RequestFinishedInfo_annotations_size(raw) };
    let mut copied_annotations = Vec::with_capacity(annotation_count as usize);
    for index in 0..annotation_count {
        // SAFETY: index is bounded by the native size above.
        let annotation = unsafe { sys::Cronet_RequestFinishedInfo_annotations_at(raw, index) };
        if let Some((_, value)) = annotations
            .iter()
            .find(|(address, _)| *address == annotation)
        {
            copied_annotations.push(value.clone());
        }
    }
    // A few native Cronet builds omit the opaque handle array from the
    // per-request listener even though they preserve it for engine listeners.
    // The safe layer owns this request's exact annotations, so retain their
    // association rather than silently returning an empty list.
    if copied_annotations.len() != annotations.len() {
        copied_annotations.clear();
        copied_annotations.extend(annotations.iter().map(|(_, value)| value.clone()));
    }

    RequestFinishedInfo {
        metrics,
        annotations: copied_annotations,
        reason,
        response: (!response.is_null()).then(|| unsafe { copy_response_info(response) }),
        error: (!error.is_null()).then(|| unsafe { copy_network_error(error) }),
    }
}

unsafe fn copy_metrics(raw: sys::Cronet_MetricsPtr) -> RequestMetrics {
    if raw.is_null() {
        return empty_metrics();
    }
    RequestMetrics {
        // SAFETY: all nested DateTime objects are borrowed from live metrics.
        request_start: unsafe { copy_date_time(sys::Cronet_Metrics_request_start_get(raw)) },
        dns_start: unsafe { copy_date_time(sys::Cronet_Metrics_dns_start_get(raw)) },
        dns_end: unsafe { copy_date_time(sys::Cronet_Metrics_dns_end_get(raw)) },
        connect_start: unsafe { copy_date_time(sys::Cronet_Metrics_connect_start_get(raw)) },
        connect_end: unsafe { copy_date_time(sys::Cronet_Metrics_connect_end_get(raw)) },
        ssl_start: unsafe { copy_date_time(sys::Cronet_Metrics_ssl_start_get(raw)) },
        ssl_end: unsafe { copy_date_time(sys::Cronet_Metrics_ssl_end_get(raw)) },
        sending_start: unsafe { copy_date_time(sys::Cronet_Metrics_sending_start_get(raw)) },
        sending_end: unsafe { copy_date_time(sys::Cronet_Metrics_sending_end_get(raw)) },
        push_start: unsafe { copy_date_time(sys::Cronet_Metrics_push_start_get(raw)) },
        push_end: unsafe { copy_date_time(sys::Cronet_Metrics_push_end_get(raw)) },
        response_start: unsafe { copy_date_time(sys::Cronet_Metrics_response_start_get(raw)) },
        request_end: unsafe { copy_date_time(sys::Cronet_Metrics_request_end_get(raw)) },
        // SAFETY: scalar getters inspect the same live metrics object.
        socket_reused: unsafe { sys::Cronet_Metrics_socket_reused_get(raw) },
        sent_byte_count: unsafe { sys::Cronet_Metrics_sent_byte_count_get(raw) },
        received_byte_count: unsafe { sys::Cronet_Metrics_received_byte_count_get(raw) },
    }
}

unsafe fn copy_date_time(raw: sys::Cronet_DateTimePtr) -> Option<i64> {
    if raw.is_null() {
        None
    } else {
        // SAFETY: the DateTime is borrowed from the live metrics object.
        Some(unsafe { sys::Cronet_DateTime_value_get(raw) })
    }
}

const fn empty_metrics() -> RequestMetrics {
    RequestMetrics {
        request_start: None,
        dns_start: None,
        dns_end: None,
        connect_start: None,
        connect_end: None,
        ssl_start: None,
        ssl_end: None,
        sending_start: None,
        sending_end: None,
        push_start: None,
        push_end: None,
        response_start: None,
        request_end: None,
        socket_reused: false,
        sent_byte_count: 0,
        received_byte_count: 0,
    }
}

pub(crate) unsafe fn copy_c_string(raw: *const c_char) -> String {
    if raw.is_null() {
        String::new()
    } else {
        // SAFETY: callers only pass NUL-terminated strings borrowed from Cronet.
        unsafe { CStr::from_ptr(raw) }
            .to_string_lossy()
            .into_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_nul_in_header() {
        assert!(matches!(
            Header::new("x-test", "bad\0value"),
            Err(Error::InvalidString {
                field: "header value"
            })
        ));
    }

    #[test]
    fn maps_every_native_request_status() {
        let statuses = [
            (
                sys::Cronet_UrlRequestStatusListener_Status_INVALID,
                RequestStatus::Invalid,
            ),
            (
                sys::Cronet_UrlRequestStatusListener_Status_IDLE,
                RequestStatus::Idle,
            ),
            (
                sys::Cronet_UrlRequestStatusListener_Status_WAITING_FOR_STALLED_SOCKET_POOL,
                RequestStatus::WaitingForStalledSocketPool,
            ),
            (
                sys::Cronet_UrlRequestStatusListener_Status_WAITING_FOR_AVAILABLE_SOCKET,
                RequestStatus::WaitingForAvailableSocket,
            ),
            (
                sys::Cronet_UrlRequestStatusListener_Status_WAITING_FOR_DELEGATE,
                RequestStatus::WaitingForDelegate,
            ),
            (
                sys::Cronet_UrlRequestStatusListener_Status_WAITING_FOR_CACHE,
                RequestStatus::WaitingForCache,
            ),
            (
                sys::Cronet_UrlRequestStatusListener_Status_DOWNLOADING_PAC_FILE,
                RequestStatus::DownloadingPacFile,
            ),
            (
                sys::Cronet_UrlRequestStatusListener_Status_RESOLVING_PROXY_FOR_URL,
                RequestStatus::ResolvingProxyForUrl,
            ),
            (
                sys::Cronet_UrlRequestStatusListener_Status_RESOLVING_HOST_IN_PAC_FILE,
                RequestStatus::ResolvingHostInPacFile,
            ),
            (
                sys::Cronet_UrlRequestStatusListener_Status_ESTABLISHING_PROXY_TUNNEL,
                RequestStatus::EstablishingProxyTunnel,
            ),
            (
                sys::Cronet_UrlRequestStatusListener_Status_RESOLVING_HOST,
                RequestStatus::ResolvingHost,
            ),
            (
                sys::Cronet_UrlRequestStatusListener_Status_CONNECTING,
                RequestStatus::Connecting,
            ),
            (
                sys::Cronet_UrlRequestStatusListener_Status_SSL_HANDSHAKE,
                RequestStatus::SslHandshake,
            ),
            (
                sys::Cronet_UrlRequestStatusListener_Status_SENDING_REQUEST,
                RequestStatus::SendingRequest,
            ),
            (
                sys::Cronet_UrlRequestStatusListener_Status_WAITING_FOR_RESPONSE,
                RequestStatus::WaitingForResponse,
            ),
            (
                sys::Cronet_UrlRequestStatusListener_Status_READING_RESPONSE,
                RequestStatus::ReadingResponse,
            ),
        ];
        for (raw, expected) in statuses {
            assert_eq!(RequestStatus::from_raw(raw), expected);
        }
        assert_eq!(RequestStatus::from_raw(99), RequestStatus::Unknown(99));
    }
}

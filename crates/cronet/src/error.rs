use std::{error, fmt};

use cronet_sys as sys;

use crate::ResponseInfo;

/// Result type used by this crate.
pub type Result<T> = std::result::Result<T, Error>;

/// An error produced while configuring or using Cronet.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Error {
    /// A string contains an interior NUL and cannot cross the C ABI.
    InvalidString { field: &'static str },
    /// A configured path cannot be represented as UTF-8.
    NonUtf8Path,
    /// A builder option is outside the supported range.
    InvalidConfiguration(&'static str),
    /// Disk caching requires a storage directory that already exists.
    StoragePathMissing,
    /// Cronet returned a synchronous API result code.
    Cronet(ResultCode),
    /// Cronet reported a network failure asynchronously.
    Network(NetworkError),
    /// The request was canceled.
    Canceled,
    /// Redirect following was disabled for this request.
    Redirect {
        location: String,
        response: Box<ResponseInfo>,
    },
    /// The response body exceeded the configured safety limit.
    ResponseTooLarge { limit: usize },
    /// Cronet reported more bytes than fit in the supplied read buffer.
    InvalidReadSize { reported: u64, capacity: u64 },
    /// A native constructor unexpectedly returned a null pointer.
    AllocationFailed(&'static str),
    /// Cronet stopped its callback channel without a terminal callback.
    CallbackChannelClosed,
    /// Engine construction was attempted outside a Tokio runtime.
    TokioRuntimeRequired,
    /// The engine has begun or completed shutdown.
    EngineShutdown,
    /// A Tokio task used for a blocking native operation could not complete.
    TokioTask(String),
    /// An asynchronous upload source failed.
    Upload(String),
    /// The bidirectional stream C API rejected an operation synchronously.
    BidirectionalApi { operation: &'static str, code: i32 },
    /// Chromium's network stack failed a bidirectional stream.
    BidirectionalStream { net_error: i32 },
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidString { field } => write!(formatter, "{field} contains an interior NUL"),
            Self::NonUtf8Path => formatter.write_str("path is not valid UTF-8"),
            Self::InvalidConfiguration(message) => formatter.write_str(message),
            Self::StoragePathMissing => {
                formatter.write_str("disk cache storage path must be an existing directory")
            }
            Self::Cronet(code) => write!(formatter, "Cronet API call failed: {code}"),
            Self::Network(error) => write!(formatter, "Cronet request failed: {error}"),
            Self::Canceled => formatter.write_str("Cronet request was canceled"),
            Self::Redirect { location, .. } => {
                write!(formatter, "redirect was not followed: {location}")
            }
            Self::ResponseTooLarge { limit } => {
                write!(formatter, "response body exceeds the {limit}-byte limit")
            }
            Self::InvalidReadSize { reported, capacity } => write!(
                formatter,
                "Cronet reported a {reported}-byte read for a {capacity}-byte buffer"
            ),
            Self::AllocationFailed(kind) => write!(formatter, "Cronet failed to create {kind}"),
            Self::CallbackChannelClosed => {
                formatter.write_str("Cronet callback channel closed before completion")
            }
            Self::TokioRuntimeRequired => {
                formatter.write_str("Cronet engine must be created inside a Tokio runtime")
            }
            Self::EngineShutdown => formatter.write_str("Cronet engine is shutting down"),
            Self::TokioTask(message) => write!(formatter, "Tokio task failed: {message}"),
            Self::Upload(message) => write!(formatter, "request upload failed: {message}"),
            Self::BidirectionalApi { operation, code } => {
                write!(
                    formatter,
                    "bidirectional stream {operation} failed with code {code}"
                )
            }
            Self::BidirectionalStream { net_error } => {
                write!(
                    formatter,
                    "bidirectional stream failed with net error {net_error}"
                )
            }
        }
    }
}

impl error::Error for Error {}

/// A synchronous result code returned by the native C API.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ResultCode {
    IllegalArgument,
    StoragePathMustExist,
    InvalidPin,
    InvalidHostname,
    InvalidHttpMethod,
    InvalidHttpHeader,
    IllegalState,
    StoragePathInUse,
    ShutdownFromNetworkThread,
    EngineAlreadyStarted,
    RequestAlreadyStarted,
    RequestNotInitialized,
    RequestAlreadyInitialized,
    RequestNotStarted,
    UnexpectedRedirect,
    UnexpectedRead,
    ReadFailed,
    NullPointer,
    NullHostname,
    NullPins,
    NullExpirationDate,
    NullEngine,
    NullUrl,
    NullCallback,
    NullExecutor,
    NullMethod,
    NullHeaderName,
    NullHeaderValue,
    NullParams,
    NullFinishedListenerExecutor,
    Unknown(i32),
}

impl ResultCode {
    pub(crate) fn from_raw(value: sys::Cronet_RESULT) -> Option<Self> {
        match value {
            sys::Cronet_RESULT_SUCCESS => None,
            sys::Cronet_RESULT_ILLEGAL_ARGUMENT => Some(Self::IllegalArgument),
            sys::Cronet_RESULT_ILLEGAL_ARGUMENT_STORAGE_PATH_MUST_EXIST => {
                Some(Self::StoragePathMustExist)
            }
            sys::Cronet_RESULT_ILLEGAL_ARGUMENT_INVALID_PIN => Some(Self::InvalidPin),
            sys::Cronet_RESULT_ILLEGAL_ARGUMENT_INVALID_HOSTNAME => Some(Self::InvalidHostname),
            sys::Cronet_RESULT_ILLEGAL_ARGUMENT_INVALID_HTTP_METHOD => {
                Some(Self::InvalidHttpMethod)
            }
            sys::Cronet_RESULT_ILLEGAL_ARGUMENT_INVALID_HTTP_HEADER => {
                Some(Self::InvalidHttpHeader)
            }
            sys::Cronet_RESULT_ILLEGAL_STATE => Some(Self::IllegalState),
            sys::Cronet_RESULT_ILLEGAL_STATE_STORAGE_PATH_IN_USE => Some(Self::StoragePathInUse),
            sys::Cronet_RESULT_ILLEGAL_STATE_CANNOT_SHUTDOWN_ENGINE_FROM_NETWORK_THREAD => {
                Some(Self::ShutdownFromNetworkThread)
            }
            sys::Cronet_RESULT_ILLEGAL_STATE_ENGINE_ALREADY_STARTED => {
                Some(Self::EngineAlreadyStarted)
            }
            sys::Cronet_RESULT_ILLEGAL_STATE_REQUEST_ALREADY_STARTED => {
                Some(Self::RequestAlreadyStarted)
            }
            sys::Cronet_RESULT_ILLEGAL_STATE_REQUEST_NOT_INITIALIZED => {
                Some(Self::RequestNotInitialized)
            }
            sys::Cronet_RESULT_ILLEGAL_STATE_REQUEST_ALREADY_INITIALIZED => {
                Some(Self::RequestAlreadyInitialized)
            }
            sys::Cronet_RESULT_ILLEGAL_STATE_REQUEST_NOT_STARTED => Some(Self::RequestNotStarted),
            sys::Cronet_RESULT_ILLEGAL_STATE_UNEXPECTED_REDIRECT => Some(Self::UnexpectedRedirect),
            sys::Cronet_RESULT_ILLEGAL_STATE_UNEXPECTED_READ => Some(Self::UnexpectedRead),
            sys::Cronet_RESULT_ILLEGAL_STATE_READ_FAILED => Some(Self::ReadFailed),
            sys::Cronet_RESULT_NULL_POINTER => Some(Self::NullPointer),
            sys::Cronet_RESULT_NULL_POINTER_HOSTNAME => Some(Self::NullHostname),
            sys::Cronet_RESULT_NULL_POINTER_SHA256_PINS => Some(Self::NullPins),
            sys::Cronet_RESULT_NULL_POINTER_EXPIRATION_DATE => Some(Self::NullExpirationDate),
            sys::Cronet_RESULT_NULL_POINTER_ENGINE => Some(Self::NullEngine),
            sys::Cronet_RESULT_NULL_POINTER_URL => Some(Self::NullUrl),
            sys::Cronet_RESULT_NULL_POINTER_CALLBACK => Some(Self::NullCallback),
            sys::Cronet_RESULT_NULL_POINTER_EXECUTOR => Some(Self::NullExecutor),
            sys::Cronet_RESULT_NULL_POINTER_METHOD => Some(Self::NullMethod),
            sys::Cronet_RESULT_NULL_POINTER_HEADER_NAME => Some(Self::NullHeaderName),
            sys::Cronet_RESULT_NULL_POINTER_HEADER_VALUE => Some(Self::NullHeaderValue),
            sys::Cronet_RESULT_NULL_POINTER_PARAMS => Some(Self::NullParams),
            sys::Cronet_RESULT_NULL_POINTER_REQUEST_FINISHED_INFO_LISTENER_EXECUTOR => {
                Some(Self::NullFinishedListenerExecutor)
            }
            other => Some(Self::Unknown(other)),
        }
    }
}

impl fmt::Display for ResultCode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{self:?}")
    }
}

/// Detailed failure copied from the callback-owned `Cronet_Error` value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetworkError {
    pub code: NetworkErrorCode,
    pub message: String,
    pub internal_error_code: i32,
    pub immediately_retryable: bool,
    pub quic_detailed_error_code: i32,
}

impl fmt::Display for NetworkError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} ({:?})", self.message, self.code)
    }
}

/// Stable classification of a Cronet request failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum NetworkErrorCode {
    Callback,
    HostnameNotResolved,
    InternetDisconnected,
    NetworkChanged,
    TimedOut,
    ConnectionClosed,
    ConnectionTimedOut,
    ConnectionRefused,
    ConnectionReset,
    AddressUnreachable,
    QuicProtocolFailed,
    Other,
    Unknown(u32),
}

impl NetworkErrorCode {
    pub(crate) fn from_raw(value: sys::Cronet_Error_ERROR_CODE) -> Self {
        match value {
            sys::Cronet_Error_ERROR_CODE_ERROR_CALLBACK => Self::Callback,
            sys::Cronet_Error_ERROR_CODE_ERROR_HOSTNAME_NOT_RESOLVED => Self::HostnameNotResolved,
            sys::Cronet_Error_ERROR_CODE_ERROR_INTERNET_DISCONNECTED => Self::InternetDisconnected,
            sys::Cronet_Error_ERROR_CODE_ERROR_NETWORK_CHANGED => Self::NetworkChanged,
            sys::Cronet_Error_ERROR_CODE_ERROR_TIMED_OUT => Self::TimedOut,
            sys::Cronet_Error_ERROR_CODE_ERROR_CONNECTION_CLOSED => Self::ConnectionClosed,
            sys::Cronet_Error_ERROR_CODE_ERROR_CONNECTION_TIMED_OUT => Self::ConnectionTimedOut,
            sys::Cronet_Error_ERROR_CODE_ERROR_CONNECTION_REFUSED => Self::ConnectionRefused,
            sys::Cronet_Error_ERROR_CODE_ERROR_CONNECTION_RESET => Self::ConnectionReset,
            sys::Cronet_Error_ERROR_CODE_ERROR_ADDRESS_UNREACHABLE => Self::AddressUnreachable,
            sys::Cronet_Error_ERROR_CODE_ERROR_QUIC_PROTOCOL_FAILED => Self::QuicProtocolFailed,
            sys::Cronet_Error_ERROR_CODE_ERROR_OTHER => Self::Other,
            other => Self::Unknown(other),
        }
    }
}

pub(crate) fn check(value: sys::Cronet_RESULT) -> Result<()> {
    ResultCode::from_raw(value).map_or(Ok(()), |code| Err(Error::Cronet(code)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn success_is_not_an_error() {
        assert_eq!(ResultCode::from_raw(sys::Cronet_RESULT_SUCCESS), None);
    }

    #[test]
    fn preserves_unknown_result_code() {
        assert_eq!(ResultCode::from_raw(-999), Some(ResultCode::Unknown(-999)));
    }
}

//! Safe Rust bindings for Chromium Cronet.

mod bidirectional;
mod engine;
mod error;
mod executor;
mod request;
mod types;

pub use bidirectional::{
    BidirectionalRequest, BidirectionalRequestBuilder, BidirectionalResponseHeaders,
    BidirectionalStream,
};
pub use engine::{CacheMode, Engine, EngineBuilder, PublicKeyPins, QuicHint};
pub use error::{Error, NetworkError, NetworkErrorCode, Result, ResultCode};
pub use request::{
    Idempotency, PendingRequest, Priority, RedirectAction, Request, RequestBuilder, RequestHandle,
    ResponseBody, StreamingResponse,
};
pub use types::{
    FinishedReason, Header, RedirectInfo, RequestFinishedInfo, RequestMetrics, RequestStatus,
    Response, ResponseInfo,
};

#[cfg(all(target_os = "android", feature = "static"))]
pub mod android {
    use std::ffi::{c_int, c_void};

    /// Initializes Cronet's Java VM bridge for a statically linked Android app.
    ///
    /// The final Android `cdylib` must export `JNI_OnLoad` and forward its
    /// `JavaVM*` argument here before creating an [`crate::Engine`]. Dynamic
    /// linking performs this initialization in Cronet's own shared library.
    ///
    /// # Safety
    ///
    /// `java_vm` must be the valid process-wide `JavaVM*` received by
    /// `JNI_OnLoad`, and initialization must happen only once.
    pub unsafe fn initialize_java_vm(java_vm: *mut c_void) -> c_int {
        // SAFETY: upheld by this function's caller contract.
        unsafe { cronet_sys::Cronet_RS_InitializeJavaVM(java_vm) }
    }
}

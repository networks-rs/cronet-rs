//! Raw, build-time generated bindings to Cronet's native C API.
//!
//! The declarations are generated from upstream `cronet_c.h` and
//! `bidirectional_stream_c.h` at build time.
//! `tokio-cronet-src` supplies the pinned headers and always builds the native
//! library from the same filtered source tree unless an explicit
//! `CRONET_LIB_DIR` override is supplied. Prefer the safe `tokio-cronet` crate.

#![allow(
    non_camel_case_types,
    non_snake_case,
    non_upper_case_globals,
    clippy::missing_safety_doc,
    clippy::doc_markdown,
    clippy::too_many_arguments,
    clippy::unreadable_literal
)]

include!(concat!(env!("OUT_DIR"), "/bindings.rs"));

#[cfg(all(target_os = "android", feature = "static"))]
unsafe extern "C" {
    /// Initializes Chromium's process-wide Java VM bridge for static Android
    /// embedding. Call this exactly once from the final library's JNI_OnLoad.
    pub fn Cronet_RS_InitializeJavaVM(vm: *mut std::ffi::c_void) -> std::ffi::c_int;
}

use std::{
    any::Any,
    panic::{self, AssertUnwindSafe},
};

#[cfg(target_os = "android")]
use std::ffi::c_void;

#[path = "../../../../crates/tokio-cronet/tests/support/portable_e2e.rs"]
mod portable_e2e;

/// Runs the exact same Tokio/Cronet suite from an Android Activity or iOS app.
#[unsafe(no_mangle)]
pub extern "C" fn cronet_rs_mobile_e2e_run() -> i32 {
    let outcome = panic::catch_unwind(AssertUnwindSafe(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(4)
            .enable_all()
            .build()
            .expect("create the Tokio runtime")
            .block_on(portable_e2e::run_all());
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
    unsafe { tokio_cronet::android::initialize_java_vm(java_vm) }
}

fn panic_message(payload: &(dyn Any + Send)) -> &str {
    payload
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("unknown panic")
}

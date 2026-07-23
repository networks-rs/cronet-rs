use std::{
    any::Any,
    fs,
    panic::{self, AssertUnwindSafe},
    path::Path,
};

#[path = "../../../../crates/cronet/tests/support/portable_e2e.rs"]
mod portable_e2e;

const RESULT_DIRECTORY: &str = "/data/storage/el2/base/files";
const RESULT_FILE: &str = "/data/storage/el2/base/files/cronet-rs-e2e.txt";

#[unsafe(no_mangle)]
pub extern "C" fn cronet_rs_ohos_e2e_run() -> i32 {
    let _ = fs::create_dir_all(RESULT_DIRECTORY);
    write_result("RUNNING\n");

    let outcome = panic::catch_unwind(AssertUnwindSafe(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(4)
            .enable_all()
            .build()
            .expect("create the Tokio runtime")
            .block_on(portable_e2e::run_all())
    }));

    match outcome {
        Ok(()) => {
            write_result("PASS\n");
            0
        }
        Err(payload) => {
            let message = panic_message(payload.as_ref());
            write_result(&format!("FAIL: {message}\n"));
            1
        }
    }
}

fn write_result(contents: &str) {
    eprintln!("cronet-rs OHOS E2E: {}", contents.trim());
    let _ = fs::write(Path::new(RESULT_FILE), contents);
}

fn panic_message(payload: &(dyn Any + Send)) -> &str {
    payload
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("unknown panic")
}

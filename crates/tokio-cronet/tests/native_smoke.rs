#![cfg(feature = "native-tests")]

#[path = "support/portable_e2e.rs"]
mod portable_e2e;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn request_api_and_tokio_io() {
    portable_e2e::request_api_and_tokio_io().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn request_controls_and_terminal_callbacks() {
    portable_e2e::request_controls_and_terminal_callbacks().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn callback_and_transport_failures_are_typed() {
    portable_e2e::callback_and_transport_failures_are_typed().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pending_upload_and_rewind_cancellation_are_safe() {
    portable_e2e::pending_upload_and_rewind_cancellation_are_safe().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancellation_and_shutdown_races_are_safe() {
    portable_e2e::cancellation_and_shutdown_races_are_safe().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn bidirectional_configuration_and_failure_are_safe() {
    portable_e2e::bidirectional_configuration_and_failure_are_safe().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn zz_engine_drop_with_active_work_is_process_safe() {
    portable_e2e::engine_drop_with_active_work_is_process_safe().await;
}

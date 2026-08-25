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

#[cfg(feature = "sse")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sse_event_source_covers_events_reconnect_and_cancel() {
    portable_e2e::sse_event_source_covers_events_reconnect_and_cancel().await;
}

#[cfg(feature = "ws")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn websocket_echo_and_close_are_terminal() {
    portable_e2e::websocket_echo_and_close_are_terminal().await;
}

#[cfg(feature = "nqe")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn nqe_observes_localhost_after_testing_override() {
    portable_e2e::nqe_observes_localhost_after_testing_override().await;
}

#[cfg(feature = "network-binding")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn network_binding_round_trips_engine_handle() {
    portable_e2e::network_binding_round_trips_engine_handle().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn zz_engine_drop_with_active_work_is_process_safe() {
    portable_e2e::engine_drop_with_active_work_is_process_safe().await;
}

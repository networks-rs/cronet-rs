# tokio-cronet-gmssl-sys

This crate is a narrow compatibility copy of `gmssl-rs-sys` from
[`GmSSL/gmssl-rs`](https://github.com/GmSSL/gmssl-rs), pinned at revision
`fe981bd09d1d176ee19c7038a777e71783901d48`.

The FFI declarations are unchanged. The build integration adds the Cargo
features `tls`, `aes`, and `sha2` and maps them to GmSSL 3.2.0's corresponding
CMake options. This lets `tokio-cronet` expose deterministic Cargo feature
switches instead of requiring callers to set `GMSSL_ENABLE_*` environment
variables. The patch can be removed once upstream provides equivalent feature
mapping. The distinct package name ensures those features remain available
when `tokio-cronet` is packaged outside this repository; dependents alias it
as `gmssl-rs-sys`, preserving the `gmssl_rs_sys` Rust crate name.

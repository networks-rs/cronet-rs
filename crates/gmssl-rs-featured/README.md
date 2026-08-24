# tokio-cronet-gmssl

This package preserves the safe Rust crate from
[`GmSSL/gmssl-rs`](https://github.com/GmSSL/gmssl-rs) revision
`fe981bd09d1d176ee19c7038a777e71783901d48`. Its Rust API is unchanged; only
the package name and the `gmssl-rs-sys` dependency source differ. Dependents
can alias it as `gmssl-rs`, so the Rust crate name remains `gmssl_rs`.

Keeping both packages together makes `gmssl`, `gmssl_tls`, `gmssl_aes`, and
`gmssl_sha2` work when this repository itself is consumed as a Git/path
dependency. The copy can be replaced with direct upstream dependencies once
`gmssl-rs-sys` exposes equivalent Cargo features.

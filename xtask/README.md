# cronet-src

Pinned source acquisition and native build support for `cronet-sys`, modeled
after `openssl-src`.

`Build` selects the requested target and dynamic/static linkage, locates a
complete source tree, and compiles Cronet. Source selection is:

1. `CRONET_SOURCE_DIR`;
2. `vendor/chromium/src` shipped by a source distribution of this crate;
3. the target-specific `$CARGO_HOME/cronet-rs/source` cache, populated from the
   pinned Chromium revision using the Cronet-only dependency filter.

No prebuilt Cronet library is downloaded or accepted. `CRONET_CACHE_DIR`
changes the persistent source cache location.

For OpenHarmony, select a complete Native SDK with
`Build::ohos_sdk_native`, `OHOS_SDK_NATIVE`, or `OHOS_NDK_HOME`. The builder
supports `armv7-unknown-linux-ohos`, `aarch64-unknown-linux-ohos`, and
`x86_64-unknown-linux-ohos`. SDK runtime locations are discovered below that
explicit root by target triple; no DevEco Studio path is assumed.

The complete filtered Chromium build closure is several GiB unpacked, so a
fully vendored edition must be distributed from the dedicated source
repository or a Cargo registry configured for large packages. The Rust wrapper
crate remains publishable to crates.io and performs the same source build when
the vendored tree is absent.

Run `cargo xtask vendor-source` from the main workspace to populate
`vendor/chromium/src` for a fully offline source release. The command refuses
to overwrite an existing destination and omits `.git`, `out`, `__pycache__`,
and `.pyc` build/repository state.

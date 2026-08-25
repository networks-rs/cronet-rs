# tokio-cronet-src

Pinned source acquisition and native build support for `tokio-cronet-sys`,
modeled after `openssl-src`.

`Build` selects the requested target and dynamic/static linkage, locates a
complete source tree, and compiles Cronet. Source selection is:

1. `CRONET_SOURCE_DIR`;
2. `vendor/chromium/src` shipped by a source distribution of this crate;
3. the target-specific `$CARGO_HOME/cronet-rs/source` cache, populated from the
   pinned Chromium revision using the Cronet-only dependency filter.

No prebuilt Cronet library is downloaded or accepted. `CRONET_CACHE_DIR`
changes the persistent source cache location.

Materialization and native builds take a cross-process lock per target cache.
The first full synchronization initializes depot_tools' gsutil package before
gclient starts parallel downloads, avoiding first-use lock races during Cargo
package verification and concurrent builds.

Chromium 146 does not publish its pinned Linux compiler or Rust host tools for
ARM64. A native `aarch64-unknown-linux-gnu` build therefore uses an installed
LLVM 22 toolchain selected with `Build::clang_dir` or `CRONET_CLANG_DIR`, the
Rust sysroot reported by `RUSTC`, and a host-native bindgen 0.72 selected with
`Build::rust_bindgen`, `CRONET_RUST_BINDGEN`, or `PATH`. This remains a native
build: the host and target are both ARM64, and no target sysroot or emulator is
introduced.

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

The ignored GN view contains a mechanically pruned build graph for the sparse
checkout, but no generated compatibility source. Platform behavior is supplied
by committed include wrappers or link-time adapters wherever possible. Exact
replacement files are reserved for GN/toolchain configuration, Android Java
threading, and definitions that cannot be extended outside their original
class or template.

Run `cargo xtask vendor-source` from the main workspace to populate
`vendor/chromium/src` for a fully offline source release. The command refuses
to overwrite an existing destination and omits `.git`, `out`, `__pycache__`,
and `.pyc` build/repository state.

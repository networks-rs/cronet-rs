# cronet-rs

Safe Rust bindings for Chromium's native Cronet C API. The repository is a
standard Cargo workspace:

```text
crates/cronet-sys  generated raw FFI bindings
crates/cronet      safe, Tokio-native streaming Rust API
xtask              `cronet-src` package plus source/build CLI
```

`cronet-sys` does not contain copied or hand-maintained Rust declarations. Its
build script uses `cronet-src` to materialize the pinned source tree, runs
bindgen directly against that tree's upstream headers, and builds Cronet
locally. The project neither publishes nor downloads prebuilt native libraries.

## Upstream compatibility

Chromium deleted `//components/cronet/native` on 2026-01-13 because that C API
was never officially supported. This workspace is therefore pinned to
`db64a84f93f16f8de53fee8d33df0a31473efefb`, the parent of the deletion commit
(Chromium 146.0.7633.0). Updating to current Chromium `main` is not possible
without first restoring or replacing the removed native API.

## Source synchronization

Install Git, then run:

```sh
cargo xtask sync
```

The sync is intentionally not a normal Chromium checkout. It uses a shallow,
blobless Git partial clone plus a sparse allow-list derived from Cronet's own
`components/cronet/android/dependencies.txt`. Chrome, Blink, Content, V8,
WebRTC, unrelated product sources, and Chromium's GCS-hosted browser/test assets
are not checked out. Before invoking `gclient`, `xtask` derives a filtered
`DEPS.cronet` from the pinned Chromium `DEPS`; this applies the Cronet allow-list
equally to Git, CIPD, and GCS dependencies. `gclient` still downloads the
third-party repositories and toolchains that the Cronet target actually needs;
a source build cannot consist of `components/cronet` alone because Cronet
depends on Chromium `base`, `net`, BoringSSL, QUICHE, and other libraries.
At the pinned revision this selects 74 of Chromium's 421 dependency entries
before the host and target conditions narrow the set further.

To download only the C headers for binding generation and IDE checks:

```sh
cargo xtask sync --api-only
CRONET_SYS_NO_LINK=1 cargo check --workspace
```

The default checkout lives under `.cronet/` and is ignored by Git. Pass
`--source-dir PATH` to `sync`, `build`, `doctor`, and `print-env`, or set
`CRONET_SOURCE_DIR`.

## Source crate and automatic native build

For an ordinary application build, no manual `xtask` step is required:

```sh
cargo build --release                    # shared Cronet (default)
cargo build --release --features static  # complete static Cronet archive
```

`static` is a single additive override instead of a mutually exclusive pair of
`dynamic`/`static` features, so Cargo feature unification remains deterministic.
The safe crate propagates it to `cronet-sys`. Without it, shared linking is
selected.

`cronet-src::Build`, modeled after `openssl-src`, resolves source in this order:

1. an explicit `CRONET_SOURCE_DIR`;
2. `vendor/chromium/src` inside a fully vendored `cronet-src` distribution;
3. a persistent target-specific source cache populated from the pinned
   Chromium revision and filtered `DEPS.cronet`.

The third path is still a source build: it downloads no native library and
does not check out Chrome, Blink, V8, WebRTC, or browser assets. The cache is
`$CARGO_HOME/cronet-rs/source` by default; `CRONET_CACHE_DIR` overrides it.

The source-build workflow verifies these native targets:

| Platform | Architectures | Rust targets |
| --- | --- | --- |
| Linux | x86-64, ARM64 | `x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu` |
| macOS | x86-64, Apple Silicon | `x86_64-apple-darwin`, `aarch64-apple-darwin` |
| Android | x86, x86-64, ARMv7, ARM64 | `i686-linux-android`, `x86_64-linux-android`, `armv7-linux-androideabi`, `aarch64-linux-android` |
| iOS | x86-64 Simulator, ARM64 Simulator/device | `x86_64-apple-ios`, `aarch64-apple-ios-sim`, `aarch64-apple-ios` |
| Windows | x86-64, ARM64 | `x86_64-pc-windows-msvc`, `aarch64-pc-windows-msvc` |
| OpenHarmony | ARMv7, ARM64, x86-64 | `armv7-unknown-linux-ohos`, `aarch64-unknown-linux-ohos`, `x86_64-unknown-linux-ohos` |

OpenHarmony builds use the same source builder and do not depend on a DevEco
Studio installation layout. Install the corresponding Rust standard library,
then provide any complete OpenHarmony Native SDK root either through
`Build::ohos_sdk_native`, `OHOS_SDK_NATIVE`, or `OHOS_NDK_HOME`:

```sh
rustup target add aarch64-unknown-linux-ohos
OHOS_SDK_NATIVE=/path/to/openharmony/native \
  cargo xtask build --release --linkage both \
    --target aarch64-unknown-linux-ohos
```

The builder discovers the selected SDK's sysroot, Clang resource directory,
compiler builtins, and unwind archive from their target triples. It does not
scan host-specific IDE directories or invoke the SDK compiler. Chromium's
pinned Clang, LLD, libc++, and libc++abi build the source; the supplied SDK
provides only the target OS headers and ABI runtime archives.

The application-level QEMU/device harness is documented in
[`tests/ohos-e2e`](tests/ohos-e2e/README.md). Its SDK, HDC, Hvigor, signer,
device, and source/output locations are all injected; the repository contains
no emulator image or host installation path.

Android source builds discover a standard NDK through `ANDROID_NDK_HOME`,
`ANDROID_NDK_ROOT`, or `NDK_HOME` and default to API 23. The generated output
contains the C library plus the minimal Java support JAR and pre-dexed JAR
needed by Chromium's Android proxy, certificate, and network-change bridges.
Static consumers initialize Cronet's Java VM from their final `JNI_OnLoad` via
`cronet::android::initialize_java_vm`; the repository's application harness
shows the complete packaging path.

iOS source builds require macOS and Xcode. They support the ARM64 device target
and both Rust Simulator targets, use relocatable `@rpath` install names for the
shared library, and propagate every required Apple framework for static mode.
Chromium 146 defaults to iOS 17 for this non-Blink build; override it through
`IPHONEOS_DEPLOYMENT_TARGET` or `cronet_src::Build::ios_deployment_target`.
Dynamic and static Android/iOS application tests are documented in
[`tests/mobile-e2e`](tests/mobile-e2e/README.md).

The complete filtered source closure is approximately 3.7 GiB unpacked. A
normal crates.io package cannot embed it because crates.io limits a `.crate`
archive to 10 MiB. Therefore `cronet-src` is intentionally ready to be split
into a dedicated source repository: that repository can ship
`vendor/chromium/src` for offline builds, while the small crates.io edition
contains the identical revision lock, filter, and build logic and materializes
the source cache on first use. A private Cargo registry with a sufficiently
large package limit can publish the fully vendored edition directly.

To prepare that fully offline source edition after synchronization:

```sh
cargo xtask vendor-source
cargo package -p cronet-src --allow-dirty --no-verify
```

`vendor-source` copies the buildable filtered closure into
`xtask/vendor/chromium/src`, preserving source/toolchain symlinks while
excluding Git metadata, native output directories, and Python cache files. The
`cronet-src` package manifest already includes `vendor/**`.

The `xtask` directory is a self-contained `cronet-src` package and is the split
boundary for a dedicated large-source repository. Until that repository has a
stable URL, an offline consumer can use a local checkout without changing
`cronet-sys`:

```toml
[patch.crates-io]
cronet-src = { path = "../cronet-src" }
```

After publishing the source repository, the patch can use its pinned `git` and
`rev` instead. The normal crates.io `cronet-src` version remains the online
source-materialization implementation with the same API and revision lock.

Applications using the default shared mode must still ship the locally built
shared library beside the program or otherwise make its output directory
visible through `PATH`, `LD_LIBRARY_PATH`, or `DYLD_LIBRARY_PATH`, as
appropriate for the platform. Static mode has no Cronet runtime-library
deployment step.
Chromium 146 targets macOS 12 or newer; set `MACOSX_DEPLOYMENT_TARGET=12.0`
when statically linking a macOS application.

## Manual source building

The full sync installs `depot_tools`, but deliberately does not run Chromium's
browser-wide hooks. Build both native forms directly from source with:

```sh
cargo xtask build --release --linkage both
eval "$(cargo xtask print-env)" # macOS/Linux: link and runtime library paths
cargo build --release
cargo build --release --features static
cargo run --release -p cronet --features native-example --example get
```

On Windows, run the `set ...` lines printed by `cargo xtask print-env` in
`cmd.exe`. Applications must ship the resulting Cronet shared library and make
it visible to the platform dynamic loader; `CRONET_LIB_DIR` only controls the
Rust link step.

Additional GN arguments can be repeated, for example:

```sh
cargo xtask build --release --linkage dynamic --target x86_64-apple-darwin
cargo xtask build --release --linkage static --target x86_64-apple-darwin
cargo xtask build --release --gn-arg 'target_cpu="x64"'
```

`cronet-sys/build.rs` uses these locations by default:

- source: `.cronet/chromium/src`
- native library: `.cronet/chromium/src/out/cronet-rs`

Override them with `CRONET_SOURCE_DIR` and `CRONET_LIB_DIR`. The latter is an
explicit system/library-development escape hatch and never triggers a binary
download. Run `cargo xtask doctor` to inspect the local setup.
`CRONET_SYS_NO_LINK=1` is only for binding checks and docs that do not link the
safe crate, and still requires a synced header tree.

Chromium's normal root GN graph imports browser-only ANGLE, V8, and test
targets even when the requested target is Cronet. `xtask` therefore generates
an ignored GN overlay containing the same sparse sources, a Cronet-only root
target, and test-target-free BUILD files. It also applies one isolated
compatibility change to the final upstream native stub: initialize
`base::CommandLine` before the newer certificate verifier reads it. The pinned
upstream checkout itself is never modified.

## Safe API

```rust,no_run
use cronet::{Engine, Request};
use tokio::io::AsyncReadExt;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let engine = Engine::builder()
        .user_agent("cronet-rs/example")
        .build()?;

    let request = Request::builder("https://example.com")?
        .header("accept", "text/html")?
        .max_response_bytes(2 * 1024 * 1024)
        .build()?;

    let mut response = engine.send(request).await?;
    let mut body = Vec::new();
    response.body.read_to_end(&mut body).await?;
    let finished = response.body.finished().await?;

    println!("{}: {} bytes", response.status(), body.len());
    println!("metrics: {:?}", finished.metrics);
    engine.shutdown().await?;
    Ok(())
}
```

The safe layer owns the engine, Tokio executor, callback contexts, requests,
upload providers, listeners, and reusable Cronet buffers. Its networking API
does not expose raw pointers or unsafe operations. The sole platform boundary
is static Android embedding: `cronet::android::initialize_java_vm` is unsafe
because the application must forward the process `JavaVM*` from `JNI_OnLoad`.
The engine must be built inside a Tokio runtime; it is cloneable, `Send + Sync`,
and can be shared by normal Tokio tasks.

`Engine::send` resolves when response headers arrive and returns a bounded
`ResponseBody`. The body implements both
`Stream<Item = cronet::Result<bytes::Bytes>>` and Tokio `AsyncRead`. Cronet does
not receive its next native `Read` call until a chunk has entered the bounded
channel, so a slow consumer applies real backpressure instead of allowing an
unbounded buffer. Dropping the send future or body cancels the request.
`Engine::execute` is the convenience path that asynchronously collects the
same stream into a buffered `Response`.

Uploads accept `Bytes`, arbitrary Tokio `AsyncRead` sources, or rewindable
`AsyncRead + AsyncSeek` sources. Request status, redirect control, cache policy,
priority, idempotency, annotations, body-size limits, final timing/traffic
metrics, engine-wide finished-request subscriptions, `NetLog`, public-key pins,
QUIC hints, HTTP/2, QUIC, and Brotli configuration are exposed. Async shutdown
cancels active work, waits for terminal and metrics callbacks, and performs the
blocking native shutdown away from Tokio worker threads.

For cancellation and status before headers arrive, start the request without
awaiting it immediately. `PendingRequest` is a normal Tokio future, and its
cloneable handle can be moved into other tasks:

```rust,no_run
# use cronet::{Engine, Request};
# use std::time::Duration;
# async fn selectable(engine: &Engine) -> cronet::Result<()> {
let request = Request::builder("https://example.com/slow")?.build()?;
let pending = engine.start(request)?;
let handle = pending.handle();

let response = tokio::select! {
    response = pending => response?,
    () = tokio::time::sleep(Duration::from_secs(10)) => {
        handle.cancel();
        return Err(cronet::Error::Canceled);
    }
};
# drop(response);
# Ok(())
# }
```

Redirects can be followed, rejected with owned redirect response metadata, or
decided by a non-blocking Tokio future through `redirect_handler`.

The production API audit and its exact test mapping are recorded in
[the capability matrix](docs/capability-matrix.md).

The upstream gRPC-support C header is generated by `cronet-sys` from the same
pinned source tree. `Engine::open_bidirectional` wraps its HTTP/2/QUIC
stream as a bounded Tokio `AsyncRead + AsyncWrite + Stream`, including response
headers, trailers, explicit flush, half-close, cancellation, and terminal
errors:

```rust,no_run
# use cronet::{BidirectionalRequest, Engine};
# use tokio::io::{AsyncReadExt, AsyncWriteExt};
# async fn rpc(engine: &Engine) -> Result<(), Box<dyn std::error::Error>> {
let request = BidirectionalRequest::builder("https://example.com.Service/Call")?
    .header("content-type", "application/grpc")?
    .disable_auto_flush(true)
    .build()?;
let mut stream = engine.open_bidirectional(request).await?;

stream.write_all(b"framed request").await?;
stream.shutdown().await?;
let response = stream.response_headers().await?;
let mut body = Vec::new();
stream.read_to_end(&mut body).await?;
let trailers = stream.trailers().await?;
# let _ = (response, trailers);
# Ok(())
# }
```

For a streaming upload:

```rust,no_run
# use cronet::{Engine, Request};
# async fn upload(engine: &Engine) -> cronet::Result<()> {
let (mut writer, reader) = tokio::io::duplex(64 * 1024);
tokio::spawn(async move {
    use tokio::io::AsyncWriteExt;
    writer.write_all(b"streamed body").await.unwrap();
});

let request = Request::builder("https://example.com/upload")?
    .method("POST")?
    .header("content-type", "application/octet-stream")?
    .body_stream(reader, Some(13))
    .build()?;
let response = engine.execute(request).await?;
# let _ = response;
# Ok(())
# }
```

## Verification

Without building Chromium, the API and generated bindings can be verified with:

```sh
cargo xtask sync --api-only
CRONET_SYS_NO_LINK=1 cargo fmt --all -- --check
CRONET_SYS_NO_LINK=1 cargo clippy --workspace --all-targets -- -D warnings
CRONET_SYS_NO_LINK=1 cargo check --workspace --all-targets
```

After a native build, both real link modes and the full safe API can be
exercised with:

```sh
eval "$(cargo xtask print-env)"
cargo test -p cronet --features native-tests --test native_smoke
cargo test -p cronet --features native-tests --test e2e_local -- --test-threads=1
cargo test -p cronet --features static,native-tests --test native_smoke
cargo test -p cronet --features static,native-tests --test e2e_local -- --test-threads=1
cargo test -p cronet --features network-tests --test e2e_network -- --test-threads=1
cargo run -p cronet --features native-example --example get
```

Set `CRONET_E2E_REQUIRE_QUIC=1` when the test network permits UDP/443 and QUIC
fallback must fail the build. The source-build workflow uses this strict mode
on Linux x86-64.

# cronet-rs

Safe, source-built Rust bindings for Chromium's native Cronet C API, with a
Tokio-native streaming interface.

`cronet-rs` ships no prebuilt native libraries. `cronet-sys` generates its raw
bindings from the pinned upstream headers and builds Cronet from source for the
current Cargo target. Shared linking is the default; the additive `static`
feature builds a complete static archive.

> **Upstream status:** Chromium removed `//components/cronet/native` on
> 2026-01-13 because the C API was never officially supported. This workspace
> is pinned to Chromium 146.0.7633.0 at
> `db64a84f93f16f8de53fee8d33df0a31473efefb`, the parent of the deletion
> commit. Moving to current Chromium requires restoring or replacing that API.

## What is included

- Generated, version-locked raw C and bidirectional-stream bindings.
- Safe `Engine`, request, response, upload, redirect, metrics, NetLog, cache,
  QUIC hint, public-key pinning, and shutdown APIs.
- Bounded response streams implementing both Tokio `AsyncRead` and
  `Stream<Item = Result<Bytes>>`, with native backpressure.
- Tokio `AsyncRead` uploads, rewindable `AsyncRead + AsyncSeek` uploads, and
  HTTP/2 or HTTP/3 bidirectional streams implementing `AsyncRead + AsyncWrite`.
- Tokio-friendly cancellation, redirect decisions, request status, finished
  request subscriptions, and asynchronous shutdown.
- Optional Rust-native DNS queries backed by Hickory.
- Source builds for Linux, macOS, Windows, Android, iOS, and OpenHarmony.

The complete safe API audit and its E2E scenario mapping live in the
[capability matrix](docs/capability-matrix.md).

## Quick start

The minimum supported Rust version is 1.85. Shared linking and Rust-native DNS
are enabled by default:

```toml
[dependencies]
cronet = "0.1"
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

To build Cronet as a static library, select the dependency feature instead:

```toml
[dependencies]
cronet = { version = "0.1", features = ["static"] }
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

| Feature | Default | Effect |
| --- | --- | --- |
| `dns` | yes | Adds the application-side Tokio/Hickory resolver |
| `static` | no | Builds and links the complete static Cronet archive |

`static` is an additive override rather than a `dynamic`/`static` feature pair,
so Cargo feature unification has one deterministic result.

The first build materializes the pinned, filtered Chromium source tree and
compiles Cronet locally:

```sh
cargo build --release
```

The filtered source closure is approximately 3.7 GiB unpacked, so the initial
build is intentionally much heavier than a normal Rust crate build. Later
builds reuse the target-specific cache under `$CARGO_HOME/cronet-rs/source`.
Set `CRONET_CACHE_DIR` to move that cache.

Applications using shared mode must deploy the generated Cronet shared library
and make it visible to the platform loader through `PATH`,
`LD_LIBRARY_PATH`, `DYLD_LIBRARY_PATH`, or the platform's normal packaging
mechanism. Static mode has no Cronet runtime-library deployment step.

### Make a request

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

Build an engine inside a Tokio runtime. `Engine` is cloneable, `Send + Sync`,
and may be shared by ordinary Tokio tasks. `Engine::send` resolves when response
headers arrive; `Engine::execute` is the convenience API that collects the same
bounded stream into a buffered response.

Dropping a pending request or response body cancels its native request. A
request handle also supports explicit cancellation from `tokio::select!` or
another task:

```rust,no_run
use cronet::{Engine, Request};
use std::time::Duration;

async fn selectable(engine: &Engine) -> cronet::Result<()> {
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
    drop(response);
    Ok(())
}
```

### Streaming and bidirectional I/O

Response bodies are bounded: Cronet receives its next native `Read` call only
after the current chunk enters the channel, so slow consumers apply real
backpressure. Uploads accept `Bytes`, Tokio `AsyncRead`, or rewindable
`AsyncRead + AsyncSeek` sources.

The upstream gRPC-support C API is exposed as a bounded HTTP/2 or HTTP/3 stream:

```rust,no_run
use cronet::{BidirectionalRequest, Engine};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

async fn rpc(engine: &Engine) -> Result<(), Box<dyn std::error::Error>> {
    let request = BidirectionalRequest::builder("https://example.com.Service/Call")?
        .header("content-type", "application/grpc")?
        .disable_auto_flush(true)
        .build()?;
    let mut stream = engine.open_bidirectional(request).await?;

    stream.write_all(b"framed request").await?;
    stream.shutdown().await?;
    let headers = stream.response_headers().await?;
    let mut body = Vec::new();
    stream.read_to_end(&mut body).await?;
    let trailers = stream.trailers().await?;
    println!(
        "{} headers, {} body bytes, {} trailers",
        headers.headers.len(),
        body.len(),
        trailers.len()
    );
    Ok(())
}
```

`BidirectionalStream` implements Tokio `AsyncRead + AsyncWrite` and
`Stream`. It also exposes response headers, trailers, explicit flush,
half-close, cancellation, and terminal errors.

## DNS and TLS boundary

The default `dns` feature provides a cloneable Tokio resolver backed by
Hickory. It supports system configuration, explicit upstream servers,
IPv4/IPv6 lookup, reverse lookup, and typed queries for Hickory record types:

```rust,no_run
use cronet::dns::{DnsResolver, RData, RecordType};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dns = DnsResolver::from_system()?;

    for address in dns.lookup_ip("example.com.").await?.iter() {
        println!("{address}");
    }

    for record in dns.lookup("example.com.", RecordType::TXT).await?.iter() {
        if let RData::TXT(value) = record {
            println!("{:?}", value.txt_data());
        }
    }
    Ok(())
}
```

This is an application-side resolver for explicit queries, prewarming,
diagnostics, and policy. Cronet's C API has no safe custom-resolver injection
point, so `Engine` requests continue to use Chromium's internal host resolver.
Disable the helper when it is not needed:

```toml
cronet = { version = "0.1", default-features = false }
```

There is no OpenSSL dependency to replace. Cronet uses Chromium's BoringSSL
internally for HTTPS and QUIC, and the C API has no TLS-provider injection
point. Adding rustls to the safe crate would create a second, unused TLS stack.
See the [DNS integration guide](docs/dns.md) for the exact boundary.

## Workspace architecture

This repository is a standard Cargo workspace:

```text
crates/cronet-sys  generated raw FFI bindings and Cargo build integration
crates/cronet      safe Tokio API
xtask              cronet-src package and source/build CLI
```

`cronet-sys` contains no copied or hand-maintained FFI declarations. Its build
script asks `cronet-src` to locate or materialize the pinned source tree, runs
bindgen against the upstream headers, and builds the selected native linkage.
The safe crate owns the engine, executor, callback contexts, requests, upload
providers, listeners, and reusable Cronet buffers.

The networking API exposes no raw pointers or unsafe operations. The sole
platform boundary is static Android embedding:
`cronet::android::initialize_java_vm` is unsafe because the application must
forward the process `JavaVM*` received by `JNI_OnLoad`.

## Supported native targets

| Platform | Architectures | Rust targets |
| --- | --- | --- |
| Linux | x86-64, ARM64 | `x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu` |
| macOS | x86-64, Apple Silicon | `x86_64-apple-darwin`, `aarch64-apple-darwin` |
| Windows | x86-64, ARM64 | `x86_64-pc-windows-msvc`, `aarch64-pc-windows-msvc` |
| Android | x86, x86-64, ARMv7, ARM64 | `i686-linux-android`, `x86_64-linux-android`, `armv7-linux-androideabi`, `aarch64-linux-android` |
| iOS | x86-64 Simulator, ARM64 Simulator/device | `x86_64-apple-ios`, `aarch64-apple-ios-sim`, `aarch64-apple-ios` |
| OpenHarmony | ARMv7, ARM64, x86-64 | `armv7-unknown-linux-ohos`, `aarch64-unknown-linux-ohos`, `x86_64-unknown-linux-ohos` |

Desktop targets run on architecture-matched CI hosts, avoiding accidental
cross-compilation of Chromium's host tools. Android and OpenHarmony use their
target SDK/NDK; iOS builds require macOS and Xcode.

## Source-build model

`cronet-src::Build` follows the same role as `openssl-src`. It resolves source
in this order:

1. `CRONET_SOURCE_DIR`, when explicitly configured;
2. `vendor/chromium/src` in a fully vendored `cronet-src` distribution;
3. a persistent target-specific cache materialized from the locked revision.

The third path downloads source and required build tools, never a native Cronet
binary. The sync is not a full Chromium checkout: it uses a shallow, blobless
partial clone, a sparse allow-list derived from
`components/cronet/android/dependencies.txt`, and a filtered `DEPS.cronet`.
Chrome, Blink, Content, V8, WebRTC, unrelated product code, and browser/test
assets are excluded. The pinned revision selects 74 of 421 top-level Chromium
dependency entries before host and target conditions narrow the set further.

Cronet still requires Chromium `base`, `net`, BoringSSL, QUICHE, libc++, and
their build toolchains, so a buildable source tree cannot consist of
`components/cronet` alone.

### Repository development

Synchronize the filtered source tree under the repository's ignored
`.cronet/` directory:

```sh
cargo xtask sync
```

To fetch only the public C headers for binding generation and IDE checks:

```sh
cargo xtask sync --api-only
CRONET_SYS_NO_LINK=1 cargo check --workspace
```

Build both native forms and expose their paths to subsequent Cargo commands:

```sh
cargo xtask build --release --linkage both
eval "$(cargo xtask print-env)" # macOS/Linux
cargo build -p cronet --release
cargo build -p cronet --release --features static
cargo run --release -p cronet --features native-example --example get
```

On Windows, apply the `set ...` lines emitted by `cargo xtask print-env` in
`cmd.exe`. Use `cargo xtask doctor` to inspect the local configuration.

Common overrides are:

| Variable | Purpose |
| --- | --- |
| `CRONET_SOURCE_DIR` | Use an existing Chromium source root |
| `CRONET_CACHE_DIR` | Move the persistent source cache |
| `CRONET_LIB_DIR` | Link an explicitly supplied local build output |
| `CRONET_SYS_NO_LINK=1` | Generate/check bindings without a native link |
| `ANDROID_NDK_HOME` | Select a standard Android NDK |
| `ANDROID_API_LEVEL` | Override Android API 23 |
| `OHOS_SDK_NATIVE` | Select an OpenHarmony Native SDK |
| `IPHONEOS_DEPLOYMENT_TARGET` | Override the default iOS 17 target |
| `MACOSX_DEPLOYMENT_TARGET` | Set the macOS deployment target |

`CRONET_LIB_DIR` is a local development escape hatch and never initiates a
binary download. `CRONET_SYS_NO_LINK=1` still requires a synchronized header
tree and is intended only for checks and documentation.

Additional GN arguments and explicit targets are available through `xtask`:

```sh
cargo xtask build --release --linkage dynamic --target x86_64-apple-darwin
cargo xtask build --release --linkage static --target x86_64-apple-darwin
cargo xtask build --release --gn-arg 'target_cpu="x64"'
```

The builder creates an ignored Cronet-only GN overlay because Chromium's normal
root graph imports browser-only targets even when only Cronet is requested. It
also applies isolated compatibility patches without modifying the pinned
upstream checkout.

### Platform notes

**Android:** set `ANDROID_NDK_HOME`, `ANDROID_NDK_ROOT`, or `NDK_HOME`.
The default API level is 23. Build output includes the native library, minimal
Java support JAR, and pre-dexed JAR used by Chromium's proxy, certificate, and
network-change bridges. Static applications must forward `JNI_OnLoad` to
`cronet::android::initialize_java_vm`.

**iOS:** build on macOS with Xcode. ARM64 device and both Rust Simulator
targets are supported. Shared libraries use relocatable `@rpath` install names;
static mode propagates the required Apple frameworks. Chromium 146 defaults to
iOS 17. See the [mobile E2E harness](tests/mobile-e2e/README.md).

**OpenHarmony:** install the target Rust standard library and provide a complete
Native SDK through `OHOS_SDK_NATIVE` or `OHOS_NDK_HOME`:

```sh
rustup target add aarch64-unknown-linux-ohos
OHOS_SDK_NATIVE=/path/to/openharmony/native \
  cargo xtask build --release --linkage both \
    --target aarch64-unknown-linux-ohos
```

The builder discovers the SDK sysroot and ABI runtime from target triples; it
does not depend on a DevEco Studio installation layout. The injectable
QEMU/device harness is documented under
[`tests/ohos-e2e`](tests/ohos-e2e/README.md).

**macOS:** Chromium 146 targets macOS 12 or newer. Set
`MACOSX_DEPLOYMENT_TARGET=12.0` when statically linking an application.

### Offline source delivery

Crates.io limits a `.crate` archive to 10 MiB, so it cannot contain the
approximately 3.7 GiB filtered source closure. The small `cronet-src` crate
therefore carries the revision lock, filter, patches, and build logic, then
materializes source on first use.

For a private registry or dedicated large-source repository, prepare the
fully vendored package with:

```sh
cargo xtask vendor-source
cargo package -p cronet-src --allow-dirty --no-verify
```

`vendor-source` writes `xtask/vendor/chromium/src`, preserving required
symlinks while excluding Git metadata, output directories, and Python caches.
Consumers can redirect `cronet-src` without changing `cronet-sys`:

```toml
[patch.crates-io]
cronet-src = { path = "../cronet-src" }
```

## Verification

Run the source-independent checks after an API-only sync:

```sh
cargo xtask sync --api-only
CRONET_SYS_NO_LINK=1 cargo fmt --all -- --check
CRONET_SYS_NO_LINK=1 cargo clippy --workspace --all-targets -- -D warnings
CRONET_SYS_NO_LINK=1 cargo test -p cronet --test dns_e2e
CRONET_SYS_NO_LINK=1 cargo check --workspace --all-targets
cargo xtask audit-e2e
```

After building the native library, exercise shared and static linking plus the
local and public-network scenarios:

```sh
eval "$(cargo xtask print-env)"
cargo test -p cronet --features native-tests --test native_smoke
cargo test -p cronet --features native-tests --test e2e_local -- --test-threads=1
cargo test -p cronet --features static,native-tests --test native_smoke
cargo test -p cronet --features static,native-tests --test e2e_local -- --test-threads=1
cargo test -p cronet --features network-tests --test e2e_network -- --test-threads=1
```

Desktop, Android, iOS, and OpenHarmony runners share
[`portable_e2e.rs`](crates/cronet/tests/support/portable_e2e.rs). Cache/NetLog
and public HTTP/2 or HTTP/3 cases add platform- and protocol-specific coverage.
The complete safety contract is documented in
[`docs/e2e-safety.md`](docs/e2e-safety.md).

Set `CRONET_E2E_REQUIRE_QUIC=1` when UDP/443 is available and QUIC fallback
must fail the test. Linux x86-64 CI uses this strict mode.

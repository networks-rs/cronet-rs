# OpenHarmony source-build and QEMU E2E

The source builder supports all three stable Rust OpenHarmony targets. A build
matrix can use any complete Native SDK; no DevEco Studio path is assumed:

```sh
export OHOS_SDK_NATIVE=/path/to/openharmony/native
for target in \
  armv7-unknown-linux-ohos \
  aarch64-unknown-linux-ohos \
  x86_64-unknown-linux-ohos
do
  cargo xtask build --release --linkage both \
    --target "$target" --source-dir .cronet/chromium/src
done
```

`run.sh` packages the safe Rust/Tokio scenarios into a minimal HAP and runs it
as an application with `ohos.permission.INTERNET`. This is intentional: a
release OpenHarmony image normally denies INET sockets to an `hdc shell`
process, so executing a bare test binary from `/data/local/tmp` is not an
application-level network E2E.

The runner requires an already-running OpenHarmony emulator or device and the
following configuration. ARM64 is the default; set `OHOS_E2E_TARGET` to one of
the other two Rust targets when a matching image/device is available:

```sh
export OHOS_SDK_NATIVE=/path/to/openharmony/native
export HDC=/path/to/hdc
export HVIGORW=/path/to/hvigorw
# Required for standalone SDK layouts; DevEco's default layout is discovered:
export OHOS_BASE_SDK_HOME=/path/to/sdk-container
# Optional when HDC sees more than one target:
export OHOS_E2E_DEVICE=127.0.0.1:5555
# Set these only when the selected Hvigor distribution requires them:
export DEVECO_SDK_HOME=/path/to/sdk-container
export NODE_HOME=/path/to/node-distribution
# Optional: export OHOS_E2E_TARGET=x86_64-unknown-linux-ohos
# Optional: export OHOS_E2E_LINKAGE=static (the default is dynamic)
tests/ohos-e2e/run.sh
```

The selected Rust target is also used to choose the SDK linker, native output
directory, HAP ABI directory, and N-API bridge compiler. The HAP project does
not fix a CMake ABI or Native SDK API version: `run.sh` reads the injected
Native SDK's `apiVersion` and applies it only to a temporary HAP copy.

The script uses the public OpenHarmony development signing material when the
selected SDK supplies it. For another distribution, set `OHOS_TOOLCHAINS` and
provide an executable `OHOS_E2E_SIGNER`. It receives these arguments:

```text
unsigned-hap signed-hap bundle-name device-udid
```

The signer, SDK, Hvigor, HDC, emulator image, and source/output directories are
all injected. No host-specific path, device identifier, certificate, QEMU
image, or prebuilt Cronet library is stored in the repository.

Both linkage modes compile the exact shared
`crates/tokio-cronet/tests/support/portable_e2e.rs` suite into the HAP. It exercises
the complete audited safe API over real local HTTP traffic, including all
request-builder options, buffered/streaming/rewindable uploads, redirects,
`AsyncRead` and `Stream` response consumption, metrics and listener events,
limits and typed failures, cancellation/drop/shutdown races, and the local
bidirectional-stream failure path. `cargo xtask audit-e2e` rejects any public
safe function that is not mapped to a concrete scenario.

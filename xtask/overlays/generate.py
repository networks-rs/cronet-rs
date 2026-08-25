#!/usr/bin/env python3
"""Refresh unavoidable replacement snapshots from pinned Chromium.

Run from the workspace root after `cargo xtask sync`. Output lives under
xtask/overlays/. Small source wrappers and cronet-rs-owned GN files are kept
by hand and are intentionally not generated here.
"""

from __future__ import annotations

from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
SRC = ROOT / ".cronet/chromium/src"
OVERLAY = ROOT / ".cronet/chromium/cronet-gn-root"
OUT = Path(__file__).resolve().parent


def read(rel: str) -> str:
    return (SRC / rel).read_text()


def write(rel: str, contents: str) -> None:
    path = OUT / rel
    path.parent.mkdir(parents=True, exist_ok=True)
    if not contents.endswith("\n"):
        contents += "\n"
    path.write_text(contents)
    print(f"wrote {path.relative_to(ROOT)} ({contents.count(chr(10))} lines)")


def replace_once(contents: str, old: str, new: str, rel: str) -> str:
    if old not in contents:
        raise SystemExit(f"missing marker in {rel}: {old[:80]!r}")
    return contents.replace(old, new, 1)


def replace_all(contents: str, old: str, new: str, rel: str, count: int | None = None) -> str:
    found = contents.count(old)
    if count is not None and found != count:
        raise SystemExit(f"{rel}: expected {count} of {old[:60]!r}, found {found}")
    if found == 0:
        raise SystemExit(f"missing marker in {rel}: {old[:80]!r}")
    return contents.replace(old, new)


def from_overlay_or_src(rel: str) -> str:
    overlay_path = OVERLAY / rel
    if overlay_path.is_file():
        return overlay_path.read_text()
    return read(rel)


def common() -> None:
    cxx_rel = "third_party/rust/chromium_crates_io/vendor/cxx-v1/include/cxx.h"
    cxx = read(cxx_rel)
    cxx = replace_once(
        cxx,
        "  using value_type = T;\n  using difference_type = std::ptrdiff_t;",
        "  using value_type = T;\n"
        "  // Older libc++ releases consult pointer_traits while checking the C++20\n"
        "  // contiguous_iterator concept. This alias is semantically identical to\n"
        "  // value_type and makes that implementation path well-formed.\n"
        "  using element_type = T;\n"
        "  using difference_type = std::ptrdiff_t;",
        cxx_rel,
    )
    write("common/" + cxx_rel, cxx)

def android() -> None:
    java_rel = "net/android/java/src/org/chromium/net/ProxyChangeListener.java"
    java = read(java_rel)
    java = replace_once(java, "import java.util.Locale;", "import java.util.Locale;\nimport java.util.concurrent.CountDownLatch;", java_rel)
    java = replace_once(
        java,
        """    private ProxyChangeListener() {
        Looper myLooper = Looper.myLooper();
        assert myLooper != null;
        mLooper = myLooper;
        mHandler = new Handler(mLooper);
    }""",
        """    private ProxyChangeListener() {
        // cronet-rs' native C API initializes from a Chromium sequenced task
        // runner, which has no Java Looper. Android service callbacks belong
        // on the application's always-running main Looper.
        mLooper = Looper.getMainLooper();
        mHandler = new Handler(mLooper);
    }""",
        java_rel,
    )
    java = replace_once(
        java,
        '''    @CalledByNative
    public void start(long nativePtr) {
        try (TraceEvent e = TraceEvent.scoped("ProxyChangeListener.start")) {
            assertOnThread();
            assert mNativePtr == 0;
            mNativePtr = nativePtr;
            registerBroadcastReceiver();
        }
    }

    @CalledByNative
    public void stop() {
        assertOnThread();
        mNativePtr = 0;
        unregisterBroadcastReceiver();
    }''',
        '''    @CalledByNative
    public void start(long nativePtr) {
        runOnThreadBlocking(
                () -> {
                    try (TraceEvent e = TraceEvent.scoped("ProxyChangeListener.start")) {
                        assert mNativePtr == 0;
                        mNativePtr = nativePtr;
                        registerBroadcastReceiver();
                    }
                });
    }

    @CalledByNative
    public void stop() {
        runOnThreadBlocking(
                () -> {
                    mNativePtr = 0;
                    unregisterBroadcastReceiver();
                });
    }''',
        java_rel,
    )
    java = replace_once(
        java,
        """    private void runOnThread(Runnable r) {
        if (onThread()) {
            r.run();
        } else {
            mHandler.post(r);
        }
    }""",
        '''    private void runOnThread(Runnable r) {
        if (onThread()) {
            r.run();
        } else {
            mHandler.post(r);
        }
    }

    private void runOnThreadBlocking(Runnable r) {
        if (onThread()) {
            r.run();
            return;
        }
        CountDownLatch done = new CountDownLatch(1);
        mHandler.post(
                () -> {
                    try {
                        r.run();
                    } finally {
                        done.countDown();
                    }
                });
        try {
            done.await();
        } catch (InterruptedException e) {
            Thread.currentThread().interrupt();
            throw new IllegalStateException("Interrupted while calling the Android proxy service", e);
        }
    }''',
        java_rel,
    )
    write("android/" + java_rel, java)

    gclient = read("build/config/gclient_args.gni")
    if "checkout_android = false" in gclient:
        gclient = replace_once(
            gclient,
            "checkout_android = false",
            "checkout_android = true  # tokio-cronet-src supplies an external Android NDK",
            "gclient_args.gni",
        )
    write("android/build/config/gclient_args.gni", gclient)

    buildconfig = read("build/config/BUILDCONFIG.gn")
    buildconfig = replace_once(
        buildconfig,
        '  assert(host_os == "linux", "Android builds are only supported on Linux.")',
        """  # The external NDK also ships native macOS host tools. Chromium labels
  # this configuration best-effort; tokio-cronet-src verifies its reduced C API graph.
  assert(host_os == "linux" || host_os == "mac",
         "Android builds require a Linux or macOS host.")""",
        "BUILDCONFIG.gn",
    )
    write("android/build/config/BUILDCONFIG.gn", buildconfig)

    compiler = read("build/config/compiler/BUILD.gn")
    compiler = replace_once(
        compiler,
        """if (use_relative_vtables_abi) {
    cflags_cc += [ "-fexperimental-relative-c++-abi-vtables" ]
    ldflags += [ "-fexperimental-relative-c++-abi-vtables" ]
  }""",
        """if (use_relative_vtables_abi) {
    cflags_cc += [ "-fexperimental-relative-c++-abi-vtables" ]
    ldflags += [ "-fexperimental-relative-c++-abi-vtables" ]
  } else if (is_android && current_cpu == "arm64") {
    # New Chromium Clang defaults to relative vtables for this ABI. Cronet's
    # static archive must remain linkable by the older lld bundled with the
    # caller-selected Android NDK.
    cflags_cc += [ "-fno-experimental-relative-c++-abi-vtables" ]
  }""",
        "compiler/BUILD.gn",
    )
    write("android/build/config/compiler/BUILD.gn", compiler)


def ohos() -> None:
    gni = read("base/allocator/partition_allocator/partition_alloc.gni")
    gni = replace_once(
        gni,
        'has_memory_tagging = current_cpu == "arm64" && is_clang && !is_asan &&\n                     !is_hwasan && (is_linux || is_android)',
        'has_memory_tagging = current_cpu == "arm64" && is_clang && !is_asan &&\n                     !is_hwasan && !cronet_target_ohos &&\n                     (is_linux || is_android)',
        "partition_alloc.gni",
    )
    write("ohos/base/allocator/partition_allocator/partition_alloc.gni", gni)

    pa_build = from_overlay_or_src("base/allocator/partition_allocator/src/partition_alloc/BUILD.gn")
    pa_build = replace_once(
        pa_build,
        'if (current_cpu == "arm64" && is_clang &&\n        (is_linux || is_chromeos || is_android || is_fuchsia)) {',
        'if (current_cpu == "arm64" && is_clang && !cronet_target_ohos &&\n        (is_linux || is_chromeos || is_android || is_fuchsia)) {',
        "partition_alloc BUILD.gn",
    )
    write("ohos/base/allocator/partition_allocator/src/partition_alloc/BUILD.gn", pa_build)

    buildconfig = read("build/config/BUILDCONFIG.gn")
    buildconfig = replace_once(
        buildconfig,
        "declare_args() {\n  # Set to enable the official build level of optimization.",
        'declare_args() {\n  # cronet-rs models OHOS as Linux for Chromium\'s existing POSIX graph.\n  # Keep the actual target identity globally visible to compatibility logic.\n  cronet_target_ohos = false\n  cronet_ohos_llvm_triple = ""\n  cronet_ohos_rust_triple = ""\n\n  # Set to enable the official build level of optimization.',
        "OHOS BUILDCONFIG.gn",
    )
    write("ohos/build/config/BUILDCONFIG.gn", buildconfig)

    rust = read("build/config/rust.gni")
    rust = replace_once(
        rust,
        'rust_abi_target = ""\nif (is_linux || is_chromeos) {',
        'rust_abi_target = ""\nif (cronet_target_ohos && is_a_target_toolchain) {\n  assert(cronet_ohos_rust_triple != "", "tokio-cronet-src requires an OHOS Rust target")\n  rust_abi_target = cronet_ohos_rust_triple\n} else if (is_linux || is_chromeos) {',
        "rust.gni",
    )
    rust = replace_once(
        rust,
        '  assert(_is_rust_abi_target_a_known_triple,\n         "`${rust_abi_target}` needs to be added to " +',
        '  assert(_is_rust_abi_target_a_known_triple || cronet_target_ohos,\n         "`${rust_abi_target}` needs to be added to " +',
        "rust.gni known triple",
    )
    write("ohos/build/config/rust.gni", rust)

    compiler = read("build/config/compiler/BUILD.gn")
    compiler = replace_once(
        compiler,
        'config("compiler") {\n  asmflags = []\n  cflags = []\n  cflags_c = []',
        'config("compiler") {\n  asmflags = []\n  cflags = []\n  if (cronet_target_ohos) {\n    assert(cronet_ohos_llvm_triple != "", "tokio-cronet-src requires an OHOS LLVM target")\n    # Action-based Clang consumers such as bindgen do not inherit the compiler\n    # executable\'s extra flags, so the ABI target must also be a config flag.\n    cflags += [ "--target=" + cronet_ohos_llvm_triple ]\n  }\n  cflags_c = []',
        "ohos compiler target",
    )
    compiler = replace_once(
        compiler,
        "if (toolchain_has_rust && _perform_consistency_checks &&\n        !rust_force_head_revision) {",
        "if (toolchain_has_rust && _perform_consistency_checks &&\n        !rust_force_head_revision && !cronet_target_ohos) {",
        "ohos rust check",
    )
    compiler = replace_once(
        compiler,
        'if (is_linux && use_lld && current_cpu != "arm" && current_cpu != "s390x") {',
        'if (is_linux && use_lld && !cronet_target_ohos &&\n        current_cpu != "arm" && current_cpu != "s390x") {',
        "ohos crel",
    )
    compiler = replace_once(
        compiler,
        'if (use_cxx23) {\n      cflags_cc += [ "-std=c++23" ]',
        'if (use_cxx23) {\n      if (cronet_target_ohos) {\n        cflags_cc += [ "-std=c++2b" ]\n      } else {\n        cflags_cc += [ "-std=c++23" ]\n      }',
        "ohos cxx23",
    )
    compiler = replace_once(
        compiler,
        'if (use_cxx23) {\n      cflags_cc += [ "-std=${standard_prefix}++23" ]',
        'if (use_cxx23) {\n      if (cronet_target_ohos) {\n        cflags_cc += [ "-std=${standard_prefix}++2b" ]\n      } else {\n        cflags_cc += [ "-std=${standard_prefix}++23" ]\n      }',
        "ohos posix cxx23",
    )
    compiler = replace_once(
        compiler,
        'if (default_toolchain != "//build/toolchain/cros:target") {\n      cflags += [\n        "-mllvm",\n        "-split-threshold-for-reg-with-hint=0",',
        'if (default_toolchain != "//build/toolchain/cros:target" &&\n        !cronet_target_ohos) {\n      cflags += [\n        "-mllvm",\n        "-split-threshold-for-reg-with-hint=0",',
        "ohos split threshold",
    )
    for old, new in [
        (
            'if (is_clang && !is_android && !is_fuchsia && !is_chromeos_device) {\n        cflags += [ "--target=x86_64-unknown-linux-gnu" ]',
            'if (is_clang && !is_android && !is_fuchsia && !is_chromeos_device &&\n          !cronet_target_ohos) {\n        cflags += [ "--target=x86_64-unknown-linux-gnu" ]',
        ),
        (
            'if (is_clang && !is_android && !is_chromeos_device) {\n        cflags += [ "--target=arm-linux-gnueabihf" ]',
            'if (is_clang && !is_android && !is_chromeos_device &&\n          !cronet_target_ohos) {\n        cflags += [ "--target=arm-linux-gnueabihf" ]',
        ),
        (
            'if (is_clang && !is_android && !is_fuchsia && !is_chromeos_device) {\n        cflags += [ "--target=aarch64-linux-gnu" ]',
            'if (is_clang && !is_android && !is_fuchsia && !is_chromeos_device &&\n          !cronet_target_ohos) {\n        cflags += [ "--target=aarch64-linux-gnu" ]',
        ),
    ]:
        compiler = replace_once(compiler, old, new, "ohos gnu target")
    write("ohos/build/config/compiler/BUILD.gn", compiler)


def main() -> None:
    if not (SRC / "components/cronet/cronet_global_state_stubs.cc").is_file():
        raise SystemExit("pinned Chromium source is missing; run cargo xtask sync")
    common()
    android()
    ohos()


if __name__ == "__main__":
    main()

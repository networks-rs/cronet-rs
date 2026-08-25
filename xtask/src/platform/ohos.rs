use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
};

use super::{PlatformBuild, PlatformKind, TargetSpec};
use crate::{
    OHOS_SDK_NATIVE_ENV, PlatformConfig, command_stdout, display_error, escape_gn_string,
    gn_string_list, gn_string_path, require_file, rust_stdlib_adjustments,
};

const AARCH64: TargetSpec = TargetSpec {
    triple: "aarch64-unknown-linux-ohos",
    gn_os: "linux",
    gn_cpu: "arm64",
};
const ARMV7: TargetSpec = TargetSpec {
    triple: "armv7-unknown-linux-ohos",
    gn_os: "linux",
    gn_cpu: "arm",
};
const X86_64: TargetSpec = TargetSpec {
    triple: "x86_64-unknown-linux-ohos",
    gn_os: "linux",
    gn_cpu: "x64",
};

struct OhosBuild(TargetSpec);

pub(super) fn resolve(target: &str) -> Option<Box<dyn PlatformBuild>> {
    let target = match target {
        "aarch64-unknown-linux-ohos" => AARCH64,
        "armv7-unknown-linux-ohos" => ARMV7,
        "x86_64-unknown-linux-ohos" => X86_64,
        _ => return None,
    };
    Some(Box::new(OhosBuild(target)))
}

impl PlatformBuild for OhosBuild {
    fn kind(&self) -> PlatformKind {
        PlatformKind::Ohos
    }

    fn target_spec(&self) -> Option<TargetSpec> {
        Some(self.0)
    }

    fn prepare_overlay(&self, source: &Path, overlay: &Path) -> Result<(), String> {
        crate::overlay_files::install(source, overlay, crate::overlay_files::OHOS)?;
        crate::overlay_files::install_source_wrappers(
            source,
            overlay,
            crate::overlay_files::OHOS_SOURCE_WRAPPERS,
        )
    }

    fn gn_args(
        &self,
        _source: &Path,
        _overlay: &Path,
        config: PlatformConfig<'_>,
    ) -> Result<Vec<String>, String> {
        let mut arguments = self.0.gn_args();
        arguments.extend(ohos_gn_args(
            self.0.triple,
            config.ohos_sdk_native,
            config.rust_sysroot,
        )?);
        Ok(arguments)
    }

    fn needs_rustc_bootstrap(&self, _config: PlatformConfig<'_>) -> bool {
        true
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct OhosTarget {
    llvm: &'static str,
    rust: &'static str,
}

fn ohos_target(target: &str) -> Option<OhosTarget> {
    match target {
        "aarch64-unknown-linux-ohos" => Some(OhosTarget {
            llvm: "aarch64-linux-ohos",
            rust: "aarch64-unknown-linux-ohos",
        }),
        "armv7-unknown-linux-ohos" => Some(OhosTarget {
            llvm: "arm-linux-ohos",
            rust: "armv7-unknown-linux-ohos",
        }),
        "x86_64-unknown-linux-ohos" => Some(OhosTarget {
            llvm: "x86_64-linux-ohos",
            rust: "x86_64-unknown-linux-ohos",
        }),
        _ => None,
    }
}

fn ohos_gn_args(
    target: &str,
    sdk_native: Option<&Path>,
    rust_sysroot: Option<&Path>,
) -> Result<Vec<String>, String> {
    let target =
        ohos_target(target).ok_or_else(|| "OHOS GN arguments require an OHOS target".to_owned())?;
    let sdk = ohos_sdk_native(sdk_native)?;
    let sdk = sdk
        .canonicalize()
        .map_err(display_error("resolve the OHOS Native SDK"))?;
    let sysroot = sdk.join("sysroot");
    if !sysroot.is_dir() {
        return Err(format!("OHOS sysroot is missing at {}", sysroot.display()));
    }

    let rustc = env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
    let rust_sysroot = if let Some(path) = rust_sysroot {
        path.to_owned()
    } else {
        let output = command_stdout(
            Command::new(&rustc).args(["--print", "sysroot"]),
            "locate the Rust sysroot",
        )?;
        PathBuf::from(output.trim())
    };
    if !rust_sysroot
        .join("lib/rustlib")
        .join(target.rust)
        .join("lib")
        .is_dir()
    {
        return Err(format!(
            "Rust target `{}` is not installed; run `rustup target add {}`",
            target.rust, target.rust
        ));
    }
    let rustc_version = command_stdout(Command::new(&rustc).arg("-V"), "read rustc version")?;
    let (removed_stdlibs, added_stdlibs) = rust_stdlib_adjustments(&rust_sysroot, target.rust)?;
    let compiler_resource_dir = ohos_compiler_resource_dir(&sdk, target.llvm)?;
    let target_runtime_dir = sdk.join("llvm/lib").join(target.llvm);
    require_file(
        &target_runtime_dir.join("libunwind.a"),
        "install a complete OHOS Native SDK containing its target runtime",
    )?;

    let mut arguments = vec![
        "custom_toolchain=\"//cronet_rs_ohos_toolchain:ohos\"".to_owned(),
        "cronet_target_ohos=true".to_owned(),
        format!("cronet_ohos_llvm_triple=\"{}\"", target.llvm),
        format!("cronet_ohos_rust_triple=\"{}\"", target.rust),
        gn_string_path("cronet_ohos_sdk_native", &sdk),
        gn_string_path("cronet_ohos_compiler_resource_dir", &compiler_resource_dir),
        gn_string_path("cronet_ohos_target_runtime_dir", &target_runtime_dir),
        gn_string_path("target_sysroot", &sysroot),
        gn_string_path("rust_sysroot_absolute", &rust_sysroot),
        format!(
            "rustc_version=\"{}\"",
            escape_gn_string(rustc_version.trim())
        ),
        gn_string_list("removed_rust_stdlib_libs", &removed_stdlibs),
        gn_string_list("added_rust_stdlib_libs", &added_stdlibs),
        // Keep the C++ compiler, headers, runtime, and ABI locked to the same
        // Chromium revision. The OHOS SDK is only an OS sysroot, just as a
        // source-building native crate treats a platform SDK independently of
        // the compiler used to build its bundled sources.
        "use_custom_libcxx=true".to_owned(),
        "use_custom_libcxx_for_host=true".to_owned(),
        "clang_use_chrome_plugins=false".to_owned(),
        // Restored build caches can contain host PCH files whose embedded
        // Chromium Clang-header mtimes predate a source/dependency refresh.
        // Clang rejects those before Ninja can repair the cross-toolchain
        // action, so keep the portable source build independent of host PCHs.
        "enable_precompiled_headers=false".to_owned(),
        "use_dbus=false".to_owned(),
        "use_gio=false".to_owned(),
        "use_glib=false".to_owned(),
        "use_udev=false".to_owned(),
        // OHOS does not expose the desktop Linux NSS certificate database.
        // Cronet still uses its bundled BoringSSL implementation for TLS.
        "use_nss_certs=false".to_owned(),
        // Negotiate authentication requires the host platform's Kerberos and
        // GSSAPI implementation, neither of which is part of the OHOS NDK.
        "use_kerberos=false".to_owned(),
        "use_allocator_shim=false".to_owned(),
        "use_thin_lto=false".to_owned(),
        "fatal_linker_warnings=false".to_owned(),
    ];
    if target.rust == "armv7-unknown-linux-ohos" {
        // Both the stable Rust target (EABI, not EABIhf) and the OHOS SDK
        // driver use the soft-float calling convention with hardware FP
        // instructions enabled.
        arguments.push("arm_float_abi=\"softfp\"".to_owned());
    }
    Ok(arguments)
}

pub(crate) fn ohos_compiler_resource_dir(sdk: &Path, llvm_target: &str) -> Result<PathBuf, String> {
    let clang_root = sdk.join("llvm/lib/clang");
    let entries = fs::read_dir(&clang_root).map_err(|error| {
        format!(
            "failed to inspect OHOS compiler runtime directory {}: {error}",
            clang_root.display()
        )
    })?;
    let mut candidates = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.join("lib")
                .join(llvm_target)
                .join("libclang_rt.builtins.a")
                .is_file()
        })
        .collect::<Vec<_>>();
    candidates.sort();
    candidates.pop().ok_or_else(|| {
        format!(
            "OHOS Native SDK {} has no compiler runtime for `{llvm_target}`",
            sdk.display()
        )
    })
}

fn ohos_sdk_native(explicit: Option<&Path>) -> Result<PathBuf, String> {
    if let Some(path) = explicit {
        return validate_ohos_sdk_native(path.to_owned(), "Build::ohos_sdk_native");
    }
    if let Some(path) = env::var_os(OHOS_SDK_NATIVE_ENV) {
        return validate_ohos_sdk_native(PathBuf::from(path), OHOS_SDK_NATIVE_ENV);
    }
    if let Some(root) = env::var_os("OHOS_NDK_HOME").map(PathBuf::from) {
        let native = root.join("native");
        return validate_ohos_sdk_native(
            if native.is_dir() { native } else { root },
            "OHOS_NDK_HOME",
        );
    }
    Err(format!(
        "OHOS Native SDK not configured; call Build::ohos_sdk_native or set {OHOS_SDK_NATIVE_ENV}/OHOS_NDK_HOME"
    ))
}

fn validate_ohos_sdk_native(path: PathBuf, source: &str) -> Result<PathBuf, String> {
    if path.join("sysroot").is_dir() {
        Ok(path)
    } else {
        Err(format!(
            "{source}={} is not an OHOS Native SDK root",
            path.display()
        ))
    }
}

use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
};

use super::{PlatformBuild, PlatformKind, TargetSpec};
use crate::{
    CHROMIUM_VERSION, CLANG_DIR_ENV, PlatformConfig, RUST_BINDGEN_ENV, command_stdout,
    display_error, ensure_symlink, escape_gn_string, gn_string_list, gn_string_path, require_file,
    rust_stdlib_adjustments,
};

const X86_64: TargetSpec = TargetSpec {
    triple: "x86_64-unknown-linux-gnu",
    gn_os: "linux",
    gn_cpu: "x64",
};
const AARCH64: TargetSpec = TargetSpec {
    triple: "aarch64-unknown-linux-gnu",
    gn_os: "linux",
    gn_cpu: "arm64",
};

struct LinuxBuild(TargetSpec);

pub(super) fn resolve(target: &str) -> Option<Box<dyn PlatformBuild>> {
    let target = match target {
        "x86_64-unknown-linux-gnu" => X86_64,
        "aarch64-unknown-linux-gnu" => AARCH64,
        _ => return None,
    };
    Some(Box::new(LinuxBuild(target)))
}

impl PlatformBuild for LinuxBuild {
    fn kind(&self) -> PlatformKind {
        PlatformKind::Linux
    }

    fn target_spec(&self) -> Option<TargetSpec> {
        Some(self.0)
    }

    fn gn_args(
        &self,
        source: &Path,
        overlay: &Path,
        config: PlatformConfig<'_>,
    ) -> Result<Vec<String>, String> {
        let mut arguments = self.0.gn_args();
        arguments.extend(desktop_linux_host_toolchain_gn_args(
            source,
            overlay,
            self.0.triple,
            config.clang_dir,
            config.rust_sysroot,
            config.rust_bindgen,
        )?);
        Ok(arguments)
    }

    fn needs_rustc_bootstrap(&self, config: PlatformConfig<'_>) -> bool {
        config.rust_sysroot.is_some()
            || requires_native_linux_arm64_tools(
                std::env::consts::OS,
                std::env::consts::ARCH,
                self.0.triple,
            )
    }
}

fn is_desktop_linux_target(target: &str) -> bool {
    super::kind(target) == Some(super::PlatformKind::Linux)
}

pub(crate) fn requires_native_linux_arm64_tools(
    host_os: &str,
    host_arch: &str,
    target: &str,
) -> bool {
    host_os == "linux" && host_arch == "aarch64" && target == "aarch64-unknown-linux-gnu"
}

fn desktop_linux_host_toolchain_gn_args(
    source: &Path,
    overlay: &Path,
    target: &str,
    explicit_clang: Option<&Path>,
    explicit_rust_sysroot: Option<&Path>,
    explicit_bindgen: Option<&Path>,
) -> Result<Vec<String>, String> {
    if !is_desktop_linux_target(target) {
        return Ok(Vec::new());
    }
    let native_arm64 =
        requires_native_linux_arm64_tools(env::consts::OS, env::consts::ARCH, target);
    let configured_clang = explicit_clang
        .map(Path::to_owned)
        .or_else(|| env::var_os(CLANG_DIR_ENV).map(PathBuf::from));
    if native_arm64 && configured_clang.is_none() {
        return Err(format!(
            "Chromium {CHROMIUM_VERSION} only publishes an x86_64 Linux compiler; install a host-native LLVM 22 toolchain and set {CLANG_DIR_ENV} to its root"
        ));
    }

    let mut arguments = Vec::new();
    let clang = if let Some(clang) = configured_clang {
        let clang = validate_external_clang(&clang)?;
        arguments.extend([
            gn_string_path("clang_base_path", &clang),
            "clang_use_chrome_plugins=false".to_owned(),
            "clang_use_raw_ptr_plugin=false".to_owned(),
            "clang_use_unsafe_buffers_plugin=false".to_owned(),
        ]);
        clang
    } else {
        source.join("third_party/llvm-build/Release+Asserts")
    };

    if native_arm64 || explicit_rust_sysroot.is_some() {
        let external_rust = external_rust_gn_args(target, explicit_rust_sysroot)?;
        let bindgen_root = prepare_host_rust_bindgen_overlay(
            overlay,
            &external_rust.sysroot,
            &clang,
            explicit_bindgen,
        )?;
        arguments.extend(external_rust.arguments);
        arguments.push(gn_string_path("rust_bindgen_root", &bindgen_root));
    }
    Ok(arguments)
}

fn validate_external_clang(directory: &Path) -> Result<PathBuf, String> {
    let directory = directory
        .canonicalize()
        .map_err(display_error("resolve the host-native LLVM toolchain"))?;
    for tool in [
        "clang",
        "clang++",
        "ld.lld",
        "llvm-ar",
        "llvm-nm",
        "llvm-objcopy",
        "llvm-readelf",
        "llvm-strip",
    ] {
        require_file(
            &directory.join("bin").join(tool),
            "install a complete LLVM 22 toolchain",
        )?;
    }
    let version = command_stdout(
        Command::new(directory.join("bin/clang")).arg("-dumpversion"),
        "read the host-native Clang version",
    )?;
    let major = version.trim().split('.').next().unwrap_or_default();
    if major != "22" {
        return Err(format!(
            "{CLANG_DIR_ENV}={} contains Clang {}, but Chromium {CHROMIUM_VERSION} requires LLVM major version 22",
            directory.display(),
            version.trim()
        ));
    }
    Ok(directory)
}

struct ExternalRust {
    sysroot: PathBuf,
    arguments: Vec<String>,
}

fn external_rust_gn_args(
    target: &str,
    explicit_sysroot: Option<&Path>,
) -> Result<ExternalRust, String> {
    let rustc = env::var_os("RUSTC")
        .map(PathBuf::from)
        .or_else(|| {
            explicit_sysroot
                .map(|sysroot| sysroot.join("bin/rustc"))
                .filter(|rustc| rustc.is_file())
        })
        .unwrap_or_else(|| PathBuf::from("rustc"));
    let sysroot = if let Some(path) = explicit_sysroot {
        path.to_owned()
    } else {
        let output = command_stdout(
            Command::new(&rustc).args(["--print", "sysroot"]),
            "locate the host-native Rust sysroot",
        )?;
        PathBuf::from(output.trim())
    };
    let sysroot = sysroot
        .canonicalize()
        .map_err(display_error("resolve the host-native Rust sysroot"))?;
    if !sysroot
        .join("lib/rustlib")
        .join(target)
        .join("lib")
        .is_dir()
    {
        return Err(format!(
            "Rust target `{target}` is not installed in {}; run `rustup target add {target}` for the selected toolchain",
            sysroot.display()
        ));
    }
    require_file(
        &sysroot.join("bin/rustfmt"),
        "install the rustfmt component for the selected Rust toolchain",
    )?;
    let rustc_version = command_stdout(Command::new(&rustc).arg("-V"), "read rustc version")?;
    let (removed_stdlibs, added_stdlibs) = rust_stdlib_adjustments(&sysroot, target)?;
    Ok(ExternalRust {
        sysroot: sysroot.clone(),
        arguments: vec![
            gn_string_path("rust_sysroot_absolute", &sysroot),
            format!(
                "rustc_version=\"{}\"",
                escape_gn_string(rustc_version.trim())
            ),
            gn_string_list("removed_rust_stdlib_libs", &removed_stdlibs),
            gn_string_list("added_rust_stdlib_libs", &added_stdlibs),
            // A custom stable Rust release is not guaranteed to share the
            // pinned compiler's LLVM revision. Keep native object boundaries
            // and let lld combine them without cross-language ThinLTO.
            "toolchain_supports_rust_thin_lto=false".to_owned(),
        ],
    })
}

fn prepare_host_rust_bindgen_overlay(
    overlay: &Path,
    rust_sysroot: &Path,
    clang: &Path,
    explicit_bindgen: Option<&Path>,
) -> Result<PathBuf, String> {
    let bindgen = explicit_bindgen
        .map(Path::to_owned)
        .or_else(|| env::var_os(RUST_BINDGEN_ENV).map(PathBuf::from))
        .or_else(|| executable_on_path("bindgen"))
        .ok_or_else(|| {
            format!(
                "host-native bindgen 0.72 is required; install `bindgen-cli` 0.72.0 or set {RUST_BINDGEN_ENV}"
            )
        })?;
    let bindgen = bindgen
        .canonicalize()
        .map_err(display_error("resolve the host-native bindgen executable"))?;
    let version = command_stdout(
        Command::new(&bindgen).arg("--version"),
        "read bindgen version",
    )?;
    if !version.trim().starts_with("bindgen 0.72.") {
        return Err(format!(
            "{} reports `{}`, but Chromium {CHROMIUM_VERSION} requires bindgen 0.72.x",
            bindgen.display(),
            version.trim()
        ));
    }
    let clang_lib = clang.join("lib");
    let has_libclang = fs::read_dir(&clang_lib)
        .map_err(display_error("inspect the host-native LLVM libraries"))?
        .flatten()
        .any(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with("libclang.so")
        });
    if !has_libclang {
        return Err(format!(
            "host-native libclang is missing below {}; install the LLVM 22 libclang development package",
            clang_lib.display()
        ));
    }

    let root = overlay.join("cronet_rs_host_rust_tools");
    if root.exists() {
        fs::remove_dir_all(&root).map_err(display_error("refresh host Rust tools overlay"))?;
    }
    fs::create_dir_all(root.join("bin"))
        .map_err(display_error("create host Rust tools overlay"))?;
    let rustfmt_wrapper = crate::workspace_root().join("xtask/wrappers/rustfmt");
    let rustfmt_config = crate::workspace_root().join("xtask/wrappers/rustfmt.toml");
    require_file(&rustfmt_wrapper, "restore the committed rustfmt wrapper")?;
    require_file(&rustfmt_config, "restore the committed rustfmt config")?;
    ensure_symlink(&bindgen, &root.join("bin/bindgen"))?;
    ensure_symlink(&rustfmt_wrapper, &root.join("bin/rustfmt"))?;
    ensure_symlink(
        &rust_sysroot.join("bin/rustfmt"),
        &root.join("bin/rustfmt.real"),
    )?;
    ensure_symlink(&rustfmt_config, &root.join("bin/rustfmt.toml"))?;
    ensure_symlink(&clang_lib, &root.join("lib"))?;
    Ok(root)
}

fn executable_on_path(name: &str) -> Option<PathBuf> {
    env::var_os("PATH").and_then(|path| {
        env::split_paths(&path)
            .map(|directory| directory.join(name))
            .find(|candidate| candidate.is_file())
    })
}

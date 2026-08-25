use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
};

use super::{PlatformBuild, PlatformKind, TargetSpec};
use crate::{
    ANDROID_API_LEVEL_ENV, ANDROID_NDK_HOME_ENV, NativeLinkage, PlatformConfig, display_error,
    ensure_symlink, gn_string_path, require_file, write_if_changed,
};

const I686: TargetSpec = TargetSpec {
    triple: "i686-linux-android",
    gn_os: "android",
    gn_cpu: "x86",
};
const X86_64: TargetSpec = TargetSpec {
    triple: "x86_64-linux-android",
    gn_os: "android",
    gn_cpu: "x64",
};
const ARMV7: TargetSpec = TargetSpec {
    triple: "armv7-linux-androideabi",
    gn_os: "android",
    gn_cpu: "arm",
};
const AARCH64: TargetSpec = TargetSpec {
    triple: "aarch64-linux-android",
    gn_os: "android",
    gn_cpu: "arm64",
};

struct AndroidBuild(TargetSpec);

pub(super) fn resolve(target: &str) -> Option<Box<dyn PlatformBuild>> {
    let target = match target {
        "i686-linux-android" => I686,
        "x86_64-linux-android" => X86_64,
        "armv7-linux-androideabi" => ARMV7,
        "aarch64-linux-android" => AARCH64,
        _ => return None,
    };
    Some(Box::new(AndroidBuild(target)))
}

impl PlatformBuild for AndroidBuild {
    fn kind(&self) -> PlatformKind {
        PlatformKind::Android
    }

    fn target_spec(&self) -> Option<TargetSpec> {
        Some(self.0)
    }

    fn prepare_overlay(&self, source: &Path, overlay: &Path) -> Result<(), String> {
        crate::overlay_files::install(source, overlay, crate::overlay_files::ANDROID)?;
        crate::overlay_files::install_source_wrappers(
            source,
            overlay,
            crate::overlay_files::ANDROID_SOURCE_WRAPPERS,
        )?;
        link_android_host_tools(source, overlay)
    }

    fn gn_args(
        &self,
        source: &Path,
        overlay: &Path,
        config: PlatformConfig<'_>,
    ) -> Result<Vec<String>, String> {
        let mut arguments = self.0.gn_args();
        arguments.extend(android_gn_args(
            source,
            overlay,
            self.0.triple,
            config.android_ndk,
            config.android_api_level,
        )?);
        Ok(arguments)
    }

    fn configure_ninja(
        &self,
        command: &mut Command,
        overlay: &Path,
        _linkages: &[NativeLinkage],
    ) -> Result<(), String> {
        // Chromium's Java helpers otherwise canonicalize the shared output
        // directory back to the source checkout and select its CIPD host JDK.
        command
            .env("CHECKOUT_SOURCE_ROOT", overlay)
            .arg(":cronet_rs_android_support_java");
        Ok(())
    }

    fn post_build(&self, build_dir: &Path, output_dir: &Path) -> Result<(), String> {
        install_android_support_dex(build_dir, output_dir)
    }

    fn filter_third_party_tests(&self) -> bool {
        false
    }
}

fn install_android_support_dex(build_dir: &Path, output_dir: &Path) -> Result<(), String> {
    let source = build_dir.join("obj/cronet_rs_android_support_java.dex.jar");
    require_file(
        &source,
        "build the Cronet Android support Java target first",
    )?;
    let contents = fs::read(&source).map_err(display_error("read Android support dex jar"))?;
    write_if_changed(
        &output_dir.join("cronet-android-support.dex.jar"),
        &contents,
        "install Android support dex jar",
    )
}

fn is_android_target(target: &str) -> bool {
    super::kind(target) == Some(super::PlatformKind::Android)
}

fn android_gn_args(
    source: &Path,
    overlay: &Path,
    target: &str,
    explicit_ndk: Option<&Path>,
    explicit_api_level: Option<u32>,
) -> Result<Vec<String>, String> {
    let ndk = android_ndk(explicit_ndk)?
        .canonicalize()
        .map_err(display_error("resolve the Android NDK"))?;
    require_file(
        &ndk.join("source.properties"),
        "install a complete Android NDK",
    )?;
    if !ndk.join("toolchains/llvm/prebuilt").is_dir() {
        return Err(format!(
            "Android NDK toolchain is missing below {}",
            ndk.display()
        ));
    }
    let revision = fs::read_to_string(ndk.join("source.properties"))
        .map_err(display_error("read the Android NDK revision"))?;
    let major = revision
        .lines()
        .find_map(|line| line.strip_prefix("Pkg.Revision = "))
        .and_then(|version| version.split('.').next())
        .and_then(|major| major.parse::<u32>().ok())
        .ok_or_else(|| {
            format!(
                "could not determine the Android NDK revision from {}",
                ndk.join("source.properties").display()
            )
        })?;
    let api_level = if let Some(level) = explicit_api_level {
        level
    } else if let Ok(value) = env::var(ANDROID_API_LEVEL_ENV) {
        value.parse::<u32>().map_err(|_| {
            format!("{ANDROID_API_LEVEL_ENV} must be an integer Android API level, got `{value}`")
        })?
    } else {
        23
    };
    if api_level < 23 {
        return Err(format!(
            "Android API level {api_level} is below Cronet's minimum API 23"
        ));
    }

    let mut arguments = vec![
        gn_string_path("android_ndk_root", &ndk),
        format!("android_ndk_version=\"r{major}\""),
        format!("android_ndk_api_level={api_level}"),
        "android_static_analysis=\"off\"".to_owned(),
        // LLVM 22's relative-vtable relocations require an equally new final
        // linker. Rust Android applications normally use the selected NDK's
        // lld (r27 currently ships lld 18), so use the portable C++ ABI for
        // source-built static libraries.
        "use_relative_vtables_abi=false".to_owned(),
    ];
    if let Some(clang_base) =
        prepare_android_clang_overlay(source, overlay, target, &ndk, api_level)?
    {
        arguments.push(gn_string_path("clang_base_path", &clang_base));
    }
    Ok(arguments)
}

/// Chromium publishes host-native Clang packages. The Linux package includes
/// Android compiler-rt runtimes, but the macOS package intentionally does not
/// because upstream Chromium only supports Android builds from Linux. Keep the
/// host-native compiler and complete only its target runtime from the selected
/// NDK in the generated GN overlay. The synchronized source/toolchain is never
/// modified, and Linux hosts that already have the runtime need no overlay.
fn prepare_android_clang_overlay(
    source: &Path,
    overlay: &Path,
    target: &str,
    ndk: &Path,
    api_level: u32,
) -> Result<Option<PathBuf>, String> {
    let clang_base = source.join("third_party/llvm-build/Release+Asserts");
    let resource_root = clang_base.join("lib/clang");
    let resource_dir = latest_clang_resource_dir(&resource_root)?;
    let (ndk_archive_name, compiler_directory) = android_compiler_runtime(target, api_level)?;
    let bundled_runtime = resource_dir
        .join("lib")
        .join(&compiler_directory)
        .join("libclang_rt.builtins.a");
    if bundled_runtime.is_file() {
        return Ok(None);
    }

    let ndk_runtime = find_android_ndk_runtime(ndk, ndk_archive_name)?;
    let generated_base = overlay.join("cronet_rs_android_clang");
    let generated_runtime = generated_base
        .join("lib/clang")
        .join(resource_dir.file_name().ok_or_else(|| {
            format!(
                "invalid Chromium Clang resource path {}",
                resource_dir.display()
            )
        })?)
        .join("lib")
        .join(&compiler_directory)
        .join("libclang_rt.builtins.a");
    if generated_runtime.is_file() && generated_base.join("bin/clang").is_file() {
        return Ok(Some(generated_base));
    }
    if generated_base.exists() {
        fs::remove_dir_all(&generated_base)
            .map_err(display_error("replace generated Android Clang overlay"))?;
    }
    fs::create_dir_all(&generated_base)
        .map_err(display_error("create generated Android Clang overlay"))?;

    mirror_directory_entries(&clang_base, &generated_base, &["lib"])?;
    let generated_lib = generated_base.join("lib");
    fs::create_dir(&generated_lib)
        .map_err(display_error("create Android Clang library overlay"))?;
    mirror_directory_entries(&clang_base.join("lib"), &generated_lib, &["clang"])?;
    let generated_clang = generated_lib.join("clang");
    fs::create_dir(&generated_clang)
        .map_err(display_error("create Android Clang resource overlay"))?;

    let resource_name = resource_dir.file_name().ok_or_else(|| {
        format!(
            "invalid Chromium Clang resource path {}",
            resource_dir.display()
        )
    })?;
    for entry in
        fs::read_dir(&resource_root).map_err(display_error("list Chromium Clang resources"))?
    {
        let entry = entry.map_err(display_error("read Chromium Clang resource entry"))?;
        if entry.file_name() != resource_name {
            ensure_symlink(&entry.path(), &generated_clang.join(entry.file_name()))?;
        }
    }

    let generated_resource = generated_clang.join(resource_name);
    fs::create_dir(&generated_resource).map_err(display_error(
        "create selected Android Clang resource overlay",
    ))?;
    mirror_directory_entries(&resource_dir, &generated_resource, &["lib"])?;
    let generated_resource_lib = generated_resource.join("lib");
    fs::create_dir(&generated_resource_lib)
        .map_err(display_error("create Android compiler runtime overlay"))?;
    mirror_directory_entries(&resource_dir.join("lib"), &generated_resource_lib, &[])?;

    let target_runtime = generated_resource_lib.join(compiler_directory);
    fs::create_dir(&target_runtime)
        .map_err(display_error("create Android target runtime directory"))?;
    fs::copy(&ndk_runtime, target_runtime.join("libclang_rt.builtins.a"))
        .map_err(display_error("install the Android compiler runtime"))?;
    Ok(Some(generated_base))
}

fn latest_clang_resource_dir(root: &Path) -> Result<PathBuf, String> {
    let mut candidates = fs::read_dir(root)
        .map_err(display_error("list Chromium Clang resource directories"))?
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.join("include").is_dir() && path.join("lib").is_dir())
        .collect::<Vec<_>>();
    candidates.sort();
    candidates.pop().ok_or_else(|| {
        format!(
            "Chromium Clang has no complete resource directory below {}",
            root.display()
        )
    })
}

pub(crate) fn android_compiler_runtime(
    target: &str,
    api_level: u32,
) -> Result<(&'static str, String), String> {
    let (archive, llvm_arch) = match target {
        "aarch64-linux-android" => ("libclang_rt.builtins-aarch64-android.a", "aarch64"),
        "armv7-linux-androideabi" => ("libclang_rt.builtins-arm-android.a", "arm"),
        "i686-linux-android" => ("libclang_rt.builtins-i686-android.a", "i686"),
        "x86_64-linux-android" => ("libclang_rt.builtins-x86_64-android.a", "x86_64"),
        other => {
            return Err(format!(
                "unsupported Android compiler runtime target `{other}`"
            ));
        }
    };
    Ok((
        archive,
        format!("{llvm_arch}-unknown-linux-android{api_level}"),
    ))
}

fn find_android_ndk_runtime(ndk: &Path, archive_name: &str) -> Result<PathBuf, String> {
    let prebuilt_root = ndk.join("toolchains/llvm/prebuilt");
    let mut candidates = Vec::new();
    for prebuilt in
        fs::read_dir(&prebuilt_root).map_err(display_error("list Android NDK toolchains"))?
    {
        let clang_root = prebuilt
            .map_err(display_error("read Android NDK toolchain entry"))?
            .path()
            .join("lib/clang");
        let Ok(versions) = fs::read_dir(clang_root) else {
            continue;
        };
        for version in versions.flatten() {
            let candidate = version.path().join("lib/linux").join(archive_name);
            if candidate.is_file() {
                candidates.push(candidate);
            }
        }
    }
    candidates.sort();
    candidates.pop().ok_or_else(|| {
        format!(
            "Android NDK {} does not contain compiler runtime `{archive_name}`",
            ndk.display()
        )
    })
}

fn mirror_directory_entries(
    source: &Path,
    destination: &Path,
    skipped: &[&str],
) -> Result<(), String> {
    for entry in fs::read_dir(source).map_err(display_error("list toolchain directory"))? {
        let entry = entry.map_err(display_error("read toolchain directory entry"))?;
        if skipped
            .iter()
            .any(|name| entry.file_name().to_string_lossy() == *name)
        {
            continue;
        }
        ensure_symlink(&entry.path(), &destination.join(entry.file_name()))?;
    }
    Ok(())
}

fn android_ndk(explicit: Option<&Path>) -> Result<PathBuf, String> {
    if let Some(path) = explicit {
        return Ok(path.to_owned());
    }
    for variable in [ANDROID_NDK_HOME_ENV, "ANDROID_NDK_ROOT", "NDK_HOME"] {
        if let Some(path) = env::var_os(variable) {
            return Ok(PathBuf::from(path));
        }
    }
    Err(format!(
        "Android NDK not configured; call Build::android_ndk or set {ANDROID_NDK_HOME_ENV}/ANDROID_NDK_ROOT/NDK_HOME"
    ))
}

pub(crate) fn patch_android_clang_dependency(
    manifest: &Path,
    target: Option<&str>,
) -> Result<(), String> {
    const MARKER: &str =
        "'condition': '(host_os == \"linux\" or checkout_android) and non_git_source',";
    const PATCH: &str = "'condition': 'host_os == \"linux\" and non_git_source',";
    if !target.is_some_and(is_android_target) {
        return Ok(());
    }
    let mut contents = fs::read_to_string(manifest).map_err(display_error(
        "read the filtered Cronet dependency manifest",
    ))?;
    if !contents.contains(MARKER) {
        return Err("Chromium changed the Android Clang package condition".to_owned());
    }
    contents = contents.replacen(MARKER, PATCH, 1);
    for unused_package in [
        r"          {
              'package': 'chromium/third_party/android_sdk/public/emulator',
              'version': Var('android_sdk_emulator_version'),
          },
",
        r"          {
              'package': 'chromium/third_party/android_sdk/public/platform-tools',
              'version': Var('android_sdk_platform-tools_version'),
          },
",
        r"          {
              'package': 'chromium/third_party/android_sdk/public/cmdline-tools',
              'version': 'gekOVsZjseS1w9BXAT3FsoW__ByGDJYS9DgqesiwKYoC',
          },
",
    ] {
        if !contents.contains(unused_package) {
            return Err("Chromium changed the Android SDK CIPD package list".to_owned());
        }
        contents = contents.replacen(unused_package, "", 1);
    }
    if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        const LINUX_JDK: &str = r"'package': 'chromium/third_party/jdk/linux-amd64',
              'version': '2iiuF-nKDH3moTImx2op4WTRetbfhzKoZhH7Xo44zGsC',";
        const MAC_JDK: &str = r"'package': 'chromium/third_party/jdk/mac-arm64',
              'version': 'mwIUzTAGHZOaLLhBZDjHjmVYQnszzFskzBEPnWZyfKoC',";
        if !contents.contains(LINUX_JDK) {
            return Err("Chromium changed the pinned Android build JDK".to_owned());
        }
        // Chromium officially drives Android from Linux and therefore locks a
        // Linux JDK. Use the same upstream JDK 23 release for an Apple Silicon
        // host; both immutable CIPD instances carry the same version tag.
        contents = contents.replacen(LINUX_JDK, MAC_JDK, 1);
    }
    write_if_changed(
        manifest,
        contents.as_bytes(),
        "select native Android host-tool packages",
    )
}

fn link_android_host_tools(source: &Path, overlay: &Path) -> Result<(), String> {
    if !cfg!(target_os = "macos") {
        return Ok(());
    }

    let sdk_root = ["ANDROID_SDK_ROOT", "ANDROID_HOME"]
        .iter()
        .find_map(env::var_os)
        .map(PathBuf::from)
        .or_else(|| {
            env::var_os("HOME")
                .map(PathBuf::from)
                .map(|home| home.join("Library/Android/sdk"))
        })
        .ok_or_else(|| {
            "Android SDK not configured; set ANDROID_SDK_ROOT or ANDROID_HOME".to_owned()
        })?;
    let local_build_tools_root = sdk_root.join("build-tools");
    let mut local_versions = fs::read_dir(&local_build_tools_root)
        .map_err(display_error("list Android SDK build tools"))?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.join("aidl").is_file() && path.join("aapt2").is_file())
        .collect::<Vec<_>>();
    local_versions.sort();
    let local_build_tools = local_versions.pop().ok_or_else(|| {
        format!(
            "no complete Android SDK build-tools installation found in {}",
            local_build_tools_root.display()
        )
    })?;

    let pinned_root = source.join("third_party/android_sdk/public/build-tools");
    let mut pinned_versions = fs::read_dir(&pinned_root)
        .map_err(display_error("list pinned Android SDK build tools"))?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect::<Vec<_>>();
    pinned_versions.sort();
    let pinned = pinned_versions.pop().ok_or_else(|| {
        format!(
            "no pinned Android SDK build tools found in {}",
            pinned_root.display()
        )
    })?;
    let destination = overlay
        .join("third_party/android_sdk/public/build-tools")
        .join(pinned.file_name().expect("pinned build tools have a name"));
    let current_is_expected = fs::symlink_metadata(&destination)
        .is_ok_and(|metadata| metadata.file_type().is_symlink())
        && fs::read_link(&destination).is_ok_and(|current| current == local_build_tools);
    if !current_is_expected {
        if let Ok(metadata) = fs::symlink_metadata(&destination) {
            if metadata.file_type().is_symlink() || metadata.is_file() {
                fs::remove_file(&destination)
                    .map_err(display_error("replace Android host build-tools link"))?;
            } else if metadata.is_dir() {
                fs::remove_dir_all(&destination)
                    .map_err(display_error("replace Android host build-tools directory"))?;
            }
        }
    }
    ensure_symlink(&local_build_tools, &destination)?;

    let java_home = source.join("third_party/jdk/current/Contents/Home");
    require_file(
        &java_home.join("bin/javap"),
        "synchronize the pinned macOS Android build JDK",
    )?;
    let current = overlay.join("third_party/jdk/current");
    let current_is_expected = fs::symlink_metadata(&current)
        .is_ok_and(|metadata| metadata.file_type().is_symlink())
        && fs::read_link(&current).is_ok_and(|current| current == java_home);
    if !current_is_expected {
        if let Ok(metadata) = fs::symlink_metadata(&current) {
            if metadata.file_type().is_symlink() {
                fs::remove_file(&current)
                    .map_err(display_error("replace the generated host JDK link"))?;
            } else if metadata.is_dir() {
                fs::remove_dir_all(&current)
                    .map_err(display_error("replace the generated host JDK directory"))?;
            } else {
                return Err(format!(
                    "{} blocks the generated host JDK link",
                    current.display()
                ));
            }
        }
    }
    ensure_symlink(&java_home, &current)?;
    crate::overlay_files::install(source, overlay, crate::overlay_files::ANDROID_MAC_HOST)
}

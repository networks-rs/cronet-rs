use fs2::FileExt;
use std::{
    collections::{BTreeMap, BTreeSet},
    env, fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, ExitStatus, Stdio},
};

mod native_extensions;
mod overlay_files;
mod platform;

pub const CHROMIUM_REVISION: &str = "db64a84f93f16f8de53fee8d33df0a31473efefb";
pub const CHROMIUM_VERSION: &str = "146.0.7633.0";
const CHROMIUM_URL: &str = "https://chromium.googlesource.com/chromium/src.git";
const DEPOT_TOOLS_URL: &str = "https://chromium.googlesource.com/chromium/tools/depot_tools.git";
pub(crate) const OHOS_SDK_NATIVE_ENV: &str = "OHOS_SDK_NATIVE";
pub(crate) const ANDROID_NDK_HOME_ENV: &str = "ANDROID_NDK_HOME";
pub(crate) const ANDROID_API_LEVEL_ENV: &str = "ANDROID_API_LEVEL";
pub(crate) const CLANG_DIR_ENV: &str = "CRONET_CLANG_DIR";
pub(crate) const RUST_BINDGEN_ENV: &str = "CRONET_RUST_BINDGEN";

/// Native library form selected by `tokio-cronet-sys` or the workspace CLI.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NativeLinkage {
    Dynamic,
    Static,
}

/// Source-build result consumed by `tokio-cronet-sys`.
#[derive(Debug)]
pub struct Artifacts {
    source_dir: PathBuf,
    lib_dir: PathBuf,
    linkage: NativeLinkage,
}

impl Artifacts {
    #[must_use]
    pub fn source_dir(&self) -> &Path {
        &self.source_dir
    }

    #[must_use]
    pub fn lib_dir(&self) -> &Path {
        &self.lib_dir
    }

    #[must_use]
    pub const fn linkage(&self) -> NativeLinkage {
        self.linkage
    }
}

/// Builder analogous to `openssl_src::Build`: it materializes the pinned
/// source tree and compiles the selected native library for Cargo's target.
#[derive(Debug)]
pub struct Build {
    source_dir: Option<PathBuf>,
    target: Option<String>,
    linkage: NativeLinkage,
    ohos_sdk_native: Option<PathBuf>,
    clang_dir: Option<PathBuf>,
    rust_sysroot: Option<PathBuf>,
    rust_bindgen: Option<PathBuf>,
    android_ndk: Option<PathBuf>,
    android_api_level: Option<u32>,
    ios_developer_dir: Option<PathBuf>,
    ios_deployment_target: Option<String>,
}

impl Default for Build {
    fn default() -> Self {
        Self::new()
    }
}

impl Build {
    #[must_use]
    pub fn new() -> Self {
        Self {
            source_dir: None,
            target: env::var("TARGET").ok(),
            linkage: NativeLinkage::Dynamic,
            ohos_sdk_native: None,
            clang_dir: None,
            rust_sysroot: None,
            rust_bindgen: None,
            android_ndk: None,
            android_api_level: None,
            ios_developer_dir: None,
            ios_deployment_target: None,
        }
    }

    pub fn source_dir(&mut self, source_dir: impl Into<PathBuf>) -> &mut Self {
        self.source_dir = Some(source_dir.into());
        self
    }

    pub fn target(&mut self, target: impl Into<String>) -> &mut Self {
        self.target = Some(target.into());
        self
    }

    pub const fn linkage(&mut self, linkage: NativeLinkage) -> &mut Self {
        self.linkage = linkage;
        self
    }

    /// Selects an `OpenHarmony` Native SDK without relying on a machine-specific
    /// installation layout. Its target sysroot and ABI runtime archives are
    /// consumed; the pinned Chromium compiler and C++ runtime are
    /// built/synchronized with Cronet.
    /// If unset, `OHOS_SDK_NATIVE` or `OHOS_NDK_HOME` is used for OHOS targets.
    pub fn ohos_sdk_native(&mut self, directory: impl Into<PathBuf>) -> &mut Self {
        self.ohos_sdk_native = Some(directory.into());
        self
    }

    /// Selects a host-native LLVM installation used instead of Chromium's
    /// synchronized compiler. The directory must contain `bin/clang` and the
    /// matching LLVM tools. If unset, `CRONET_CLANG_DIR` is used.
    pub fn clang_dir(&mut self, directory: impl Into<PathBuf>) -> &mut Self {
        self.clang_dir = Some(directory.into());
        self
    }

    /// Selects the Rust sysroot that contains the requested target standard
    /// library. If unset, the sysroot reported by Cargo's `RUSTC` is used when
    /// a host-native Rust toolchain is required.
    pub fn rust_sysroot(&mut self, directory: impl Into<PathBuf>) -> &mut Self {
        self.rust_sysroot = Some(directory.into());
        self
    }

    /// Selects a host-native bindgen 0.72 executable for Chromium's Rust/C
    /// binding generators. If unset, `CRONET_RUST_BINDGEN` or `PATH` is used.
    pub fn rust_bindgen(&mut self, executable: impl Into<PathBuf>) -> &mut Self {
        self.rust_bindgen = Some(executable.into());
        self
    }

    /// Selects an Android NDK root. If unset, `ANDROID_NDK_HOME`,
    /// `ANDROID_NDK_ROOT`, or `NDK_HOME` is used for Android targets.
    pub fn android_ndk(&mut self, directory: impl Into<PathBuf>) -> &mut Self {
        self.android_ndk = Some(directory.into());
        self
    }

    /// Selects the Android API level used by the native library. Cronet's
    /// pinned minimum, API 23, is the default.
    pub const fn android_api_level(&mut self, api_level: u32) -> &mut Self {
        self.android_api_level = Some(api_level);
        self
    }

    /// Selects an Xcode Developer directory for iOS builds. If unset, the
    /// standard `DEVELOPER_DIR`/`xcode-select` discovery is used.
    pub fn ios_developer_dir(&mut self, directory: impl Into<PathBuf>) -> &mut Self {
        self.ios_developer_dir = Some(directory.into());
        self
    }

    /// Overrides Chromium's pinned iOS deployment target. If unset,
    /// `IPHONEOS_DEPLOYMENT_TARGET` or Chromium's default is used.
    pub fn ios_deployment_target(&mut self, version: impl Into<String>) -> &mut Self {
        self.ios_deployment_target = Some(version.into());
        self
    }

    pub fn build(&mut self) -> Result<Artifacts, String> {
        let target = self
            .target
            .as_deref()
            .ok_or_else(|| "Cronet source build target is not set".to_owned())?;
        if !native_target_supported(target) {
            return Err(format!("unsupported Cronet native target `{target}`"));
        }
        let source_dir = self
            .source_dir
            .clone()
            .unwrap_or_else(|| source_dir(target));
        let lib_dir = ensure_native_from_source_configured(
            &source_dir,
            target,
            self.linkage,
            PlatformConfig {
                ohos_sdk_native: self.ohos_sdk_native.as_deref(),
                clang_dir: self.clang_dir.as_deref(),
                rust_sysroot: self.rust_sysroot.as_deref(),
                rust_bindgen: self.rust_bindgen.as_deref(),
                android_ndk: self.android_ndk.as_deref(),
                android_api_level: self.android_api_level,
                ios_developer_dir: self.ios_developer_dir.as_deref(),
                ios_deployment_target: self.ios_deployment_target.as_deref(),
            },
        )?;
        Ok(Artifacts {
            source_dir,
            lib_dir,
            linkage: self.linkage,
        })
    }
}

/// Chooses an explicitly configured tree, a tree vendored in this source
/// package, or a persistent target-specific source cache.
#[must_use]
pub fn source_dir(target: &str) -> PathBuf {
    if let Some(source) = env::var_os("CRONET_SOURCE_DIR") {
        return PathBuf::from(source);
    }
    let bundled = Path::new(env!("CARGO_MANIFEST_DIR")).join("vendor/chromium/src");
    if bundled
        .join("components/cronet/native/include/cronet_c.h")
        .is_file()
    {
        return bundled;
    }
    source_cache_root()
        .join(CHROMIUM_REVISION)
        .join(target)
        .join("chromium/src")
}

fn source_cache_root() -> PathBuf {
    if let Some(path) = env::var_os("CRONET_CACHE_DIR") {
        return PathBuf::from(path).join("source");
    }
    if let Some(path) = env::var_os("CARGO_HOME") {
        return PathBuf::from(path).join("cronet-rs/source");
    }
    env::var_os("HOME").map_or_else(
        || env::temp_dir().join("cronet-rs/source"),
        |home| PathBuf::from(home).join(".cargo/cronet-rs/source"),
    )
}

impl NativeLinkage {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Dynamic => "dynamic",
            Self::Static => "static",
        }
    }

    const fn ninja_target(self) -> &'static str {
        match self {
            Self::Dynamic => "components/cronet:cronet",
            Self::Static => "components/cronet:cronet_static",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LinkageSelection {
    Dynamic,
    Static,
    Both,
}

impl LinkageSelection {
    const fn linkages(self) -> &'static [NativeLinkage] {
        match self {
            Self::Dynamic => &[NativeLinkage::Dynamic],
            Self::Static => &[NativeLinkage::Static],
            Self::Both => &[NativeLinkage::Dynamic, NativeLinkage::Static],
        }
    }
}

pub fn cli_main() {
    if let Err(error) = run() {
        eprintln!("error: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let mut args = env::args_os().skip(1);
    let command = args.next().and_then(|value| value.into_string().ok());
    let rest = args.collect::<Vec<_>>();
    match command.as_deref() {
        Some("sync") => sync(&rest),
        Some("build") => build(&rest),
        Some("vendor-source") => vendor_source(&rest),
        Some("doctor") => doctor(&rest),
        Some("print-env") => print_env(&rest),
        Some("audit-e2e") => audit_e2e(&rest),
        Some("help" | "-h" | "--help") | None => {
            usage();
            Ok(())
        }
        Some(other) => Err(format!(
            "unknown xtask command `{other}`; try `cargo xtask help`"
        )),
    }
}

fn usage() {
    println!(
        "cronet-rs workspace helper\n\n\
         usage:\n\
           cargo xtask sync [--api-only] [--target TARGET] [--source-dir PATH]\n\
           cargo xtask build [--release] [--linkage dynamic|static|both] [--target TARGET] [--source-dir PATH] [--gn-arg ARG]...\n\
           cargo xtask vendor-source [--source-dir PATH] [--output PATH]\n\
           cargo xtask doctor [--source-dir PATH]\n\
           cargo xtask print-env [--source-dir PATH]\n\
           cargo xtask audit-e2e\n\n\
         `sync` uses a blobless sparse checkout pinned immediately before the\n\
         upstream native API was deleted. `--api-only` fetches just the public\n\
         C API and is enough for cargo check with CRONET_SYS_NO_LINK=1."
    );
}

/// Fails when a public safe-binding function has no named runtime scenario.
///
/// The manifest intentionally lives outside the Rust sources. Adding an API
/// therefore requires an explicit test mapping in the same change instead of
/// silently inheriting a broad documentation claim.
fn audit_e2e(args: &[std::ffi::OsString]) -> Result<(), String> {
    if !args.is_empty() {
        return Err("audit-e2e does not accept options".to_owned());
    }
    let root = workspace_root();
    let expected = public_safe_api(&root)?;
    let manifest_path = root.join("tests/e2e-coverage.tsv");
    let manifest = fs::read_to_string(&manifest_path)
        .map_err(display_error("read the safe API E2E coverage manifest"))?;
    let mut mapped = BTreeMap::<String, String>::new();
    for (index, line) in manifest.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (api, scenario) = line.split_once('\t').ok_or_else(|| {
            format!(
                "{}:{} must contain API<TAB>SCENARIO",
                manifest_path.display(),
                index + 1
            )
        })?;
        if api.is_empty() || scenario.is_empty() || scenario.chars().any(char::is_whitespace) {
            return Err(format!(
                "{}:{} has an invalid API or scenario",
                manifest_path.display(),
                index + 1
            ));
        }
        if mapped.insert(api.to_owned(), scenario.to_owned()).is_some() {
            return Err(format!("duplicate E2E mapping for `{api}`"));
        }
    }

    let mapped_apis = mapped.keys().cloned().collect::<BTreeSet<_>>();
    let missing = expected
        .difference(&mapped_apis)
        .cloned()
        .collect::<Vec<_>>();
    let stale = mapped_apis
        .difference(&expected)
        .cloned()
        .collect::<Vec<_>>();
    if !missing.is_empty() || !stale.is_empty() {
        return Err(format!(
            "safe API E2E manifest is out of date\nmissing:\n  {}\nstale:\n  {}",
            missing.join("\n  "),
            stale.join("\n  ")
        ));
    }

    let mut test_sources = String::new();
    collect_test_sources(&root.join("tests"), &mut test_sources)?;
    collect_test_sources(&root.join("crates/tokio-cronet/tests"), &mut test_sources)?;
    let missing_scenarios = mapped
        .values()
        .filter(|scenario| !test_sources.contains(scenario.as_str()))
        .cloned()
        .collect::<BTreeSet<_>>();
    if !missing_scenarios.is_empty() {
        return Err(format!(
            "E2E manifest references scenarios absent from test sources:\n  {}",
            missing_scenarios
                .into_iter()
                .collect::<Vec<_>>()
                .join("\n  ")
        ));
    }
    println!(
        "E2E coverage manifest maps all {} public safe functions",
        expected.len()
    );
    Ok(())
}

fn public_safe_api(root: &Path) -> Result<BTreeSet<String>, String> {
    let source = root.join("crates/tokio-cronet/src");
    let mut api = BTreeSet::new();
    for file in [
        "bidirectional.rs",
        "dns.rs",
        "engine.rs",
        "nqe.rs",
        "request.rs",
        "sse.rs",
        "types.rs",
        "websocket.rs",
        "lib.rs",
    ] {
        let contents = fs::read_to_string(source.join(file))
            .map_err(display_error("read a safe-binding source file"))?;
        api.extend(public_functions_in_source(file, &contents));
    }
    Ok(api)
}

fn public_functions_in_source(file: &str, source: &str) -> BTreeSet<String> {
    let mut functions = BTreeSet::new();
    let mut inherent_impl = None::<String>;
    for line in source.lines() {
        if line.starts_with("impl ") && line.ends_with(" {") && !line.contains(" for ") {
            let candidate = line
                .trim_start_matches("impl ")
                .trim_end_matches(" {")
                .trim();
            if !candidate.contains(['<', '>', ' ']) {
                inherent_impl = Some(candidate.to_owned());
            }
        }
        if let Some(name) = public_function_name(line.trim()) {
            let qualified = if let Some(owner) = &inherent_impl {
                format!("{owner}::{name}")
            } else if file == "lib.rs" && line.starts_with("    ") {
                format!("android::{name}")
            } else {
                name.to_owned()
            };
            functions.insert(qualified);
        }
        if line == "}" {
            inherent_impl = None;
        }
    }
    functions
}

fn public_function_name(line: &str) -> Option<&str> {
    let mut remainder = line.strip_prefix("pub ")?;
    loop {
        let next = ["async ", "const ", "unsafe "]
            .into_iter()
            .find_map(|qualifier| remainder.strip_prefix(qualifier));
        let Some(next) = next else { break };
        remainder = next;
    }
    let remainder = remainder.strip_prefix("fn ")?;
    let end = remainder.find(['(', '<'])?;
    Some(&remainder[..end])
}

fn collect_test_sources(directory: &Path, output: &mut String) -> Result<(), String> {
    for entry in fs::read_dir(directory).map_err(display_error("list E2E test sources"))? {
        let entry = entry.map_err(display_error("read an E2E test source entry"))?;
        let path = entry.path();
        if path.is_dir() {
            if path.file_name().and_then(|name| name.to_str()) != Some("target") {
                collect_test_sources(&path, output)?;
            }
        } else if matches!(
            path.extension().and_then(|extension| extension.to_str()),
            Some("rs" | "sh" | "kt" | "swift" | "m")
        ) {
            output.push_str(
                &fs::read_to_string(&path).map_err(display_error("read an E2E test source"))?,
            );
        }
    }
    Ok(())
}

/// Ensures that a release Cronet library exists for `target`, synchronizing
/// only the pinned Cronet build closure and compiling it when necessary.
///
/// This entry point is used by `tokio-cronet-sys` for every linked build. The source
/// directory should be target-specific so libraries for different
/// architectures never overwrite each other.
pub fn ensure_native_from_source(
    source: &Path,
    target: &str,
    linkage: NativeLinkage,
) -> Result<PathBuf, String> {
    ensure_native_from_source_configured(source, target, linkage, PlatformConfig::default())
}

#[derive(Clone, Copy, Debug, Default)]
struct PlatformConfig<'a> {
    ohos_sdk_native: Option<&'a Path>,
    clang_dir: Option<&'a Path>,
    rust_sysroot: Option<&'a Path>,
    rust_bindgen: Option<&'a Path>,
    android_ndk: Option<&'a Path>,
    android_api_level: Option<u32>,
    ios_developer_dir: Option<&'a Path>,
    ios_deployment_target: Option<&'a str>,
}

fn ensure_native_from_source_configured(
    source: &Path,
    target: &str,
    linkage: NativeLinkage,
    platform: PlatformConfig<'_>,
) -> Result<PathBuf, String> {
    let source = absolute_path(source)?;
    let _source_lock = SourceOperationLock::acquire(&source)?;
    let header = source.join("components/cronet/native/include/cronet_c.h");
    let bidirectional_header =
        source.join("components/grpc_support/include/bidirectional_stream_c.h");
    let lib_dir = native_output_dir(&source, Some(target));
    if header.is_file()
        && bidirectional_header.is_file()
        && native_library_exists(&source, Some(target), linkage)
        && !native_extensions::requires_rebuild(&source, &lib_dir)
    {
        return Ok(lib_dir);
    }

    if !source_tree_buildable(&source) {
        sync_source(&source, false, Some(target))?;
    }
    build_native_unlocked(
        BuildOptions {
            common: CommonOptions {
                source_dir: source.clone(),
            },
            release: true,
            target: Some(target.to_owned()),
            linkage: match linkage {
                NativeLinkage::Dynamic => LinkageSelection::Dynamic,
                NativeLinkage::Static => LinkageSelection::Static,
            },
            gn_args: Vec::new(),
        },
        platform,
    )?;
    Ok(native_output_dir(&source, Some(target)))
}

fn source_tree_buildable(source: &Path) -> bool {
    source.join(".gn").is_file()
        && source
            .join("components/cronet/native/include/cronet_c.h")
            .is_file()
        && source
            .join("components/grpc_support/include/bidirectional_stream_c.h")
            .is_file()
        && source.join("third_party/boringssl/src/BUILD.gn").is_file()
        && source.join("build/util/LASTCHANGE").is_file()
        && host_gn(source).is_file()
        && host_ninja(source).is_file()
}

/// Returns whether the native build and release pipeline supports this Rust
/// target triple.
#[must_use]
pub fn native_target_supported(target: &str) -> bool {
    platform::resolve(Some(target)).is_ok()
}

fn sync(args: &[std::ffi::OsString]) -> Result<(), String> {
    let options = SyncOptions::parse(args)?;
    let source = absolute_path(&options.source_dir)?;
    let _source_lock = SourceOperationLock::acquire(&source)?;
    sync_source(&source, options.api_only, options.target.as_deref())
}

fn sync_source(source: &Path, api_only: bool, target: Option<&str>) -> Result<(), String> {
    let chromium_root = source
        .parent()
        .ok_or_else(|| "source directory needs a parent".to_owned())?;
    fs::create_dir_all(chromium_root).map_err(display_error("create Chromium directory"))?;

    init_or_update_sparse_checkout(source, api_only)?;
    if api_only {
        println!("Cronet C API synchronized at {}", source.display());
        return Ok(());
    }

    let depot_tools = depot_tools_dir(source)?;
    clone_or_update_depot_tools(&depot_tools)?;
    write_gclient(chromium_root, target)?;
    initialize_depot_tools_gsutil(&depot_tools)?;
    let gclient = depot_tools.join(if cfg!(windows) {
        "gclient.bat"
    } else {
        "gclient"
    });
    run_command(
        command_with_depot_tools(&gclient, &depot_tools)
            .current_dir(chromium_root)
            .args([
                "sync",
                "--no-history",
                "--nohooks",
                "--revision",
                &format!("src@{CHROMIUM_REVISION}"),
            ]),
        "synchronize Cronet's external build dependencies",
    )?;
    // A normal `gclient runhooks` also updates GPU, Skia, Dawn, telemetry,
    // WebRTC, and browser test assets. None are present in this sparse tree.
    // LASTCHANGE is the only unconditional generated input used by Cronet's GN
    // graph; toolchains themselves are pinned dependencies synchronized above.
    run_command(
        command_with_depot_tools(Path::new(host_python()), &depot_tools)
            .current_dir(chromium_root)
            .args([
                "src/build/util/lastchange.py",
                "-o",
                "src/build/util/LASTCHANGE",
            ]),
        "generate Chromium LASTCHANGE for the Cronet build",
    )?;
    println!(
        "Cronet source and build dependencies synchronized at {}",
        source.display()
    );
    Ok(())
}

fn build(args: &[std::ffi::OsString]) -> Result<(), String> {
    let mut options = BuildOptions::parse(args)?;
    options.common.source_dir = absolute_path(&options.common.source_dir)?;
    let _source_lock = SourceOperationLock::acquire(&options.common.source_dir)?;
    build_native_unlocked(options, PlatformConfig::default())
}

fn common_gn_args(release: bool) -> Vec<String> {
    vec![
        format!("is_debug={}", !release),
        "is_component_build=false".to_owned(),
        "is_cronet_build=true".to_owned(),
        // Cronet defaults this off (`use_blink && !is_cronet_build`). The
        // committed WebSocket wrapper needs `net::WebSocketChannel` in libnet.
        "enable_websockets=true".to_owned(),
        "enable_disk_cache_sql_backend=false".to_owned(),
        "enable_device_bound_sessions=false".to_owned(),
        "enable_perfetto_trace_processor_sqlite=false".to_owned(),
        "use_platform_icu_alternatives=false".to_owned(),
        // Standalone Cronet has no Chromium UI tree or host development
        // packages. Linux uses the non-GIO proxy fallback and Chromium's
        // built-in root store.
        "use_glib=false".to_owned(),
        "use_gio=false".to_owned(),
        "use_nss_certs=false".to_owned(),
        // Keep the build compatible with older Xcode SDKs that predate the
        // split DarwinFoundation{1,2,3}.modulemap files.
        "use_clang_modules=false".to_owned(),
        "use_remoteexec=false".to_owned(),
        "use_siso=false".to_owned(),
        "treat_warnings_as_errors=false".to_owned(),
        "symbol_level=1".to_owned(),
    ]
}

#[allow(clippy::too_many_lines)] // Native GN configuration and packaging form one atomic build transaction.
fn build_native_unlocked(options: BuildOptions, config: PlatformConfig<'_>) -> Result<(), String> {
    let source = source_path_for_external_tools(&options.common.source_dir)?;
    require_file(
        &source.join("components/cronet/native/include/cronet_c.h"),
        "run `cargo xtask sync` first",
    )?;
    let depot_tools = depot_tools_dir(&source)?;
    let gn = host_gn(&source);
    let ninja = host_ninja(&source);
    require_file(&gn, "run `cargo xtask sync` (without --api-only) first")?;
    require_file(&ninja, "run `cargo xtask sync` (without --api-only) first")?;

    let platform_build = platform::resolve(options.target.as_deref())?;
    let uses_external_rust = platform_build.needs_rustc_bootstrap(config);
    let out_dir = native_output_dir(&source, options.target.as_deref());
    let overlay = write_cronet_overlay(&source, platform_build.as_ref())?;
    let overlay_out_dir = native_output_dir(&overlay, options.target.as_deref());
    let mut gn_args = common_gn_args(options.release);
    gn_args.extend(platform_build.gn_args(&source, &overlay, config)?);
    gn_args.extend(options.gn_args);

    run_command(
        command_with_depot_tools(&gn, &depot_tools)
            .current_dir(&overlay)
            .arg("gen")
            .arg(&overlay_out_dir)
            .arg(format!("--root={}", overlay.display()))
            .arg(format!("--args={}", gn_args.join(" "))),
        "generate the Cronet Ninja build",
    )?;

    let mut ninja_command = command_with_depot_tools(&ninja, &depot_tools);
    ninja_command
        .current_dir(&overlay)
        // Cargo exports TARGET to build scripts, while Chromium's bindgen
        // wrapper deliberately rejects it because bindgen would otherwise
        // override the explicit target flags generated by GN.
        .env_remove("TARGET")
        .arg("-C")
        .arg(&overlay_out_dir);
    if uses_external_rust {
        // Chromium's external-Rust path still emits a small set of -Z build
        // flags even when rustc_nightly_capability is false. Scope the stable
        // compiler opt-in to this native build subprocess; never mutate the
        // caller's Cargo environment.
        ninja_command.env("RUSTC_BOOTSTRAP", "1");
    }
    for linkage in options.linkage.linkages() {
        ninja_command.arg(linkage.ninja_target());
    }
    if options.linkage.linkages().contains(&NativeLinkage::Static) {
        // A GN static_library records these runtime archives as final-link
        // inputs without making them build dependencies. We package them into
        // Cronet's complete archive, so request fresh target-ABI copies rather
        // than accidentally reusing artifacts from an older GN configuration.
        let extension = platform_build.static_archive_extension();
        ninja_command.arg(format!(
            "obj/buildtools/third_party/libc++/libc++.{extension}"
        ));
        ninja_command.arg(format!(
            "obj/buildtools/third_party/libc++abi/libc++abi.{extension}"
        ));
    }
    platform_build.configure_ninja(&mut ninja_command, &overlay, options.linkage.linkages())?;
    run_command(&mut ninja_command, "compile Cronet from source")?;
    platform_build.post_build(&overlay_out_dir, &out_dir)?;
    if options.linkage.linkages().contains(&NativeLinkage::Static) {
        let external_clang = config
            .clang_dir
            .map(Path::to_owned)
            .or_else(|| env::var_os(CLANG_DIR_ENV).map(PathBuf::from));
        bundle_static_archive(
            &source,
            &overlay_out_dir,
            &out_dir,
            external_clang.as_deref(),
            uses_external_rust,
        )?;
        write_static_link_manifest(
            &gn,
            &depot_tools,
            &overlay,
            &overlay_out_dir,
            &out_dir,
            options.target.as_deref(),
        )?;
    }
    println!(
        "Cronet {} library built in {}",
        options
            .linkage
            .linkages()
            .iter()
            .map(|linkage| linkage.as_str())
            .collect::<Vec<_>>()
            .join(" and "),
        out_dir.display()
    );
    print_env_for_output(&source, &out_dir, options.target.as_deref());
    Ok(())
}

#[allow(clippy::too_many_lines)] // Archive discovery, MRI assembly, and symbol isolation are one transaction.
fn bundle_static_archive(
    source: &Path,
    build_dir: &Path,
    output_dir: &Path,
    external_clang: Option<&Path>,
    uses_external_rust: bool,
) -> Result<(), String> {
    let raw_name = native_static_archive_name("cronet_static_raw");
    let bundled_name = native_static_archive_name("cronet_static");
    let raw_archive = build_dir.join(raw_name);
    require_file(&raw_archive, "build the Cronet static GN target first")?;

    let ninja_file = build_dir.join("obj/components/cronet/cronet_static.ninja");
    let ninja = fs::read_to_string(&ninja_file)
        .map_err(display_error("read Cronet static Ninja target"))?;
    let rust_archives = ninja
        .lines()
        .find_map(|line| line.strip_prefix("  rlibs = "))
        .ok_or_else(|| {
            format!(
                "{} does not contain the Cronet Rust archive list",
                ninja_file.display()
            )
        })?;
    let mut archives = vec![raw_archive];
    for relative in [
        "obj/buildtools/third_party/libc++/libc++.a",
        "obj/buildtools/third_party/libc++/libc++.lib",
        "obj/buildtools/third_party/libc++abi/libc++abi.a",
        "obj/buildtools/third_party/libc++abi/libc++abi.lib",
    ] {
        let archive = build_dir.join(relative);
        if archive.is_file() {
            archives.push(archive);
        }
    }
    for relative in rust_archives.split_ascii_whitespace() {
        if uses_external_rust && is_chromium_rust_allocator_shim(relative) {
            // An external Rust sysroot is also used by the final Cargo
            // artifact, so bundling Chromium's allocator shim would define
            // the same allocator ABI twice.
            continue;
        }
        let archive = build_dir.join(relative);
        require_file(&archive, "the GN Rust dependency must be built")?;
        archives.push(archive);
    }

    let temporary_name = format!(".{bundled_name}.{}", std::process::id());
    let temporary = build_dir.join(&temporary_name);
    if temporary.exists() {
        fs::remove_file(&temporary).map_err(display_error("replace static archive temporary"))?;
    }
    let llvm_ar = llvm_tool(source, external_clang, "llvm-ar");
    require_file(&llvm_ar, "run `cargo xtask sync` first")?;
    let mut child = Command::new(&llvm_ar)
        .arg("-M")
        .current_dir(build_dir)
        .stdin(Stdio::piped())
        .spawn()
        .map_err(|error| format!("could not start llvm-ar: {error}"))?;
    {
        let input = child
            .stdin
            .as_mut()
            .ok_or_else(|| "llvm-ar did not open its MRI input".to_owned())?;
        writeln!(input, "create {temporary_name}")
            .map_err(display_error("write llvm-ar MRI command"))?;
        for archive in &archives {
            let relative = archive.strip_prefix(build_dir).map_err(|_| {
                format!(
                    "static dependency {} is outside {}",
                    archive.display(),
                    build_dir.display()
                )
            })?;
            writeln!(input, "addlib {}", relative.display())
                .map_err(display_error("write llvm-ar MRI command"))?;
        }
        writeln!(input, "save\nend").map_err(display_error("write llvm-ar MRI command"))?;
    }
    let status = child
        .wait()
        .map_err(|error| format!("could not wait for llvm-ar: {error}"))?;
    check_status(status, "bundle the complete Cronet static archive")?;
    let bundled = output_dir.join(bundled_name);
    if bundled.exists() {
        fs::remove_file(&bundled).map_err(display_error("replace Cronet static archive"))?;
    }
    fs::rename(&temporary, &bundled).map_err(display_error("install Cronet static archive"))?;

    // A Rust application supplies its own panic personality. Chromium's
    // private Rust standard library exports the same C symbol from inside the
    // complete native archive; give that internal copy a private namespace so
    // the two toolchains can coexist in one final Rust link.
    let llvm_objcopy = llvm_tool(source, external_clang, "llvm-objcopy");
    require_file(&llvm_objcopy, "run `cargo xtask sync` first")?;
    check_status(
        Command::new(llvm_objcopy)
            .arg("--redefine-sym=rust_eh_personality=cronet_rs_chromium_rust_eh_personality")
            // Mach-O object symbols carry the platform C underscore in the
            // object table, unlike ELF/COFF. llvm-objcopy quietly ignores a
            // missing spelling, so applying both keeps the archive portable.
            .arg("--redefine-sym=_rust_eh_personality=_cronet_rs_chromium_rust_eh_personality")
            .arg(&bundled)
            .status()
            .map_err(display_error("start llvm-objcopy for the static archive"))?,
        "isolate Chromium's Rust panic personality",
    )
}

fn is_chromium_rust_allocator_shim(path: &str) -> bool {
    let path = path.replace('\\', "/");
    path.rsplit_once('/').is_some_and(|(directory, file)| {
        directory.ends_with("obj/build/rust/allocator")
            && file.starts_with("liballocator_")
            && Path::new(file)
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("rlib"))
    })
}

fn llvm_tool(source: &Path, external_clang: Option<&Path>, name: &str) -> PathBuf {
    let binary = if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_owned()
    };
    external_clang.map_or_else(
        || {
            source
                .join("third_party/llvm-build/Release+Asserts/bin")
                .join(&binary)
        },
        |directory| directory.join("bin").join(&binary),
    )
}

fn native_static_archive_name(stem: &str) -> String {
    if cfg!(windows) {
        format!("{stem}.lib")
    } else {
        format!("lib{stem}.a")
    }
}

fn write_static_link_manifest(
    gn: &Path,
    depot_tools: &Path,
    overlay: &Path,
    overlay_out_dir: &Path,
    output_dir: &Path,
    target: Option<&str>,
) -> Result<(), String> {
    let describe = |property: &str| {
        let mut command = command_with_depot_tools(gn, depot_tools);
        command
            .current_dir(overlay)
            .arg("desc")
            .arg(overlay_out_dir)
            .arg("//components/cronet:cronet_static")
            .arg(property);
        command_stdout(&mut command, "inspect Cronet static link requirements")
    };
    let libraries = describe("libs")?;
    let target_macos = target.map_or(cfg!(target_os = "macos"), |target| target.contains("apple"));
    let frameworks = if target_macos {
        describe("frameworks")?
    } else {
        String::new()
    };
    let mut manifest =
        String::from("# Generated from the pinned GN target; consumed by tokio-cronet-sys.\n");
    for library in libraries
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
    {
        if Path::new(library)
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("lds"))
        {
            // Cargo can propagate native libraries from a `links` crate, but
            // not a raw linker argument. Give lld the script through `-l`;
            // it recognizes a non-ELF input as a linker script and does not
            // add a DT_NEEDED entry for it.
            let source = overlay.join(library.trim_start_matches("//"));
            let name = "cronet_android_linker_script";
            let script = fs::read_to_string(&source)
                .map_err(display_error("read the Android linker script"))?;
            write_if_changed(
                &output_dir.join(format!("lib{name}.so")),
                script.as_bytes(),
                "install the Android linker script",
            )?;
            manifest.push_str("linker-script=");
            manifest.push_str(name);
            manifest.push('\n');
        } else {
            manifest.push_str("lib=");
            manifest.push_str(library);
            manifest.push('\n');
        }
    }
    for framework in frameworks
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
    {
        manifest.push_str("framework=");
        manifest.push_str(framework.strip_suffix(".framework").unwrap_or(framework));
        manifest.push('\n');
    }
    write_if_changed(
        &output_dir.join("cronet-static-link.txt"),
        manifest.as_bytes(),
        "write Cronet static link manifest",
    )
}

fn vendor_source(args: &[std::ffi::OsString]) -> Result<(), String> {
    let mut options = VendorOptions::parse(args)?;
    options.source_dir = absolute_path(&options.source_dir)?;
    let _source_lock = SourceOperationLock::acquire(&options.source_dir)?;
    if !source_tree_buildable(&options.source_dir) {
        sync_source(&options.source_dir, false, None)?;
    }
    if options.output.exists() {
        return Err(format!(
            "vendored source destination {} already exists",
            options.output.display()
        ));
    }
    let source = options
        .source_dir
        .canonicalize()
        .map_err(display_error("resolve source tree for vendoring"))?;
    let output = absolute_path(&options.output)?;
    if output.starts_with(&source) {
        return Err("vendored source destination cannot be inside the source tree".to_owned());
    }
    let parent = output
        .parent()
        .ok_or_else(|| "vendored source destination has no parent".to_owned())?;
    fs::create_dir_all(parent).map_err(display_error("create vendored source parent"))?;
    let temporary = parent.join(format!(".cronet-source-{}", std::process::id()));
    if temporary.exists() {
        fs::remove_dir_all(&temporary)
            .map_err(display_error("remove stale vendored source temporary"))?;
    }
    fs::create_dir_all(&temporary).map_err(display_error("create vendored source temporary"))?;
    if let Err(error) = copy_source_tree(&source, &temporary) {
        let _ = fs::remove_dir_all(&temporary);
        return Err(error);
    }
    fs::write(
        temporary.join(".cronet-source"),
        format!("chromium_revision={CHROMIUM_REVISION}\nchromium_version={CHROMIUM_VERSION}\n"),
    )
    .map_err(display_error("write vendored source marker"))?;
    fs::rename(&temporary, &output).map_err(display_error("install vendored source tree"))?;
    println!("Vendored Cronet source written to {}", output.display());
    Ok(())
}

fn absolute_path(path: &Path) -> Result<PathBuf, String> {
    if path.is_absolute() {
        Ok(path.to_owned())
    } else {
        env::current_dir()
            .map(|current| current.join(path))
            .map_err(display_error("resolve current directory"))
    }
}

fn copy_source_tree(source: &Path, destination: &Path) -> Result<(), String> {
    for entry in fs::read_dir(source).map_err(display_error("read source tree"))? {
        let entry = entry.map_err(display_error("read source tree entry"))?;
        let name = entry.file_name();
        let name_text = name.to_string_lossy();
        if matches!(name_text.as_ref(), ".git" | "out" | "__pycache__")
            || name_text.ends_with(".pyc")
        {
            continue;
        }
        copy_source_entry(&entry.path(), &destination.join(name))?;
    }
    Ok(())
}

fn copy_source_entry(source: &Path, destination: &Path) -> Result<(), String> {
    let metadata = fs::symlink_metadata(source).map_err(display_error("inspect source entry"))?;
    if metadata.file_type().is_symlink() {
        let target = fs::read_link(source).map_err(display_error("read source symlink"))?;
        create_source_symlink(source, &target, destination)
            .map_err(display_error("copy source symlink"))?;
    } else if metadata.is_dir() {
        fs::create_dir(destination).map_err(display_error("create source directory"))?;
        copy_source_tree(source, destination)?;
    } else if metadata.is_file() {
        fs::copy(source, destination).map_err(display_error("copy source file"))?;
    }
    Ok(())
}

fn is_native_library_output(name: &str, linkage: NativeLinkage) -> bool {
    let extension = Path::new(name).extension().and_then(|value| value.to_str());
    match linkage {
        NativeLinkage::Dynamic => {
            let is_cronet = name.starts_with("libcronet.") || name.starts_with("cronet.");
            is_cronet
                && (name.contains(".so.")
                    || extension.is_some_and(|value| {
                        ["so", "dylib", "dll", "lib", "pdb"]
                            .iter()
                            .any(|expected| value.eq_ignore_ascii_case(expected))
                    }))
        }
        NativeLinkage::Static => {
            (name == "libcronet_static.a" || name == "cronet_static.lib")
                && extension.is_some_and(|value| {
                    ["a", "lib"]
                        .iter()
                        .any(|expected| value.eq_ignore_ascii_case(expected))
                })
        }
    }
}

pub(crate) fn rust_stdlib_adjustments(
    rust_sysroot: &Path,
    target: &str,
) -> Result<(Vec<String>, Vec<String>), String> {
    // Chromium names every rlib that must be handed to the C++ linker. Rust
    // occasionally adds or renames internal std crates, so derive the delta
    // from the selected toolchain instead of binding tokio-cronet-src to one rustc.
    const CHROMIUM_STDLIBS: &[&str] = &[
        "std",
        "alloc",
        "cfg_if",
        "compiler_builtins",
        "core",
        "getopts",
        "hashbrown",
        "panic_abort",
        "panic_unwind",
        "rustc_demangle",
        "std_detect",
        "test",
        "unwind",
        "addr2line",
        "gimli",
        "libc",
        "memchr",
        "miniz_oxide",
        "object",
        "adler",
    ];
    const CONDITIONALLY_COPIED: &[&str] = &[
        "proc_macro",
        "profiler_builtins",
        "rustc_std_workspace_alloc",
        "rustc_std_workspace_core",
        "rustc_std_workspace_std",
    ];

    let directory = rust_sysroot.join("lib/rustlib").join(target).join("lib");
    let mut actual = BTreeSet::new();
    for entry in fs::read_dir(&directory).map_err(display_error("read the Rust target stdlib"))? {
        let name = entry
            .map_err(display_error("read a Rust target stdlib entry"))?
            .file_name();
        let name = name.to_string_lossy();
        let Some(name) = name
            .strip_prefix("lib")
            .and_then(|name| name.strip_suffix(".rlib"))
        else {
            continue;
        };
        if let Some((crate_name, hash)) = name.rsplit_once('-') {
            if !crate_name.is_empty() && hash.chars().all(|character| character.is_ascii_hexdigit())
            {
                actual.insert(crate_name.to_owned());
            }
        }
    }
    let expected = CHROMIUM_STDLIBS
        .iter()
        .map(|name| (*name).to_owned())
        .collect::<BTreeSet<_>>();
    let excluded = CONDITIONALLY_COPIED
        .iter()
        .map(|name| (*name).to_owned())
        .collect::<BTreeSet<_>>();
    let removed = expected.difference(&actual).cloned().collect();
    let added = actual
        .difference(&expected)
        .filter(|name| !excluded.contains(*name))
        .cloned()
        .collect();
    Ok((removed, added))
}

pub(crate) fn gn_string_path(name: &str, path: &Path) -> String {
    format!(
        "{name}=\"{}\"",
        escape_gn_string(&path.to_string_lossy().replace('\\', "/"))
    )
}

pub(crate) fn gn_string_list(name: &str, values: &[String]) -> String {
    let values = values
        .iter()
        .map(|value| format!("\"{}\"", escape_gn_string(value)))
        .collect::<Vec<_>>()
        .join(",");
    format!("{name}=[{values}]")
}

pub(crate) fn escape_gn_string(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

fn doctor(args: &[std::ffi::OsString]) -> Result<(), String> {
    let options = CommonOptions::parse(args)?;
    let source = options.source_dir;
    let checks = [
        ("git", command_exists("git")),
        ("clang", command_exists("clang")),
        (
            "Cronet header",
            source
                .join("components/cronet/native/include/cronet_c.h")
                .is_file(),
        ),
        (
            "pinned Chromium revision",
            git_output(&source, &["rev-parse", "HEAD"])
                .is_some_and(|value| value.trim() == CHROMIUM_REVISION),
        ),
        (
            "depot_tools",
            depot_tools_dir(&source).is_ok_and(|path| path.join("gn").exists()),
        ),
        (
            "Cronet dynamic library",
            native_library_exists(&source, None, NativeLinkage::Dynamic),
        ),
        (
            "Cronet static library",
            native_library_exists(&source, None, NativeLinkage::Static),
        ),
    ];
    let mut ok = true;
    for (name, present) in checks {
        println!("{:<28} {}", name, if present { "ok" } else { "missing" });
        ok &= present;
    }
    if ok {
        Ok(())
    } else {
        Err("one or more prerequisites are missing".to_owned())
    }
}

fn print_env(args: &[std::ffi::OsString]) -> Result<(), String> {
    let options = CommonOptions::parse(args)?;
    print_env_for(&options.source_dir);
    Ok(())
}

fn print_env_for(source: &Path) {
    print_env_for_output(source, &source.join("out/cronet-rs"), None);
}

fn print_env_for_output(source: &Path, lib_dir: &Path, target: Option<&str>) {
    let target_windows = target.map_or(cfg!(windows), |target| target.contains("windows"));
    if target_windows {
        println!("set CRONET_SOURCE_DIR={}", source.display());
        println!("set CRONET_LIB_DIR={}", lib_dir.display());
        println!("set PATH={};%PATH%", lib_dir.display());
        return;
    }

    println!("export CRONET_SOURCE_DIR={}", shell_quote(source));
    println!("export CRONET_LIB_DIR={}", shell_quote(lib_dir));
    let target_macos = target.map_or(cfg!(target_os = "macos"), |target| target.contains("apple"));
    let runtime_variable = if target_macos {
        "DYLD_LIBRARY_PATH"
    } else {
        "LD_LIBRARY_PATH"
    };
    println!(
        "export {runtime_variable}={}${{{runtime_variable}:+:${runtime_variable}}}",
        shell_quote(lib_dir)
    );
}

fn shell_quote(path: &Path) -> String {
    format!("'{}'", path.to_string_lossy().replace('\'', "'\"'\"'"))
}

struct CommonOptions {
    source_dir: PathBuf,
}

struct SyncOptions {
    source_dir: PathBuf,
    api_only: bool,
    target: Option<String>,
}

impl SyncOptions {
    fn parse(args: &[std::ffi::OsString]) -> Result<Self, String> {
        let mut source_dir = default_source_dir();
        let mut api_only = false;
        let mut target = None;
        let mut index = 0;
        while index < args.len() {
            match args[index].to_str() {
                Some("--source-dir") => {
                    index += 1;
                    source_dir = args
                        .get(index)
                        .map(PathBuf::from)
                        .ok_or_else(|| "--source-dir requires a path".to_owned())?;
                }
                Some("--api-only") => api_only = true,
                Some("--target") => {
                    index += 1;
                    let value = args
                        .get(index)
                        .and_then(|value| value.to_str())
                        .ok_or_else(|| "--target requires a UTF-8 target triple".to_owned())?;
                    if !native_target_supported(value) {
                        return Err(format!("unsupported Cronet native target `{value}`"));
                    }
                    target = Some(value.to_owned());
                }
                Some(value) => return Err(format!("unexpected option `{value}`")),
                None => return Err("arguments must be valid UTF-8".to_owned()),
            }
            index += 1;
        }
        Ok(Self {
            source_dir,
            api_only,
            target,
        })
    }
}

impl CommonOptions {
    fn parse(args: &[std::ffi::OsString]) -> Result<Self, String> {
        let mut source_dir = default_source_dir();
        let mut index = 0;
        while index < args.len() {
            match args[index].to_str() {
                Some("--source-dir") => {
                    index += 1;
                    source_dir = args
                        .get(index)
                        .map(PathBuf::from)
                        .ok_or_else(|| "--source-dir requires a path".to_owned())?;
                }
                Some(value) => return Err(format!("unexpected option `{value}`")),
                None => return Err("arguments must be valid UTF-8".to_owned()),
            }
            index += 1;
        }
        Ok(Self { source_dir })
    }
}

struct BuildOptions {
    common: CommonOptions,
    release: bool,
    target: Option<String>,
    linkage: LinkageSelection,
    gn_args: Vec<String>,
}

impl BuildOptions {
    fn parse(args: &[std::ffi::OsString]) -> Result<Self, String> {
        let mut source_dir = default_source_dir();
        let mut release = false;
        let mut target = None;
        let mut linkage = LinkageSelection::Dynamic;
        let mut gn_args = Vec::new();
        let mut index = 0;
        while index < args.len() {
            match args[index].to_str() {
                Some("--source-dir") => {
                    index += 1;
                    source_dir = args
                        .get(index)
                        .map(PathBuf::from)
                        .ok_or_else(|| "--source-dir requires a path".to_owned())?;
                }
                Some("--release") => release = true,
                Some("--target") => {
                    index += 1;
                    target = Some(
                        args.get(index)
                            .and_then(|value| value.to_str())
                            .ok_or_else(|| "--target requires a UTF-8 target triple".to_owned())?
                            .to_owned(),
                    );
                }
                Some("--linkage") => {
                    index += 1;
                    linkage = match args.get(index).and_then(|value| value.to_str()) {
                        Some("dynamic") => LinkageSelection::Dynamic,
                        Some("static") => LinkageSelection::Static,
                        Some("both") => LinkageSelection::Both,
                        Some(value) => {
                            return Err(format!(
                                "--linkage must be dynamic, static, or both; got `{value}`"
                            ));
                        }
                        None => {
                            return Err("--linkage requires dynamic, static, or both".to_owned());
                        }
                    };
                }
                Some("--gn-arg") => {
                    index += 1;
                    gn_args.push(
                        args.get(index)
                            .and_then(|value| value.to_str())
                            .ok_or_else(|| "--gn-arg requires a UTF-8 value".to_owned())?
                            .to_owned(),
                    );
                }
                Some(value) => return Err(format!("unexpected option `{value}`")),
                None => return Err("arguments must be valid UTF-8".to_owned()),
            }
            index += 1;
        }
        Ok(Self {
            common: CommonOptions { source_dir },
            release,
            target,
            linkage,
            gn_args,
        })
    }
}

struct VendorOptions {
    source_dir: PathBuf,
    output: PathBuf,
}

impl VendorOptions {
    fn parse(args: &[std::ffi::OsString]) -> Result<Self, String> {
        let mut source_dir = default_source_dir();
        let mut output = Path::new(env!("CARGO_MANIFEST_DIR")).join("vendor/chromium/src");
        let mut index = 0;
        while index < args.len() {
            match args[index].to_str() {
                Some("--source-dir") => {
                    index += 1;
                    source_dir = args
                        .get(index)
                        .map(PathBuf::from)
                        .ok_or_else(|| "--source-dir requires a path".to_owned())?;
                }
                Some("--output") => {
                    index += 1;
                    output = args
                        .get(index)
                        .map(PathBuf::from)
                        .ok_or_else(|| "--output requires a path".to_owned())?;
                }
                Some(value) => return Err(format!("unexpected option `{value}`")),
                None => return Err("arguments must be valid UTF-8".to_owned()),
            }
            index += 1;
        }
        Ok(Self { source_dir, output })
    }
}

pub(crate) fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask must be inside the workspace")
        .to_owned()
}

fn default_source_dir() -> PathBuf {
    env::var_os("CRONET_SOURCE_DIR").map_or_else(
        || workspace_root().join(".cronet/chromium/src"),
        PathBuf::from,
    )
}

fn depot_tools_dir(source: &Path) -> Result<PathBuf, String> {
    source_operation_root(source).map(|root| root.join("depot_tools"))
}

fn source_operation_root(source: &Path) -> Result<&Path, String> {
    source.parent().and_then(Path::parent).ok_or_else(|| {
        format!(
            "source directory {} must use a ROOT/chromium/src layout",
            source.display()
        )
    })
}

struct SourceOperationLock {
    file: fs::File,
}

impl SourceOperationLock {
    fn acquire(source: &Path) -> Result<Self, String> {
        let root = source_operation_root(source)?;
        fs::create_dir_all(root).map_err(display_error("create Cronet source cache root"))?;
        let path = root.join(".tokio-cronet.lock");
        let file = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&path)
            .map_err(display_error("open Cronet source cache lock"))?;
        match FileExt::try_lock_exclusive(&file) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                println!(
                    "==> wait for another Cronet source operation using {}",
                    root.display()
                );
                FileExt::lock_exclusive(&file)
                    .map_err(display_error("wait for Cronet source cache lock"))?;
            }
            Err(error) => {
                return Err(format!(
                    "could not lock Cronet source cache {}: {error}",
                    path.display()
                ));
            }
        }
        Ok(Self { file })
    }
}

impl Drop for SourceOperationLock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
    }
}

fn host_gn(source: &Path) -> PathBuf {
    let platform = if cfg!(target_os = "windows") {
        "win"
    } else if cfg!(target_os = "macos") {
        "mac"
    } else {
        "linux64"
    };
    source
        .join("buildtools")
        .join(platform)
        .join(if cfg!(windows) { "gn.exe" } else { "gn" })
}

fn host_ninja(source: &Path) -> PathBuf {
    source
        .join("third_party/ninja")
        .join(if cfg!(windows) { "ninja.exe" } else { "ninja" })
}

fn host_python() -> &'static str {
    if cfg!(windows) { "python" } else { "python3" }
}

fn init_or_update_sparse_checkout(source: &Path, api_only: bool) -> Result<(), String> {
    if !source.join(".git").exists() {
        fs::create_dir_all(source).map_err(display_error("create source directory"))?;
        run_command(
            Command::new("git").current_dir(source).arg("init"),
            "initialize Chromium checkout",
        )?;
        run_command(
            Command::new("git")
                .current_dir(source)
                .args(["remote", "add", "origin", CHROMIUM_URL]),
            "configure Chromium remote",
        )?;
        run_command(
            Command::new("git").current_dir(source).args([
                "config",
                "remote.origin.promisor",
                "true",
            ]),
            "enable partial clone",
        )?;
        run_command(
            Command::new("git").current_dir(source).args([
                "config",
                "remote.origin.partialclonefilter",
                "blob:none",
            ]),
            "configure partial clone filter",
        )?;
        run_command(
            Command::new("git")
                .current_dir(source)
                .args(["sparse-checkout", "init", "--no-cone"]),
            "initialize sparse checkout",
        )?;
    }

    let sparse_file = source.join(".git/info/sparse-checkout");
    let patterns = if api_only {
        API_SPARSE_PATTERNS
    } else {
        BUILD_SPARSE_PATTERNS
    };
    let manifest_changed =
        fs::read_to_string(&sparse_file).map_or(true, |current| current != patterns);
    if manifest_changed {
        fs::write(&sparse_file, patterns)
            .map_err(display_error("write sparse-checkout manifest"))?;
    }

    let already_pinned = git_output(source, &["rev-parse", "HEAD"])
        .is_some_and(|value| value.trim() == CHROMIUM_REVISION);
    if !already_pinned {
        run_command(
            Command::new("git").current_dir(source).args([
                "fetch",
                "--depth=1",
                "--filter=blob:none",
                "origin",
                CHROMIUM_REVISION,
            ]),
            "fetch pinned Chromium tree",
        )?;
        run_command(
            Command::new("git")
                .current_dir(source)
                .args(["checkout", "--detach", "FETCH_HEAD"]),
            "check out pinned Cronet source",
        )?;
    }
    if manifest_changed || !already_pinned {
        run_command(
            Command::new("git")
                .current_dir(source)
                .args(["sparse-checkout", "reapply"]),
            "apply Cronet sparse-checkout manifest",
        )?;
    }
    Ok(())
}

fn clone_or_update_depot_tools(path: &Path) -> Result<(), String> {
    if path.join(".git").exists() {
        return run_command(
            Command::new("git")
                .current_dir(path)
                .args(["pull", "--ff-only"]),
            "update depot_tools",
        );
    }
    fs::create_dir_all(path.parent().unwrap()).map_err(display_error("create tool directory"))?;
    run_command(
        Command::new("git")
            .args(["clone", "--depth=1", "--filter=blob:none", DEPOT_TOOLS_URL])
            .arg(path),
        "clone depot_tools",
    )
}

fn initialize_depot_tools_gsutil(depot_tools: &Path) -> Result<(), String> {
    let gsutil = depot_tools.join("gsutil.py");
    if !gsutil.is_file() {
        return Err(format!(
            "depot_tools gsutil launcher is missing at {}",
            gsutil.display()
        ));
    }
    run_command(
        command_with_depot_tools(Path::new(host_python()), depot_tools)
            .current_dir(depot_tools)
            .arg(gsutil)
            .arg("version"),
        "initialize depot_tools gsutil before parallel dependency downloads",
    )
}

fn write_gclient(chromium_root: &Path, target: Option<&str>) -> Result<(), String> {
    let mut filter = Command::new(host_python());
    filter
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("filter_deps.py"))
        .args(["--unconditional", "src/third_party/lss"])
        .arg(chromium_root.join("src/DEPS"))
        .arg(chromium_root.join("src/DEPS.cronet"));
    for rule in CRONET_DEPENDENCY_RULES {
        filter.arg(rule);
    }
    run_command(&mut filter, "generate the Cronet-only dependency manifest")?;
    platform::android::patch_android_clang_dependency(
        &chromium_root.join("src/DEPS.cronet"),
        target,
    )?;

    let target_os = match target.and_then(platform::kind) {
        Some(platform::PlatformKind::Android) => "['android']",
        Some(platform::PlatformKind::Ios) => "['ios']",
        _ => "[]",
    };
    let contents = format!(
        "solutions = [{{\n\
         \x20 'name': 'src',\n\
         \x20 'url': '{CHROMIUM_URL}',\n\
         \x20 'managed': False,\n\
         \x20 'deps_file': 'DEPS.cronet',\n\
         \x20 'custom_deps': {{}},\n\
         \x20 'custom_vars': {{\n\
         \x20   'checkout_pgo_profiles': False,\n\
         \x20   'checkout_openxr': False,\n\
         \x20   'checkout_telemetry_dependencies': False,\n\
         \x20   'checkout_wpr_archives': False,\n\
         \x20 }},\n\
         }}]\n\
         target_os = {target_os}\n\
         cache_dir = None\n"
    );
    fs::write(chromium_root.join(".gclient"), contents).map_err(display_error("write .gclient"))
}

fn write_cronet_overlay(
    source: &Path,
    platform: &dyn platform::PlatformBuild,
) -> Result<PathBuf, String> {
    let source = source_path_for_external_tools(source)?;
    let overlay = source
        .parent()
        .expect("Chromium src directory must have a parent")
        .join("cronet-gn-root");
    let platform_marker = overlay.join(".cronet-rs-platform");
    let platform_key = platform.cache_key();
    if overlay.exists()
        && fs::read_to_string(&platform_marker).is_ok_and(|current| current.trim() != platform_key)
    {
        // Platform overlays have intentionally different source selections.
        // Persistent target-specific Ninja output is outside this generated
        // directory, so replacing the overlay when the target changes is safe.
        fs::remove_dir_all(&overlay).map_err(display_error("replace Cronet GN overlay"))?;
    }
    fs::create_dir_all(&overlay).map_err(display_error("create Cronet GN overlay"))?;
    write_if_changed(
        &platform_marker,
        format!("{platform_key}\n").as_bytes(),
        "write the Cronet overlay platform marker",
    )?;
    ensure_symlink(&source.join(".gn"), &overlay.join(".gn"))?;

    for entry in fs::read_dir(&source).map_err(display_error("list Chromium source directory"))? {
        let entry = entry.map_err(display_error("read Chromium source entry"))?;
        let name = entry.file_name();
        let name_text = name.to_string_lossy();
        if matches!(
            name_text.as_ref(),
            ".git"
                | ".gn"
                | "BUILD.gn"
                | "DEPS.cronet"
                | "base"
                | "build"
                | "buildtools"
                | "components"
                | "crypto"
                | "net"
                | "out"
                | "testing"
                | "third_party"
                | "url"
        ) {
            continue;
        }
        ensure_symlink(&entry.path(), &overlay.join(name))?;
    }
    let source_out = source.join("out");
    fs::create_dir_all(&source_out).map_err(display_error("create Chromium output directory"))?;
    ensure_symlink(&source_out, &overlay.join("out"))?;
    let filter_tests = true;
    write_build_overlay(&source, &overlay, filter_tests)?;
    write_buildtools_overlay(&source, &overlay)?;
    for directory in ["base", "crypto", "net", "url"] {
        write_test_filtered_directory(
            &source.join(directory),
            &overlay.join(directory),
            filter_tests,
        )?;
    }
    write_cronet_component_overlay(&source, &overlay, filter_tests)?;
    write_testing_overlay(&source, &overlay)?;
    write_third_party_overlay(&source, &overlay, platform.filter_third_party_tests())?;
    write_cxx_libcxx_compat_overlay(&source, &overlay)?;
    overlay_files::install(&source, &overlay, overlay_files::COMMON)?;
    platform.prepare_overlay(&source, &overlay)?;
    Ok(overlay)
}

fn source_path_for_external_tools(source: &Path) -> Result<PathBuf, String> {
    #[cfg(windows)]
    {
        // `canonicalize` produces a `\\?\` extended-length path on Windows.
        // Rust can consume it, but GN interprets the prefix as `/?/` when it
        // parses a command-line path. `absolute` keeps the ordinary drive form.
        std::path::absolute(source).map_err(display_error("resolve Chromium source directory"))
    }
    #[cfg(not(windows))]
    {
        source
            .canonicalize()
            .map_err(display_error("resolve Chromium source directory"))
    }
}

fn write_third_party_overlay(
    source: &Path,
    overlay: &Path,
    filter_tests: bool,
) -> Result<(), String> {
    let source_third_party = source.join("third_party");
    let overlay_third_party = overlay.join("third_party");
    replace_generated_link_with_directory(&overlay_third_party)?;
    for entry in fs::read_dir(&source_third_party)
        .map_err(display_error("list Chromium third_party directory"))?
    {
        let entry = entry.map_err(display_error("read Chromium third_party entry"))?;
        let destination = overlay_third_party.join(entry.file_name());
        if entry.file_name() == "BUILD.gn" {
            continue;
        }
        if entry.file_name() == "rust" && destination.is_dir() {
            // The cxx/libc++ compatibility overlay materializes this symlink
            // after the first generation. Its contents are refreshed below.
            continue;
        }
        if entry.path().join("BUILD.gn").is_file() {
            let preserve_android_java_graph = !filter_tests
                && matches!(
                    entry.file_name().to_str(),
                    Some(
                        "android_build_tools"
                            | "android_deps"
                            | "androidx"
                            | "aosp_dalvik"
                            | "byte_buddy"
                            | "google-truth"
                            | "hamcrest"
                            | "icu4j"
                            | "junit"
                            | "mockito"
                            | "sqlite4java"
                    )
                );
            write_test_filtered_directory(
                &entry.path(),
                &destination,
                !preserve_android_java_graph,
            )?;
        } else {
            ensure_symlink(&entry.path(), &destination)?;
        }
    }
    let root_build = source_third_party.join("BUILD.gn");
    if root_build.is_file() {
        let build = fs::read_to_string(root_build)
            .map_err(display_error("read upstream third_party/BUILD.gn"))?;
        fs::write(
            overlay_third_party.join("BUILD.gn"),
            remove_testonly_gn_blocks(build),
        )
        .map_err(display_error("write test-filtered third_party/BUILD.gn"))?;
    }
    Ok(())
}

fn write_cxx_libcxx_compat_overlay(source: &Path, overlay: &Path) -> Result<(), String> {
    const DIRECTORIES: &[&str] = &["rust", "chromium_crates_io", "vendor", "cxx-v1", "include"];

    let mut source_directory = source.join("third_party");
    let mut overlay_directory = overlay.join("third_party");
    for (index, component) in DIRECTORIES.iter().enumerate() {
        source_directory.push(component);
        overlay_directory.push(component);
        replace_generated_link_with_directory(&overlay_directory)?;
        let excluded = DIRECTORIES.get(index + 1).copied().unwrap_or("cxx.h");
        for entry in fs::read_dir(&source_directory).map_err(display_error(
            "list the cxx compatibility overlay directory",
        ))? {
            let entry = entry.map_err(display_error("read a cxx compatibility overlay entry"))?;
            if entry.file_name() != excluded {
                ensure_symlink(&entry.path(), &overlay_directory.join(entry.file_name()))?;
            }
        }
    }
    Ok(())
}

#[allow(clippy::too_many_lines)] // Filtering and exact GN compatibility rewrites are intentionally atomic.
pub(crate) fn write_test_filtered_directory(
    source: &Path,
    overlay: &Path,
    filter_tests: bool,
) -> Result<(), String> {
    const UNUSED_TRACE_PROCESSOR_TYPE: &str = "  if (!is_win) {\n    libtrace_processor_target_type = \"source_set\"\n  } else {\n    libtrace_processor_target_type = \"component\"\n  }\n\n";

    replace_generated_link_with_directory(overlay)?;
    for entry in fs::read_dir(source).map_err(|error| {
        format!(
            "failed to list Chromium subtree {}: {error}",
            source.display()
        )
    })? {
        let entry = entry.map_err(display_error("read Chromium source subtree entry"))?;
        if entry.file_name() == "BUILD.gn"
            || (source.file_name().is_some_and(|name| name == "base")
                && source.join("test").is_dir()
                && entry.file_name() == "test")
        {
            continue;
        }
        let destination = overlay.join(entry.file_name());
        if entry.path().is_dir() {
            write_test_filtered_directory(&entry.path(), &destination, filter_tests)?;
        } else {
            ensure_symlink(&entry.path(), &destination)?;
        }
    }
    if source.file_name().is_some_and(|name| name == "base") && source.join("test").is_dir() {
        write_test_filtered_directory(&source.join("test"), &overlay.join("test"), filter_tests)?;
    }
    let build_path = source.join("BUILD.gn");
    if !build_path.is_file() {
        return Ok(());
    }
    let build = fs::read_to_string(&build_path).map_err(display_error("read upstream BUILD.gn"))?;
    let mut build = if !filter_tests || source.ends_with("third_party/googletest") {
        build
    } else {
        remove_testonly_gn_blocks(build)
    };
    if filter_tests && source.ends_with("base/test") {
        const UNUSED_TEST_TRACE_PROCESSOR_TYPE: &str = "_target_type = \"shared_library\"\nif (is_ios) {\n  _target_type = \"ios_framework_bundle\"\n}\n\n";
        if !build.contains(UNUSED_TEST_TRACE_PROCESSOR_TYPE) {
            return Err(
                "upstream base/test BUILD.gn changed around its trace processor target type"
                    .to_owned(),
            );
        }
        build = build.replacen(UNUSED_TEST_TRACE_PROCESSOR_TYPE, "", 1);
    }
    if source.ends_with("build/android") {
        build = remove_named_gn_blocks(build, "python_library", "resource_sizes_py")?;
        build = remove_named_gn_blocks(build, "group", "stack_tools")?;
    }
    if source.file_name().is_some_and(|name| name == "net") {
        const UNUSED_TEST_TYPE: &str = "if (is_cronet_build) {\n  _test_target_type = \"cronet_test\"\n} else {\n  _test_target_type = \"test\"\n}\n\n";
        if filter_tests {
            if !build.contains(UNUSED_TEST_TYPE) {
                return Err(
                    "upstream net/BUILD.gn changed around its unit-test target type".to_owned(),
                );
            }
            build = build.replacen(UNUSED_TEST_TYPE, "", 1);
        }
        build = remove_named_gn_blocks(build, "component", "extras")?;
        build = remove_named_gn_blocks(build, "component", "shared_dictionary_info")?;
    }
    if source.ends_with("net/third_party/quiche") {
        build = remove_named_gn_blocks(build, "component", "blind_sign_auth")?;
        build = remove_named_gn_blocks(build, "proto_library", "blind_sign_auth_proto")?;
    }
    if source.ends_with("components/metrics") {
        // Upstream's iOS product build enables the full metrics component,
        // which pulls Mojo, UI and Chrome-only reporting code into GN even
        // though native Cronet consumes only `library_support`. Preserve the
        // normal `is_cronet_build` pruning for this standalone C API build.
        const IOS_PRODUCT_METRICS: &str = "if (!is_cronet_build || is_ios) {";
        if !build.contains(IOS_PRODUCT_METRICS) {
            return Err(
                "upstream metrics BUILD.gn changed around its iOS product graph".to_owned(),
            );
        }
        build = build.replacen(IOS_PRODUCT_METRICS, "if (!is_cronet_build) {", 1);
    }
    if source.ends_with("third_party/perfetto") {
        build = remove_gn_block_with_header(
            build,
            "target(libtrace_processor_target_type, \"libtrace_processor\")",
        )?;
        if !build.contains(UNUSED_TRACE_PROCESSOR_TYPE) {
            return Err(
                "upstream Perfetto BUILD.gn changed around its trace processor target type"
                    .to_owned(),
            );
        }
        build = build.replacen(UNUSED_TRACE_PROCESSOR_TYPE, "", 1);
    }
    fs::write(overlay.join("BUILD.gn"), build)
        .map_err(display_error("write test-filtered BUILD.gn"))
}

fn write_testing_overlay(source: &Path, overlay: &Path) -> Result<(), String> {
    let source_testing = source.join("testing");
    let overlay_testing = overlay.join("testing");
    replace_generated_link_with_directory(&overlay_testing)?;
    for entry in
        fs::read_dir(&source_testing).map_err(display_error("list Chromium testing directory"))?
    {
        let entry = entry.map_err(display_error("read Chromium testing entry"))?;
        if entry.file_name() == "BUILD.gn" {
            continue;
        }
        ensure_symlink(&entry.path(), &overlay_testing.join(entry.file_name()))?;
    }
    let build = fs::read_to_string(source_testing.join("BUILD.gn"))
        .map_err(display_error("read upstream testing/BUILD.gn"))?;
    let build = remove_named_gn_blocks(build, "group", "run_perf_test")?;
    fs::write(overlay_testing.join("BUILD.gn"), build)
        .map_err(display_error("write Cronet-only testing/BUILD.gn"))
}

fn write_build_overlay(source: &Path, overlay: &Path, filter_tests: bool) -> Result<(), String> {
    let source_build = source.join("build");
    let overlay_build = overlay.join("build");
    replace_generated_link_with_directory(&overlay_build)?;
    for entry in
        fs::read_dir(&source_build).map_err(display_error("list Chromium build directory"))?
    {
        let entry = entry.map_err(display_error("read Chromium build entry"))?;
        if matches!(
            entry.file_name().to_str(),
            Some("BUILD.gn" | "android" | "config")
        ) {
            continue;
        }
        ensure_symlink(&entry.path(), &overlay_build.join(entry.file_name()))?;
    }
    let build = fs::read_to_string(source_build.join("BUILD.gn"))
        .map_err(display_error("read upstream build/BUILD.gn"))?;
    let build = remove_named_gn_blocks(build, "group", "gold_common_pytype")?;
    fs::write(overlay_build.join("BUILD.gn"), build)
        .map_err(display_error("write Cronet-only build/BUILD.gn"))?;
    ensure_symlink(&source_build.join("config"), &overlay_build.join("config"))?;
    write_test_filtered_directory(
        &source_build.join("android"),
        &overlay_build.join("android"),
        filter_tests,
    )
}

fn write_buildtools_overlay(source: &Path, overlay: &Path) -> Result<(), String> {
    let source_buildtools = source.join("buildtools");
    let overlay_buildtools = overlay.join("buildtools");
    replace_generated_link_with_directory(&overlay_buildtools)?;
    for entry in fs::read_dir(&source_buildtools)
        .map_err(display_error("list Chromium buildtools directory"))?
    {
        let entry = entry.map_err(display_error("read Chromium buildtools entry"))?;
        if entry.file_name() != "third_party" {
            ensure_symlink(&entry.path(), &overlay_buildtools.join(entry.file_name()))?;
        }
    }

    let source_third_party = source_buildtools.join("third_party");
    let overlay_third_party = overlay_buildtools.join("third_party");
    replace_generated_link_with_directory(&overlay_third_party)?;
    for entry in fs::read_dir(&source_third_party).map_err(display_error(
        "list Chromium buildtools third_party directory",
    ))? {
        let entry = entry.map_err(display_error("read Chromium buildtools third_party entry"))?;
        if entry.file_name() != "libc++" {
            ensure_symlink(&entry.path(), &overlay_third_party.join(entry.file_name()))?;
        }
    }

    let source_libcxx = source_third_party.join("libc++");
    let overlay_libcxx = overlay_third_party.join("libc++");
    replace_generated_link_with_directory(&overlay_libcxx)?;
    for entry in
        fs::read_dir(&source_libcxx).map_err(display_error("list Chromium libc++ build files"))?
    {
        let entry = entry.map_err(display_error("read a Chromium libc++ build entry"))?;
        ensure_symlink(&entry.path(), &overlay_libcxx.join(entry.file_name()))?;
    }
    Ok(())
}

#[allow(clippy::too_many_lines)] // The Cronet component graph is pruned as one version-locked unit.
fn write_cronet_component_overlay(
    source: &Path,
    overlay: &Path,
    filter_tests: bool,
) -> Result<(), String> {
    let source_components = source.join("components");
    let overlay_components = overlay.join("components");
    replace_generated_link_with_directory(&overlay_components)?;
    for entry in
        fs::read_dir(&source_components).map_err(display_error("list Chromium components"))?
    {
        let entry = entry.map_err(display_error("read Chromium component entry"))?;
        if entry.file_name() == "cronet" {
            continue;
        }
        let destination = overlay_components.join(entry.file_name());
        if entry.path().is_dir() {
            write_test_filtered_directory(&entry.path(), &destination, filter_tests)?;
        } else {
            ensure_symlink(&entry.path(), &destination)?;
        }
    }

    let source_cronet = source_components.join("cronet");
    let overlay_cronet = overlay_components.join("cronet");
    replace_generated_link_with_directory(&overlay_cronet)?;
    for entry in
        fs::read_dir(&source_cronet).map_err(display_error("list Chromium Cronet directory"))?
    {
        let entry = entry.map_err(display_error("read Chromium Cronet entry"))?;
        if matches!(entry.file_name().to_str(), Some("BUILD.gn" | "native")) {
            continue;
        }
        ensure_symlink(&entry.path(), &overlay_cronet.join(entry.file_name()))?;
    }

    let source_native = source_cronet.join("native");
    let overlay_native = overlay_cronet.join("native");
    replace_generated_link_with_directory(&overlay_native)?;
    for entry in fs::read_dir(&source_native)
        .map_err(display_error("list Chromium Cronet native directory"))?
    {
        let entry = entry.map_err(display_error("read Chromium Cronet native entry"))?;
        if entry.file_name() == "BUILD.gn" {
            continue;
        }
        ensure_symlink(&entry.path(), &overlay_native.join(entry.file_name()))?;
    }
    native_extensions::apply(overlay)?;
    Ok(())
}

pub(crate) fn replace_generated_link_with_directory(path: &Path) -> Result<(), String> {
    if let Ok(metadata) = fs::symlink_metadata(path) {
        if metadata.file_type().is_symlink() {
            remove_overlay_symlink(path)?;
        } else if metadata.is_dir() {
            return Ok(());
        } else {
            return Err(format!(
                "{} blocks the generated Cronet GN overlay",
                path.display()
            ));
        }
    }
    fs::create_dir_all(path).map_err(display_error("create Cronet GN overlay directory"))
}

pub(crate) fn replace_generated_link_with_file(path: &Path) -> Result<(), String> {
    if let Ok(metadata) = fs::symlink_metadata(path) {
        if metadata.file_type().is_symlink() {
            remove_overlay_symlink(path)?;
        } else if metadata.is_file() {
            fs::remove_file(path).map_err(display_error("replace Cronet overlay link"))?;
        } else {
            return Err(format!(
                "{} blocks the generated Cronet GN overlay",
                path.display()
            ));
        }
    }
    Ok(())
}

fn remove_overlay_symlink(path: &Path) -> Result<(), String> {
    let result = fs::remove_file(path).or_else(|error| {
        if path.is_dir() {
            // Windows requires directory symlinks to be removed with the
            // directory API. This removes only the link, never its target.
            fs::remove_dir(path)
        } else {
            Err(error)
        }
    });
    result.map_err(|error| {
        format!(
            "failed to replace Cronet overlay link {}: {error}",
            path.display()
        )
    })
}

pub(crate) fn write_if_changed(
    path: &Path,
    contents: &[u8],
    action: &'static str,
) -> Result<(), String> {
    if fs::read(path).is_ok_and(|current| current == contents) {
        return Ok(());
    }
    fs::write(path, contents).map_err(display_error(action))
}

fn remove_named_gn_blocks(mut source: String, kind: &str, name: &str) -> Result<String, String> {
    let needle = format!("{kind}(\"{name}\")");
    let mut removed = 0;
    while let Some(call_start) = source.find(&needle) {
        let line_start = source[..call_start]
            .rfind('\n')
            .map_or(0, |index| index + 1);
        let open = next_gn_open_brace(&source, call_start + needle.len())
            .ok_or_else(|| format!("malformed upstream GN target {needle}"))?;
        let close = matching_gn_brace(&source, open)
            .ok_or_else(|| format!("unclosed upstream GN target {needle}"))?;
        let mut end = close + 1;
        while source
            .as_bytes()
            .get(end)
            .is_some_and(u8::is_ascii_whitespace)
        {
            end += 1;
        }
        source.replace_range(line_start..end, "");
        removed += 1;
    }
    if removed == 0 {
        Err(format!(
            "upstream Cronet BUILD.gn no longer defines {needle}"
        ))
    } else {
        Ok(source)
    }
}

fn remove_gn_block_with_header(mut source: String, header: &str) -> Result<String, String> {
    let call_start = source
        .find(header)
        .ok_or_else(|| format!("upstream BUILD.gn no longer defines {header}"))?;
    let line_start = source[..call_start]
        .rfind('\n')
        .map_or(0, |index| index + 1);
    let open = next_gn_open_brace(&source, call_start + header.len())
        .ok_or_else(|| format!("malformed upstream GN target {header}"))?;
    let close = matching_gn_brace(&source, open)
        .ok_or_else(|| format!("unclosed upstream GN target {header}"))?;
    let mut end = close + 1;
    while source
        .as_bytes()
        .get(end)
        .is_some_and(u8::is_ascii_whitespace)
    {
        end += 1;
    }
    source.replace_range(line_start..end, "");
    Ok(source)
}

fn remove_testonly_gn_blocks(mut source: String) -> String {
    const KINDS: &[&str] = &[
        "action",
        "android_aidl",
        "android_apk",
        "android_assets",
        "android_java_prebuilt",
        "android_library",
        "android_nocompile_test_suite",
        "android_resources",
        "androidx_android_aar_prebuilt",
        "androidx_java_group",
        "bundle_data_from_filelist",
        "bundle_data",
        "component",
        "copy",
        "cronet_instrumentation_test_apk",
        "executable",
        "fuzzer_test",
        "generate_jni",
        "generate_jni_registration",
        "group",
        "incremental_javac_prebuilt",
        "instrumentation_test_apk",
        "java_cpp_enum",
        "java_group",
        "java_library",
        "java_prebuilt",
        "lint_test",
        "perfetto_generate_unittests",
        "perfetto_unittest_source_set",
        "python_library",
        "robolectric_binary",
        "robolectric_library",
        "script_test",
        "shared_library",
        "shared_library_with_jni",
        "source_set",
        "static_library",
        "target",
        "test",
    ];

    // `str::lines` removes both bytes from CRLF, while the offset scan below
    // advances one byte for each line ending. Normalize Windows checkouts so
    // calculated byte offsets continue to address the original GN tokens.
    if source.contains("\r\n") {
        source = source.replace("\r\n", "\n");
    }

    loop {
        let candidate = source
            .lines()
            .scan(0_usize, |offset, line| {
                let line_start = *offset;
                *offset += line.len() + 1;
                Some((line_start, line))
            })
            .filter_map(|(line_start, line)| {
                let trimmed = line.trim_start();
                let kind = KINDS
                    .iter()
                    .find(|kind| trimmed.starts_with(&format!("{kind}(")))?;
                Some((line_start + line.len() - trimmed.len(), *kind))
            })
            .find_map(|(call_start, kind)| {
                let open = next_gn_open_brace(&source, call_start)?;
                let close = matching_gn_brace(&source, open)?;
                let body = &source[open + 1..close];
                let header = &source[call_start..open];
                (matches!(
                    kind,
                    "android_nocompile_test_suite"
                        | "cronet_instrumentation_test_apk"
                        | "fuzzer_test"
                        | "instrumentation_test_apk"
                        | "lint_test"
                        | "robolectric_binary"
                        | "robolectric_library"
                        | "script_test"
                        | "test"
                ) || header.contains("unittest")
                    || body
                        .lines()
                        .any(|line| line.trim().starts_with("testonly = true")))
                .then_some((call_start, close))
            });

        let Some((call_start, close)) = candidate else {
            return source;
        };
        let line_start = source[..call_start]
            .rfind('\n')
            .map_or(0, |index| index + 1);
        let mut end = close + 1;
        while source
            .as_bytes()
            .get(end)
            .is_some_and(u8::is_ascii_whitespace)
        {
            end += 1;
        }
        source.replace_range(line_start..end, "");
    }
}

fn next_gn_open_brace(source: &str, start: usize) -> Option<usize> {
    let mut in_string = false;
    let mut escaped = false;
    let mut in_comment = false;
    for (index, &byte) in source.as_bytes().iter().enumerate().skip(start) {
        if in_comment {
            if byte == b'\n' {
                in_comment = false;
            }
            continue;
        }
        if in_string {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                in_string = false;
            }
            continue;
        }
        match byte {
            b'#' => in_comment = true,
            b'"' => in_string = true,
            b'{' => return Some(index),
            _ => {}
        }
    }
    None
}

fn matching_gn_brace(source: &str, open: usize) -> Option<usize> {
    let bytes = source.as_bytes();
    let mut depth = 0_usize;
    let mut in_string = false;
    let mut escaped = false;
    let mut in_comment = false;
    for (index, &byte) in bytes.iter().enumerate().skip(open) {
        if in_comment {
            if byte == b'\n' {
                in_comment = false;
            }
            continue;
        }
        if in_string {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                in_string = false;
            }
            continue;
        }
        match byte {
            b'#' => in_comment = true,
            b'"' => in_string = true,
            b'{' => depth += 1,
            b'}' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(index);
                }
            }
            _ => {}
        }
    }
    None
}

pub(crate) fn ensure_symlink(target: &Path, link: &Path) -> Result<(), String> {
    if let Ok(metadata) = fs::symlink_metadata(link) {
        if metadata.file_type().is_symlink() {
            if fs::read_link(link).is_ok_and(|existing| existing == target) {
                return Ok(());
            }
            remove_overlay_symlink(link)?;
            return create_symlink(target, link)
                .map_err(display_error("create Cronet GN overlay link"));
        }
        if metadata.is_file() && target.is_file() {
            fs::remove_file(link).map_err(display_error("replace Cronet overlay file"))?;
            return create_symlink(target, link)
                .map_err(display_error("create Cronet GN overlay link"));
        }
        if metadata.is_dir() && target.is_dir() {
            return Ok(());
        }
        return Err(format!(
            "{} already exists and is not the expected Cronet overlay link",
            link.display()
        ));
    }
    create_symlink(target, link).map_err(display_error("create Cronet GN overlay link"))
}

#[cfg(unix)]
fn create_symlink(target: &Path, link: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

#[cfg(windows)]
fn create_symlink(target: &Path, link: &Path) -> std::io::Result<()> {
    if target.is_dir() {
        std::os::windows::fs::symlink_dir(target, link)
    } else {
        std::os::windows::fs::symlink_file(target, link)
    }
}

#[cfg(unix)]
fn create_source_symlink(_source: &Path, target: &Path, link: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

#[cfg(windows)]
fn create_source_symlink(source: &Path, target: &Path, link: &Path) -> std::io::Result<()> {
    if source.is_dir() {
        std::os::windows::fs::symlink_dir(target, link)
    } else {
        std::os::windows::fs::symlink_file(target, link)
    }
}

#[cfg(test)]
fn cronet_dependency_allowed(path: &str) -> bool {
    CRONET_DEPENDENCY_RULES.iter().any(|rule| {
        rule.strip_suffix('*')
            .map_or(path == *rule, |prefix| path.starts_with(prefix))
    })
}

const CRONET_DEPENDENCY_RULES: &[&str] = &[
    "src/build/linux/debian_*",
    "src/buildtools/linux64",
    "src/buildtools/mac",
    "src/buildtools/win",
    "src/net/third_party/quiche/src",
    "src/third_party/android_build_tools/*",
    "src/third_party/android_deps/*",
    "src/third_party/android_sdk/public",
    "src/third_party/androidx/cipd",
    "src/third_party/aosp_dalvik/cipd",
    "src/third_party/boringssl/src",
    "src/third_party/catapult",
    "src/third_party/ced/src",
    "src/third_party/compiler-rt/src",
    "src/third_party/cpu_features/src",
    "src/third_party/googletest/src",
    "src/third_party/icu",
    "src/third_party/icu4j/cipd",
    "src/third_party/jdk/current",
    "src/third_party/jsoncpp/source",
    "src/third_party/junit/src",
    "src/third_party/kotlin_stdlib/cipd",
    "src/third_party/libc++/src",
    "src/third_party/libc++abi/src",
    "src/third_party/libunwindstack",
    "src/third_party/libunwind/src",
    "src/third_party/lss",
    "src/third_party/lzma_sdk/bin/*",
    "src/third_party/llvm-build/Release+Asserts",
    "src/third_party/llvm-libc/src",
    "src/third_party/ninja",
    "src/third_party/perfetto",
    "src/third_party/re2/src",
    "src/third_party/r8/cipd",
    "src/third_party/r8/d8/cipd",
    "src/third_party/rust-toolchain",
    "src/third_party/sqlite4java/cipd",
    "src/third_party/turbine/cipd",
    "src/third_party/zstd/src",
];

fn command_with_depot_tools(program: &Path, depot_tools: &Path) -> Command {
    let mut command = Command::new(program);
    let old_path = env::var_os("PATH").unwrap_or_default();
    let paths = std::iter::once(depot_tools.to_owned()).chain(env::split_paths(&old_path));
    if let Ok(joined) = env::join_paths(paths) {
        command.env("PATH", joined);
    }
    let old_python_path = env::var_os("PYTHONPATH").unwrap_or_default();
    let python_paths =
        std::iter::once(depot_tools.to_owned()).chain(env::split_paths(&old_python_path));
    if let Ok(joined) = env::join_paths(python_paths) {
        command.env("PYTHONPATH", joined);
    }
    command.env("DEPOT_TOOLS_UPDATE", "0");
    command
}

fn run_command(command: &mut Command, description: &str) -> Result<(), String> {
    println!("==> {description}");
    let status = command
        .status()
        .map_err(|error| format!("could not {description}: {error}"))?;
    check_status(status, description)
}

pub(crate) fn command_stdout(command: &mut Command, description: &str) -> Result<String, String> {
    let output = command
        .output()
        .map_err(|error| format!("could not {description}: {error}"))?;
    check_status(output.status, description)?;
    String::from_utf8(output.stdout)
        .map_err(|error| format!("{description} returned non-UTF-8 output: {error}"))
}

fn check_status(status: ExitStatus, description: &str) -> Result<(), String> {
    if status.success() {
        Ok(())
    } else {
        Err(format!("failed to {description}: {status}"))
    }
}

fn command_exists(program: &str) -> bool {
    Command::new(program)
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success())
}

fn git_output(directory: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .current_dir(directory)
        .args(args)
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

fn native_output_dir(source: &Path, target: Option<&str>) -> PathBuf {
    let base = source.join("out/cronet-rs");
    target.map_or(base.clone(), |target| {
        base.join(target.replace(|character: char| !character.is_ascii_alphanumeric(), "_"))
    })
}

fn native_library_exists(source: &Path, target: Option<&str>, linkage: NativeLinkage) -> bool {
    let Ok(entries) = fs::read_dir(native_output_dir(source, target)) else {
        return false;
    };
    entries.flatten().any(|entry| {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        is_native_library_output(&name, linkage)
    })
}

pub(crate) fn require_file(path: &Path, help: &str) -> Result<(), String> {
    path.is_file()
        .then_some(())
        .ok_or_else(|| format!("{} is missing; {help}", path.display()))
}

pub(crate) fn display_error(action: &'static str) -> impl FnOnce(std::io::Error) -> String {
    move |error| format!("failed to {action}: {error}")
}

// Non-cone gitignore syntax is intentional: the root Chromium repository is a
// partial clone and these patterns prevent checkout of Chrome, Blink, Content,
// V8, WebRTC, and other unrelated product sources. The allow-list is based on
// components/cronet/android/dependencies.txt at CHROMIUM_REVISION, plus the GN
// and toolchain inputs needed to configure a desktop build.
const API_SPARSE_PATTERNS: &str = r"/*
!/*/
/chrome/
!/chrome/*/
/chrome/VERSION
/components/
!/components/*/
/components/cronet/
/components/grpc_support/
!/components/grpc_support/*/
/components/grpc_support/include/
";

const BUILD_SPARSE_PATTERNS: &str = r"/*
!/*/
/.gn
/base/
/build/
/build_overrides/
/buildtools/
/chrome/
!/chrome/*/
/chrome/VERSION
/chrome/version.gni
/chrome/app/
!/chrome/app/*/
/chrome/app/theme/
!/chrome/app/theme/*/
/chrome/app/theme/chromium/
!/chrome/app/theme/chromium/*
/chrome/app/theme/chromium/BRANDING
/components/
!/components/*/
/components/cbor/
/components/cronet/
/components/grpc_support/
/components/metrics/
/components/network_time/
/components/prefs/
/components/unexportable_keys/
/crypto/
/extensions/
!/extensions/*/
/extensions/buildflags/
/ios/
!/ios/*/
/ios/features.gni
/net/
/testing/
/third_party/
!/third_party/*/
/third_party/abseil-cpp/
/third_party/android_build_tools/
/third_party/android_deps/
/third_party/android_sdk/
/third_party/androidx/
/third_party/aosp_dalvik/
/third_party/apple_apsl/
/third_party/boringssl/
/third_party/brotli/
/third_party/byte_buddy/
/third_party/catapult/
/third_party/ced/
/third_party/compiler-rt/
/third_party/cpu_features/
/third_party/googletest/
/third_party/google-truth/
/third_party/hamcrest/
/third_party/icu/
/third_party/icu4j/
/third_party/ijar/
/third_party/jdk/
/third_party/jinja2/
/third_party/jni_zero/
/third_party/jsoncpp/
/third_party/junit/
/third_party/kotlin_stdlib/
/third_party/libc++/
/third_party/libc++abi/
/third_party/libevent/
/third_party/libunwindstack/
/third_party/libunwind/
/third_party/lss/
/third_party/lzma_sdk/
/third_party/markupsafe/
/third_party/llvm-build/
/third_party/llvm-libc/
/third_party/metrics_proto/
/third_party/mockito/
/third_party/modp_b64/
/third_party/perfetto/
/third_party/protobuf/
/third_party/re2/
/third_party/r8/
/third_party/rust/
!/third_party/rust/*/
/third_party/rust/BUILD.gn
/third_party/rust/chromium_crates_io/
/third_party/rust/anstyle/
/third_party/rust/clap/
/third_party/rust/clap_builder/
/third_party/rust/clap_lex/
/third_party/rust/codespan_reporting/
/third_party/rust/cxx/
/third_party/rust/cxxbridge_cmd/
/third_party/rust/cxxbridge_macro/
/third_party/rust/equivalent/
/third_party/rust/foldhash/
/third_party/rust/hashbrown/
/third_party/rust/hmac_sha256/
/third_party/rust/indexmap/
/third_party/rust/itoa/
/third_party/rust/log/
/third_party/rust/memchr/
/third_party/rust/proc_macro2/
/third_party/rust/quote/
/third_party/rust/rustc_demangle/
/third_party/rust/rustc_demangle_capi/
/third_party/rust/ryu/
/third_party/rust/serde/
/third_party/rust/serde_core/
/third_party/rust/serde_derive/
/third_party/rust/serde_json_lenient/
/third_party/rust/strsim/
/third_party/rust/syn/
/third_party/rust/termcolor/
/third_party/rust/unicode_ident/
/third_party/rust/unicode_width/
/third_party/simdutf/
/third_party/sqlite4java/
/third_party/turbine/
/third_party/rust-toolchain/
/third_party/zlib/
/third_party/zstd/
/tools/
/url/
";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_qualified_public_safe_functions() {
        let source = r"
pub struct Example;
impl Example {
    pub fn plain(&self) {}
    pub async fn asynchronous(&self) {}
    pub const fn constant() {}
    pub unsafe fn unsafe_entry() {}
    pub(crate) fn internal() {}
}
impl Default for Example {
    fn default() -> Self { Self }
}
";
        assert_eq!(
            public_functions_in_source("example.rs", source),
            [
                "Example::asynchronous",
                "Example::constant",
                "Example::plain",
                "Example::unsafe_entry",
            ]
            .into_iter()
            .map(str::to_owned)
            .collect()
        );
    }

    #[test]
    fn recognizes_the_cfg_gated_android_entry() {
        let source = r"
pub mod android {
    pub unsafe fn initialize_java_vm(pointer: *mut ()) -> i32 { 0 }
}
";
        assert_eq!(
            public_functions_in_source("lib.rs", source),
            ["android::initialize_java_vm".to_owned()]
                .into_iter()
                .collect()
        );
    }

    #[test]
    fn committed_overlay_files_are_typed_wrappers() {
        let rustfmt = include_str!("../wrappers/rustfmt");
        assert!(rustfmt.contains("rustfmt.real"));
        let rustfmt = include_str!("../wrappers/rustfmt.toml");
        assert!(rustfmt.contains("normalize_doc_attributes"));
        let angle = include_str!("../overlays/common/third_party/angle/dotfile_settings.gni");
        assert!(angle.contains("exec_script_allowlist = []"));
        let close = include_str!("../overlays/ohos/base/files/scoped_file_linux.cc");
        assert!(close.contains("scoped_file_linux_upstream.cc"));
        assert!(close.contains("COMPONENT_BUILD"));
        let ios = include_str!("../overlays/ios/base/features.cc");
        assert!(ios.contains("features_upstream.cc"));
        let ashmem = include_str!("../overlays/android/base/android/linker/ashmem.cc");
        assert!(ashmem.contains("ashmem_upstream.cc"));
        let resolver = include_str!("../overlays/ohos/net/dns/public/scoped_res_state.cc");
        assert!(resolver.contains("scoped_res_state_upstream.cc"));
        let wrapper = include_str!("../../crates/tokio-cronet-sys/native/cronet_rs_bind.cc");
        assert!(wrapper.contains("Cronet_RS_Engine_StartWithParams"));
        assert!(wrapper.contains("CommandLine::Init"));
    }

    #[test]
    fn keeps_cronet_deps_and_rejects_browser_deps() {
        assert!(cronet_dependency_allowed("src/third_party/boringssl/src"));
        assert!(cronet_dependency_allowed("src/net/third_party/quiche/src"));
        assert!(cronet_dependency_allowed("src/third_party/ninja"));
        assert!(cronet_dependency_allowed("src/third_party/lss"));
        assert!(cronet_dependency_allowed("src/third_party/junit/src"));
        assert!(!cronet_dependency_allowed(
            "src/third_party/robolectric/cipd"
        ));
        assert!(!cronet_dependency_allowed("src/buildtools/reclient"));
        assert!(cronet_dependency_allowed(
            "src/build/linux/debian_bullseye_amd64-sysroot"
        ));
        assert!(!cronet_dependency_allowed("src/third_party/dawn"));
        assert!(!cronet_dependency_allowed("src/v8"));
    }

    #[test]
    fn maps_every_published_rust_target_to_gn() {
        use platform::PlatformKind::{Android, Ios, Linux, MacOs, Ohos, Windows};

        let targets = [
            ("x86_64-unknown-linux-gnu", Linux, "linux", "x64"),
            ("aarch64-unknown-linux-gnu", Linux, "linux", "arm64"),
            ("i686-linux-android", Android, "android", "x86"),
            ("x86_64-linux-android", Android, "android", "x64"),
            ("armv7-linux-androideabi", Android, "android", "arm"),
            ("aarch64-linux-android", Android, "android", "arm64"),
            ("x86_64-apple-darwin", MacOs, "mac", "x64"),
            ("aarch64-apple-darwin", MacOs, "mac", "arm64"),
            ("x86_64-apple-ios", Ios, "ios", "x64"),
            ("aarch64-apple-ios", Ios, "ios", "arm64"),
            ("aarch64-apple-ios-sim", Ios, "ios", "arm64"),
            ("x86_64-pc-windows-msvc", Windows, "win", "x64"),
            ("aarch64-pc-windows-msvc", Windows, "win", "arm64"),
            ("armv7-unknown-linux-ohos", Ohos, "linux", "arm"),
            ("aarch64-unknown-linux-ohos", Ohos, "linux", "arm64"),
            ("x86_64-unknown-linux-ohos", Ohos, "linux", "x64"),
        ];
        for (target, kind, gn_os, gn_cpu) in targets {
            let target_build = platform::resolve(Some(target)).unwrap();
            let specification = target_build.target_spec().unwrap();
            assert_eq!(target_build.kind(), kind);
            assert_eq!(specification.triple, target);
            assert_eq!(specification.gn_os, gn_os);
            assert_eq!(specification.gn_cpu, gn_cpu);
        }
        let host = platform::resolve(None).unwrap();
        assert_eq!(host.kind(), platform::PlatformKind::Host);
        assert!(host.target_spec().is_none());
        assert!(platform::resolve(Some("wasm32-unknown-unknown")).is_err());
    }

    #[test]
    fn keeps_platform_only_build_hooks_isolated() {
        let android = platform::resolve(Some("aarch64-linux-android")).unwrap();
        let windows = platform::resolve(Some("aarch64-pc-windows-msvc")).unwrap();
        let macos = platform::resolve(Some("aarch64-apple-darwin")).unwrap();

        assert!(!android.filter_third_party_tests());
        assert!(macos.filter_third_party_tests());
        assert_eq!(windows.static_archive_extension(), "lib");
        assert_eq!(macos.static_archive_extension(), "a");

        let android_root = include_str!("../overlays/android/BUILD.gn");
        assert!(android_root.contains("cronet_rs_android_support_java"));
        let host_root = include_str!("../overlays/common/BUILD.gn");
        assert!(!host_root.contains("cronet_rs_android_support_java"));
    }

    #[test]
    fn skips_android_dependency_rewrites_for_desktop_and_ios_syncs() {
        let nonexistent = Path::new("this-manifest-must-not-be-opened");
        for target in [
            None,
            Some("x86_64-pc-windows-msvc"),
            Some("aarch64-pc-windows-msvc"),
            Some("aarch64-apple-ios-sim"),
        ] {
            platform::android::patch_android_clang_dependency(nonexistent, target).unwrap();
        }
        assert!(
            platform::android::patch_android_clang_dependency(
                nonexistent,
                Some("aarch64-linux-android")
            )
            .is_err()
        );
    }

    #[test]
    fn standalone_build_avoids_linux_host_development_packages() {
        let arguments = common_gn_args(true);
        for argument in ["use_glib=false", "use_gio=false", "use_nss_certs=false"] {
            assert!(arguments.iter().any(|candidate| candidate == argument));
        }
    }

    #[test]
    fn linux_arm64_native_build_requires_native_host_tools() {
        assert!(platform::linux::requires_native_linux_arm64_tools(
            "linux",
            "aarch64",
            "aarch64-unknown-linux-gnu"
        ));
        assert!(!platform::linux::requires_native_linux_arm64_tools(
            "linux",
            "x86_64",
            "aarch64-unknown-linux-gnu"
        ));
        assert!(!platform::linux::requires_native_linux_arm64_tools(
            "linux",
            "aarch64",
            "x86_64-unknown-linux-gnu"
        ));
    }

    #[test]
    fn static_packaging_uses_the_configured_llvm_tools() {
        let binary = if cfg!(windows) {
            "llvm-ar.exe"
        } else {
            "llvm-ar"
        };
        assert_eq!(
            llvm_tool(
                Path::new("chromium"),
                Some(Path::new("native-llvm")),
                "llvm-ar"
            ),
            Path::new("native-llvm/bin").join(binary)
        );
        assert_eq!(
            llvm_tool(Path::new("chromium"), None, "llvm-ar"),
            Path::new("chromium/third_party/llvm-build/Release+Asserts/bin").join(binary)
        );
    }

    #[test]
    fn external_rust_static_packaging_excludes_only_the_allocator_shim() {
        assert!(is_chromium_rust_allocator_shim(
            "obj/build/rust/allocator/liballocator_6ead5877.rlib"
        ));
        assert!(is_chromium_rust_allocator_shim(
            r"obj\build\rust\allocator\liballocator_6ead5877.rlib"
        ));
        assert!(!is_chromium_rust_allocator_shim(
            "obj/build/rust/allocator/liballoc_error_handler_impl_ffi.rlib"
        ));
        assert!(!is_chromium_rust_allocator_shim(
            "obj/other/liballocator_6ead5877.rlib"
        ));
    }

    #[test]
    fn maps_all_android_compiler_runtimes() {
        let cases = [
            (
                "aarch64-linux-android",
                "libclang_rt.builtins-aarch64-android.a",
                "aarch64-unknown-linux-android23",
            ),
            (
                "armv7-linux-androideabi",
                "libclang_rt.builtins-arm-android.a",
                "arm-unknown-linux-android23",
            ),
            (
                "x86_64-linux-android",
                "libclang_rt.builtins-x86_64-android.a",
                "x86_64-unknown-linux-android23",
            ),
            (
                "i686-linux-android",
                "libclang_rt.builtins-i686-android.a",
                "i686-unknown-linux-android23",
            ),
        ];
        for (target, archive, directory) in cases {
            assert_eq!(
                platform::android::android_compiler_runtime(target, 23).unwrap(),
                (archive, directory.to_owned())
            );
        }
    }

    #[test]
    fn source_lock_matches_build_constants() {
        let lock = include_str!("../SOURCE.lock");
        assert!(lock.contains(&format!("chromium_revision={CHROMIUM_REVISION}")));
        assert!(lock.contains(&format!("chromium_version={CHROMIUM_VERSION}")));
        assert!(lock.contains("source_layout=vendor/chromium/src"));
    }

    #[test]
    fn serializes_operations_on_one_source_cache() {
        let root =
            env::temp_dir().join(format!("tokio-cronet-src-lock-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let source = root.join("chromium/src");
        let lock = SourceOperationLock::acquire(&source).unwrap();
        let lock_path = root.join(".tokio-cronet.lock");
        let competing = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&lock_path)
            .unwrap();

        assert!(FileExt::try_lock_exclusive(&competing).is_err());
        drop(lock);
        FileExt::try_lock_exclusive(&competing).unwrap();
        FileExt::unlock(&competing).unwrap();

        drop(competing);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn source_builder_rejects_unsupported_targets_before_materializing_source() {
        let result = Build::new().target("wasm32-unknown-unknown").build();
        assert!(result.is_err());
    }

    #[test]
    fn discovers_ohos_runtime_only_below_the_selected_sdk() {
        let root = env::temp_dir().join(format!(
            "tokio-cronet-src-ohos-sdk-test-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        for version in ["15.0.4", "22.1.0"] {
            let runtime = root
                .join("llvm/lib/clang")
                .join(version)
                .join("lib/aarch64-linux-ohos");
            fs::create_dir_all(&runtime).unwrap();
            fs::write(runtime.join("libclang_rt.builtins.a"), []).unwrap();
        }

        let selected =
            platform::ohos::ohos_compiler_resource_dir(&root, "aarch64-linux-ohos").unwrap();
        assert_eq!(selected, root.join("llvm/lib/clang/22.1.0"));
        assert!(platform::ohos::ohos_compiler_resource_dir(&root, "x86_64-linux-ohos").is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn vendored_tree_excludes_repository_and_build_state() {
        let root =
            env::temp_dir().join(format!("tokio-cronet-src-copy-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let source = root.join("source");
        let destination = root.join("destination");
        fs::create_dir_all(source.join("nested")).unwrap();
        fs::create_dir_all(source.join(".git")).unwrap();
        fs::create_dir_all(source.join("out")).unwrap();
        fs::write(source.join("nested/input.cc"), "source").unwrap();
        fs::write(source.join(".git/config"), "git").unwrap();
        fs::write(source.join("out/object.o"), "object").unwrap();
        fs::create_dir(&destination).unwrap();

        copy_source_tree(&source, &destination).unwrap();

        assert_eq!(
            fs::read_to_string(destination.join("nested/input.cc")).unwrap(),
            "source"
        );
        assert!(!destination.join(".git").exists());
        assert!(!destination.join("out").exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn recognizes_only_native_library_outputs() {
        assert!(is_native_library_output(
            "libcronet.146.0.7633.0.dylib",
            NativeLinkage::Dynamic
        ));
        assert!(is_native_library_output(
            "cronet.146.0.7633.0.dll",
            NativeLinkage::Dynamic
        ));
        assert!(is_native_library_output(
            "cronet.146.0.7633.0.dll.lib",
            NativeLinkage::Dynamic
        ));
        assert!(is_native_library_output(
            "libcronet_static.a",
            NativeLinkage::Static
        ));
        assert!(is_native_library_output(
            "cronet_static.lib",
            NativeLinkage::Static
        ));
        assert!(!is_native_library_output(
            "libcronet.146.0.7633.0.dylib.TOC",
            NativeLinkage::Dynamic
        ));
        assert!(!is_native_library_output(
            "unrelated.lib",
            NativeLinkage::Dynamic
        ));
    }

    #[test]
    fn removes_testonly_component_without_misreading_interpolation() {
        let source = r#"component("runtime") {
  sources = [ "${root}/runtime.cc" ]
}

component("test_support") {
  testonly = true
  deps = [ ":runtime" ]
}
"#;
        for source in [source.to_owned(), source.replace('\n', "\r\n")] {
            let filtered = remove_testonly_gn_blocks(source);
            assert!(filtered.contains("component(\"runtime\")"));
            assert!(filtered.contains("${root}/runtime.cc"));
            assert!(!filtered.contains("test_support"));
        }
    }

    #[test]
    fn removes_named_block_with_nested_braces() {
        let source = r#"group("keep") {
  deps = []
}

group("remove") {
  if (is_mac) {
    deps = [ "${root}/mac" ]
  }
}
"#;
        let filtered = remove_named_gn_blocks(source.to_owned(), "group", "remove").unwrap();
        assert!(filtered.contains("group(\"keep\")"));
        assert!(!filtered.contains("group(\"remove\")"));
    }

    #[test]
    fn materializes_a_directory_over_an_overlay_symlink() {
        let root = env::temp_dir().join(format!(
            "cronet-rs-overlay-link-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let target = root.join("target");
        let link = root.join("link");
        fs::create_dir_all(&target).unwrap();
        create_symlink(&target, &link).unwrap();

        replace_generated_link_with_directory(&link).unwrap();

        let metadata = fs::symlink_metadata(&link).unwrap();
        assert!(metadata.is_dir());
        assert!(!metadata.file_type().is_symlink());
        fs::remove_dir_all(root).unwrap();
    }
}

use std::{
    collections::BTreeSet,
    env, fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, ExitStatus, Stdio},
};

pub const CHROMIUM_REVISION: &str = "db64a84f93f16f8de53fee8d33df0a31473efefb";
pub const CHROMIUM_VERSION: &str = "146.0.7633.0";
const CHROMIUM_URL: &str = "https://chromium.googlesource.com/chromium/src.git";
const DEPOT_TOOLS_URL: &str = "https://chromium.googlesource.com/chromium/tools/depot_tools.git";
const OHOS_SDK_NATIVE_ENV: &str = "OHOS_SDK_NATIVE";

/// Native library form selected by `cronet-sys` or the workspace CLI.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NativeLinkage {
    Dynamic,
    Static,
}

/// Source-build result consumed by `cronet-sys`.
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
    rust_sysroot: Option<PathBuf>,
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
            rust_sysroot: None,
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

    /// Selects the Rust sysroot that contains the requested OHOS standard
    /// library. If unset, the sysroot reported by Cargo's `RUSTC` is used.
    pub fn rust_sysroot(&mut self, directory: impl Into<PathBuf>) -> &mut Self {
        self.rust_sysroot = Some(directory.into());
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
            self.ohos_sdk_native.as_deref(),
            self.rust_sysroot.as_deref(),
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
           cargo xtask sync [--api-only] [--source-dir PATH]\n\
           cargo xtask build [--release] [--linkage dynamic|static|both] [--target TARGET] [--source-dir PATH] [--gn-arg ARG]...\n\
           cargo xtask vendor-source [--source-dir PATH] [--output PATH]\n\
           cargo xtask doctor [--source-dir PATH]\n\
           cargo xtask print-env [--source-dir PATH]\n\n\
         `sync` uses a blobless sparse checkout pinned immediately before the\n\
         upstream native API was deleted. `--api-only` fetches just the public\n\
         C API and is enough for cargo check with CRONET_SYS_NO_LINK=1."
    );
}

/// Ensures that a release Cronet library exists for `target`, synchronizing
/// only the pinned Cronet build closure and compiling it when necessary.
///
/// This entry point is used by `cronet-sys` for every linked build. The source
/// directory should be target-specific so libraries for different
/// architectures never overwrite each other.
pub fn ensure_native_from_source(
    source: &Path,
    target: &str,
    linkage: NativeLinkage,
) -> Result<PathBuf, String> {
    ensure_native_from_source_configured(source, target, linkage, None, None)
}

fn ensure_native_from_source_configured(
    source: &Path,
    target: &str,
    linkage: NativeLinkage,
    ohos_sdk_native: Option<&Path>,
    rust_sysroot: Option<&Path>,
) -> Result<PathBuf, String> {
    let header = source.join("components/cronet/native/include/cronet_c.h");
    let bidirectional_header =
        source.join("components/grpc_support/include/bidirectional_stream_c.h");
    if header.is_file()
        && bidirectional_header.is_file()
        && native_library_exists(source, Some(target), linkage)
    {
        return Ok(native_output_dir(source, Some(target)));
    }

    if !source_tree_buildable(source) {
        sync(&["--source-dir".into(), source.as_os_str().to_owned()])?;
    }
    build_native(
        BuildOptions {
            common: CommonOptions {
                source_dir: source.to_owned(),
                api_only: false,
            },
            release: true,
            target: Some(target.to_owned()),
            linkage: match linkage {
                NativeLinkage::Dynamic => LinkageSelection::Dynamic,
                NativeLinkage::Static => LinkageSelection::Static,
            },
            gn_args: Vec::new(),
        },
        ohos_sdk_native,
        rust_sysroot,
    )?;
    Ok(native_output_dir(source, Some(target)))
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
    gn_target_args(target).is_ok()
}

fn sync(args: &[std::ffi::OsString]) -> Result<(), String> {
    let options = CommonOptions::parse(args, true)?;
    // Commands below change their working directory to the Chromium root.
    // Resolve a caller-provided relative path first so tool entry points remain
    // valid on every host, independent of where the cache is located.
    let source = if options.source_dir.is_absolute() {
        options.source_dir
    } else {
        env::current_dir()
            .map_err(display_error("resolve the current directory"))?
            .join(options.source_dir)
    };
    let chromium_root = source
        .parent()
        .ok_or_else(|| "source directory needs a parent".to_owned())?;
    fs::create_dir_all(chromium_root).map_err(display_error("create Chromium directory"))?;

    init_or_update_sparse_checkout(&source, options.api_only)?;
    if options.api_only {
        println!("Cronet C API synchronized at {}", source.display());
        return Ok(());
    }

    let depot_tools = depot_tools_dir(&source)?;
    clone_or_update_depot_tools(&depot_tools)?;
    write_gclient(chromium_root)?;
    write_cronet_overlay(&source)?;

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
    let options = BuildOptions::parse(args)?;
    build_native(options, None, None)
}

fn build_native(
    options: BuildOptions,
    ohos_sdk_native: Option<&Path>,
    rust_sysroot: Option<&Path>,
) -> Result<(), String> {
    let source = options
        .common
        .source_dir
        .canonicalize()
        .map_err(display_error("resolve the Chromium source directory"))?;
    require_file(
        &source.join("components/cronet/native/include/cronet_c.h"),
        "run `cargo xtask sync` first",
    )?;
    let depot_tools = depot_tools_dir(&source)?;
    let gn = host_gn(&source);
    let ninja = host_ninja(&source);
    require_file(&gn, "run `cargo xtask sync` (without --api-only) first")?;
    require_file(&ninja, "run `cargo xtask sync` (without --api-only) first")?;

    let out_dir = native_output_dir(&source, options.target.as_deref());
    let overlay = write_cronet_overlay(&source)?;
    let overlay_out_dir = native_output_dir(&overlay, options.target.as_deref());
    let mut gn_args = vec![
        format!("is_debug={}", !options.release),
        "is_component_build=false".to_owned(),
        "is_cronet_build=true".to_owned(),
        "enable_disk_cache_sql_backend=false".to_owned(),
        "enable_device_bound_sessions=false".to_owned(),
        "enable_perfetto_trace_processor_sqlite=false".to_owned(),
        "use_platform_icu_alternatives=false".to_owned(),
        // Keep the build compatible with older Xcode SDKs that predate the
        // split DarwinFoundation{1,2,3}.modulemap files.
        "use_clang_modules=false".to_owned(),
        "use_remoteexec=false".to_owned(),
        "use_siso=false".to_owned(),
        "treat_warnings_as_errors=false".to_owned(),
        "symbol_level=1".to_owned(),
    ];
    if let Some(target) = options.target.as_deref() {
        gn_args.extend(gn_target_args(target)?);
        if is_ohos_target(target) {
            gn_args.extend(ohos_gn_args(target, ohos_sdk_native, rust_sysroot)?);
        }
    }
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
        .arg("-C")
        .arg(&overlay_out_dir);
    if options.target.as_deref().is_some_and(is_ohos_target) {
        // Chromium's external-Rust path still emits a small set of -Z build
        // flags even when rustc_nightly_capability is false. Scope the stable
        // compiler opt-in to this native build subprocess; never mutate the
        // caller's Cargo environment.
        ninja_command.env("RUSTC_BOOTSTRAP", "1");
    }
    for linkage in options.linkage.linkages() {
        ninja_command.arg(linkage.ninja_target());
    }
    run_command(&mut ninja_command, "compile Cronet from source")?;
    if options.linkage.linkages().contains(&NativeLinkage::Static) {
        bundle_static_archive(&source, &out_dir)?;
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

fn bundle_static_archive(source: &Path, output_dir: &Path) -> Result<(), String> {
    let raw_name = native_static_archive_name("cronet_static_raw");
    let bundled_name = native_static_archive_name("cronet_static");
    let raw_archive = output_dir.join(raw_name);
    require_file(&raw_archive, "build the Cronet static GN target first")?;

    let ninja_file = output_dir.join("obj/components/cronet/cronet_static.ninja");
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
        let archive = output_dir.join(relative);
        if archive.is_file() {
            archives.push(archive);
        }
    }
    for relative in rust_archives.split_ascii_whitespace() {
        let archive = output_dir.join(relative);
        require_file(&archive, "the GN Rust dependency must be built")?;
        archives.push(archive);
    }

    let temporary_name = format!(".{bundled_name}.{}", std::process::id());
    let temporary = output_dir.join(&temporary_name);
    if temporary.exists() {
        fs::remove_file(&temporary).map_err(display_error("replace static archive temporary"))?;
    }
    let llvm_ar = source
        .join("third_party/llvm-build/Release+Asserts/bin")
        .join(if cfg!(windows) {
            "llvm-ar.exe"
        } else {
            "llvm-ar"
        });
    require_file(&llvm_ar, "run `cargo xtask sync` first")?;
    let mut child = Command::new(&llvm_ar)
        .arg("-M")
        .current_dir(output_dir)
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
            let relative = archive.strip_prefix(output_dir).map_err(|_| {
                format!(
                    "static dependency {} is outside {}",
                    archive.display(),
                    output_dir.display()
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
    Ok(())
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
        String::from("# Generated from the pinned GN target; consumed by cronet-sys.\n");
    for library in libraries
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
    {
        manifest.push_str("lib=");
        manifest.push_str(library);
        manifest.push('\n');
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
    fs::write(output_dir.join("cronet-static-link.txt"), manifest)
        .map_err(display_error("write Cronet static link manifest"))
}

fn vendor_source(args: &[std::ffi::OsString]) -> Result<(), String> {
    let options = VendorOptions::parse(args)?;
    if !source_tree_buildable(&options.source_dir) {
        sync(&[
            "--source-dir".into(),
            options.source_dir.as_os_str().to_owned(),
        ])?;
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

fn gn_target_args(target: &str) -> Result<Vec<String>, String> {
    let (target_os, target_cpu) = match target {
        "x86_64-unknown-linux-gnu" | "x86_64-unknown-linux-ohos" => ("linux", "x64"),
        "aarch64-unknown-linux-gnu" | "aarch64-unknown-linux-ohos" => ("linux", "arm64"),
        "x86_64-apple-darwin" => ("mac", "x64"),
        "aarch64-apple-darwin" => ("mac", "arm64"),
        "x86_64-pc-windows-msvc" => ("win", "x64"),
        "aarch64-pc-windows-msvc" => ("win", "arm64"),
        "armv7-unknown-linux-ohos" => ("linux", "arm"),
        other => return Err(format!("unsupported Cronet native target `{other}`")),
    };
    Ok(vec![
        format!("target_os=\"{target_os}\""),
        format!("target_cpu=\"{target_cpu}\""),
    ])
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

fn is_ohos_target(target: &str) -> bool {
    ohos_target(target).is_some()
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

fn ohos_compiler_resource_dir(sdk: &Path, llvm_target: &str) -> Result<PathBuf, String> {
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

fn rust_stdlib_adjustments(
    rust_sysroot: &Path,
    target: &str,
) -> Result<(Vec<String>, Vec<String>), String> {
    // Chromium names every rlib that must be handed to the C++ linker. Rust
    // occasionally adds or renames internal std crates, so derive the delta
    // from the selected toolchain instead of binding cronet-src to one rustc.
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
        if let Some((crate_name, hash)) = name.rsplit_once('-')
            && !crate_name.is_empty()
            && hash.chars().all(|character| character.is_ascii_hexdigit())
        {
            actual.insert(crate_name.to_owned());
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

fn gn_string_path(name: &str, path: &Path) -> String {
    format!(
        "{name}=\"{}\"",
        escape_gn_string(&path.to_string_lossy().replace('\\', "/"))
    )
}

fn gn_string_list(name: &str, values: &[String]) -> String {
    let values = values
        .iter()
        .map(|value| format!("\"{}\"", escape_gn_string(value)))
        .collect::<Vec<_>>()
        .join(",");
    format!("{name}=[{values}]")
}

fn escape_gn_string(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

fn doctor(args: &[std::ffi::OsString]) -> Result<(), String> {
    let options = CommonOptions::parse(args, false)?;
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
    let options = CommonOptions::parse(args, false)?;
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
    api_only: bool,
}

impl CommonOptions {
    fn parse(args: &[std::ffi::OsString], allow_api_only: bool) -> Result<Self, String> {
        let mut source_dir = default_source_dir();
        let mut api_only = false;
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
                Some("--api-only") if allow_api_only => api_only = true,
                Some(value) => return Err(format!("unexpected option `{value}`")),
                None => return Err("arguments must be valid UTF-8".to_owned()),
            }
            index += 1;
        }
        Ok(Self {
            source_dir,
            api_only,
        })
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
            common: CommonOptions {
                source_dir,
                api_only: false,
            },
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

fn workspace_root() -> PathBuf {
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
    source
        .parent()
        .and_then(Path::parent)
        .map(|root| root.join("depot_tools"))
        .ok_or_else(|| {
            format!(
                "source directory {} must use a ROOT/chromium/src layout",
                source.display()
            )
        })
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

fn write_gclient(chromium_root: &Path) -> Result<(), String> {
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

    let contents = format!(
        "solutions = [{{\n\
         \x20 'name': 'src',\n\
         \x20 'url': '{CHROMIUM_URL}',\n\
         \x20 'managed': False,\n\
         \x20 'deps_file': 'DEPS.cronet',\n\
         \x20 'custom_deps': {{}},\n\
         \x20 'custom_vars': {{\n\
         \x20   'checkout_pgo_profiles': False,\n\
         \x20   'checkout_telemetry_dependencies': False,\n\
         \x20   'checkout_wpr_archives': False,\n\
         \x20 }},\n\
         }}]\n\
         target_os = []\n"
    );
    fs::write(chromium_root.join(".gclient"), contents).map_err(display_error("write .gclient"))
}

fn write_cronet_overlay(source: &Path) -> Result<PathBuf, String> {
    const ANGLE_IMPORT: &str = "import(\"//third_party/angle/dotfile_settings.gni\")\n";
    const ANGLE_ALLOWLIST: &str = "    angle_dotfile_settings.exec_script_allowlist +\n";

    let source = source
        .canonicalize()
        .map_err(display_error("resolve Chromium source directory"))?;
    let overlay = source
        .parent()
        .expect("Chromium src directory must have a parent")
        .join("cronet-gn-root");
    fs::create_dir_all(&overlay).map_err(display_error("create Cronet GN overlay"))?;

    for entry in fs::read_dir(&source).map_err(display_error("list Chromium source directory"))? {
        let entry = entry.map_err(display_error("read Chromium source entry"))?;
        let name = entry.file_name();
        let name_text = name.to_string_lossy();
        if matches!(
            name_text.as_ref(),
            ".git"
                | ".gn"
                | ".gn.cronet"
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
    write_build_overlay(&source, &overlay)?;
    write_buildtools_overlay(&source, &overlay)?;
    write_ohos_overlay(&source, &overlay)?;
    for directory in ["base", "crypto", "net", "url"] {
        write_test_filtered_directory(&source.join(directory), &overlay.join(directory))?;
    }
    patch_ohos_partition_alloc(&source, &overlay)?;
    patch_ohos_base_process(&source, &overlay)?;
    patch_ohos_resolver(&source, &overlay)?;
    patch_ohos_link_closure(&source, &overlay)?;
    write_cronet_component_overlay(&source, &overlay)?;
    write_testing_overlay(&source, &overlay)?;
    write_third_party_overlay(&source, &overlay)?;
    write_cxx_libcxx_compat_overlay(&source, &overlay)?;

    let upstream = fs::read_to_string(source.join(".gn"))
        .map_err(display_error("read Chromium GN dotfile"))?;
    if !upstream.contains(ANGLE_IMPORT) || !upstream.contains(ANGLE_ALLOWLIST) {
        return Err(
            "Chromium .gn layout changed; cannot remove its unrelated ANGLE bootstrap".to_owned(),
        );
    }
    let filtered = upstream
        .replacen(ANGLE_IMPORT, "", 1)
        .replacen(ANGLE_ALLOWLIST, "", 1);
    fs::write(overlay.join(".gn"), filtered).map_err(display_error("write Cronet GN dotfile"))?;
    fs::write(
        overlay.join("BUILD.gn"),
        "# Generated by cronet-rs xtask; do not edit.\n\n\
         group(\"cronet_rs\") {\n\
         \x20 deps = [ \"//components/cronet:cronet\" ]\n\
         }\n",
    )
    .map_err(display_error("write Cronet-only root BUILD.gn"))?;
    Ok(overlay)
}

fn write_third_party_overlay(source: &Path, overlay: &Path) -> Result<(), String> {
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
            write_test_filtered_directory(&entry.path(), &destination)?;
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
    const MARKER: &str = "  using value_type = T;\n  using difference_type = std::ptrdiff_t;";
    const PATCH: &str = "  using value_type = T;\n  // Older libc++ releases consult pointer_traits while checking the C++20\n  // contiguous_iterator concept. This alias is semantically identical to\n  // value_type and makes that implementation path well-formed.\n  using element_type = T;\n  using difference_type = std::ptrdiff_t;";

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

    let header = fs::read_to_string(source_directory.join("cxx.h"))
        .map_err(display_error("read the cxx bridge header"))?;
    if !header.contains(MARKER) {
        return Err("the cxx Slice iterator changed around its type aliases".to_owned());
    }
    let header = header.replacen(MARKER, PATCH, 1);
    write_if_changed(
        &overlay_directory.join("cxx.h"),
        header.as_bytes(),
        "write the libc++ compatibility cxx bridge header",
    )
}

fn write_test_filtered_directory(source: &Path, overlay: &Path) -> Result<(), String> {
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
            || (source.ends_with("base/allocator/partition_allocator")
                && entry.file_name() == "partition_alloc.gni")
            || (source.ends_with("base/allocator/partition_allocator/src/partition_alloc")
                && entry.file_name() == "aarch64_support.h")
            || (source.ends_with("base/process") && entry.file_name() == "set_process_title.cc")
            || (source.ends_with("net/dns/public") && entry.file_name() == "scoped_res_state.cc")
            || (source.ends_with("base/debug") && entry.file_name() == "stack_trace_posix.cc")
            || (source.ends_with("net/cert/internal")
                && entry.file_name() == "system_trust_store.cc")
        {
            continue;
        }
        let destination = overlay.join(entry.file_name());
        if entry.path().is_dir() {
            write_test_filtered_directory(&entry.path(), &destination)?;
        } else {
            ensure_symlink(&entry.path(), &destination)?;
        }
    }
    if source.file_name().is_some_and(|name| name == "base") && source.join("test").is_dir() {
        write_test_filtered_directory(&source.join("test"), &overlay.join("test"))?;
    }
    let build_path = source.join("BUILD.gn");
    if !build_path.is_file() {
        return Ok(());
    }
    let build = fs::read_to_string(&build_path).map_err(display_error("read upstream BUILD.gn"))?;
    let mut build = if source.ends_with("third_party/googletest") {
        build
    } else {
        remove_testonly_gn_blocks(build)
    };
    if source.file_name().is_some_and(|name| name == "net") {
        const UNUSED_TEST_TYPE: &str = "if (is_cronet_build) {\n  _test_target_type = \"cronet_test\"\n} else {\n  _test_target_type = \"test\"\n}\n\n";
        if !build.contains(UNUSED_TEST_TYPE) {
            return Err(
                "upstream net/BUILD.gn changed around its unit-test target type".to_owned(),
            );
        }
        build = build.replacen(UNUSED_TEST_TYPE, "", 1);
        build = remove_named_gn_blocks(build, "component", "extras")?;
        build = remove_named_gn_blocks(build, "component", "shared_dictionary_info")?;
    }
    if source.ends_with("net/third_party/quiche") {
        build = remove_named_gn_blocks(build, "component", "blind_sign_auth")?;
        build = remove_named_gn_blocks(build, "proto_library", "blind_sign_auth_proto")?;
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

fn write_build_overlay(source: &Path, overlay: &Path) -> Result<(), String> {
    let source_build = source.join("build");
    let overlay_build = overlay.join("build");
    replace_generated_link_with_directory(&overlay_build)?;
    for entry in
        fs::read_dir(&source_build).map_err(display_error("list Chromium build directory"))?
    {
        let entry = entry.map_err(display_error("read Chromium build entry"))?;
        if matches!(entry.file_name().to_str(), Some("BUILD.gn" | "config")) {
            continue;
        }
        ensure_symlink(&entry.path(), &overlay_build.join(entry.file_name()))?;
    }
    let build = fs::read_to_string(source_build.join("BUILD.gn"))
        .map_err(display_error("read upstream build/BUILD.gn"))?;
    let build = remove_named_gn_blocks(build, "group", "gold_common_pytype")?;
    fs::write(overlay_build.join("BUILD.gn"), build)
        .map_err(display_error("write Cronet-only build/BUILD.gn"))
}

fn write_buildtools_overlay(source: &Path, overlay: &Path) -> Result<(), String> {
    const MUSL_MARKER: &str = "#define _LIBCPP_HAS_MUSL_LIBC 0";
    const MUSL_PATCH: &str = "#if defined(__OHOS__)\n#define _LIBCPP_HAS_MUSL_LIBC 1\n#else\n#define _LIBCPP_HAS_MUSL_LIBC 0\n#endif";

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
        if entry.file_name() != "__config_site" {
            ensure_symlink(&entry.path(), &overlay_libcxx.join(entry.file_name()))?;
        }
    }
    let config = fs::read_to_string(source_libcxx.join("__config_site"))
        .map_err(display_error("read Chromium libc++ configuration"))?;
    if !config.contains(MUSL_MARKER) {
        return Err("Chromium libc++ configuration changed around its musl setting".to_owned());
    }
    let config = config.replacen(MUSL_MARKER, MUSL_PATCH, 1);
    write_if_changed(
        &overlay_libcxx.join("__config_site"),
        config.as_bytes(),
        "write the OHOS libc++ configuration",
    )
}

fn patch_ohos_partition_alloc(source: &Path, overlay: &Path) -> Result<(), String> {
    const MTE_MARKER: &str = "has_memory_tagging = current_cpu == \"arm64\" && is_clang && !is_asan &&\n                     !is_hwasan && (is_linux || is_android)";
    const MTE_PATCH: &str = "has_memory_tagging = current_cpu == \"arm64\" && is_clang && !is_asan &&\n                     !is_hwasan && !cronet_target_ohos &&\n                     (is_linux || is_android)";
    const MTE_FLAGS_MARKER: &str = "if (current_cpu == \"arm64\" && is_clang &&\n        (is_linux || is_chromeos || is_android || is_fuchsia)) {";
    const MTE_FLAGS_PATCH: &str = "if (current_cpu == \"arm64\" && is_clang && !cronet_target_ohos &&\n        (is_linux || is_chromeos || is_android || is_fuchsia)) {";
    const IFUNC_MARKER: &str =
        "#if PA_BUILDFLAG(IS_ANDROID) || PA_BUILDFLAG(IS_LINUX)\n#define HAS_HW_CAPS";
    const IFUNC_PATCH: &str = "#if (PA_BUILDFLAG(IS_ANDROID) || PA_BUILDFLAG(IS_LINUX)) && __has_include(<sys/ifunc.h>)\n#define HAS_HW_CAPS";

    let source_partition = source.join("base/allocator/partition_allocator");
    let overlay_partition = overlay.join("base/allocator/partition_allocator");

    let gni_path = source_partition.join("partition_alloc.gni");
    let gni = fs::read_to_string(&gni_path)
        .map_err(display_error("read PartitionAlloc configuration"))?;
    if !gni.contains(MTE_MARKER) {
        return Err("PartitionAlloc changed around its memory-tagging setting".to_owned());
    }
    let gni = gni.replacen(MTE_MARKER, MTE_PATCH, 1);
    let overlay_gni = overlay_partition.join("partition_alloc.gni");
    replace_generated_link_with_file(&overlay_gni)?;
    write_if_changed(
        &overlay_gni,
        gni.as_bytes(),
        "write the OHOS PartitionAlloc configuration",
    )?;

    let overlay_build = overlay_partition.join("src/partition_alloc/BUILD.gn");
    let build = fs::read_to_string(&overlay_build)
        .map_err(display_error("read filtered PartitionAlloc BUILD.gn"))?;
    if !build.contains(MTE_FLAGS_MARKER) {
        return Err("PartitionAlloc changed around its MTE compiler flags".to_owned());
    }
    let build = build.replacen(MTE_FLAGS_MARKER, MTE_FLAGS_PATCH, 1);
    write_if_changed(
        &overlay_build,
        build.as_bytes(),
        "write the OHOS PartitionAlloc build configuration",
    )?;

    let header_path = source_partition.join("src/partition_alloc/aarch64_support.h");
    let header = fs::read_to_string(&header_path)
        .map_err(display_error("read PartitionAlloc AArch64 support header"))?;
    if !header.contains(IFUNC_MARKER) {
        return Err("PartitionAlloc changed around its Linux ifunc support".to_owned());
    }
    let header = header.replacen(IFUNC_MARKER, IFUNC_PATCH, 1);
    let overlay_header = overlay_partition.join("src/partition_alloc/aarch64_support.h");
    replace_generated_link_with_file(&overlay_header)?;
    write_if_changed(
        &overlay_header,
        header.as_bytes(),
        "write the portable PartitionAlloc AArch64 support header",
    )
}

fn patch_ohos_base_process(source: &Path, overlay: &Path) -> Result<(), String> {
    const MARKER: &str = "#if BUILDFLAG(IS_POSIX) && !BUILDFLAG(IS_APPLE) && !BUILDFLAG(IS_SOLARIS) && \\\n    !BUILDFLAG(IS_ANDROID) && !BUILDFLAG(IS_FUCHSIA)";
    const PATCH: &str = "#if BUILDFLAG(IS_POSIX) && !BUILDFLAG(IS_APPLE) && !BUILDFLAG(IS_SOLARIS) && \\\n    !BUILDFLAG(IS_ANDROID) && !BUILDFLAG(IS_FUCHSIA) && !defined(__OHOS__)";

    let relative = Path::new("base/process/set_process_title.cc");
    let contents = fs::read_to_string(source.join(relative))
        .map_err(display_error("read Chromium process-title implementation"))?;
    if !contents.contains(MARKER) {
        return Err("Chromium changed around its POSIX process-title implementation".to_owned());
    }
    let contents = contents.replacen(MARKER, PATCH, 1);
    let overlay_path = overlay.join(relative);
    replace_generated_link_with_file(&overlay_path)?;
    write_if_changed(
        &overlay_path,
        contents.as_bytes(),
        "write the portable process-title implementation",
    )
}

#[allow(clippy::too_many_lines)] // Exact upstream anchors and the generated OHOS implementation stay together.
fn patch_ohos_resolver(source: &Path, overlay: &Path) -> Result<(), String> {
    const INCLUDE_MARKER: &str = "#include <cstring>\n#include <memory>";
    const INCLUDE_PATCH: &str = "#include <arpa/inet.h>\n\n#include <algorithm>\n#include <cstdio>\n#include <cstdlib>\n#include <cstring>\n#include <memory>\n#include <new>";
    const NAMESPACE_MARKER: &str = "namespace net {\n\nScopedResState::ScopedResState() {";
    const OHOS_PARSER: &str = r#"namespace net {

#if defined(__OHOS__)
namespace {

// The OHOS musl resolver deliberately exposes only the process-global
// res_init() API; its res_state type is retained for source compatibility but
// res_ninit()/res_nclose() are not exported. Chromium needs an owned snapshot
// of resolv.conf for the built-in DNS client, so construct that snapshot here.
int InitializeOhosResolverState(struct __res_state* state) {
  UNSAFE_TODO(memset(state, 0, sizeof(*state)));
  state->retrans = RES_TIMEOUT;
  state->retry = RES_DFLRETRY;
  state->ndots = 1;
  state->options = RES_RECURSE | RES_DEFNAMES | RES_DNSRCH;

  FILE* file = fopen(_PATH_RESCONF, "r");
  if (!file) {
    return -1;
  }

  int search_count = 0;
  char* search_storage = state->defdname;
  size_t search_storage_left = sizeof(state->defdname);
  char line[512];
  while (fgets(line, sizeof(line), file)) {
    if (char* comment = strchr(line, '#')) {
      *comment = '\0';
    }
    char* save = nullptr;
    char* directive = strtok_r(line, " \t\r\n", &save);
    if (!directive) {
      continue;
    }

    if (strcmp(directive, "nameserver") == 0 && state->nscount < MAXNS) {
      char* address = strtok_r(nullptr, " \t\r\n", &save);
      if (!address) {
        continue;
      }
      const int index = state->nscount;
      auto& ipv4 = state->nsaddr_list[index];
      if (inet_pton(AF_INET, address, &ipv4.sin_addr) == 1) {
        ipv4.sin_family = AF_INET;
        ipv4.sin_port = htons(53);
        ++state->nscount;
        continue;
      }
      auto* ipv6 = new (std::nothrow) sockaddr_in6{};
      if (ipv6 && inet_pton(AF_INET6, address, &ipv6->sin6_addr) == 1) {
        ipv6->sin6_family = AF_INET6;
        ipv6->sin6_port = htons(53);
        state->_u._ext.nsaddrs[index] = ipv6;
        ++state->nscount;
      } else {
        delete ipv6;
      }
      continue;
    }

    if (strcmp(directive, "search") == 0 ||
        strcmp(directive, "domain") == 0) {
      search_count = 0;
      search_storage = state->defdname;
      search_storage_left = sizeof(state->defdname);
      UNSAFE_TODO(memset(state->dnsrch, 0, sizeof(state->dnsrch)));
      while (search_count < MAXDNSRCH) {
        char* domain = strtok_r(nullptr, " \t\r\n", &save);
        if (!domain) {
          break;
        }
        const size_t length = strlen(domain) + 1;
        if (length > search_storage_left) {
          break;
        }
        UNSAFE_TODO(memcpy(search_storage, domain, length));
        state->dnsrch[search_count++] = search_storage;
        search_storage += length;
        search_storage_left -= length;
        if (strcmp(directive, "domain") == 0) {
          break;
        }
      }
      continue;
    }

    if (strcmp(directive, "options") == 0) {
      for (char* option = strtok_r(nullptr, " \t\r\n", &save); option;
           option = strtok_r(nullptr, " \t\r\n", &save)) {
        if (strncmp(option, "ndots:", 6) == 0) {
          state->ndots = std::min<unsigned long>(
              strtoul(option + 6, nullptr, 10), RES_MAXNDOTS);
        } else if (strncmp(option, "attempts:", 9) == 0) {
          state->retry = std::min<unsigned long>(
              strtoul(option + 9, nullptr, 10), RES_MAXRETRY);
        } else if (strncmp(option, "timeout:", 8) == 0) {
          state->retrans = std::min<unsigned long>(
              strtoul(option + 8, nullptr, 10), RES_MAXRETRANS);
        } else if (strcmp(option, "rotate") == 0) {
          state->options |= RES_ROTATE;
        }
      }
    }
  }
  fclose(file);

  if (state->nscount == 0) {
    return -1;
  }
  state->options |= RES_INIT;
  return 0;
}

}  // namespace
#endif  // defined(__OHOS__)

ScopedResState::ScopedResState() {"#;
    const CONSTRUCTOR_MARKER: &str = "#if BUILDFLAG(IS_OPENBSD) || BUILDFLAG(IS_FUCHSIA)\n  // Note: res_ninit in glibc always returns 0 and sets RES_INIT.";
    const CONSTRUCTOR_PATCH: &str = "#if defined(__OHOS__)\n  res_init_result_ = InitializeOhosResolverState(&res_);\n#elif BUILDFLAG(IS_OPENBSD) || BUILDFLAG(IS_FUCHSIA)\n  // Note: res_ninit in glibc always returns 0 and sets RES_INIT.";
    const DESTRUCTOR_MARKER: &str = "ScopedResState::~ScopedResState() {\n#if !BUILDFLAG(IS_OPENBSD) && !BUILDFLAG(IS_FUCHSIA)\n\n  // Prefer res_ndestroy where available.";
    const DESTRUCTOR_PATCH: &str = "ScopedResState::~ScopedResState() {\n#if defined(__OHOS__)\n  for (int i = 0; i < res_.nscount; ++i) {\n    if (!res_.nsaddr_list[i].sin_family) {\n      delete res_._u._ext.nsaddrs[i];\n    }\n  }\n#elif !BUILDFLAG(IS_OPENBSD) && !BUILDFLAG(IS_FUCHSIA)\n\n  // Prefer res_ndestroy where available.";

    let relative = Path::new("net/dns/public/scoped_res_state.cc");
    let contents = fs::read_to_string(source.join(relative))
        .map_err(display_error("read Chromium resolver state implementation"))?;
    for marker in [
        INCLUDE_MARKER,
        NAMESPACE_MARKER,
        CONSTRUCTOR_MARKER,
        DESTRUCTOR_MARKER,
    ] {
        if !contents.contains(marker) {
            return Err("Chromium changed around its POSIX resolver state".to_owned());
        }
    }
    let contents = contents
        .replacen(INCLUDE_MARKER, INCLUDE_PATCH, 1)
        .replacen(NAMESPACE_MARKER, OHOS_PARSER, 1)
        .replacen(CONSTRUCTOR_MARKER, CONSTRUCTOR_PATCH, 1)
        .replacen(DESTRUCTOR_MARKER, DESTRUCTOR_PATCH, 1);
    let overlay_path = overlay.join(relative);
    replace_generated_link_with_file(&overlay_path)?;
    write_if_changed(
        &overlay_path,
        contents.as_bytes(),
        "write the OHOS resolver state implementation",
    )
}

fn patch_ohos_link_closure(source: &Path, overlay: &Path) -> Result<(), String> {
    const BASE_BUILD_MARKER: &str = "    ]\n    if (!is_cronet_build) {\n      # These dependencies are not required on Android.";
    const BASE_BUILD_PATCH: &str = "    ]\n    if (cronet_target_ohos) {\n      sources += [ \"nix/xdg_util_ohos.cc\" ]\n    }\n    if (!is_cronet_build) {\n      # These dependencies are not required on Android.";
    const NET_BUILD_MARKER: &str =
        "      \"proxy_resolution/proxy_config_service_linux.h\",\n    ]\n    if (use_glib) {";
    const NET_BUILD_PATCH: &str = "      \"proxy_resolution/proxy_config_service_linux.h\",\n    ]\n    if (cronet_target_ohos) {\n      sources += [ \"cert/test_root_certs_builtin.cc\" ]\n    }\n    if (use_glib) {";
    const XDG_OHOS: &str = r#"// Generated by cronet-rs for non-desktop OHOS targets.
#include "base/nix/xdg_util.h"

namespace base::nix {

DesktopEnvironment GetDesktopEnvironment(Environment*) {
  return DESKTOP_ENVIRONMENT_OTHER;
}

}  // namespace base::nix
"#;
    const TRUST_MARKER: &str = "                                  TrustStoreNSS::UseTrustFromAllUserSlots()));\n}\n\n#elif BUILDFLAG(IS_MAC)";
    const TRUST_PATCH: &str = r"                                  TrustStoreNSS::UseTrustFromAllUserSlots()));
}

#elif defined(__OHOS__)

std::unique_ptr<SystemTrustStore> CreateSslSystemTrustStoreChromeRoot(
    std::unique_ptr<TrustStoreChrome> chrome_root) {
  // OHOS does not expose a platform trust-store API through its Native SDK.
  // Retain Chromium's versioned public Chrome Root Store.
  return CreateChromeOnlySystemTrustStore(std::move(chrome_root));
}

#elif BUILDFLAG(IS_MAC)";
    const STACK_MARKER: &str = "#if defined(HAVE_BACKTRACE)\nvoid StackTrace::OutputToStreamWithPrefixImpl(\n    std::ostream* os,\n    cstring_view prefix_string) const {\n  StreamBacktraceOutputHandler handler(os);\n  ProcessBacktrace(addresses(), prefix_string, &handler);\n}\n#endif";
    const STACK_PATCH: &str = r"#if defined(HAVE_BACKTRACE)
void StackTrace::OutputToStreamWithPrefixImpl(
    std::ostream* os,
    cstring_view prefix_string) const {
  StreamBacktraceOutputHandler handler(os);
  ProcessBacktrace(addresses(), prefix_string, &handler);
}
#elif defined(__OHOS__)
void StackTrace::OutputToStreamWithPrefixImpl(
    std::ostream* os,
    cstring_view prefix_string) const {
  for (const void* address : addresses()) {
    *os << prefix_string << address << '\n';
  }
}
#endif";

    let base_build_path = overlay.join("base/BUILD.gn");
    let base_build = fs::read_to_string(&base_build_path)
        .map_err(display_error("read filtered base BUILD.gn"))?;
    if !base_build.contains(BASE_BUILD_MARKER) {
        return Err("Chromium base BUILD.gn changed around its Linux sources".to_owned());
    }
    write_if_changed(
        &base_build_path,
        base_build
            .replacen(BASE_BUILD_MARKER, BASE_BUILD_PATCH, 1)
            .as_bytes(),
        "write the OHOS base source selection",
    )?;
    write_if_changed(
        &overlay.join("base/nix/xdg_util_ohos.cc"),
        XDG_OHOS.as_bytes(),
        "write the OHOS desktop-environment stub",
    )?;

    let net_build_path = overlay.join("net/BUILD.gn");
    let net_build =
        fs::read_to_string(&net_build_path).map_err(display_error("read filtered net BUILD.gn"))?;
    if !net_build.contains(NET_BUILD_MARKER) {
        return Err("Chromium net BUILD.gn changed around its Linux sources".to_owned());
    }
    write_if_changed(
        &net_build_path,
        net_build
            .replacen(NET_BUILD_MARKER, NET_BUILD_PATCH, 1)
            .as_bytes(),
        "write the OHOS net source selection",
    )?;

    patch_exact_source_file(
        source,
        overlay,
        "net/cert/internal/system_trust_store.cc",
        TRUST_MARKER,
        TRUST_PATCH,
        "write the OHOS Chrome Root Store implementation",
    )?;
    patch_exact_source_file(
        source,
        overlay,
        "base/debug/stack_trace_posix.cc",
        STACK_MARKER,
        STACK_PATCH,
        "write the OHOS stack-trace stream implementation",
    )
}

fn patch_exact_source_file(
    source: &Path,
    overlay: &Path,
    relative: &str,
    marker: &str,
    patch: &str,
    description: &'static str,
) -> Result<(), String> {
    let contents = fs::read_to_string(source.join(relative))
        .map_err(display_error("read Chromium platform source"))?;
    if !contents.contains(marker) {
        return Err(format!("Chromium changed around `{relative}`"));
    }
    let overlay_path = overlay.join(relative);
    replace_generated_link_with_file(&overlay_path)?;
    write_if_changed(
        &overlay_path,
        contents.replacen(marker, patch, 1).as_bytes(),
        description,
    )
}

fn write_ohos_overlay(source: &Path, overlay: &Path) -> Result<(), String> {
    write_ohos_toolchain(overlay)?;
    patch_ohos_rust_target(source, overlay)?;
    patch_ohos_compiler_config(source, overlay)
}

fn write_ohos_toolchain(overlay: &Path) -> Result<(), String> {
    let directory = overlay.join("cronet_rs_ohos_toolchain");
    replace_generated_link_with_directory(&directory)?;
    let contents = r#"# Generated by cronet-rs. This is intentionally isolated from Chromium.
import("//build/toolchain/gcc_toolchain.gni")

declare_args() {
  cronet_ohos_sdk_native = ""
  cronet_ohos_compiler_resource_dir = ""
  cronet_ohos_target_runtime_dir = ""
}

assert(cronet_ohos_sdk_native != "", "cronet-src requires the OHOS Native SDK")
assert(cronet_ohos_llvm_triple != "", "cronet-src requires an OHOS LLVM target")
assert(cronet_ohos_compiler_resource_dir != "", "cronet-src requires the OHOS compiler runtime")
assert(cronet_ohos_target_runtime_dir != "", "cronet-src requires the OHOS target runtime")

_clang_base = "//third_party/llvm-build/Release+Asserts"
_bin = rebase_path(_clang_base + "/bin", root_build_dir)
_sysroot = cronet_ohos_sdk_native + "/sysroot"
_resource_dir = rebase_path(cronet_ohos_compiler_resource_dir, root_build_dir)
_target_runtime_dir = rebase_path(cronet_ohos_target_runtime_dir, root_build_dir)
_rust_triple = cronet_ohos_rust_triple
_llvm_triple = cronet_ohos_llvm_triple
_target_flag = "--target=" + cronet_ohos_llvm_triple

gcc_toolchain("ohos") {
  cc = _bin + "/clang"
  cxx = _bin + "/clang++"
  ld = cxx
  ar = _bin + "/llvm-ar"
  nm = _bin + "/llvm-nm"
  readelf = _bin + "/llvm-readelf"
  strip = _bin + "/llvm-strip"

  extra_cflags = _target_flag + " --sysroot=" + _sysroot +
                 " -D__MUSL__ -DCRONET_TARGET_OHOS" +
                 " -fno-addrsig -Wno-unknown-warning-option"
  extra_cppflags = "-Qunused-arguments"
  extra_cxxflags = extra_cflags
  extra_asmflags = _target_flag + " --sysroot=" + _sysroot
  # Chromium's upstream Clang understands the OHOS ABI but does not know the
  # SDK's compiler-runtime layout. Keep the compiler pinned while selecting
  # the ABI support archives shipped for the target SDK.
  extra_ldflags = _target_flag + " --sysroot=" + _sysroot + " -fuse-ld=lld" +
                  " -nostartfiles -resource-dir=" + _resource_dir +
                  " -L" + _target_runtime_dir

  toolchain_args = {
    current_cpu = target_cpu
    current_os = "linux"
    cronet_target_ohos = true
    cronet_ohos_llvm_triple = _llvm_triple
    cronet_ohos_rust_triple = _rust_triple
    clang_base_path = _clang_base
    is_clang = true
    use_remoteexec = false
  }
}
"#;
    write_if_changed(
        &directory.join("BUILD.gn"),
        contents.as_bytes(),
        "write the OHOS GN toolchain",
    )
}

fn patch_ohos_rust_target(source: &Path, overlay: &Path) -> Result<(), String> {
    const BUILDCONFIG_MARKER: &str =
        "declare_args() {\n  # Set to enable the official build level of optimization.";
    const BUILDCONFIG_PATCH: &str = "declare_args() {\n  # cronet-rs models OHOS as Linux for Chromium's existing POSIX graph.\n  # Keep the actual target identity globally visible to compatibility logic.\n  cronet_target_ohos = false\n  cronet_ohos_llvm_triple = \"\"\n  cronet_ohos_rust_triple = \"\"\n\n  # Set to enable the official build level of optimization.";
    const RUST_TARGET_MARKER: &str = "rust_abi_target = \"\"\nif (is_linux || is_chromeos) {";
    const RUST_TARGET_PATCH: &str = "rust_abi_target = \"\"\nif (cronet_target_ohos && is_a_target_toolchain) {\n  assert(cronet_ohos_rust_triple != \"\", \"cronet-src requires an OHOS Rust target\")\n  rust_abi_target = cronet_ohos_rust_triple\n} else if (is_linux || is_chromeos) {";
    const KNOWN_TARGET_MARKER: &str = "  assert(_is_rust_abi_target_a_known_triple,\n         \"`${rust_abi_target}` needs to be added to \" +";
    const KNOWN_TARGET_PATCH: &str = "  assert(_is_rust_abi_target_a_known_triple || cronet_target_ohos,\n         \"`${rust_abi_target}` needs to be added to \" +";

    let source_config = source.join("build/config");
    let overlay_config = overlay.join("build/config");
    replace_generated_link_with_directory(&overlay_config)?;
    for entry in
        fs::read_dir(&source_config).map_err(display_error("list Chromium build config"))?
    {
        let entry = entry.map_err(display_error("read Chromium build config entry"))?;
        if matches!(
            entry.file_name().to_str(),
            Some("BUILDCONFIG.gn" | "rust.gni" | "compiler")
        ) {
            continue;
        }
        ensure_symlink(&entry.path(), &overlay_config.join(entry.file_name()))?;
    }

    let build_config_path = source_config.join("BUILDCONFIG.gn");
    let build_config = fs::read_to_string(&build_config_path)
        .map_err(display_error("read Chromium BUILDCONFIG.gn"))?;
    if !build_config.contains(BUILDCONFIG_MARKER) {
        return Err("Chromium BUILDCONFIG.gn changed around its global arguments".to_owned());
    }
    let build_config = build_config.replacen(BUILDCONFIG_MARKER, BUILDCONFIG_PATCH, 1);
    let overlay_build_config = overlay_config.join("BUILDCONFIG.gn");
    replace_generated_link_with_file(&overlay_build_config)?;
    write_if_changed(
        &overlay_build_config,
        build_config.as_bytes(),
        "write the global OHOS build arguments",
    )?;

    let rust_path = source_config.join("rust.gni");
    let rust = fs::read_to_string(&rust_path).map_err(display_error("read Chromium rust.gni"))?;
    if !rust.contains(RUST_TARGET_MARKER) || !rust.contains(KNOWN_TARGET_MARKER) {
        return Err("Chromium rust.gni changed around its Rust ABI mapping".to_owned());
    }
    let rust = rust
        .replacen(RUST_TARGET_MARKER, RUST_TARGET_PATCH, 1)
        .replacen(KNOWN_TARGET_MARKER, KNOWN_TARGET_PATCH, 1);
    write_if_changed(
        &overlay_config.join("rust.gni"),
        rust.as_bytes(),
        "write the OHOS Rust ABI patch",
    )
}

fn patch_ohos_compiler_config(source: &Path, overlay: &Path) -> Result<(), String> {
    const TARGET_MARKER: &str =
        "config(\"compiler\") {\n  asmflags = []\n  cflags = []\n  cflags_c = []";
    const TARGET_PATCH: &str = "config(\"compiler\") {\n  asmflags = []\n  cflags = []\n  if (cronet_target_ohos) {\n    assert(cronet_ohos_llvm_triple != \"\", \"cronet-src requires an OHOS LLVM target\")\n    # Action-based Clang consumers such as bindgen do not inherit the compiler\n    # executable's extra flags, so the ABI target must also be a config flag.\n    cflags += [ \"--target=\" + cronet_ohos_llvm_triple ]\n  }\n  cflags_c = []";
    const RUST_CHECK_MARKER: &str = "if (toolchain_has_rust && _perform_consistency_checks &&\n        !rust_force_head_revision) {";
    const RUST_CHECK_PATCH: &str = "if (toolchain_has_rust && _perform_consistency_checks &&\n        !rust_force_head_revision && !cronet_target_ohos) {";
    const CREL_MARKER: &str =
        "if (is_linux && use_lld && current_cpu != \"arm\" && current_cpu != \"s390x\") {";
    const CREL_PATCH: &str = "if (is_linux && use_lld && !cronet_target_ohos &&\n        current_cpu != \"arm\" && current_cpu != \"s390x\") {";
    const CXX23_MARKER: &str = "if (use_cxx23) {\n      cflags_cc += [ \"-std=c++23\" ]";
    const CXX23_PATCH: &str = "if (use_cxx23) {\n      if (cronet_target_ohos) {\n        cflags_cc += [ \"-std=c++2b\" ]\n      } else {\n        cflags_cc += [ \"-std=c++23\" ]\n      }";
    const POSIX_CXX23_MARKER: &str =
        "if (use_cxx23) {\n      cflags_cc += [ \"-std=${standard_prefix}++23\" ]";
    const POSIX_CXX23_PATCH: &str = "if (use_cxx23) {\n      if (cronet_target_ohos) {\n        cflags_cc += [ \"-std=${standard_prefix}++2b\" ]\n      } else {\n        cflags_cc += [ \"-std=${standard_prefix}++23\" ]\n      }";
    const SPLIT_THRESHOLD_MARKER: &str = "if (default_toolchain != \"//build/toolchain/cros:target\") {\n      cflags += [\n        \"-mllvm\",\n        \"-split-threshold-for-reg-with-hint=0\",";
    const SPLIT_THRESHOLD_PATCH: &str = "if (default_toolchain != \"//build/toolchain/cros:target\" &&\n        !cronet_target_ohos) {\n      cflags += [\n        \"-mllvm\",\n        \"-split-threshold-for-reg-with-hint=0\",";
    const GNU_TARGET_MARKERS: [(&str, &str); 3] = [
        (
            "if (is_clang && !is_android && !is_fuchsia && !is_chromeos_device) {\n        cflags += [ \"--target=x86_64-unknown-linux-gnu\" ]",
            "if (is_clang && !is_android && !is_fuchsia && !is_chromeos_device &&\n          !cronet_target_ohos) {\n        cflags += [ \"--target=x86_64-unknown-linux-gnu\" ]",
        ),
        (
            "if (is_clang && !is_android && !is_chromeos_device) {\n        cflags += [ \"--target=arm-linux-gnueabihf\" ]",
            "if (is_clang && !is_android && !is_chromeos_device &&\n          !cronet_target_ohos) {\n        cflags += [ \"--target=arm-linux-gnueabihf\" ]",
        ),
        (
            "if (is_clang && !is_android && !is_fuchsia && !is_chromeos_device) {\n        cflags += [ \"--target=aarch64-linux-gnu\" ]",
            "if (is_clang && !is_android && !is_fuchsia && !is_chromeos_device &&\n          !cronet_target_ohos) {\n        cflags += [ \"--target=aarch64-linux-gnu\" ]",
        ),
    ];

    let source_compiler = source.join("build/config/compiler");
    let overlay_compiler = overlay.join("build/config/compiler");
    replace_generated_link_with_directory(&overlay_compiler)?;
    for entry in
        fs::read_dir(&source_compiler).map_err(display_error("list Chromium compiler config"))?
    {
        let entry = entry.map_err(display_error("read Chromium compiler config entry"))?;
        if entry.file_name() == "BUILD.gn" {
            continue;
        }
        ensure_symlink(&entry.path(), &overlay_compiler.join(entry.file_name()))?;
    }

    let compiler = fs::read_to_string(source_compiler.join("BUILD.gn"))
        .map_err(display_error("read Chromium compiler BUILD.gn"))?;
    if !compiler.contains(TARGET_MARKER)
        || !compiler.contains(RUST_CHECK_MARKER)
        || !compiler.contains(CREL_MARKER)
        || !compiler.contains(CXX23_MARKER)
        || !compiler.contains(POSIX_CXX23_MARKER)
        || !compiler.contains(SPLIT_THRESHOLD_MARKER)
    {
        return Err(
            "Chromium compiler config changed around an OHOS compatibility anchor".to_owned(),
        );
    }
    let mut compiler = compiler
        .replacen(TARGET_MARKER, TARGET_PATCH, 1)
        .replacen(RUST_CHECK_MARKER, RUST_CHECK_PATCH, 1)
        .replacen(CREL_MARKER, CREL_PATCH, 1)
        .replacen(CXX23_MARKER, CXX23_PATCH, 1)
        .replacen(POSIX_CXX23_MARKER, POSIX_CXX23_PATCH, 1)
        .replacen(SPLIT_THRESHOLD_MARKER, SPLIT_THRESHOLD_PATCH, 1);
    for (marker, patch) in GNU_TARGET_MARKERS {
        if !compiler.contains(marker) {
            return Err("Chromium compiler config changed around a Linux target triple".to_owned());
        }
        compiler = compiler.replacen(marker, patch, 1);
    }
    write_if_changed(
        &overlay_compiler.join("BUILD.gn"),
        compiler.as_bytes(),
        "write the OHOS Rust compiler patch",
    )
}

fn write_cronet_component_overlay(source: &Path, overlay: &Path) -> Result<(), String> {
    const INCLUDE_MARKER: &str = "#include \"base/at_exit.h\"\n";
    const INIT_MARKER: &str = "#endif\n\n  base::FeatureList::InitInstance";

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
            write_test_filtered_directory(&entry.path(), &destination)?;
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
        if matches!(
            entry.file_name().to_str(),
            Some("BUILD.gn" | "native" | "cronet_global_state_stubs.cc")
        ) {
            continue;
        }
        ensure_symlink(&entry.path(), &overlay_cronet.join(entry.file_name()))?;
    }

    // The final native-C-API revision predates a net/ change that started
    // consulting base::CommandLine while creating the Chrome root store. The
    // native standalone initialization stub must initialize that singleton.
    // Keep this compatibility patch in the generated overlay, never in the
    // pinned upstream checkout.
    let stubs_path = source_cronet.join("cronet_global_state_stubs.cc");
    let mut stubs = fs::read_to_string(&stubs_path)
        .map_err(display_error("read upstream Cronet global-state stubs"))?;
    if !stubs.contains(INCLUDE_MARKER) || !stubs.contains(INIT_MARKER) {
        return Err("upstream Cronet global-state stub layout changed".to_owned());
    }
    stubs = stubs.replacen(
        INCLUDE_MARKER,
        "#include \"base/at_exit.h\"\n#include \"base/command_line.h\"\n",
        1,
    );
    stubs = stubs.replacen(
        INIT_MARKER,
        "#endif\n\n  if (!base::CommandLine::InitializedForCurrentProcess()) {\n    base::CommandLine::Init(0, nullptr);\n  }\n\n  base::FeatureList::InitInstance",
        1,
    );
    let overlay_stubs = overlay_cronet.join("cronet_global_state_stubs.cc");
    replace_generated_link_with_file(&overlay_stubs)?;
    write_if_changed(
        &overlay_stubs,
        stubs.as_bytes(),
        "write compatible Cronet global-state stubs",
    )?;

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
    let native_build = fs::read_to_string(source_native.join("BUILD.gn"))
        .map_err(display_error("read upstream Cronet native BUILD.gn"))?;
    let native_build =
        remove_named_gn_blocks(native_build, "source_set", "cronet_native_unittests")?;
    fs::write(overlay_native.join("BUILD.gn"), native_build)
        .map_err(display_error("write library-only Cronet native BUILD.gn"))?;

    let mut build = fs::read_to_string(source_cronet.join("BUILD.gn"))
        .map_err(display_error("read upstream Cronet BUILD.gn"))?;
    for (kind, name) in [
        ("source_set", "cronet_common_unittests"),
        ("test", "cronet_tests"),
        ("test", "cronet_unittests"),
        ("action", "generate_license"),
        ("copy", "cronet_package_copy"),
        ("copy", "cronet_package_headers"),
        ("group", "cronet_package"),
        ("executable", "cronet_sample"),
        ("test", "cronet_sample_test"),
    ] {
        build = remove_named_gn_blocks(build, kind, name)?;
    }
    for variable in ["_cronet_shared_lib_file_name", "_package_dir"] {
        build = remove_gn_assignment(build, variable)?;
    }
    patch_cronet_shared_library(&mut build)?;
    append_static_gn_target(&mut build);
    fs::write(overlay_cronet.join("BUILD.gn"), build)
        .map_err(display_error("write library-only Cronet BUILD.gn"))
}

fn patch_cronet_shared_library(build: &mut String) -> Result<(), String> {
    const MARKER: &str =
        "  shared_library(\"cronet\") {\n    output_name = _cronet_shared_lib_name";
    const PATCH: &str = "  shared_library(\"cronet\") {\n    output_name = _cronet_shared_lib_name\n    if (is_mac) {\n      # Make a source-built dylib relocatable with the application bundle.\n      ldflags = [ \"-Wl,-install_name,@rpath/$shlib_prefix$_cronet_shared_lib_name$shlib_extension\" ]\n    }";
    if !build.contains(MARKER) {
        return Err("upstream Cronet shared-library target changed".to_owned());
    }
    *build = build.replacen(MARKER, PATCH, 1);
    Ok(())
}

fn append_static_gn_target(build: &mut String) {
    build.push_str(
        r#"

# Complete archive used by cronet-rs' `static` Cargo feature. Unlike a normal
# GN static_library, complete_static_lib includes the transitive source sets
# and static dependencies needed by a non-GN final linker.
if (!is_android) {
  static_library("cronet_static") {
    output_name = "cronet_static_raw"
    output_dir = root_out_dir
    complete_static_lib = true

    deps = [
      "//base",
      "//components/cronet:cronet_common",
      "//components/cronet/native:cronet_native_impl",
      "//net",
    ]

    sources = [ "cronet_global_state_stubs.cc" ]
    configs += [ "//build/config/compiler:no_exit_time_destructors" ]
  }
}
"#,
    );
}

fn remove_gn_assignment(mut source: String, variable: &str) -> Result<String, String> {
    let marker = format!("{variable} =");
    let start = source
        .find(&marker)
        .ok_or_else(|| format!("upstream Cronet BUILD.gn no longer assigns {variable}"))?;
    let line_start = source[..start].rfind('\n').map_or(0, |index| index + 1);
    let end = source[start..]
        .find("\n\n")
        .map(|index| start + index + 2)
        .ok_or_else(|| format!("could not find the end of assignment {variable}"))?;
    source.replace_range(line_start..end, "");
    Ok(source)
}

fn replace_generated_link_with_directory(path: &Path) -> Result<(), String> {
    if let Ok(metadata) = fs::symlink_metadata(path) {
        if metadata.file_type().is_symlink() {
            fs::remove_file(path).map_err(display_error("replace Cronet overlay link"))?;
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

fn replace_generated_link_with_file(path: &Path) -> Result<(), String> {
    if let Ok(metadata) = fs::symlink_metadata(path) {
        if metadata.file_type().is_symlink() {
            fs::remove_file(path).map_err(display_error("replace Cronet overlay link"))?;
        } else if metadata.is_file() {
            return Ok(());
        } else {
            return Err(format!(
                "{} blocks the generated Cronet GN overlay",
                path.display()
            ));
        }
    }
    Ok(())
}

fn write_if_changed(path: &Path, contents: &[u8], action: &'static str) -> Result<(), String> {
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
        "android_library",
        "bundle_data_from_filelist",
        "component",
        "copy",
        "executable",
        "fuzzer_test",
        "generate_jni",
        "group",
        "perfetto_generate_unittests",
        "perfetto_unittest_source_set",
        "shared_library",
        "source_set",
        "static_library",
        "target",
        "test",
    ];

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
                (matches!(kind, "fuzzer_test" | "test")
                    || header.contains("unittest")
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

fn ensure_symlink(target: &Path, link: &Path) -> Result<(), String> {
    if let Ok(metadata) = fs::symlink_metadata(link) {
        if metadata.file_type().is_symlink()
            && fs::read_link(link).is_ok_and(|existing| existing == target)
        {
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
    "src/third_party/boringssl/src",
    "src/third_party/ced/src",
    "src/third_party/compiler-rt/src",
    "src/third_party/cpu_features/src",
    "src/third_party/googletest/src",
    "src/third_party/icu",
    "src/third_party/jsoncpp/source",
    "src/third_party/libc++/src",
    "src/third_party/libc++abi/src",
    "src/third_party/libunwind/src",
    "src/third_party/lss",
    "src/third_party/llvm-build/Release+Asserts",
    "src/third_party/llvm-libc/src",
    "src/third_party/ninja",
    "src/third_party/perfetto",
    "src/third_party/re2/src",
    "src/third_party/rust-toolchain",
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

fn command_stdout(command: &mut Command, description: &str) -> Result<String, String> {
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
    target
        .filter(|target| is_ohos_target(target))
        .map_or(base.clone(), |target| {
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

fn require_file(path: &Path, help: &str) -> Result<(), String> {
    path.is_file()
        .then_some(())
        .ok_or_else(|| format!("{} is missing; {help}", path.display()))
}

fn display_error(action: &'static str) -> impl FnOnce(std::io::Error) -> String {
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
/net/
/testing/
/third_party/
!/third_party/*/
/third_party/abseil-cpp/
/third_party/apple_apsl/
/third_party/boringssl/
/third_party/brotli/
/third_party/ced/
/third_party/compiler-rt/
/third_party/cpu_features/
/third_party/googletest/
/third_party/icu/
/third_party/jni_zero/
/third_party/jsoncpp/
/third_party/libc++/
/third_party/libc++abi/
/third_party/libevent/
/third_party/libunwind/
/third_party/lss/
/third_party/llvm-build/
/third_party/llvm-libc/
/third_party/metrics_proto/
/third_party/modp_b64/
/third_party/perfetto/
/third_party/protobuf/
/third_party/re2/
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
    fn keeps_cronet_deps_and_rejects_browser_deps() {
        assert!(cronet_dependency_allowed("src/third_party/boringssl/src"));
        assert!(cronet_dependency_allowed("src/net/third_party/quiche/src"));
        assert!(cronet_dependency_allowed("src/third_party/ninja"));
        assert!(cronet_dependency_allowed("src/third_party/lss"));
        assert!(!cronet_dependency_allowed("src/buildtools/reclient"));
        assert!(cronet_dependency_allowed(
            "src/build/linux/debian_bullseye_amd64-sysroot"
        ));
        assert!(!cronet_dependency_allowed("src/third_party/dawn"));
        assert!(!cronet_dependency_allowed("src/v8"));
    }

    #[test]
    fn maps_every_published_rust_target_to_gn() {
        let targets = [
            "x86_64-unknown-linux-gnu",
            "aarch64-unknown-linux-gnu",
            "x86_64-apple-darwin",
            "aarch64-apple-darwin",
            "x86_64-pc-windows-msvc",
            "aarch64-pc-windows-msvc",
            "armv7-unknown-linux-ohos",
            "aarch64-unknown-linux-ohos",
            "x86_64-unknown-linux-ohos",
        ];
        for target in targets {
            let arguments = gn_target_args(target).unwrap();
            assert!(arguments[0].starts_with("target_os="));
            assert!(arguments[1].starts_with("target_cpu="));
        }
        assert!(gn_target_args("wasm32-unknown-unknown").is_err());
    }

    #[test]
    fn source_lock_matches_build_constants() {
        let lock = include_str!("../SOURCE.lock");
        assert!(lock.contains(&format!("chromium_revision={CHROMIUM_REVISION}")));
        assert!(lock.contains(&format!("chromium_version={CHROMIUM_VERSION}")));
        assert!(lock.contains("source_layout=vendor/chromium/src"));
    }

    #[test]
    fn source_builder_rejects_unsupported_targets_before_materializing_source() {
        let result = Build::new().target("wasm32-unknown-unknown").build();
        assert!(result.is_err());
    }

    #[test]
    fn discovers_ohos_runtime_only_below_the_selected_sdk() {
        let root = env::temp_dir().join(format!("cronet-src-ohos-sdk-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        for version in ["15.0.4", "22.1.0"] {
            let runtime = root
                .join("llvm/lib/clang")
                .join(version)
                .join("lib/aarch64-linux-ohos");
            fs::create_dir_all(&runtime).unwrap();
            fs::write(runtime.join("libclang_rt.builtins.a"), []).unwrap();
        }

        let selected = ohos_compiler_resource_dir(&root, "aarch64-linux-ohos").unwrap();
        assert_eq!(selected, root.join("llvm/lib/clang/22.1.0"));
        assert!(ohos_compiler_resource_dir(&root, "x86_64-linux-ohos").is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn vendored_tree_excludes_repository_and_build_state() {
        let root = env::temp_dir().join(format!("cronet-src-copy-test-{}", std::process::id()));
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
        let filtered = remove_testonly_gn_blocks(source.to_owned());
        assert!(filtered.contains("component(\"runtime\")"));
        assert!(filtered.contains("${root}/runtime.cc"));
        assert!(!filtered.contains("test_support"));
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
}

use std::{
    env, fs,
    io::Read,
    path::{Path, PathBuf},
};

const SOURCE_ENV: &str = "CRONET_SOURCE_DIR";
const LIB_ENV: &str = "CRONET_LIB_DIR";
const CACHE_ENV: &str = "CRONET_CACHE_DIR";
const NO_LINK_ENV: &str = "CRONET_SYS_NO_LINK";
const OHOS_SDK_NATIVE_ENV: &str = "OHOS_SDK_NATIVE";

fn main() {
    for variable in [
        SOURCE_ENV,
        LIB_ENV,
        CACHE_ENV,
        NO_LINK_ENV,
        OHOS_SDK_NATIVE_ENV,
        "OHOS_NDK_HOME",
        "ANDROID_NDK_HOME",
        "ANDROID_NDK_ROOT",
        "NDK_HOME",
        "ANDROID_API_LEVEL",
        "DEVELOPER_DIR",
        "IPHONEOS_DEPLOYMENT_TARGET",
        "CRONET_CLANG_DIR",
        "CRONET_RUST_BINDGEN",
        "RUSTC",
    ] {
        println!("cargo:rerun-if-env-changed={variable}");
    }
    println!("cargo:rerun-if-changed=wrapper.h");
    println!("cargo:rerun-if-changed=cronet_rs_c.h");
    println!("cargo:rerun-if-changed=native/BUILD.gn");
    println!("cargo:rerun-if-changed=native/cronet_rs_android_jni_onload.cc");
    println!("cargo:rerun-if-changed=native/cronet_rs_android_static_support.cc");
    println!("cargo:rerun-if-changed=native/cronet_rs_bind.cc");
    println!("cargo:rerun-if-changed=native/cronet_rs_ohos.cc");
    println!("cargo:rerun-if-changed=native/cronet_rs_websocket.cc");

    let manifest_dir = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let workspace = manifest_dir
        .parent()
        .and_then(Path::parent)
        .expect("tokio-cronet-sys must remain inside the workspace");
    let target = env::var("TARGET").expect("Cargo did not set TARGET");
    let linkage = if env::var_os("CARGO_FEATURE_STATIC").is_some() {
        tokio_cronet_src::NativeLinkage::Static
    } else {
        tokio_cronet_src::NativeLinkage::Dynamic
    };
    let no_link = env::var_os(NO_LINK_ENV).is_some() || env::var_os("DOCS_RS").is_some();

    if let Some(lib_dir) = env::var_os(LIB_ENV).map(PathBuf::from) {
        let source = configured_source(workspace, &target);
        generate_bindings(&manifest_dir, &source_headers(&source));
        if !no_link {
            let lib_dir = select_library_dir(&lib_dir, linkage);
            emit_link(&lib_dir, &source, linkage);
        }
        return;
    }

    if no_link {
        let source = configured_source(workspace, &target);
        generate_bindings(&manifest_dir, &source_headers(&source));
        println!("cargo:warning=generated Cronet bindings without linking the native library");
        return;
    }

    assert!(
        tokio_cronet_src::native_target_supported(&target),
        "Cronet native libraries are not supported for Rust target `{target}`"
    );

    let native = build_from_source(workspace, &target, linkage);

    generate_bindings(&manifest_dir, &native.headers);
    emit_link(&native.lib_dir, &native.root, linkage);
}

struct NativeInput {
    headers: PathBuf,
    lib_dir: PathBuf,
    root: PathBuf,
}

fn build_from_source(
    workspace: &Path,
    target: &str,
    linkage: tokio_cronet_src::NativeLinkage,
) -> NativeInput {
    let source = source_for_build(workspace, target);
    println!(
        "cargo:warning=compiling Cronet {} {} library from its filtered source tree for {target}",
        tokio_cronet_src::CHROMIUM_VERSION,
        linkage.as_str()
    );
    let artifacts = tokio_cronet_src::Build::new()
        .source_dir(&source)
        .target(target)
        .linkage(linkage)
        .build()
        .unwrap_or_else(|error| panic!("failed to build Cronet from source: {error}"));
    NativeInput {
        headers: artifacts.source_dir().to_owned(),
        lib_dir: artifacts.lib_dir().to_owned(),
        root: artifacts.source_dir().to_owned(),
    }
}

fn configured_source(workspace: &Path, target: &str) -> PathBuf {
    if let Some(source) = env::var_os(SOURCE_ENV) {
        return PathBuf::from(source);
    }
    let repository_source = workspace.join(".cronet/chromium/src");
    if repository_source
        .join("components/cronet/native/include/cronet_c.h")
        .is_file()
    {
        return repository_source;
    }
    tokio_cronet_src::source_dir(target)
}

fn source_for_build(workspace: &Path, target: &str) -> PathBuf {
    configured_source(workspace, target)
}

fn source_headers(source: &Path) -> PathBuf {
    let header = source.join("components/cronet/native/include/cronet_c.h");
    assert!(
        header.is_file(),
        "Cronet header not found at {}. Run `cargo xtask sync --api-only`, set {SOURCE_ENV}, or remove {NO_LINK_ENV} to allow automatic native setup.",
        header.display()
    );
    source.to_owned()
}

fn generate_bindings(manifest_dir: &Path, root: &Path) {
    let (include, grpc_include, generated) = if root.join("components/cronet/native").is_dir() {
        let native = root.join("components/cronet/native");
        (
            native.join("include"),
            root.join("components/grpc_support/include"),
            native.join("generated"),
        )
    } else {
        (
            root.join("include"),
            root.join("include"),
            root.join("generated"),
        )
    };
    for path in [
        include.join("cronet_c.h"),
        include.join("cronet_export.h"),
        grpc_include.join("bidirectional_stream_c.h"),
        generated.join("cronet.idl_c.h"),
    ] {
        assert!(
            path.is_file(),
            "Cronet API file not found at {}",
            path.display()
        );
        println!("cargo:rerun-if-changed={}", path.display());
    }

    let bindings = bindgen::Builder::default()
        .header(manifest_dir.join("wrapper.h").display().to_string())
        // Upstream's generated header uses C++ `bool` without including
        // <stdbool.h>; Cronet itself consumes this public C ABI as C++.
        .clang_args(["-x", "c++", "-std=c++17"])
        .clang_arg(format!("-I{}", include.display()))
        .clang_arg(format!("-I{}", grpc_include.display()))
        .clang_arg(format!("-I{}", generated.display()))
        .clang_arg(format!("-I{}", manifest_dir.display()))
        .allowlist_function("Cronet_.*")
        .allowlist_type("Cronet_.*")
        .allowlist_var("Cronet_.*")
        .allowlist_type("stream_engine")
        .allowlist_function("bidirectional_stream_.*")
        .allowlist_type("bidirectional_stream.*")
        .constified_enum("Cronet_.*")
        .prepend_enum_name(false)
        .derive_default(true)
        .generate_comments(true)
        .layout_tests(false)
        .parse_callbacks(Box::new(bindgen::CargoCallbacks::new()))
        .generate()
        .expect("failed to generate Cronet bindings with libclang");
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    bindings
        .write_to_file(out_dir.join("bindings.rs"))
        .expect("failed to write generated Cronet bindings");
}

fn select_library_dir(base: &Path, linkage: tokio_cronet_src::NativeLinkage) -> PathBuf {
    let nested = base.join(linkage.as_str());
    if nested.is_dir() {
        nested
    } else {
        base.to_owned()
    }
}

fn emit_link(lib_dir: &Path, root: &Path, linkage: tokio_cronet_src::NativeLinkage) {
    assert!(
        native_library_exists(lib_dir, linkage),
        "Cronet {} library not found in {}",
        linkage.as_str(),
        lib_dir.display()
    );
    if linkage == tokio_cronet_src::NativeLinkage::Static {
        assert!(
            portable_static_archive_exists(lib_dir),
            "Cronet static library in {} is a GN thin archive or has an invalid format",
            lib_dir.display()
        );
    }
    println!("cargo:rustc-link-search=native={}", lib_dir.display());
    for entry in fs::read_dir(lib_dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_file())
    {
        println!("cargo:rerun-if-changed={}", entry.display());
    }
    match linkage {
        tokio_cronet_src::NativeLinkage::Dynamic => {
            println!("cargo:rustc-link-lib=dylib={}", dynamic_link_name(lib_dir));
        }
        tokio_cronet_src::NativeLinkage::Static => {
            println!("cargo:rustc-link-lib=static=cronet_static");
            emit_static_link_requirements(lib_dir);
        }
    }
    println!("cargo:root={}", root.display());
    println!("cargo:libdir={}", lib_dir.display());
    println!("cargo:linkage={}", linkage.as_str());
    if env::var("TARGET").is_ok_and(|target| target.contains("android")) {
        let support_jar = lib_dir.join("cronet-android-support.jar");
        assert!(
            support_jar.is_file(),
            "Cronet Android support jar not found at {}",
            support_jar.display()
        );
        // Cargo exposes this to immediate dependants as
        // DEP_CRONET_ANDROID_SUPPORT_JAR. Android packaging tools must dex the
        // jar because native Chromium networking uses Android Java services.
        println!("cargo:android_support_jar={}", support_jar.display());
        let support_dex_jar = lib_dir.join("cronet-android-support.dex.jar");
        assert!(
            support_dex_jar.is_file(),
            "Cronet Android support dex jar not found at {}",
            support_dex_jar.display()
        );
        println!(
            "cargo:android_support_dex_jar={}",
            support_dex_jar.display()
        );
    }
}

fn dynamic_link_name(lib_dir: &Path) -> String {
    if let Ok(entries) = fs::read_dir(lib_dir) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if let Some(link_name) = name.strip_suffix(".dll.lib") {
                return format!("{link_name}.dll");
            }
        }
    }
    format!("cronet.{}", tokio_cronet_src::CHROMIUM_VERSION)
}

fn emit_static_link_requirements(lib_dir: &Path) {
    let manifest = lib_dir.join("cronet-static-link.txt");
    let contents = fs::read_to_string(&manifest).unwrap_or_else(|error| {
        panic!(
            "could not read Cronet static link manifest {}: {error}",
            manifest.display()
        )
    });
    println!("cargo:rerun-if-changed={}", manifest.display());
    for line in contents.lines().map(str::trim) {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(library) = line.strip_prefix("lib=") {
            let library = library.strip_suffix(".lib").unwrap_or(library);
            println!("cargo:rustc-link-lib={library}");
        } else if let Some(name) = line.strip_prefix("linker-script=") {
            // The .so-named input contains an lld script, not an ELF shared
            // object. Passing it through -l makes this requirement propagate
            // from tokio-cronet-sys to the final Cargo link without a runtime dep.
            println!("cargo:rustc-link-lib=dylib={name}");
        } else if let Some(framework) = line.strip_prefix("framework=") {
            println!("cargo:rustc-link-lib=framework={framework}");
        } else {
            panic!(
                "invalid line `{line}` in Cronet static link manifest {}",
                manifest.display()
            );
        }
    }
}

fn native_library_exists(directory: &Path, linkage: tokio_cronet_src::NativeLinkage) -> bool {
    let Ok(entries) = fs::read_dir(directory) else {
        return false;
    };
    entries.flatten().any(|entry| {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let extension = Path::new(name.as_ref())
            .extension()
            .and_then(|value| value.to_str());
        match linkage {
            tokio_cronet_src::NativeLinkage::Dynamic => {
                let cronet_name = name.starts_with("libcronet.") || name.starts_with("cronet.");
                cronet_name
                    && (name.contains(".so.")
                        || extension.is_some_and(|value| {
                            ["so", "dylib", "dll", "lib"]
                                .iter()
                                .any(|expected| value.eq_ignore_ascii_case(expected))
                        }))
            }
            tokio_cronet_src::NativeLinkage::Static => {
                name == "libcronet_static.a" || name == "cronet_static.lib"
            }
        }
    })
}

fn portable_static_archive_exists(directory: &Path) -> bool {
    for name in ["libcronet_static.a", "cronet_static.lib"] {
        let path = directory.join(name);
        let Ok(mut file) = fs::File::open(path) else {
            continue;
        };
        let mut magic = [0_u8; 8];
        if file.read_exact(&mut magic).is_ok() && &magic == b"!<arch>\n" {
            return true;
        }
    }
    false
}

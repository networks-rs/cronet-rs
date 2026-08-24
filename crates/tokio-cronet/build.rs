use std::{env, fs, path::PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=GMSSL_DIR");
    println!("cargo:rerun-if-env-changed=GMSSL_CFLAGS");
    println!("cargo:rerun-if-env-changed=GMSSL_ENABLE_TLS");
    println!("cargo:rerun-if-env-changed=DEP_GMSSL_INCLUDE");

    if env::var_os("CARGO_FEATURE_GMSSL_TLS").is_none() {
        return;
    }

    let installed_prefix = env::var_os("GMSSL_DIR").map(PathBuf::from);
    let include_dir = installed_prefix.as_ref().map_or_else(
        || {
            env::var_os("DEP_GMSSL_INCLUDE")
                .map(PathBuf::from)
                .expect("gmssl-rs-sys did not publish its GmSSL include directory")
        },
        |prefix| prefix.join("include"),
    );
    assert!(
        include_dir.join("gmssl/tls.h").is_file(),
        "GmSSL TLS header is missing at {}; check GMSSL_DIR or GMSSL_SOURCE_DIR",
        include_dir.display()
    );

    println!("cargo:rerun-if-changed=src/gmssl_shim.c");
    let mut shim = cc::Build::new();
    shim.file("src/gmssl_shim.c")
        .include(&include_dir)
        .warnings(true);
    let flags = env::var("GMSSL_CFLAGS").unwrap_or_else(|_| {
        installed_prefix.as_ref().map_or_else(
            || bundled_gmssl_cflags(&include_dir),
            |prefix| {
                panic!(
                    "GMSSL_CFLAGS must contain the -DENABLE_* flags used to build the GmSSL installation at {}",
                    prefix.display()
                )
            },
        )
    });
    assert!(
        flags.split_whitespace().any(|flag| flag == "-DENABLE_TLS"),
        "GmSSL must be built with ENABLE_TLS=ON and GMSSL_CFLAGS must include -DENABLE_TLS"
    );
    for flag in flags.split_whitespace() {
        shim.flag(flag);
    }
    shim.compile("tokio_cronet_gmssl_shim");
}

fn bundled_gmssl_cflags(include_dir: &std::path::Path) -> String {
    let prefix = include_dir
        .parent()
        .expect("gmssl-rs-sys published an invalid include directory");
    let cache_path = prefix.join("build/CMakeCache.txt");
    let cache = fs::read_to_string(&cache_path).unwrap_or_else(|error| {
        panic!(
            "failed to read gmssl-rs-sys configuration at {}: {error}",
            cache_path.display()
        )
    });
    cache
        .lines()
        .filter_map(|line| {
            let (name, setting) = line.split_once(':')?;
            if !name.starts_with("ENABLE_") {
                return None;
            }
            let (_, value) = setting.split_once('=')?;
            matches!(value, "1" | "ON" | "TRUE" | "YES").then(|| format!("-D{name}"))
        })
        .collect::<Vec<_>>()
        .join(" ")
}

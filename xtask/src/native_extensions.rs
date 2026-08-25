//! Links committed cronet-rs C wrappers into the generated Cronet overlay.
//!
//! Upstream Chromium C/C++ is not modified. The wrappers and their GN target
//! live in `crates/tokio-cronet-sys` and are only symlinked into a new overlay
//! directory. The Cronet shared/static library then depends on that target.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::{ensure_symlink, replace_generated_link_with_directory, workspace_root};

pub fn apply(overlay: &Path) -> Result<(), String> {
    let sys = workspace_root().join("crates/tokio-cronet-sys");
    let dest = overlay.join("components/cronet_rs");
    replace_generated_link_with_directory(&dest)?;
    ensure_symlink(&sys.join("cronet_rs_c.h"), &dest.join("cronet_rs_c.h"))?;
    ensure_symlink(
        &sys.join("native/cronet_rs_bind.cc"),
        &dest.join("cronet_rs_bind.cc"),
    )?;
    ensure_symlink(
        &sys.join("native/cronet_rs_websocket.cc"),
        &dest.join("cronet_rs_websocket.cc"),
    )?;
    ensure_symlink(&sys.join("native/BUILD.gn"), &dest.join("BUILD.gn"))?;
    ensure_symlink(
        &sys.join("native/cronet_rs_android_jni_onload.cc"),
        &dest.join("cronet_rs_android_jni_onload.cc"),
    )?;
    ensure_symlink(
        &sys.join("native/cronet_rs_android_static_support.cc"),
        &dest.join("cronet_rs_android_static_support.cc"),
    )?;
    ensure_symlink(
        &sys.join("native/cronet_rs_ohos.cc"),
        &dest.join("cronet_rs_ohos.cc"),
    )?;
    Ok(())
}

/// True when the overlay is missing the committed wrapper target or when those
/// sources are newer than the already-built Cronet library.
#[must_use]
pub fn requires_rebuild(source: &Path, lib_dir: &Path) -> bool {
    let overlay = cronet_overlay_root(source);
    if !overlay.join("components/cronet_rs/BUILD.gn").is_file()
        || !overlay
            .join("components/cronet_rs/cronet_rs_websocket.cc")
            .is_file()
    {
        return true;
    }
    let Some(lib_mtime) = newest_library_mtime(lib_dir) else {
        return true;
    };
    crate_wrapper_paths()
        .into_iter()
        .any(|path| file_mtime(&path).is_some_and(|mtime| mtime > lib_mtime))
}

fn cronet_overlay_root(source: &Path) -> PathBuf {
    source
        .parent()
        .expect("Chromium src directory must have a parent")
        .join("cronet-gn-root")
}

fn crate_wrapper_paths() -> [PathBuf; 7] {
    let sys = workspace_root().join("crates/tokio-cronet-sys");
    [
        sys.join("cronet_rs_c.h"),
        sys.join("native/BUILD.gn"),
        sys.join("native/cronet_rs_android_jni_onload.cc"),
        sys.join("native/cronet_rs_android_static_support.cc"),
        sys.join("native/cronet_rs_bind.cc"),
        sys.join("native/cronet_rs_ohos.cc"),
        sys.join("native/cronet_rs_websocket.cc"),
    ]
}

fn newest_library_mtime(lib_dir: &Path) -> Option<SystemTime> {
    std::fs::read_dir(lib_dir)
        .ok()?
        .flatten()
        .filter(|entry| entry.file_name().to_string_lossy().contains("cronet"))
        .filter_map(|entry| file_mtime(&entry.path()))
        .max()
}

fn file_mtime(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path)
        .and_then(|meta| meta.modified())
        .ok()
}

#[cfg(test)]
mod tests {
    #[test]
    fn committed_overlay_target_lists_wrapper_sources() {
        let build = include_str!("../../crates/tokio-cronet-sys/native/BUILD.gn");
        assert!(build.contains("source_set(\"cronet_rs_native\")"));
        assert!(build.contains("cronet_rs_bind.cc"));
        assert!(build.contains("cronet_rs_websocket.cc"));
        assert!(build.contains("cronet_rs_c.h"));
        assert!(build.contains("cronet_rs_android_jni_onload.cc"));
        assert!(build.contains("cronet_rs_android_static"));
        assert!(build.contains("cronet_rs_ohos.cc"));
        assert!(build.contains("//components/cronet/native:cronet_native_impl"));
    }
}

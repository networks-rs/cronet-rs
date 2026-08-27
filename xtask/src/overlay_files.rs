//! Installs committed overlay files and source wrappers.
//!
//! Overlay content lives under `xtask/overlays/` as real `.cc`, `.java`, `.gn`,
//! and `.gni` files. This module only creates links in the generated GN view;
//! it does not generate source text or splice Chromium files in memory.

use std::path::Path;

use crate::{
    display_error, ensure_symlink, replace_generated_link_with_directory,
    replace_generated_link_with_file, workspace_root,
};

#[derive(Clone, Copy)]
pub(crate) struct OverlayFile {
    pub(crate) dest: &'static str,
    pub(crate) src: &'static str,
}

#[derive(Clone, Copy)]
pub(crate) struct SourceWrapper {
    pub(crate) dest: &'static str,
    pub(crate) src: &'static str,
    pub(crate) upstream_alias: &'static str,
}

pub(crate) const COMMON: &[OverlayFile] = &[
    OverlayFile {
        dest: "BUILD.gn",
        src: "common/BUILD.gn",
    },
    OverlayFile {
        dest: "third_party/angle/dotfile_settings.gni",
        src: "common/third_party/angle/dotfile_settings.gni",
    },
    OverlayFile {
        dest: "components/cronet/BUILD.gn",
        src: "common/components/cronet/BUILD.gn",
    },
    OverlayFile {
        dest: "components/cronet/native/BUILD.gn",
        src: "common/components/cronet/native/BUILD.gn",
    },
    OverlayFile {
        dest: "third_party/rust/chromium_crates_io/vendor/cxx-v1/include/cxx.h",
        src: "common/third_party/rust/chromium_crates_io/vendor/cxx-v1/include/cxx.h",
    },
];

pub(crate) const ANDROID: &[OverlayFile] = &[
    OverlayFile {
        dest: "BUILD.gn",
        src: "android/BUILD.gn",
    },
    OverlayFile {
        dest: "net/android/java/src/org/chromium/net/ProxyChangeListener.java",
        src: "android/net/android/java/src/org/chromium/net/ProxyChangeListener.java",
    },
    OverlayFile {
        dest: "build/config/gclient_args.gni",
        src: "android/build/config/gclient_args.gni",
    },
    OverlayFile {
        dest: "build/config/BUILDCONFIG.gn",
        src: "android/build/config/BUILDCONFIG.gn",
    },
    OverlayFile {
        dest: "build/config/compiler/BUILD.gn",
        src: "android/build/config/compiler/BUILD.gn",
    },
];

pub(crate) const ANDROID_MAC_HOST: &[OverlayFile] = &[
    OverlayFile {
        dest: "build/config/android/config.gni",
        src: "android/build/config/android/config.gni",
    },
    OverlayFile {
        dest: "third_party/jdk/BUILD.gn",
        src: "android/third_party/jdk/BUILD.gn",
    },
];

pub(crate) const ANDROID_SOURCE_WRAPPERS: &[SourceWrapper] = &[SourceWrapper {
    dest: "base/android/linker/ashmem.cc",
    src: "android/base/android/linker/ashmem.cc",
    upstream_alias: "base/android/linker/ashmem_upstream.cc",
}];

pub(crate) const IOS: &[SourceWrapper] = &[SourceWrapper {
    dest: "base/features.cc",
    src: "ios/base/features.cc",
    upstream_alias: "base/features_upstream.cc",
}];

pub(crate) const WINDOWS: &[OverlayFile] = &[OverlayFile {
    dest: "net/tools/root_store_tool/BUILD.gn",
    src: "windows/net/tools/root_store_tool/BUILD.gn",
}];

pub(crate) const WINDOWS_SOURCE_WRAPPERS: &[SourceWrapper] = &[SourceWrapper {
    dest: "build/vs_toolchain.py",
    src: "windows/build/vs_toolchain.py",
    upstream_alias: "build/vs_toolchain_upstream.py",
}];

pub(crate) const OHOS: &[OverlayFile] = &[
    OverlayFile {
        dest: "cronet_rs_ohos_toolchain/BUILD.gn",
        src: "ohos/BUILD.gn",
    },
    OverlayFile {
        dest: "base/allocator/partition_allocator/partition_alloc.gni",
        src: "ohos/base/allocator/partition_allocator/partition_alloc.gni",
    },
    OverlayFile {
        dest: "base/allocator/partition_allocator/src/partition_alloc/BUILD.gn",
        src: "ohos/base/allocator/partition_allocator/src/partition_alloc/BUILD.gn",
    },
    OverlayFile {
        dest: "base/process/set_process_title.cc",
        src: "ohos/base/process/set_process_title.cc",
    },
    OverlayFile {
        dest: "build/config/BUILDCONFIG.gn",
        src: "ohos/build/config/BUILDCONFIG.gn",
    },
    OverlayFile {
        dest: "build/config/rust.gni",
        src: "ohos/build/config/rust.gni",
    },
    OverlayFile {
        dest: "build/config/compiler/BUILD.gn",
        src: "ohos/build/config/compiler/BUILD.gn",
    },
];

pub(crate) const OHOS_SOURCE_WRAPPERS: &[SourceWrapper] = &[
    SourceWrapper {
        dest: "buildtools/third_party/libc++/__config_site",
        src: "ohos/buildtools/third_party/libc++/__config_site",
        upstream_alias: "buildtools/third_party/libc++/__config_site_upstream",
    },
    SourceWrapper {
        dest: "base/allocator/partition_allocator/src/partition_alloc/aarch64_support.h",
        src: "ohos/base/allocator/partition_allocator/src/partition_alloc/aarch64_support.h",
        upstream_alias: "base/allocator/partition_allocator/src/partition_alloc/aarch64_support_upstream.h",
    },
    SourceWrapper {
        dest: "base/files/scoped_file_linux.cc",
        src: "ohos/base/files/scoped_file_linux.cc",
        upstream_alias: "base/files/scoped_file_linux_upstream.cc",
    },
    SourceWrapper {
        dest: "base/debug/stack_trace_posix.cc",
        src: "ohos/base/debug/stack_trace_posix.cc",
        upstream_alias: "base/debug/stack_trace_posix_upstream.cc",
    },
    SourceWrapper {
        dest: "net/dns/public/scoped_res_state.cc",
        src: "ohos/net/dns/public/scoped_res_state.cc",
        upstream_alias: "net/dns/public/scoped_res_state_upstream.cc",
    },
];

pub(crate) fn install(source: &Path, overlay: &Path, files: &[OverlayFile]) -> Result<(), String> {
    for file in files {
        install_one(source, overlay, *file)?;
    }
    Ok(())
}

pub(crate) fn install_source_wrappers(
    source: &Path,
    overlay: &Path,
    wrappers: &[SourceWrapper],
) -> Result<(), String> {
    for wrapper in wrappers {
        install_one(
            source,
            overlay,
            OverlayFile {
                dest: wrapper.dest,
                src: wrapper.src,
            },
        )?;
        let alias = overlay.join(wrapper.upstream_alias);
        materialize_parent(source, overlay, wrapper.upstream_alias)?;
        replace_generated_link_with_file(&alias)?;
        ensure_symlink(&source.join(wrapper.dest), &alias)?;
    }
    Ok(())
}

fn install_one(source: &Path, overlay: &Path, file: OverlayFile) -> Result<(), String> {
    let src = workspace_root().join("xtask/overlays").join(file.src);
    if !src.is_file() {
        return Err(format!(
            "committed overlay file {} is missing",
            src.display()
        ));
    }
    let dest = overlay.join(file.dest);
    materialize_parent(source, overlay, file.dest)?;
    replace_generated_link_with_file(&dest)?;
    ensure_symlink(&src, &dest)
}

fn materialize_parent(source: &Path, overlay: &Path, dest_rel: &str) -> Result<(), String> {
    let dest = overlay.join(dest_rel);
    let Some(parent) = dest.parent() else {
        return Ok(());
    };
    if parent
        .symlink_metadata()
        .is_ok_and(|metadata| metadata.is_dir() && !metadata.file_type().is_symlink())
    {
        return Ok(());
    }
    replace_generated_link_with_directory(parent)?;
    let source_file = source.join(dest_rel);
    let Some(source_parent) = source_file.parent() else {
        return Ok(());
    };
    if !source_parent.is_dir() {
        return Ok(());
    }
    let skip = dest.file_name();
    for entry in
        std::fs::read_dir(source_parent).map_err(display_error("list Chromium overlay parent"))?
    {
        let entry = entry.map_err(display_error("read Chromium overlay parent entry"))?;
        if skip == Some(entry.file_name().as_os_str()) {
            continue;
        }
        ensure_symlink(&entry.path(), &parent.join(entry.file_name()))?;
    }
    Ok(())
}

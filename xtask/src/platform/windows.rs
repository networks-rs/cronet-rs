use std::{fs, path::Path, path::PathBuf};

use super::{PlatformBuild, PlatformKind, StaticObjectSet, TargetSpec};
use crate::{PlatformConfig, display_error, require_file};

const X86_64: TargetSpec = TargetSpec {
    triple: "x86_64-pc-windows-msvc",
    gn_os: "win",
    gn_cpu: "x64",
};
const AARCH64: TargetSpec = TargetSpec {
    triple: "aarch64-pc-windows-msvc",
    gn_os: "win",
    gn_cpu: "arm64",
};

struct WindowsBuild(TargetSpec);

// Chromium models its Windows libc++ runtime as a source_set, so no .lib
// exists for the generic archive bundler. Preserve its compiled objects as
// members of cronet_static.lib instead.
pub(super) const LIBCXX_OBJECT_SETS: &[StaticObjectSet] = &[StaticObjectSet {
    ninja_target: "libc++",
    ninja_file: "obj/buildtools/third_party/libc++/libc++.ninja",
}];

pub(super) fn resolve(target: &str) -> Option<Box<dyn PlatformBuild>> {
    let target = match target {
        "x86_64-pc-windows-msvc" => X86_64,
        "aarch64-pc-windows-msvc" => AARCH64,
        _ => return None,
    };
    Some(Box::new(WindowsBuild(target)))
}

impl PlatformBuild for WindowsBuild {
    fn kind(&self) -> PlatformKind {
        PlatformKind::Windows
    }

    fn target_spec(&self) -> Option<TargetSpec> {
        Some(self.0)
    }

    fn prepare_overlay(&self, source: &Path, overlay: &Path) -> Result<(), String> {
        crate::overlay_files::install(source, overlay, crate::overlay_files::WINDOWS)?;
        crate::overlay_files::install_source_wrappers(
            source,
            overlay,
            crate::overlay_files::WINDOWS_SOURCE_WRAPPERS,
        )
    }

    fn gn_args(
        &self,
        _source: &Path,
        _overlay: &Path,
        _config: PlatformConfig<'_>,
    ) -> Result<Vec<String>, String> {
        let mut arguments = self.0.gn_args();
        // Cronet normally disables PartitionAlloc to keep the native library
        // small. Chromium's Windows system-allocator path is incomplete at
        // this revision, so use its standard Windows allocator configuration.
        arguments.push("use_partition_alloc=true".to_owned());
        Ok(arguments)
    }

    fn builds_libcxx_runtime_archives(&self) -> bool {
        false
    }

    fn extra_static_object_sets(&self) -> &'static [StaticObjectSet] {
        LIBCXX_OBJECT_SETS
    }

    fn extra_static_archives(&self, source: &Path) -> Result<Vec<PathBuf>, String> {
        let archive_name = match self.0.gn_cpu {
            "x64" => "clang_rt.builtins-x86_64.lib",
            "arm64" => "clang_rt.builtins-aarch64.lib",
            cpu => return Err(format!("unsupported Windows compiler runtime CPU `{cpu}`")),
        };
        let clang_lib = source.join("third_party/llvm-build/Release+Asserts/lib/clang");
        let mut archives = Vec::new();
        for entry in fs::read_dir(&clang_lib)
            .map_err(display_error("list the bundled Clang runtime versions"))?
        {
            let entry = entry.map_err(display_error("read a bundled Clang runtime version"))?;
            let archive = entry.path().join("lib/windows").join(archive_name);
            if archive.is_file() {
                archives.push(archive);
            }
        }
        if archives.len() != 1 {
            return Err(format!(
                "expected one bundled Windows compiler runtime `{archive_name}` below {}, found {}",
                clang_lib.display(),
                archives.len()
            ));
        }
        let archive = archives.pop().unwrap();
        require_file(&archive, "synchronize the bundled Windows Clang toolchain")?;
        Ok(vec![archive])
    }

    fn static_archive_extension(&self) -> &'static str {
        "lib"
    }
}

use std::path::Path;

use super::{PlatformBuild, PlatformKind, TargetSpec};
use crate::{
    PlatformConfig, ohos_gn_args, patch_ohos_base_process, patch_ohos_compiler_config,
    patch_ohos_fd_close_interposer, patch_ohos_libcxx_config, patch_ohos_link_closure,
    patch_ohos_partition_alloc, patch_ohos_resolver, patch_ohos_rust_target, write_ohos_toolchain,
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
        write_ohos_toolchain(overlay)?;
        patch_ohos_rust_target(source, overlay)?;
        patch_ohos_compiler_config(source, overlay)?;
        patch_ohos_libcxx_config(source, overlay)?;
        patch_ohos_partition_alloc(source, overlay)?;
        patch_ohos_base_process(source, overlay)?;
        patch_ohos_fd_close_interposer(source, overlay)?;
        patch_ohos_resolver(source, overlay)?;
        patch_ohos_link_closure(source, overlay)
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

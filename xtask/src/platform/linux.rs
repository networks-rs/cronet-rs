use std::path::Path;

use super::{PlatformBuild, PlatformKind, TargetSpec};
use crate::{
    PlatformConfig, desktop_linux_host_toolchain_gn_args, requires_native_linux_arm64_tools,
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

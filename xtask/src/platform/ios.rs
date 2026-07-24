use std::path::Path;

use super::{PlatformBuild, PlatformKind, TargetSpec};
use crate::{PlatformConfig, ios_gn_args, patch_ios_base_features};

const X86_64_SIMULATOR: TargetSpec = TargetSpec {
    triple: "x86_64-apple-ios",
    gn_os: "ios",
    gn_cpu: "x64",
};
const AARCH64_DEVICE: TargetSpec = TargetSpec {
    triple: "aarch64-apple-ios",
    gn_os: "ios",
    gn_cpu: "arm64",
};
const AARCH64_SIMULATOR: TargetSpec = TargetSpec {
    triple: "aarch64-apple-ios-sim",
    gn_os: "ios",
    gn_cpu: "arm64",
};

struct IosBuild(TargetSpec);

pub(super) fn resolve(target: &str) -> Option<Box<dyn PlatformBuild>> {
    let target = match target {
        "x86_64-apple-ios" => X86_64_SIMULATOR,
        "aarch64-apple-ios" => AARCH64_DEVICE,
        "aarch64-apple-ios-sim" => AARCH64_SIMULATOR,
        _ => return None,
    };
    Some(Box::new(IosBuild(target)))
}

impl PlatformBuild for IosBuild {
    fn kind(&self) -> PlatformKind {
        PlatformKind::Ios
    }

    fn target_spec(&self) -> Option<TargetSpec> {
        Some(self.0)
    }

    fn prepare_overlay(&self, source: &Path, overlay: &Path) -> Result<(), String> {
        patch_ios_base_features(source, overlay, Some(self.0.triple))
    }

    fn gn_args(
        &self,
        _source: &Path,
        _overlay: &Path,
        config: PlatformConfig<'_>,
    ) -> Result<Vec<String>, String> {
        let mut arguments = self.0.gn_args();
        arguments.extend(ios_gn_args(
            self.0.triple,
            config.ios_developer_dir,
            config.ios_deployment_target,
        )?);
        Ok(arguments)
    }
}

use std::{
    env,
    path::{Path, PathBuf},
};

use super::{PlatformBuild, PlatformKind, TargetSpec};
use crate::{PlatformConfig, display_error, escape_gn_string, gn_string_path};

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
        crate::overlay_files::install_source_wrappers(source, overlay, crate::overlay_files::IOS)
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

fn ios_gn_args(
    target: &str,
    explicit_developer_dir: Option<&Path>,
    explicit_deployment_target: Option<&str>,
) -> Result<Vec<String>, String> {
    if !cfg!(target_os = "macos") {
        return Err("iOS source builds require a macOS host with Xcode".to_owned());
    }
    let target_environment = match target {
        "aarch64-apple-ios" => "device",
        "aarch64-apple-ios-sim" | "x86_64-apple-ios" => "simulator",
        _ => return Err(format!("unsupported iOS target `{target}`")),
    };
    let mut arguments = vec![
        format!("target_environment=\"{target_environment}\""),
        "target_platform=\"iphoneos\"".to_owned(),
        "use_system_xcode=true".to_owned(),
        "ios_enable_code_signing=false".to_owned(),
    ];
    if let Some(directory) = explicit_developer_dir
        .map(Path::to_owned)
        .or_else(|| env::var_os("DEVELOPER_DIR").map(PathBuf::from))
    {
        let directory = directory
            .canonicalize()
            .map_err(display_error("resolve the Xcode Developer directory"))?;
        if !directory.join("Platforms").is_dir() {
            return Err(format!(
                "Xcode Developer directory is invalid: {}",
                directory.display()
            ));
        }
        arguments.push(gn_string_path("ios_sdk_developer_dir", &directory));
    }
    if let Some(version) = explicit_deployment_target
        .map(str::to_owned)
        .or_else(|| env::var("IPHONEOS_DEPLOYMENT_TARGET").ok())
    {
        if version.trim().is_empty() {
            return Err("iOS deployment target cannot be empty".to_owned());
        }
        arguments.push(format!(
            "ios_deployment_target=\"{}\"",
            escape_gn_string(&version)
        ));
    }
    Ok(arguments)
}

//! Platform-specific Cronet source-build policies.
//!
//! The root build driver owns the invariant GN/Ninja transaction. Each
//! implementation in this module owns target mapping, toolchain arguments,
//! overlay replacements that cannot be extra sources, and platform-only
//! packaging (Android support jars, OHOS toolchain, iOS SDK args).
//!
//! Extra C/C++ and GN files, plus any Chromium replacements that cannot be
//! extra sources, are committed under `xtask/overlays/` as real files of the
//! original type and only symlinked into the overlay.

use std::{path::Path, process::Command};

use crate::{NativeLinkage, PlatformConfig};

pub(crate) mod android;
mod host;
pub(crate) mod ios;
pub(crate) mod linux;
mod macos;
pub(crate) mod ohos;
mod windows;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum PlatformKind {
    Host,
    Linux,
    Android,
    Ios,
    MacOs,
    Ohos,
    Windows,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct TargetSpec {
    pub(super) triple: &'static str,
    pub(super) gn_os: &'static str,
    pub(super) gn_cpu: &'static str,
}

impl TargetSpec {
    pub(super) fn gn_args(self) -> Vec<String> {
        vec![
            format!("target_os=\"{}\"", self.gn_os),
            format!("target_cpu=\"{}\"", self.gn_cpu),
        ]
    }
}

/// Defines the policy hooks for one target platform.
///
/// Methods run in build order: `prepare_overlay`, `gn_args`,
/// `configure_ninja`, then `post_build`. Defaults keep ordinary desktop
/// platforms intentionally small.
pub(super) trait PlatformBuild {
    fn kind(&self) -> PlatformKind;

    fn target_spec(&self) -> Option<TargetSpec>;

    fn target(&self) -> Option<&'static str> {
        self.target_spec().map(|target| target.triple)
    }

    fn cache_key(&self) -> String {
        format!("v2:{}", self.target().unwrap_or("host"))
    }

    fn prepare_overlay(&self, _source: &Path, _overlay: &Path) -> Result<(), String> {
        Ok(())
    }

    fn gn_args(
        &self,
        _source: &Path,
        _overlay: &Path,
        _config: PlatformConfig<'_>,
    ) -> Result<Vec<String>, String> {
        Ok(self
            .target_spec()
            .map_or_else(Vec::new, TargetSpec::gn_args))
    }

    fn needs_rustc_bootstrap(&self, _config: PlatformConfig<'_>) -> bool {
        false
    }

    fn configure_ninja(
        &self,
        _command: &mut Command,
        _overlay: &Path,
        _linkages: &[NativeLinkage],
    ) -> Result<(), String> {
        Ok(())
    }

    fn post_build(&self, _build_dir: &Path, _output_dir: &Path) -> Result<(), String> {
        Ok(())
    }

    fn filter_third_party_tests(&self) -> bool {
        true
    }

    fn static_archive_extension(&self) -> &'static str {
        "a"
    }
}

pub(super) fn resolve(target: Option<&str>) -> Result<Box<dyn PlatformBuild>, String> {
    let Some(target) = target else {
        return Ok(Box::new(host::HostBuild));
    };
    linux::resolve(target)
        .or_else(|| android::resolve(target))
        .or_else(|| ios::resolve(target))
        .or_else(|| macos::resolve(target))
        .or_else(|| ohos::resolve(target))
        .or_else(|| windows::resolve(target))
        .ok_or_else(|| format!("unsupported Cronet native target `{target}`"))
}

pub(super) fn kind(target: &str) -> Option<PlatformKind> {
    resolve(Some(target)).ok().map(|platform| platform.kind())
}

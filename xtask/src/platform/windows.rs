use super::{PlatformBuild, PlatformKind, TargetSpec};

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

    fn static_archive_extension(&self) -> &'static str {
        "lib"
    }
}

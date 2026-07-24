use super::{PlatformBuild, PlatformKind, TargetSpec};

const X86_64: TargetSpec = TargetSpec {
    triple: "x86_64-apple-darwin",
    gn_os: "mac",
    gn_cpu: "x64",
};
const AARCH64: TargetSpec = TargetSpec {
    triple: "aarch64-apple-darwin",
    gn_os: "mac",
    gn_cpu: "arm64",
};

struct MacOsBuild(TargetSpec);

pub(super) fn resolve(target: &str) -> Option<Box<dyn PlatformBuild>> {
    let target = match target {
        "x86_64-apple-darwin" => X86_64,
        "aarch64-apple-darwin" => AARCH64,
        _ => return None,
    };
    Some(Box::new(MacOsBuild(target)))
}

impl PlatformBuild for MacOsBuild {
    fn kind(&self) -> PlatformKind {
        PlatformKind::MacOs
    }

    fn target_spec(&self) -> Option<TargetSpec> {
        Some(self.0)
    }
}

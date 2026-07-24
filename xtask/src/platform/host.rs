use super::{PlatformBuild, PlatformKind, TargetSpec};

pub(super) struct HostBuild;

impl PlatformBuild for HostBuild {
    fn kind(&self) -> PlatformKind {
        PlatformKind::Host
    }

    fn target_spec(&self) -> Option<TargetSpec> {
        None
    }
}

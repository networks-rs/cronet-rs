use super::{PlatformBuild, PlatformKind, StaticObjectSet, TargetSpec};

pub(super) struct HostBuild;

impl PlatformBuild for HostBuild {
    fn kind(&self) -> PlatformKind {
        PlatformKind::Host
    }

    fn target_spec(&self) -> Option<TargetSpec> {
        None
    }

    fn builds_libcxx_runtime_archives(&self) -> bool {
        !cfg!(target_os = "windows")
    }

    fn extra_static_object_sets(&self) -> &'static [StaticObjectSet] {
        if cfg!(target_os = "windows") {
            super::windows::LIBCXX_OBJECT_SETS
        } else {
            &[]
        }
    }
}

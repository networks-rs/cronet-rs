use std::{path::Path, process::Command};

use super::{PlatformBuild, PlatformKind, TargetSpec};
use crate::{
    NativeLinkage, PlatformConfig, android_gn_args, install_android_support_dex,
    patch_android_gclient_args, patch_android_host_os, patch_android_host_tools,
    patch_android_ndk_compat, patch_android_proxy_listener, patch_android_relative_vtables,
};

const I686: TargetSpec = TargetSpec {
    triple: "i686-linux-android",
    gn_os: "android",
    gn_cpu: "x86",
};
const X86_64: TargetSpec = TargetSpec {
    triple: "x86_64-linux-android",
    gn_os: "android",
    gn_cpu: "x64",
};
const ARMV7: TargetSpec = TargetSpec {
    triple: "armv7-linux-androideabi",
    gn_os: "android",
    gn_cpu: "arm",
};
const AARCH64: TargetSpec = TargetSpec {
    triple: "aarch64-linux-android",
    gn_os: "android",
    gn_cpu: "arm64",
};

struct AndroidBuild(TargetSpec);

pub(super) fn resolve(target: &str) -> Option<Box<dyn PlatformBuild>> {
    let target = match target {
        "i686-linux-android" => I686,
        "x86_64-linux-android" => X86_64,
        "armv7-linux-androideabi" => ARMV7,
        "aarch64-linux-android" => AARCH64,
        _ => return None,
    };
    Some(Box::new(AndroidBuild(target)))
}

impl PlatformBuild for AndroidBuild {
    fn kind(&self) -> PlatformKind {
        PlatformKind::Android
    }

    fn target_spec(&self) -> Option<TargetSpec> {
        Some(self.0)
    }

    fn prepare_overlay(&self, source: &Path, overlay: &Path) -> Result<(), String> {
        patch_android_host_os(source, overlay)?;
        patch_android_gclient_args(source, overlay)?;
        patch_android_relative_vtables(source, overlay)?;
        patch_android_ndk_compat(source, overlay, Some(self.0.triple))?;
        patch_android_proxy_listener(source, overlay, Some(self.0.triple))?;
        patch_android_host_tools(source, overlay, Some(self.0.triple))
    }

    fn gn_args(
        &self,
        source: &Path,
        overlay: &Path,
        config: PlatformConfig<'_>,
    ) -> Result<Vec<String>, String> {
        let mut arguments = self.0.gn_args();
        arguments.extend(android_gn_args(
            source,
            overlay,
            self.0.triple,
            config.android_ndk,
            config.android_api_level,
        )?);
        Ok(arguments)
    }

    fn configure_ninja(
        &self,
        command: &mut Command,
        overlay: &Path,
        _linkages: &[NativeLinkage],
    ) -> Result<(), String> {
        // Chromium's Java helpers otherwise canonicalize the shared output
        // directory back to the source checkout and select its CIPD host JDK.
        command
            .env("CHECKOUT_SOURCE_ROOT", overlay)
            .arg(":cronet_rs_android_support_java");
        Ok(())
    }

    fn post_build(&self, build_dir: &Path, output_dir: &Path) -> Result<(), String> {
        install_android_support_dex(build_dir, output_dir)
    }

    fn filter_third_party_tests(&self) -> bool {
        false
    }

    fn append_root_build(&self, contents: &mut String) {
        contents.push_str(
            r#"

import("//build/config/android/rules.gni")

# base_java is compiled against a placeholder BuildConfig but intentionally
# does not package it. Native-only consumers do not have an APK target to
# generate the usual replacement, so compile the pinned placeholder here.
android_library("cronet_rs_android_build_config_java") {
  srcjar_deps = [ "//build/android:placeholder_build_config_srcjar" ]
}

# Chromium's Android net implementation calls these Java classes from native
# code. Keep the jar deliberately narrower than the public Cronet Java API: a
# Rust application uses Cronet through cronet-sys and only needs the platform
# bridge and its runtime dependencies.
dist_jar("cronet_rs_android_support_java") {
  output = "$root_out_dir/cronet-android-support.jar"
  deps = [
    ":cronet_rs_android_build_config_java",
    "//net/android:net_java",
  ]
  jar_excluded_patterns = [ "META-INF/versions/*/module-info.class" ]
  requires_android = true
}
"#,
        );
    }
}

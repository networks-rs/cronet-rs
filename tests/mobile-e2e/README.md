# Android and iOS E2E

This directory builds one platform-neutral Rust runner around the public safe
API, embeds it with the source-built Cronet library in a minimal native app, and
runs it on an Android device/emulator or the iOS Simulator. The suite exercises
Tokio streaming upload and download, finished-request metrics, response limits,
cancellation, async shutdown, and bidirectional-stream error delivery.

Android requires an SDK, NDK, JDK, and either a connected device or an AVD:

```sh
ANDROID_SDK_ROOT="$HOME/Android/Sdk" \
ANDROID_NDK_HOME="$ANDROID_SDK_ROOT/ndk/27.1.12297006" \
tests/mobile-e2e/android/run.sh
```

iOS requires Xcode and an installed iOS Simulator runtime:

```sh
IOS_E2E_LINKAGE=dynamic tests/mobile-e2e/ios/run.sh
IOS_E2E_LINKAGE=static tests/mobile-e2e/ios/run.sh
```

Both scripts accept `CRONET_SOURCE_DIR` and `CRONET_LIB_DIR`. Android also
accepts `ANDROID_E2E_TARGET`/`ANDROID_E2E_LINKAGE`/`ANDROID_E2E_AVD`; iOS
accepts `IOS_E2E_TARGET`/`IOS_E2E_LINKAGE`/`IOS_SIMULATOR_UDID`. The iOS
source uses Chromium's minimum iOS 17 deployment target by default; set
`IPHONEOS_DEPLOYMENT_TARGET` consistently for the native build and app if a
different supported target is required. Toolchains and devices are discovered
from standard SDK locations or explicit environment variables rather than
repository-local paths.

#!/usr/bin/env bash
set -euo pipefail

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)
APP_ROOT="$ROOT/tests/mobile-e2e/ios"
RUNNER_MANIFEST="$ROOT/tests/mobile-e2e/runner/Cargo.toml"
BUNDLE_ID=io.github.southorange.cronet.e2e
TARGET=${IOS_E2E_TARGET:-aarch64-apple-ios-sim}
DEPLOYMENT_TARGET=${IPHONEOS_DEPLOYMENT_TARGET:-17.0}
LINKAGE=${IOS_E2E_LINKAGE:-dynamic}

fail() {
  echo "cronet-rs iOS E2E: $*" >&2
  exit 1
}

case "$TARGET" in
  aarch64-apple-ios-sim) SDK=iphonesimulator; ARCH=arm64; MIN_FLAG=-mios-simulator-version-min ;;
  x86_64-apple-ios) SDK=iphonesimulator; ARCH=x86_64; MIN_FLAG=-mios-simulator-version-min ;;
  *) fail "runtime E2E requires an iOS Simulator target, got $TARGET" ;;
esac
case "$LINKAGE" in
  dynamic) ;;
  static) ;;
  *) fail "unsupported iOS E2E linkage: $LINKAGE" ;;
esac

command -v xcrun >/dev/null || fail "Xcode command-line tools are required"
SDK_PATH=$(xcrun --sdk "$SDK" --show-sdk-path)
SOURCE_DIR=${CRONET_SOURCE_DIR:-"$ROOT/.cronet/chromium/src"}
TARGET_DIRECTORY=${TARGET//-/_}
LIB_DIR=${CRONET_LIB_DIR:-"$SOURCE_DIR/out/cronet-rs/$TARGET_DIRECTORY"}
if [[ "$LINKAGE" == dynamic ]]; then
  CRONET_NATIVE_PATTERN='libcronet.*.dylib'
else
  CRONET_NATIVE_PATTERN='libcronet_static.a'
fi
if ! compgen -G "$LIB_DIR/$CRONET_NATIVE_PATTERN" >/dev/null; then
  cargo xtask sync --target "$TARGET" --source-dir "$SOURCE_DIR"
  IPHONEOS_DEPLOYMENT_TARGET="$DEPLOYMENT_TARGET" cargo xtask build --release \
    --linkage "$LINKAGE" --target "$TARGET" --source-dir "$SOURCE_DIR"
fi
if [[ "$LINKAGE" == dynamic ]]; then
  CRONET_LIBRARY=$(find "$LIB_DIR" -maxdepth 1 -type f -name 'libcronet.*.dylib' | head -n 1)
  [[ -n "$CRONET_LIBRARY" ]] || fail "Cronet shared library not found below $LIB_DIR"
else
  [[ -f "$LIB_DIR/libcronet_static.a" ]] || fail "Cronet static library not found below $LIB_DIR"
  [[ -f "$LIB_DIR/cronet-static-link.txt" ]] || fail "Cronet static link manifest is missing"
fi

CARGO_ARGS=(build --manifest-path "$RUNNER_MANIFEST" --target "$TARGET" --release)
if [[ "$LINKAGE" == static ]]; then
  CARGO_ARGS+=(--features static)
fi
env \
  CRONET_SOURCE_DIR="$SOURCE_DIR" \
  CRONET_LIB_DIR="$LIB_DIR" \
  IPHONEOS_DEPLOYMENT_TARGET="$DEPLOYMENT_TARGET" \
  cargo "${CARGO_ARGS[@]}"
RUNNER_LIBRARY="$ROOT/tests/mobile-e2e/runner/target/$TARGET/release/libcronet_mobile_e2e_runner.a"
[[ -f "$RUNNER_LIBRARY" ]] || fail "mobile E2E runner static library was not produced"

WORK="$APP_ROOT/.build/$TARGET"
APP="$WORK/CronetE2E.app"
rm -rf "$WORK"
mkdir -p "$APP"
cp "$APP_ROOT/Info.plist" "$APP/Info.plist"

LINK_ARGS=()
if [[ "$LINKAGE" == static ]]; then
  while IFS= read -r line; do
    case "$line" in
      lib=*)
        library=${line#lib=}
        library=${library#lib}
        library=${library%.a}
        library=${library%.dylib}
        LINK_ARGS+=("-l$library")
        ;;
      framework=*) LINK_ARGS+=("-framework" "${line#framework=}") ;;
    esac
  done < "$LIB_DIR/cronet-static-link.txt"
else
  LINK_ARGS+=("$CRONET_LIBRARY" "-Wl,-rpath,@executable_path/Frameworks")
fi

xcrun --sdk "$SDK" clang -arch "$ARCH" "$MIN_FLAG=$DEPLOYMENT_TARGET" \
  -isysroot "$SDK_PATH" -fobjc-arc -fblocks "$APP_ROOT/main.m" \
  "$RUNNER_LIBRARY" "${LINK_ARGS[@]}" \
  -framework UIKit -framework Foundation -o "$APP/CronetE2E"
if [[ "$LINKAGE" == dynamic ]]; then
  mkdir -p "$APP/Frameworks"
  cp "$CRONET_LIBRARY" "$APP/Frameworks/"
  codesign --force --sign - "$APP/Frameworks/$(basename "$CRONET_LIBRARY")" >/dev/null
fi
codesign --force --sign - "$APP" >/dev/null

DEVICE=${IOS_SIMULATOR_UDID:-}
if [[ -z "$DEVICE" ]]; then
  DEVICE=$(xcrun simctl list devices available | awk -F '[()]' '/Booted/ { print $2; exit }')
fi
if [[ -z "$DEVICE" ]]; then
  DEVICE=$(xcrun simctl list devices available | awk -F '[()]' '/iPhone/ && !/unavailable/ { print $2; exit }')
fi
[[ -n "$DEVICE" ]] || fail "no available iOS Simulator was found"
xcrun simctl boot "$DEVICE" >/dev/null 2>&1 || true
xcrun simctl bootstatus "$DEVICE" -b
xcrun simctl uninstall "$DEVICE" "$BUNDLE_ID" >/dev/null 2>&1 || true
xcrun simctl install "$DEVICE" "$APP"
xcrun simctl launch "$DEVICE" "$BUNDLE_ID" >/dev/null

RESULT=''
for _ in $(seq 1 90); do
  CONTAINER=$(xcrun simctl get_app_container "$DEVICE" "$BUNDLE_ID" data 2>/dev/null || true)
  if [[ -n "$CONTAINER" && -f "$CONTAINER/Documents/cronet-rs-e2e.txt" ]]; then
    RESULT=$(tr -d '\r' < "$CONTAINER/Documents/cronet-rs-e2e.txt")
  fi
  case "$RESULT" in
    PASS*|FAIL*) break ;;
  esac
  sleep 1
done
printf '%s\n' "$RESULT"
[[ "$RESULT" == PASS* ]] || fail "simulator application E2E did not pass"

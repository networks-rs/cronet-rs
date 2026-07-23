#!/usr/bin/env bash
set -euo pipefail

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)
APP_ROOT="$ROOT/tests/mobile-e2e/android"
RUNNER_MANIFEST="$ROOT/tests/mobile-e2e/runner/Cargo.toml"
PACKAGE=io.github.southorange.cronet.e2e
TARGET=${ANDROID_E2E_TARGET:-aarch64-linux-android}
API_LEVEL=${ANDROID_API_LEVEL:-23}
LINKAGE=${ANDROID_E2E_LINKAGE:-dynamic}

fail() {
  echo "cronet-rs Android E2E: $*" >&2
  exit 1
}

case "$TARGET" in
  aarch64-linux-android) ABI=arm64-v8a; LLVM_TARGET=aarch64-linux-android ;;
  armv7-linux-androideabi) ABI=armeabi-v7a; LLVM_TARGET=armv7a-linux-androideabi ;;
  x86_64-linux-android) ABI=x86_64; LLVM_TARGET=x86_64-linux-android ;;
  i686-linux-android) ABI=x86; LLVM_TARGET=i686-linux-android ;;
  *) fail "unsupported Android E2E target: $TARGET" ;;
esac
case "$LINKAGE" in
  dynamic) ;;
  static) ;;
  *) fail "unsupported Android E2E linkage: $LINKAGE" ;;
esac

ANDROID_SDK_ROOT=${ANDROID_SDK_ROOT:-${ANDROID_HOME:-}}
[[ -n "$ANDROID_SDK_ROOT" ]] || fail "set ANDROID_SDK_ROOT or ANDROID_HOME"
ANDROID_NDK_HOME=${ANDROID_NDK_HOME:-${ANDROID_NDK_ROOT:-${NDK_HOME:-}}}
if [[ -z "$ANDROID_NDK_HOME" && -d "$ANDROID_SDK_ROOT/ndk" ]]; then
  ANDROID_NDK_HOME=$(find "$ANDROID_SDK_ROOT/ndk" -mindepth 1 -maxdepth 1 -type d | sort | tail -n 1)
fi
[[ -f "$ANDROID_NDK_HOME/source.properties" ]] || fail "set ANDROID_NDK_HOME to a complete NDK"

BUILD_TOOLS=${ANDROID_BUILD_TOOLS:-}
if [[ -z "$BUILD_TOOLS" ]]; then
  BUILD_TOOLS=$(find "$ANDROID_SDK_ROOT/build-tools" -mindepth 1 -maxdepth 1 -type d | sort | tail -n 1)
fi
for tool in aapt2 d8 apksigner zipalign; do
  [[ -x "$BUILD_TOOLS/$tool" ]] || fail "Android build tool is missing: $BUILD_TOOLS/$tool"
done
ADB=${ADB:-"$ANDROID_SDK_ROOT/platform-tools/adb"}
[[ -x "$ADB" ]] || fail "adb is missing: $ADB"

ANDROID_JAR=${ANDROID_JAR:-}
if [[ -z "$ANDROID_JAR" ]]; then
  ANDROID_JAR=$(find "$ANDROID_SDK_ROOT/platforms" -mindepth 2 -maxdepth 2 -name android.jar | sort | tail -n 1)
fi
[[ -f "$ANDROID_JAR" ]] || fail "install an Android SDK platform"

PREBUILT=$(find "$ANDROID_NDK_HOME/toolchains/llvm/prebuilt" -mindepth 1 -maxdepth 1 -type d | head -n 1)
LINKER="$PREBUILT/bin/${LLVM_TARGET}${API_LEVEL}-clang"
[[ -x "$LINKER" ]] || fail "Android linker is missing: $LINKER"
LINKER_ENV=CARGO_TARGET_$(printf '%s' "$TARGET" | tr '[:lower:]-' '[:upper:]_')_LINKER

SOURCE_DIR=${CRONET_SOURCE_DIR:-"$ROOT/.cronet/chromium/src"}
TARGET_DIRECTORY=${TARGET//-/_}
LIB_DIR=${CRONET_LIB_DIR:-"$SOURCE_DIR/out/cronet-rs/$TARGET_DIRECTORY"}
if [[ "$LINKAGE" == dynamic ]]; then
  CRONET_NATIVE_PATTERN='libcronet.*.so'
else
  CRONET_NATIVE_PATTERN='libcronet_static.a'
fi
if ! compgen -G "$LIB_DIR/$CRONET_NATIVE_PATTERN" >/dev/null; then
  cargo xtask sync --target "$TARGET" --source-dir "$SOURCE_DIR"
  ANDROID_NDK_HOME="$ANDROID_NDK_HOME" cargo xtask build --release \
    --linkage "$LINKAGE" --target "$TARGET" --source-dir "$SOURCE_DIR"
fi
CRONET_LIBRARY=''
if [[ "$LINKAGE" == dynamic ]]; then
  CRONET_LIBRARY=$(find "$LIB_DIR" -maxdepth 1 -type f -name 'libcronet.*.so' | head -n 1)
  [[ -n "$CRONET_LIBRARY" ]] || fail "Cronet shared library not found below $LIB_DIR"
fi
CRONET_ANDROID_SUPPORT_JAR="$LIB_DIR/cronet-android-support.jar"
[[ -f "$CRONET_ANDROID_SUPPORT_JAR" ]] || fail \
  "Cronet Android support jar not found at $CRONET_ANDROID_SUPPORT_JAR; rebuild the native target"
CRONET_ANDROID_SUPPORT_DEX_JAR="$LIB_DIR/cronet-android-support.dex.jar"
[[ -f "$CRONET_ANDROID_SUPPORT_DEX_JAR" ]] || fail \
  "Cronet Android support dex jar not found at $CRONET_ANDROID_SUPPORT_DEX_JAR; rebuild the native target"

CARGO_ARGS=(build --manifest-path "$RUNNER_MANIFEST" --target "$TARGET" --release)
if [[ "$LINKAGE" == static ]]; then
  CARGO_ARGS+=(--features static)
fi
env \
  CRONET_SOURCE_DIR="$SOURCE_DIR" \
  CRONET_LIB_DIR="$LIB_DIR" \
  ANDROID_NDK_HOME="$ANDROID_NDK_HOME" \
  "$LINKER_ENV=$LINKER" \
  cargo "${CARGO_ARGS[@]}"
RUNNER_LIBRARY="$ROOT/tests/mobile-e2e/runner/target/$TARGET/release/libcronet_mobile_e2e_runner.so"
[[ -f "$RUNNER_LIBRARY" ]] || fail "mobile E2E runner was not produced"

WORK="$APP_ROOT/.build/$TARGET"
rm -rf "$WORK"
mkdir -p "$WORK/classes" "$WORK/dex" "$WORK/apk/lib/$ABI"
javac -source 8 -target 8 -classpath "$ANDROID_JAR:$CRONET_ANDROID_SUPPORT_JAR" \
  -d "$WORK/classes" \
  "$APP_ROOT/src/io/github/southorange/cronet/e2e/MainActivity.java" \
  "$APP_ROOT/src/internal/org/jni_zero/JniInit.java"
jar cf "$WORK/classes.jar" -C "$WORK/classes" .
unzip -p "$CRONET_ANDROID_SUPPORT_DEX_JAR" classes.dex >"$WORK/cronet-support.dex"
"$BUILD_TOOLS/d8" --lib "$ANDROID_JAR" --min-api "$API_LEVEL" \
  --output "$WORK/dex" "$WORK/classes.jar" "$WORK/cronet-support.dex"
"$BUILD_TOOLS/aapt2" link -I "$ANDROID_JAR" --manifest "$APP_ROOT/AndroidManifest.xml" \
  --min-sdk-version "$API_LEVEL" --target-sdk-version 35 -o "$WORK/app-unsigned-unaligned.apk"
cp "$WORK/dex/classes.dex" "$WORK/apk/"
cp "$RUNNER_LIBRARY" "$WORK/apk/lib/$ABI/"
if [[ "$LINKAGE" == dynamic ]]; then
  cp "$CRONET_LIBRARY" "$WORK/apk/lib/$ABI/"
fi
(cd "$WORK/apk" && zip -q -r -u "$WORK/app-unsigned-unaligned.apk" classes.dex lib)
"$BUILD_TOOLS/zipalign" -f 4 "$WORK/app-unsigned-unaligned.apk" "$WORK/app-unsigned.apk"

KEYSTORE=${ANDROID_E2E_KEYSTORE:-"$APP_ROOT/debug.jks"}
if [[ ! -f "$KEYSTORE" ]]; then
  keytool -genkeypair -noprompt -keystore "$KEYSTORE" -storepass android \
    -keypass android -alias androiddebugkey -dname 'CN=cronet-rs E2E' \
    -keyalg RSA -keysize 2048 -validity 10000
fi
"$BUILD_TOOLS/apksigner" sign --ks "$KEYSTORE" --ks-pass pass:android \
  --key-pass pass:android --out "$WORK/app.apk" "$WORK/app-unsigned.apk"

EMULATOR_PID=''
if ! "$ADB" devices | awk 'NR > 1 && $2 == "device" { found=1 } END { exit !found }'; then
  EMULATOR=${EMULATOR:-"$ANDROID_SDK_ROOT/emulator/emulator"}
  [[ -x "$EMULATOR" ]] || fail "no device is connected and the emulator is missing"
  AVD=${ANDROID_E2E_AVD:-$($EMULATOR -list-avds 2>/dev/null | \
    grep -E '^[A-Za-z0-9_.-]+$' | head -n 1)}
  [[ -n "$AVD" ]] || fail "no Android Virtual Device is available"
  "$EMULATOR" -avd "$AVD" -no-window -no-audio -no-snapshot-save \
    >"$WORK/emulator.log" 2>&1 &
  EMULATOR_PID=$!
  trap '[[ -z "$EMULATOR_PID" ]] || kill "$EMULATOR_PID" 2>/dev/null || true' EXIT
  "$ADB" wait-for-device
  BOOTED=''
  for _ in $(seq 1 180); do
    if [[ $("$ADB" shell getprop sys.boot_completed 2>/dev/null | tr -d '\r') == 1 ]]; then
      BOOTED=1
      break
    fi
    kill -0 "$EMULATOR_PID" 2>/dev/null || {
      tail -n 100 "$WORK/emulator.log" >&2
      fail "the Android emulator exited before boot completed"
    }
    sleep 1
  done
  [[ -n "$BOOTED" ]] || fail "the Android emulator did not finish booting"
fi

DEVICE_ABI=$("$ADB" shell getprop ro.product.cpu.abi | tr -d '\r')
[[ "$DEVICE_ABI" == "$ABI" ]] || fail "connected device ABI is $DEVICE_ABI, but $TARGET needs $ABI"
"$ADB" uninstall "$PACKAGE" >/dev/null 2>&1 || true
"$ADB" install "$WORK/app.apk" >/dev/null
"$ADB" logcat -c
"$ADB" shell am start -W -n "$PACKAGE/.MainActivity" >/dev/null

RESULT=''
for _ in $(seq 1 90); do
  RESULT=$("$ADB" shell run-as "$PACKAGE" cat files/cronet-rs-e2e.txt 2>/dev/null | tr -d '\r' || true)
  case "$RESULT" in
    PASS*|FAIL*) break ;;
  esac
  sleep 1
done
printf '%s\n' "$RESULT"
[[ "$RESULT" == PASS* ]] || {
  "$ADB" logcat -d -v threadtime | rg \
    'cronet-rs-e2e|chromium|jni_zero|AndroidRuntime|F libc|ClassNotFoundException' >&2 || true
  fail "application E2E did not pass"
}

#!/usr/bin/env bash
set -euo pipefail

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
HAP_TEMPLATE="$ROOT/tests/ohos-e2e/hap"
RUNNER_MANIFEST="$ROOT/tests/ohos-e2e/runner/Cargo.toml"
BUNDLE_NAME=io.github.southorange.cronet.e2e
TARGET=${OHOS_E2E_TARGET:-aarch64-unknown-linux-ohos}
LINKAGE=${OHOS_E2E_LINKAGE:-dynamic}

fail() {
  echo "cronet-rs OHOS E2E: $*" >&2
  exit 1
}

case "$TARGET" in
  aarch64-unknown-linux-ohos) ABI=arm64-v8a ;;
  armv7-unknown-linux-ohos) ABI=armeabi-v7a ;;
  x86_64-unknown-linux-ohos) ABI=x86_64 ;;
  *) fail "unsupported OpenHarmony E2E target: $TARGET" ;;
esac
case "$LINKAGE" in
  dynamic) CRONET_NATIVE_PATTERN='libcronet.*.so' ;;
  static) CRONET_NATIVE_PATTERN='libcronet_static.a' ;;
  *) fail "unsupported OpenHarmony E2E linkage: $LINKAGE" ;;
esac

find_command() {
  local variable=$1
  local fallback=$2
  local value=${!variable:-}
  if [[ -n "$value" ]]; then
    [[ -x "$value" ]] || fail "$variable is not executable: $value"
    printf '%s\n' "$value"
    return
  fi
  command -v "$fallback" 2>/dev/null || true
}

SDK_NATIVE=${OHOS_SDK_NATIVE:-}
if [[ -z "$SDK_NATIVE" && -n "${OHOS_NDK_HOME:-}" ]]; then
  if [[ -d "$OHOS_NDK_HOME/llvm" && -d "$OHOS_NDK_HOME/sysroot" ]]; then
    SDK_NATIVE=$OHOS_NDK_HOME
  elif [[ -d "$OHOS_NDK_HOME/native" ]]; then
    SDK_NATIVE=$OHOS_NDK_HOME/native
  fi
fi
[[ -n "$SDK_NATIVE" ]] || fail \
  "set OHOS_SDK_NATIVE (or OHOS_NDK_HOME) to a complete OpenHarmony Native SDK"
[[ -d "$SDK_NATIVE/llvm" && -d "$SDK_NATIVE/sysroot" ]] || fail \
  "not an OpenHarmony Native SDK: $SDK_NATIVE"
SDK_PACKAGE="$SDK_NATIVE/oh-uni-package.json"
[[ -f "$SDK_PACKAGE" ]] || fail "Native SDK package metadata is missing: $SDK_PACKAGE"
COMPILE_SDK=$(sed -nE 's/.*"apiVersion":[[:space:]]*"?([0-9]+)"?.*/\1/p' \
  "$SDK_PACKAGE" | head -n 1)
[[ "$COMPILE_SDK" =~ ^[0-9]+$ ]] || fail \
  "could not determine compileSdkVersion from $SDK_PACKAGE"

WORK=$(mktemp -d "${TMPDIR:-/tmp}/cronet-ohos-e2e.XXXXXX")
trap 'rm -rf "$WORK"' EXIT
SDK_BASE=${OHOS_BASE_SDK_HOME:-}
SDK_COMPONENT_ROOT=$(dirname "$SDK_NATIVE")
if [[ -z "$SDK_BASE" || ! -d "$SDK_BASE/$COMPILE_SDK/native" ]]; then
  mkdir -p "$WORK/sdk/$COMPILE_SDK"
  for component in ets js native previewer toolchains; do
    [[ -f "$SDK_COMPONENT_ROOT/$component/oh-uni-package.json" ]] || fail \
      "SDK component metadata is missing: $SDK_COMPONENT_ROOT/$component/oh-uni-package.json"
    ln -s "$SDK_COMPONENT_ROOT/$component" "$WORK/sdk/$COMPILE_SDK/$component"
  done
  SDK_BASE="$WORK/sdk"
fi
[[ -d "$SDK_BASE" ]] || fail \
  "set OHOS_BASE_SDK_HOME to the SDK container that owns the injected Native SDK"
export OHOS_BASE_SDK_HOME=$SDK_BASE

HAP_ROOT="$WORK/hap"
cp -R "$HAP_TEMPLATE" "$HAP_ROOT"
rm -rf "$HAP_ROOT/.hvigor" "$HAP_ROOT/.ohos-sign" \
  "$HAP_ROOT/entry/build" "$HAP_ROOT/entry/libs"
sed -i.bak -E \
  "s/(\"compileSdkVersion\"[[:space:]]*:[[:space:]]*)[0-9]+/\1$COMPILE_SDK/" \
  "$HAP_ROOT/build-profile.json5"
rm -f "$HAP_ROOT/build-profile.json5.bak"

TOOLCHAINS=${OHOS_TOOLCHAINS:-"$(dirname "$SDK_NATIVE")/toolchains"}
HDC=$(find_command HDC hdc)
[[ -n "$HDC" ]] || {
  [[ -x "$TOOLCHAINS/hdc" ]] || fail "set HDC or add hdc to PATH"
  HDC=$TOOLCHAINS/hdc
}
HDC_COMMAND=("$HDC")
if [[ -n "${OHOS_E2E_DEVICE:-}" ]]; then
  HDC_COMMAND+=(-t "$OHOS_E2E_DEVICE")
fi
HVIGORW=$(find_command HVIGORW hvigorw)
[[ -n "$HVIGORW" ]] || fail "set HVIGORW or add hvigorw to PATH"

SOURCE_DIR=${CRONET_SOURCE_DIR:-"$ROOT/.cronet/chromium/src"}
TARGET_DIRECTORY=${TARGET//-/_}
LIB_DIR=${CRONET_LIB_DIR:-"$SOURCE_DIR/out/cronet-rs/$TARGET_DIRECTORY"}
if ! compgen -G "$LIB_DIR/$CRONET_NATIVE_PATTERN" >/dev/null; then
  cargo xtask sync --source-dir "$SOURCE_DIR"
  OHOS_SDK_NATIVE="$SDK_NATIVE" cargo xtask build \
    --release --linkage "$LINKAGE" --target "$TARGET" --source-dir "$SOURCE_DIR"
fi
CRONET_LIBRARY=''
if [[ "$LINKAGE" == dynamic ]]; then
  CRONET_LIBRARY=$(find "$LIB_DIR" -maxdepth 1 -type f -name 'libcronet.*.so' | head -n 1)
  [[ -n "$CRONET_LIBRARY" ]] || fail "Cronet shared library not found below $LIB_DIR"
fi

LINKER="$SDK_NATIVE/llvm/bin/$TARGET-clang"
CXX="$SDK_NATIVE/llvm/bin/$TARGET-clang++"
[[ -x "$LINKER" ]] || fail "OpenHarmony linker is missing: $LINKER"
[[ -x "$CXX" ]] || fail "OpenHarmony C++ compiler is missing: $CXX"
LINKER_ENV=CARGO_TARGET_$(printf '%s' "$TARGET" | tr '[:lower:]-' '[:upper:]_')_LINKER
CARGO_ARGS=(build --manifest-path "$RUNNER_MANIFEST" --target "$TARGET" --release)
if [[ "$LINKAGE" == static ]]; then
  CARGO_ARGS+=(--features static)
fi
env \
  CRONET_SOURCE_DIR="$SOURCE_DIR" \
  CRONET_LIB_DIR="$LIB_DIR" \
  "$LINKER_ENV=$LINKER" \
  cargo "${CARGO_ARGS[@]}"

RUNNER_LIBRARY="$ROOT/tests/ohos-e2e/runner/target/$TARGET/release/libcronet_ohos_e2e_runner.so"
[[ -f "$RUNNER_LIBRARY" ]] || fail "E2E runner was not produced: $RUNNER_LIBRARY"
rm -rf "$HAP_ROOT/entry/libs"
mkdir -p "$HAP_ROOT/entry/libs/$ABI"
cp "$RUNNER_LIBRARY" "$HAP_ROOT/entry/libs/$ABI/"
if [[ "$LINKAGE" == dynamic ]]; then
  cp "$CRONET_LIBRARY" "$HAP_ROOT/entry/libs/$ABI/"
fi
"$CXX" -shared -fPIC -std=c++17 -nostdlib++ \
  -Wl,-soname,libentry.so \
  "$HAP_ROOT/entry/src/main/cpp/napi_init.cpp" \
  -lace_napi.z -ldl \
  -o "$HAP_ROOT/entry/libs/$ABI/libentry.so"

HVIGOR_ARGS=(
  assembleHap --mode module
  -p product=default
  -p module=entry@default
  -p buildMode=debug
  --no-daemon
)
if [[ -n "${NODE_HOME:-}" ]]; then
  HVIGOR_ARGS+=(--node-home "$NODE_HOME")
fi
(cd "$HAP_ROOT" && "$HVIGORW" "${HVIGOR_ARGS[@]}")

UNSIGNED_HAP="$HAP_ROOT/entry/build/default/outputs/default/entry-default-unsigned.hap"
[[ -f "$UNSIGNED_HAP" ]] || fail "Hvigor did not produce $UNSIGNED_HAP"
SIGN_DIR="$HAP_ROOT/.ohos-sign"
SIGNED_HAP="$SIGN_DIR/cronet-e2e-signed.hap"
mkdir -p "$SIGN_DIR"

UDID=$("${HDC_COMMAND[@]}" shell 'bm get --udid' | tail -n 1 | tr -d '\r\n ')
[[ -n "$UDID" ]] || fail "could not read the target device UDID"

if [[ -n "${OHOS_E2E_SIGNER:-}" ]]; then
  [[ -x "$OHOS_E2E_SIGNER" ]] || fail "OHOS_E2E_SIGNER is not executable"
  "$OHOS_E2E_SIGNER" "$UNSIGNED_HAP" "$SIGNED_HAP" "$BUNDLE_NAME" "$UDID"
else
  SIGN_TOOL="$TOOLCHAINS/lib/hap-sign-tool.jar"
  KEYSTORE="$TOOLCHAINS/lib/OpenHarmony.p12"
  PROFILE_TEMPLATE="$TOOLCHAINS/lib/UnsgnedDebugProfileTemplate.json"
  PROFILE_CERT="$TOOLCHAINS/lib/OpenHarmonyProfileDebug.pem"
  for tool in java jq keytool; do
    command -v "$tool" >/dev/null || fail \
      "$tool is required for the SDK development signer; alternatively set OHOS_E2E_SIGNER"
  done
  for file in "$SIGN_TOOL" "$KEYSTORE" "$PROFILE_TEMPLATE" "$PROFILE_CERT"; do
    [[ -f "$file" ]] || fail \
      "SDK development signing material is missing: $file; set OHOS_E2E_SIGNER"
  done

  NOW=$(date +%s)
  jq --arg bundle "$BUNDLE_NAME" --arg udid "$UDID" \
    --argjson before "$((NOW - 3600))" --argjson after "$((NOW + 31536000))" \
    '.validity["not-before"]=$before
     | .validity["not-after"]=$after
     | .["bundle-info"]["bundle-name"]=$bundle
     | .["debug-info"]["device-ids"]=[$udid]' \
    "$PROFILE_TEMPLATE" > "$SIGN_DIR/profile.json"
  jq -r '.["bundle-info"]["development-certificate"]' \
    "$SIGN_DIR/profile.json" > "$SIGN_DIR/app-leaf.pem"
  keytool -exportcert -rfc -alias 'openharmony application ca' \
    -storetype PKCS12 -keystore "$KEYSTORE" -storepass 123456 \
    > "$SIGN_DIR/app-ca.pem"
  keytool -exportcert -rfc -alias 'openharmony application root ca' \
    -storetype PKCS12 -keystore "$KEYSTORE" -storepass 123456 \
    > "$SIGN_DIR/app-root.pem"
  cp "$SIGN_DIR/app-root.pem" "$SIGN_DIR/app-chain.pem"
  cat "$SIGN_DIR/app-ca.pem" "$SIGN_DIR/app-leaf.pem" >> "$SIGN_DIR/app-chain.pem"

  java -jar "$SIGN_TOOL" sign-profile -mode localSign \
    -keyAlias 'openharmony application profile debug' -keyPwd 123456 \
    -profileCertFile "$PROFILE_CERT" -inFile "$SIGN_DIR/profile.json" \
    -signAlg SHA256withECDSA -keystoreFile "$KEYSTORE" -keystorePwd 123456 \
    -outFile "$SIGN_DIR/profile.p7b"
  java -jar "$SIGN_TOOL" sign-app -mode localSign \
    -keyAlias 'openharmony application release' -keyPwd 123456 \
    -appCertFile "$SIGN_DIR/app-chain.pem" -profileFile "$SIGN_DIR/profile.p7b" \
    -inFile "$UNSIGNED_HAP" -signAlg SHA256withECDSA \
    -keystoreFile "$KEYSTORE" -keystorePwd 123456 -outFile "$SIGNED_HAP" \
    -compatibleVersion 12 -signCode 0
fi
[[ -f "$SIGNED_HAP" ]] || fail "signer did not produce $SIGNED_HAP"

"${HDC_COMMAND[@]}" uninstall "$BUNDLE_NAME" >/dev/null 2>&1 || true
"${HDC_COMMAND[@]}" install -r "$SIGNED_HAP"
"${HDC_COMMAND[@]}" shell "aa start -b $BUNDLE_NAME -a EntryAbility"

RESULT=''
for _ in $(seq 1 60); do
  RESULT=$("${HDC_COMMAND[@]}" shell -b "$BUNDLE_NAME" \
    'cat data/storage/el2/base/files/cronet-rs-e2e.txt 2>/dev/null' \
    2>/dev/null | tr -d '\r' || true)
  case "$RESULT" in
    PASS*|FAIL*) break ;;
  esac
  sleep 1
done

printf '%s\n' "$RESULT"
[[ "$RESULT" == PASS* ]] || fail "application E2E did not pass"

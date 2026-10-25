#!/bin/sh
set -eu

SWITCH_SOURCE_ROOT="${PROJECT_DIR}"
. "${PROJECT_DIR}/scripts/workspace-env.sh"
cd "${PROJECT_DIR}"

LIBUSB_PKG_CONFIG=""
if [ -n "${LIBUSB_DYLIB_ARM64:-}" ]; then
  LIBUSB_PKG_CONFIG="$(dirname "${LIBUSB_DYLIB_ARM64}")/pkgconfig"
fi
export PKG_CONFIG_PATH="${LIBUSB_PKG_CONFIG:+${LIBUSB_PKG_CONFIG}:}${PKG_CONFIG_PATH:-/opt/homebrew/lib/pkgconfig}"
MACOS_MIN="${MACOSX_DEPLOYMENT_TARGET:-27.0}"
export MACOSX_DEPLOYMENT_TARGET="${MACOS_MIN}"

[ "${ARCHS}" = "arm64" ] || { echo "错误：Switch 仅构建 Apple Silicon 版本" >&2; exit 1; }
mkdir -p "${TARGET_TEMP_DIR}/backend"
python3 "${PROJECT_DIR}/scripts/build-module-supervisor.py"
python3 "${PROJECT_DIR}/scripts/build-evs.py"
# All production backend services run in the single Rust daemon.
CARGO_BIN="${SWITCH_CARGO_BIN:-$(command -v cargo)}"
[ -x "${CARGO_BIN}" ] || { echo "错误：未找到 Rust cargo (${CARGO_BIN})" >&2; exit 1; }
set -- $("${PROJECT_DIR}/scripts/version.sh")
export SWITCH_APP_VERSION=$1
export SWITCH_BUILD_NUMBER=$2
export SWITCH_SOURCE_DIGEST=$(python3 "${PROJECT_DIR}/scripts/source-identity.py")
SOURCE_DIGEST_BEFORE_BUILD=$SWITCH_SOURCE_DIGEST
"${CARGO_BIN}" build --locked --release \
  --manifest-path "${PROJECT_DIR}/rust/Cargo.toml" \
  -p proletariusd
[ "$(python3 "${PROJECT_DIR}/scripts/source-identity.py")" = "$SOURCE_DIGEST_BEFORE_BUILD" ] || {
  echo "错误：构建期间源码发生变化，拒绝嵌入不匹配的源码身份。" >&2
  exit 1
}

# 合并到 app 资源目录
BACKEND_DIR="${TARGET_BUILD_DIR}/${UNLOCALIZED_RESOURCES_FOLDER_PATH}/backend"
rm -rf "${BACKEND_DIR}"
mkdir -p "${BACKEND_DIR}"
cp "${CARGO_TARGET_DIR}/release/switchd-rs" "${BACKEND_DIR}/switchd-rs"
LIBUSB="${LIBUSB_DYLIB_ARM64:-/opt/homebrew/opt/libusb/lib/libusb-1.0.0.dylib}"
[ -f "$LIBUSB" ] || { echo "错误：未找到 arm64 libusb (${LIBUSB})，请设置 LIBUSB_DYLIB_ARM64" >&2; exit 1; }
cp "$LIBUSB" "${BACKEND_DIR}/libusb-1.0.0.dylib"
for backend in "${BACKEND_DIR}/switchd-rs"; do
  old=$(otool -L "$backend" 2>/dev/null | awk '/libusb/ {print $1; exit}')
  if [ -n "$old" ]; then
    install_name_tool -change "$old" "@loader_path/libusb-1.0.0.dylib" "$backend"
  fi
  if otool -L "$backend" | awk '/libusb/ {print $1}' | grep -qv '^@loader_path/libusb-1.0.0.dylib$'; then
    echo "错误：后端仍引用开发机的 libusb 路径，拒绝打包。" >&2
    exit 1
  fi
done

# Keep source-license notices with the redistributed runtime too.
NOTICES_DIR="${TARGET_BUILD_DIR}/${UNLOCALIZED_RESOURCES_FOLDER_PATH}/licenses"
mkdir -p "${NOTICES_DIR}"
cp "${PROJECT_DIR}/THIRD_PARTY_NOTICES.md" "${NOTICES_DIR}/THIRD_PARTY_NOTICES.md"
cp "${PROJECT_DIR}/third_party/evs/NOTICE.md" "${NOTICES_DIR}/EVS-NOTICE.md"
cp "${PROJECT_DIR}/third_party/evs/readme.txt" "${NOTICES_DIR}/EVS-reference-readme.txt"
cp "${PROJECT_DIR}/LICENSE" "${NOTICES_DIR}/Proletarius-LICENSE"
for notice in LICENSE-MIT LICENSE-APACHE COPYRIGHT COPYRIGHT-library.html; do
  cp "${PROJECT_DIR}/licenses/rust-std-${notice}" "${NOTICES_DIR}/rust-std-${notice}"
done
cp -R "${PROJECT_DIR}/licenses/module-voice-supervisor" "${NOTICES_DIR}/"
for dependency in euicc-go uicc-go quectel-qmi-go swu-go opencore-amr; do
  cp "${PROJECT_DIR}/licenses/${dependency}-LICENSE" "${NOTICES_DIR}/${dependency}-LICENSE"
done
cp "${PROJECT_DIR}/data/carriers/ios-carrier-profiles-MIT.txt" "${NOTICES_DIR}/ios-carrier-profiles-MIT.txt"
cp "${PROJECT_DIR}/licenses/euicc-go-bertlv-LICENSE" "${NOTICES_DIR}/euicc-go-bertlv-LICENSE"
cp "${PROJECT_DIR}/licenses/opencore-amr-NOTICE" "${NOTICES_DIR}/opencore-amr-NOTICE"
# Redistribute only the source-pinned public notice. Environment is never a trust root.
LIBUSB_LICENSE="${PROJECT_DIR}/licenses/libusb-COPYING"
[ -f "$LIBUSB_LICENSE" ] && [ ! -L "$LIBUSB_LICENSE" ] &&
[ "$(stat -f '%z' "$LIBUSB_LICENSE")" = 26436 ] &&
[ "$(shasum -a 256 "$LIBUSB_LICENSE" | awk '{print $1}')" = 5df07007198989c622f5d41de8d703e7bef3d0e79d62e24332ee739a452af62a ] || {
  echo "错误：源码 libusb 许可缺失或校验不符，拒绝构建。" >&2
  exit 1
}
cp -X "$LIBUSB_LICENSE" "${NOTICES_DIR}/libusb-COPYING"

# 所有嵌套 Mach-O 在 lipo/install_name_tool 完成后统一进行 ad-hoc 签名。
for code in \
"${BACKEND_DIR}/libusb-1.0.0.dylib" \
"${BACKEND_DIR}/switchd-rs"; do
  /usr/bin/codesign --force --sign - --timestamp=none "$code"
done

# 增量构建时 Xcode 可能跳过外层签名；重新封装资源签名，首次构建仍会由 Xcode 最终签名覆盖。
APP_BUNDLE="${TARGET_BUILD_DIR}/${WRAPPER_NAME}"
if [ -d "${APP_BUNDLE}" ]; then
  /usr/bin/codesign --force --sign - --timestamp=none "${APP_BUNDLE}"
fi

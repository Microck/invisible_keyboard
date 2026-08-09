#!/usr/bin/env bash
# Build the pinned Gate 0A capture stack in a Crabbox. This script never
# installs into the host and never modifies an existing output directory.

set -euo pipefail
IFS=$'\n\t'
umask 022

readonly LIBSIGROK_REPOSITORY="https://github.com/sigrokproject/libsigrok.git"
readonly LIBSIGROK_COMMIT="0bc2487778e660f4d3116729b6f4aee2b1996bb0"
readonly SIGROK_CLI_REPOSITORY="https://github.com/sigrokproject/sigrok-cli.git"
readonly SIGROK_CLI_COMMIT="f44dd91347e7ac797cefc23162b9fcf0b7329f1f"
readonly SIGROK_FIRMWARE_REPOSITORY="https://github.com/sigrokproject/sigrok-firmware.git"
readonly SIGROK_FIRMWARE_COMMIT="11eed913a9c535b87b5d5b5b92d2622cf34cee8b"
readonly DSVIEW_REPOSITORY="https://github.com/DreamSourceLab/DSView"
readonly DSVIEW_COMMIT="886b847c21c606df3138ce7ad8f8e8c363ee758b"
readonly SIGROK_EXTRACTOR_COMMIT="69747ca48b3d17ac7532439e487b9ba829828cfd"

readonly FPGA_SOURCE_NAME="DSLogicPlus.bin"
readonly FPGA_INSTALLED_NAME="dreamsourcelab-dslogic-plus-fpga.fw"
readonly FPGA_SHA256="e16c70e4af9181044e3ec3a363e4bb109f1d731b4f9f046f8bc1d1015a7842a7"
readonly FPGA_BYTES="341160"
readonly FX2_SOURCE_NAME="DSLogicPlus.fw"
readonly FX2_INSTALLED_NAME="dreamsourcelab-dslogic-plus-fx2.fw"
readonly FX2_SHA256="9197e08dd33a9cda61cc8cd3de559cbae9b049b2dfa827c089ffe0528f5dcc5e"
readonly FX2_BYTES="8120"
readonly DSVIEW_COPYING_SHA256="0b383d5a63da644f628d99c33976ea6487ed89aaa59f0b3257992deac1171e6b"
readonly DSVIEW_COPYING_BYTES="35821"

die() {
  printf 'gate0a-stack-build: %s\n' "$*" >&2
  exit 1
}

usage() {
  printf 'usage: IKB_CRABBOX_BUILD=1 %s ABSOLUTE_EMPTY_OUTPUT_DIRECTORY\n' "$0" >&2
  exit 64
}

require_command() {
  command -v "$1" >/dev/null 2>&1 || die "required command not found: $1"
}

verify_sha256() {
  local expected="$1"
  local path="$2"
  local actual
  actual="$(sha256sum "$path")"
  actual="${actual%% *}"
  [[ "$actual" == "$expected" ]] || die "SHA-256 mismatch for $path"
}

verify_bytes() {
  local expected="$1"
  local path="$2"
  local actual
  actual="$(stat -c '%s' "$path")"
  [[ "$actual" == "$expected" ]] || die "size mismatch for $path: $actual, expected $expected"
}

resolve_bazel_runfile() {
  local setting_name="$1"
  local logical_path="${!setting_name:-}"
  local resolved_path=""

  [[ -n "$logical_path" ]] ||
    die "missing Bazel runfile identity: $setting_name"
  if [[ -n "${RUNFILES_DIR:-}" && -f "$RUNFILES_DIR/$logical_path" ]]; then
    resolved_path="$RUNFILES_DIR/$logical_path"
  elif [[ -n "${RUNFILES_MANIFEST_FILE:-}" ]]; then
    resolved_path="$(
      awk -v key="$logical_path" '
        index($0, key " ") == 1 {
          sub(/^[^ ]+ /, "")
          print
          exit
        }
      ' "$RUNFILES_MANIFEST_FILE"
    )"
  fi
  [[ -n "$resolved_path" && -f "$resolved_path" ]] ||
    die "missing Bazel runfile: $logical_path"
  printf '%s\n' "$resolved_path"
}

clone_exact() {
  local repository="$1"
  local commit="$2"
  local destination="$3"

  git init -q "$destination"
  git -C "$destination" remote add origin "$repository"
  git -C "$destination" fetch -q --depth=1 origin "$commit"
  git -C "$destination" checkout -q --detach FETCH_HEAD
  [[ "$(git -C "$destination" rev-parse HEAD)" == "$commit" ]] ||
    die "checkout mismatch for $repository"
}

require_command awk
require_command readlink

# These paths come from declared sh_binary data. Resolving them before the
# authorization guard lets the contract test exercise the real Bazel
# entrypoint without starting the upstream build.
readonly PATCH_SOURCE="$(resolve_bazel_runfile IKB_GATE0A_PATCH_RLOCATION)"
readonly PROVENANCE_SOURCE="$(resolve_bazel_runfile IKB_GATE0A_PROVENANCE_RLOCATION)"
readonly BUILD_SCRIPT_SOURCE="$(readlink -f "${BASH_SOURCE[0]}")"
[[ -f "$BUILD_SCRIPT_SOURCE" ]] || die "missing build recipe source"

[[ "${IKB_CRABBOX_BUILD:-}" == "1" ]] ||
  die "refusing to compile outside the explicit Crabbox build environment"
[[ "$#" == "1" ]] || usage
[[ "$1" == /* ]] || usage

readonly OUTPUT_ROOT="$1"
case "$OUTPUT_ROOT" in
  /|/home|/home/user) die "refusing broad output path: $OUTPUT_ROOT" ;;
esac

if [[ -e "$OUTPUT_ROOT" ]]; then
  [[ -d "$OUTPUT_ROOT" ]] || die "output path exists and is not a directory"
  [[ -z "$(find "$OUTPUT_ROOT" -mindepth 1 -maxdepth 1 -print -quit)" ]] ||
    die "output directory is not empty: $OUTPUT_ROOT"
else
  mkdir -p "$OUTPUT_ROOT"
fi

for command_name in \
  autoconf automake curl file gcc git libtoolize make pkg-config sha256sum \
  readelf sed strings tar; do
  require_command "$command_name"
done

readonly WORK_ROOT="$OUTPUT_ROOT/work"
readonly INSTALL_PREFIX="/opt/invisible-keyboard/gate0a"
readonly DESTDIR="$OUTPUT_ROOT/sysroot"
readonly PREFIX="$DESTDIR$INSTALL_PREFIX"
readonly ARTIFACT_ROOT="$OUTPUT_ROOT/artifacts"
readonly LOG_ROOT="$OUTPUT_ROOT/logs"
readonly SOURCE_ROOT="$OUTPUT_ROOT/corresponding-source"
mkdir -p "$WORK_ROOT" "$PREFIX" "$ARTIFACT_ROOT" "$LOG_ROOT" "$SOURCE_ROOT"

export LC_ALL=C
export TZ=UTC
export SOURCE_DATE_EPOCH=0
export CFLAGS="-O2 -g0 -ffile-prefix-map=$OUTPUT_ROOT=/usr/src/gate0a -fdebug-prefix-map=$OUTPUT_ROOT=/usr/src/gate0a"

clone_exact "$LIBSIGROK_REPOSITORY" "$LIBSIGROK_COMMIT" "$WORK_ROOT/libsigrok"
clone_exact "$SIGROK_CLI_REPOSITORY" "$SIGROK_CLI_COMMIT" "$WORK_ROOT/sigrok-cli"
clone_exact "$SIGROK_FIRMWARE_REPOSITORY" "$SIGROK_FIRMWARE_COMMIT" "$WORK_ROOT/sigrok-firmware"

git -C "$WORK_ROOT/libsigrok" apply --check "$PATCH_SOURCE"
git -C "$WORK_ROOT/libsigrok" apply "$PATCH_SOURCE"
git -C "$WORK_ROOT/libsigrok" diff --check

(
  cd "$WORK_ROOT/libsigrok"
  ./autogen.sh
) >"$LOG_ROOT/libsigrok-autogen.log" 2>&1

mkdir "$WORK_ROOT/libsigrok-build"
(
  cd "$WORK_ROOT/libsigrok-build"
  "$WORK_ROOT/libsigrok/configure" \
    --prefix="$INSTALL_PREFIX" \
    --disable-shared \
    --enable-static \
    --disable-bindings \
    --disable-all-drivers \
    --enable-dreamsourcelab-dslogic
  make -j"$(getconf _NPROCESSORS_ONLN)"
  make DESTDIR="$DESTDIR" install
) >"$LOG_ROOT/libsigrok-build.log" 2>&1

# Keep the compiled install prefix fixed while pointing this one build at the
# DESTDIR-staged static library. Applying a pkg-config sysroot globally would
# also redirect system dependencies such as glib into the empty DESTDIR.
readonly BUILD_PKGCONFIG="$WORK_ROOT/build-pkgconfig"
mkdir "$BUILD_PKGCONFIG"
sed "s|^prefix=.*|prefix=$PREFIX|" \
  "$PREFIX/lib/pkgconfig/libsigrok.pc" >"$BUILD_PKGCONFIG/libsigrok.pc"

(
  cd "$WORK_ROOT/sigrok-cli"
  ./autogen.sh
) >"$LOG_ROOT/sigrok-cli-autogen.log" 2>&1

mkdir "$WORK_ROOT/sigrok-cli-build"
(
  cd "$WORK_ROOT/sigrok-cli-build"
  PKG_CONFIG_PATH="$BUILD_PKGCONFIG" \
  PKG_CONFIG="pkg-config --static" \
    "$WORK_ROOT/sigrok-cli/configure" \
      --prefix="$INSTALL_PREFIX" \
      --without-libsigrokdecode
  make -j"$(getconf _NPROCESSORS_ONLN)"
  make DESTDIR="$DESTDIR" install
) >"$LOG_ROOT/sigrok-cli-build.log" 2>&1

readonly CAPTURE_BINARY="$PREFIX/bin/sigrok-cli"
[[ -x "$CAPTURE_BINARY" ]] || die "capture binary was not installed"
if ldd "$CAPTURE_BINARY" | grep -q 'libsigrok'; then
  die "capture binary still has a dynamic libsigrok dependency"
fi
strings "$CAPTURE_BINARY" | grep -F \
  'IKB_DSLOGIC_TRANSFER_V1 completed_transfers=%lu anomalies=0' >/dev/null ||
  die "patched transfer receipt was not embedded in capture binary"

"$CAPTURE_BINARY" --version >"$LOG_ROOT/sigrok-cli-version.log" 2>&1
ldd "$CAPTURE_BINARY" >"$LOG_ROOT/sigrok-cli-ldd.log"
file "$CAPTURE_BINARY" >"$LOG_ROOT/sigrok-cli-file.log"

readonly FIRMWARE_STAGE="$WORK_ROOT/device-firmware"
mkdir "$FIRMWARE_STAGE"

curl -fsSLo "$FIRMWARE_STAGE/$FPGA_INSTALLED_NAME" \
  "$DSVIEW_REPOSITORY/raw/$DSVIEW_COMMIT/DSView/res/$FPGA_SOURCE_NAME"
curl -fsSLo "$FIRMWARE_STAGE/$FX2_INSTALLED_NAME" \
  "$DSVIEW_REPOSITORY/raw/$DSVIEW_COMMIT/DSView/res/$FX2_SOURCE_NAME"
curl -fsSLo "$FIRMWARE_STAGE/COPYING.dsview" \
  "$DSVIEW_REPOSITORY/raw/$DSVIEW_COMMIT/COPYING"

verify_sha256 "$FPGA_SHA256" "$FIRMWARE_STAGE/$FPGA_INSTALLED_NAME"
verify_bytes "$FPGA_BYTES" "$FIRMWARE_STAGE/$FPGA_INSTALLED_NAME"
verify_sha256 "$FX2_SHA256" "$FIRMWARE_STAGE/$FX2_INSTALLED_NAME"
verify_bytes "$FX2_BYTES" "$FIRMWARE_STAGE/$FX2_INSTALLED_NAME"
verify_sha256 "$DSVIEW_COPYING_SHA256" "$FIRMWARE_STAGE/COPYING.dsview"
verify_bytes "$DSVIEW_COPYING_BYTES" "$FIRMWARE_STAGE/COPYING.dsview"

cat >"$FIRMWARE_STAGE/firmware-manifest.json" <<EOF
{
  "schema": "invisible-keyboard-dslogic-plus-firmware-v1",
  "dsview_commit": "$DSVIEW_COMMIT",
  "sigrok_extractor_commit": "$SIGROK_EXTRACTOR_COMMIT",
  "files": [
    {
      "installed_name": "$FPGA_INSTALLED_NAME",
      "source_name": "$FPGA_SOURCE_NAME",
      "bytes": $FPGA_BYTES,
      "sha256": "$FPGA_SHA256"
    },
    {
      "installed_name": "$FX2_INSTALLED_NAME",
      "source_name": "$FX2_SOURCE_NAME",
      "bytes": $FX2_BYTES,
      "sha256": "$FX2_SHA256"
    }
  ]
}
EOF
chmod 0644 "$FIRMWARE_STAGE"/*

tar \
  --format=ustar \
  --sort=name \
  --owner=0 \
  --group=0 \
  --numeric-owner \
  --mtime='@0' \
  -C "$FIRMWARE_STAGE" \
  -cf "$ARTIFACT_ROOT/gate0a-dslogic-plus-firmware.tar" \
  COPYING.dsview \
  firmware-manifest.json \
  "$FPGA_INSTALLED_NAME" \
  "$FX2_INSTALLED_NAME"

readonly EXPECTED_FIRMWARE_LIST="$WORK_ROOT/expected-firmware-list.txt"
cat >"$EXPECTED_FIRMWARE_LIST" <<EOF
COPYING.dsview
firmware-manifest.json
$FPGA_INSTALLED_NAME
$FX2_INSTALLED_NAME
EOF
tar -tf "$ARTIFACT_ROOT/gate0a-dslogic-plus-firmware.tar" >"$WORK_ROOT/actual-firmware-list.txt"
cmp "$EXPECTED_FIRMWARE_LIST" "$WORK_ROOT/actual-firmware-list.txt" >/dev/null ||
  die "firmware archive member list is not canonical"

install -m 0755 "$CAPTURE_BINARY" "$ARTIFACT_ROOT/sigrok-cli-gate0a"
install -m 0644 "$PATCH_SOURCE" "$ARTIFACT_ROOT/libsigrok-gate0a.patch"
install -m 0644 "$PROVENANCE_SOURCE" "$ARTIFACT_ROOT/capture-stack-provenance.md"

git -C "$WORK_ROOT/libsigrok" archive \
  --format=tar \
  --prefix="libsigrok-$LIBSIGROK_COMMIT/" \
  "$LIBSIGROK_COMMIT" >"$SOURCE_ROOT/libsigrok-$LIBSIGROK_COMMIT.tar"
git -C "$WORK_ROOT/sigrok-cli" archive \
  --format=tar \
  --prefix="sigrok-cli-$SIGROK_CLI_COMMIT/" \
  "$SIGROK_CLI_COMMIT" >"$SOURCE_ROOT/sigrok-cli-$SIGROK_CLI_COMMIT.tar"
git -C "$WORK_ROOT/sigrok-firmware" archive \
  --format=tar \
  --prefix="sigrok-firmware-$SIGROK_FIRMWARE_COMMIT/" \
  "$SIGROK_FIRMWARE_COMMIT" >"$SOURCE_ROOT/sigrok-firmware-$SIGROK_FIRMWARE_COMMIT.tar"
install -m 0644 "$PATCH_SOURCE" "$SOURCE_ROOT/libsigrok-gate0a.patch"
install -m 0644 "$PROVENANCE_SOURCE" "$SOURCE_ROOT/capture-stack-provenance.md"
install -m 0755 "$BUILD_SCRIPT_SOURCE" "$SOURCE_ROOT/build-gate0a-capture-stack.sh"
install -m 0644 "$WORK_ROOT/libsigrok/COPYING" "$SOURCE_ROOT/COPYING.libsigrok"
install -m 0644 "$WORK_ROOT/sigrok-cli/COPYING" "$SOURCE_ROOT/COPYING.sigrok-cli"
install -m 0644 "$WORK_ROOT/sigrok-firmware/COPYING" "$SOURCE_ROOT/COPYING.sigrok-firmware"
install -m 0644 "$FIRMWARE_STAGE/COPYING.dsview" "$SOURCE_ROOT/COPYING.dsview"

{
  printf 'libsigrok_commit=%s\n' "$LIBSIGROK_COMMIT"
  printf 'sigrok_cli_commit=%s\n' "$SIGROK_CLI_COMMIT"
  printf 'sigrok_firmware_commit=%s\n' "$SIGROK_FIRMWARE_COMMIT"
  printf 'dsview_commit=%s\n' "$DSVIEW_COMMIT"
  printf 'sigrok_extractor_commit=%s\n' "$SIGROK_EXTRACTOR_COMMIT"
  printf 'patch_sha256=%s\n' "$(sha256sum "$PATCH_SOURCE" | cut -d' ' -f1)"
  printf 'cc=%s\n' "$(gcc --version | head -n1)"
  printf 'host=%s\n' "$(uname -m)-linux"
  printf 'byte_order=little\n'
  printf '\nELF needed libraries:\n'
  readelf -d "$CAPTURE_BINARY" | awk '$2 == "(NEEDED)" { print $5 }'
} >"$ARTIFACT_ROOT/BUILD-INFO.txt"

(
  cd "$OUTPUT_ROOT"
  find artifacts corresponding-source logs -type f -print0 |
    LC_ALL=C sort -z |
    xargs -0 sha256sum
) >"$OUTPUT_ROOT/SHA256SUMS"

printf 'Gate 0A capture stack built in %s\n' "$OUTPUT_ROOT"
printf 'Capture binary: %s\n' "$ARTIFACT_ROOT/sigrok-cli-gate0a"
printf 'Firmware bundle: %s\n' "$ARTIFACT_ROOT/gate0a-dslogic-plus-firmware.tar"
printf 'Checksums: %s\n' "$OUTPUT_ROOT/SHA256SUMS"

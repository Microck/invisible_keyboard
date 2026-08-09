#!/usr/bin/env bash
set -euo pipefail

usage() {
  echo "usage: $0 --source-dir DIR --manifest FILE --lock FILE --cache-dir DIR --left-elf FILE --left-hex FILE --left-uf2 FILE --right-elf FILE --right-hex FILE --right-uf2 FILE --provenance FILE" >&2
  exit 2
}

source_dir=
manifest_file=
lock_file=
cache_dir=
left_elf=
left_hex=
left_uf2=
right_elf=
right_hex=
right_uf2=
provenance=
while (($# > 0)); do
  case "$1" in
    --source-dir) source_dir=$2; shift 2 ;;
    --manifest) manifest_file=$2; shift 2 ;;
    --lock) lock_file=$2; shift 2 ;;
    --cache-dir) cache_dir=$2; shift 2 ;;
    --left-elf) left_elf=$2; shift 2 ;;
    --left-hex) left_hex=$2; shift 2 ;;
    --left-uf2) left_uf2=$2; shift 2 ;;
    --right-elf) right_elf=$2; shift 2 ;;
    --right-hex) right_hex=$2; shift 2 ;;
    --right-uf2) right_uf2=$2; shift 2 ;;
    --provenance) provenance=$2; shift 2 ;;
    *) usage ;;
  esac
done

[[ -n "$source_dir" && -n "$manifest_file" && -n "$lock_file" &&
   -n "$cache_dir" &&
   -n "$left_elf" && -n "$left_hex" && -n "$left_uf2" &&
   -n "$right_elf" && -n "$right_hex" && -n "$right_uf2" &&
   -n "$provenance" ]] || usage

source_dir=$(cd "$source_dir" && pwd -P)
mkdir -p "$cache_dir"
cache_root=$(cd "$cache_dir" && pwd -P)

if [[ $(uname -s) != Linux || $(uname -m) != x86_64 ]]; then
  echo "Zephyr SDK 1.0.1 lock supports only Linux x86_64 executors" >&2
  exit 1
fi

for command_name in curl find git grep cmake ninja python3.12 sha256sum tar wget; do
  command -v "$command_name" >/dev/null || {
    echo "missing cross-build prerequisite: $command_name" >&2
    exit 1
  }
done

lock_value() {
  local key=$1
  local value
  value=$(sed -n "s/^  ${key}: *\"\{0,1\}\([^\"]*\)\"\{0,1\}$/\\1/p" "$lock_file")
  [[ -n "$value" ]] || {
    echo "missing lock key: $key" >&2
    exit 1
  }
  printf '%s' "$value"
}

zephyr_version=$(lock_value zephyr_version)
zephyr_commit=$(lock_value zephyr_commit)
cmsis_6_commit=$(lock_value cmsis_6_commit)
hal_nordic_commit=$(lock_value hal_nordic_commit)
west_version=$(lock_value west_version)
sdk_version=$(lock_value sdk_version)
sdk_minimal_url=$(lock_value sdk_minimal_url)
sdk_minimal_sha256=$(lock_value sdk_minimal_sha256)
sdk_arm_url=$(lock_value sdk_arm_url)
sdk_arm_sha256=$(lock_value sdk_arm_sha256)
board=$(lock_value board)
plus_pin_source_commit=$(lock_value plus_pin_source_commit)

grep -Fq "revision: ${zephyr_commit}" "$manifest_file" || {
  echo "west manifest and dependency lock disagree on Zephyr" >&2
  exit 1
}

downloads_dir=${cache_root}/downloads
workspace_dir=${cache_root}/workspace
manifest_dir=${workspace_dir}/application
venv_dir=${cache_root}/venv
sdk_layout=gnu-arm-final-path-v2
sdk_dir=${cache_root}/zephyr-sdk-${sdk_version}-${sdk_layout}
mkdir -p "$downloads_dir" "$manifest_dir"

sdk_staging=
sdk_installing=
build_root=
cleanup() {
  if [[ -n "$sdk_staging" && -d "$sdk_staging" ]]; then
    rm -rf -- "$sdk_staging"
  fi
  if [[ -n "$build_root" && -d "$build_root" ]]; then
    rm -rf -- "$build_root"
  fi
  if [[ -n "$sdk_installing" && -d "$sdk_installing" ]]; then
    rm -rf -- "$sdk_installing"
  fi
}
trap cleanup EXIT

download_checked() {
  local url=$1
  local expected_sha256=$2
  local destination=$3
  if [[ ! -f "$destination" ]] ||
     ! printf '%s  %s\n' "$expected_sha256" "$destination" | sha256sum --check --status; then
    local partial=${destination}.partial
    rm -f "$partial"
    echo "downloading $(basename "$destination")" >&2
    curl --fail --location --retry 3 --output "$partial" "$url"
    printf '%s  %s\n' "$expected_sha256" "$partial" | sha256sum --check --status || {
      echo "checksum mismatch: $url" >&2
      rm -f "$partial"
      exit 1
    }
    mv "$partial" "$destination"
  fi
}

minimal_archive=${downloads_dir}/zephyr-sdk-${sdk_version}_linux-x86_64_minimal.tar.xz
arm_archive=${downloads_dir}/toolchain_gnu_linux-x86_64_arm-zephyr-eabi.tar.xz
download_checked "$sdk_minimal_url" "$sdk_minimal_sha256" "$minimal_archive"
download_checked "$sdk_arm_url" "$sdk_arm_sha256" "$arm_archive"

sdk_marker=${sdk_dir}/.ikb-sdk-complete
expected_sdk_marker="${sdk_layout} ${sdk_minimal_sha256} ${sdk_arm_sha256}"
if [[ ! -f "$sdk_marker" ]] || [[ $(<"$sdk_marker") != "$expected_sdk_marker" ]]; then
  if [[ -e "$sdk_dir" ]]; then
    echo "SDK cache is incomplete or does not match the lock: $sdk_dir" >&2
    echo "remove that exact directory and rerun the Bazel target" >&2
    exit 1
  fi
  sdk_staging=$(mktemp -d "${cache_root}/sdk-staging.XXXXXX")
  tar -xJf "$minimal_archive" -C "$sdk_staging"
  extracted_sdk=$(find "$sdk_staging" -mindepth 1 -maxdepth 1 -type d -name 'zephyr-sdk-*' -print -quit)
  [[ -n "$extracted_sdk" ]] || {
    echo "minimal SDK archive has an unexpected layout" >&2
    exit 1
  }

  # SDK 1.0.1 split GNU archives contain the target directory at their root.
  # The official bundle workflow extracts them below gnu/, which is also the
  # only location searched by cmake/zephyr/gnu/generic.cmake.
  mkdir "$extracted_sdk/gnu"
  tar -xJf "$arm_archive" -C "$extracted_sdk/gnu"

  arm_gcc=${extracted_sdk}/gnu/arm-zephyr-eabi/bin/arm-zephyr-eabi-gcc
  [[ -x "$arm_gcc" ]] || {
    echo "ARM SDK archive has an unexpected layout: $arm_gcc" >&2
    exit 1
  }

  # setup.sh patches host-tool ELF interpreters to the current absolute SDK
  # path. Move the tree into its final cache location before running setup so
  # those interpreters do not retain the temporary staging directory.
  mv "$extracted_sdk" "$sdk_dir"
  sdk_installing=$sdk_dir
  rm -rf -- "$sdk_staging"
  sdk_staging=

  # Install the pinned host-tools payload through the SDK's own setup logic.
  # Omitting -c avoids the user CMake package registry; the build supplies the
  # SDK path and toolchain variant explicitly below.
  (
    cd "$sdk_dir"
    ./setup.sh -t arm-zephyr-eabi -h
  )
  [[ -d "$sdk_dir/hosttools/sysroots/x86_64-pokysdk-linux" ]] || {
    echo "SDK host tools installation did not produce the x86_64 sysroot" >&2
    exit 1
  }

  printf '%s' "$expected_sdk_marker" >"$sdk_marker"
  sdk_installing=
fi

if [[ ! -x "$venv_dir/bin/python" ]]; then
  python3.12 -m venv "$venv_dir"
fi
west_marker=${venv_dir}/.ikb-west-${west_version}
if [[ ! -x "$venv_dir/bin/west" || ! -f "$west_marker" ]]; then
  "$venv_dir/bin/python" -m pip --disable-pip-version-check --quiet install \
    "west==${west_version}"
  : >"$west_marker"
fi
cp "$manifest_file" "$manifest_dir/west.yml"
if [[ ! -f "$workspace_dir/.west/config" ]]; then
  "$venv_dir/bin/west" init -l "$manifest_dir"
fi
workspace_matches_lock() {
  [[ -d "$workspace_dir/zephyr/.git" &&
     -d "$workspace_dir/modules/hal/cmsis_6/.git" &&
     -d "$workspace_dir/modules/hal/nordic/.git" ]] &&
    [[ $(git -C "$workspace_dir/zephyr" rev-parse HEAD 2>/dev/null) == "$zephyr_commit" ]] &&
    [[ $(git -C "$workspace_dir/modules/hal/cmsis_6" rev-parse HEAD 2>/dev/null) == "$cmsis_6_commit" ]] &&
    [[ $(git -C "$workspace_dir/modules/hal/nordic" rev-parse HEAD 2>/dev/null) == "$hal_nordic_commit" ]]
}

if ! workspace_matches_lock; then
  (
    cd "$workspace_dir"
    "$venv_dir/bin/west" update --narrow
  )
fi

[[ $(git -C "$workspace_dir/zephyr" rev-parse HEAD) == "$zephyr_commit" ]] || {
  echo "checked-out Zephyr does not match dependency lock" >&2
  exit 1
}
[[ $(git -C "$workspace_dir/modules/hal/cmsis_6" rev-parse HEAD) == "$cmsis_6_commit" ]] || {
  echo "checked-out CMSIS 6 does not match dependency lock" >&2
  exit 1
}
[[ $(git -C "$workspace_dir/modules/hal/nordic" rev-parse HEAD) == "$hal_nordic_commit" ]] || {
  echo "checked-out hal_nordic does not match dependency lock" >&2
  exit 1
}

requirements_marker=${venv_dir}/.ikb-requirements-${zephyr_commit}
if [[ ! -f "$requirements_marker" ]]; then
  "$venv_dir/bin/python" -m pip --disable-pip-version-check --quiet install \
    -r "$workspace_dir/zephyr/scripts/requirements-base.txt"
  : >"$requirements_marker"
fi

build_root=$(mktemp -d)
repository_root=$(cd "${source_dir}/../.." && pwd -P)

# Keep full DWARF while replacing the three executor-specific debug path roots
# with stable virtual locations. Debug-only mapping leaves loadable firmware
# bytes unchanged; the fail-closed scan below rejects any unhandled path.
reproducible_path_flags="\
-fdebug-prefix-map=${repository_root}=/usr/src/invisible-keyboard \
-fdebug-prefix-map=${cache_root}=/usr/src/invisible-keyboard/.zephyr-cache \
-fdebug-prefix-map=${build_root}=/usr/src/invisible-keyboard/.zephyr-build"

build_hand() {
  local hand=$1
  local output_elf=$2
  local output_hex=$3
  local output_uf2=$4
  local build_dir=${build_root}/${hand}
  local hand_config=${source_dir}/${hand}-hand.conf

  PATH="${venv_dir}/bin:${PATH}" \
  ZEPHYR_BASE="${workspace_dir}/zephyr" \
  ZEPHYR_SDK_INSTALL_DIR="$sdk_dir" \
  ZEPHYR_TOOLCHAIN_VARIANT=zephyr/gnu \
  cmake -S "$source_dir" -B "$build_dir" -G Ninja \
    -DBOARD="$board" \
    -DDTC_OVERLAY_FILE="${source_dir}/boards/xiao_ble_nrf52840_plus.overlay" \
    -DEXTRA_CONF_FILE="$hand_config" \
    -DEXTRA_CFLAGS="$reproducible_path_flags" \
    -DEXTRA_CXXFLAGS="$reproducible_path_flags" \
    -DEXTRA_AFLAGS="$reproducible_path_flags" \
    -DZEPHYR_MODULES="${workspace_dir}/modules/hal/cmsis_6;${workspace_dir}/modules/hal/nordic"
  cmake --build "$build_dir"
  mkdir -p "$(dirname "$output_elf")" "$(dirname "$output_hex")" \
    "$(dirname "$output_uf2")"
  cp "$build_dir/zephyr/zephyr.elf" "$output_elf"
  cp "$build_dir/zephyr/zephyr.hex" "$output_hex"
  cp "$build_dir/zephyr/zephyr.uf2" "$output_uf2"

  # Publishing an ELF with a real executor path would make its hash depend on
  # the Bazel output base or temporary directory. Fail before provenance is
  # written so a path-dependent artifact cannot look like a valid build.
  for forbidden_path in "$repository_root" "$cache_root" "$build_root"; do
    if LC_ALL=C grep -aFq -- "$forbidden_path" "$output_elf"; then
      echo "published ELF retains executor path: $forbidden_path" >&2
      exit 1
    fi
  done
}

build_hand left "$left_elf" "$left_hex" "$left_uf2"
build_hand right "$right_elf" "$right_hex" "$right_uf2"

controller_id_from_conf() {
  local config_file=$1
  local controller_id
  controller_id=$(sed -n 's/^CONFIG_IKB_CONTROLLER_ID="\(.*\)"$/\1/p' "$config_file")
  [[ -n "$controller_id" ]] || {
    echo "missing provisioned controller identity: $config_file" >&2
    exit 1
  }
  printf '%s' "$controller_id"
}

file_sha256() {
  local checksum
  checksum=$(sha256sum "$1")
  printf '%s' "${checksum%% *}"
}

left_controller_id=$(controller_id_from_conf "${source_dir}/left-hand.conf")
right_controller_id=$(controller_id_from_conf "${source_dir}/right-hand.conf")
left_controller_sha256=$(printf '%s' "$left_controller_id" | sha256sum)
left_controller_sha256=${left_controller_sha256%% *}
right_controller_sha256=$(printf '%s' "$right_controller_id" | sha256sum)
right_controller_sha256=${right_controller_sha256%% *}
left_elf_sha256=$(file_sha256 "$left_elf")
left_hex_sha256=$(file_sha256 "$left_hex")
left_uf2_sha256=$(file_sha256 "$left_uf2")
right_elf_sha256=$(file_sha256 "$right_elf")
right_hex_sha256=$(file_sha256 "$right_hex")
right_uf2_sha256=$(file_sha256 "$right_uf2")
firmware_build_sha256=$(
  printf 'left_uf2_sha256=%s\nright_uf2_sha256=%s\n' \
    "$left_uf2_sha256" "$right_uf2_sha256" | sha256sum
)
firmware_build_sha256=${firmware_build_sha256%% *}
mkdir -p "$(dirname "$provenance")"
printf '%s\n' \
  'schema=invisible-keyboard-zephyr-build-provenance-v1' \
  "firmware_build=ikb-zephyr-uf2-pair-sha256:${firmware_build_sha256}" \
  "board=${board}" \
  "zephyr_version=${zephyr_version}" \
  "zephyr_commit=${zephyr_commit}" \
  "sdk_version=${sdk_version}" \
  "plus_pin_source_commit=${plus_pin_source_commit}" \
  "left_controller_id=${left_controller_id}" \
  "left_controller_sha256=${left_controller_sha256}" \
  "right_controller_id=${right_controller_id}" \
  "right_controller_sha256=${right_controller_sha256}" \
  "left_elf_sha256=${left_elf_sha256}" \
  "left_hex_sha256=${left_hex_sha256}" \
  "left_uf2_sha256=${left_uf2_sha256}" \
  "right_elf_sha256=${right_elf_sha256}" \
  "right_hex_sha256=${right_hex_sha256}" \
  "right_uf2_sha256=${right_uf2_sha256}" >"$provenance"

#!/usr/bin/env bash

set -euo pipefail

rust_target="${1:?usage: upload-sentry-debug-files.sh <rust-target> <binary> [--no-upload]}"
binary="${2:?usage: upload-sentry-debug-files.sh <rust-target> <binary> [--no-upload]}"
mode="${3:-}"

if [[ -n "$mode" && "$mode" != "--no-upload" ]]; then
  echo >&2 "unknown option: $mode"
  exit 1
fi
if [[ "$mode" != "--no-upload" ]]; then
  sentry_auth_token="${SENTRY_AUTH_TOKEN:-}"
  if [[ -z "${sentry_auth_token//[[:space:]]/}" ]]; then
    echo >&2 "SENTRY_AUTH_TOKEN is required for tagged release builds"
    exit 1
  fi
fi
if [[ ! -f "$binary" ]]; then
  echo >&2 "binary not found: $binary"
  exit 1
fi

case "$rust_target" in
  *-apple-darwin)
    debug_link="$binary.dSYM"
    if [[ ! -d "$debug_link" ]]; then
      echo >&2 "debug information not found: $debug_link"
      exit 1
    fi
    debug_path="$(cd "$debug_link" && pwd -P)"
    check_path="$(find "$debug_path/Contents/Resources/DWARF" -maxdepth 1 -type f -print -quit)"
    upload_paths=("$binary" "$debug_path")
    ;;
  *-unknown-linux-gnu)
    if ! command -v objcopy >/dev/null; then
      echo >&2 "objcopy is required to extract Linux debug information"
      exit 1
    fi
    debug_path="$binary.debug"
    objcopy --only-keep-debug "$binary" "$debug_path"
    objcopy --strip-debug --strip-unneeded "$binary"
    (
      cd "$(dirname "$binary")"
      objcopy --add-gnu-debuglink="$(basename "$debug_path")" "$(basename "$binary")"
    )
    check_path="$debug_path"
    upload_paths=("$binary" "$debug_path")
    ;;
  *-pc-windows-msvc)
    debug_path="${binary%.exe}.pdb"
    check_path="$debug_path"
    upload_paths=("$binary" "$debug_path")
    ;;
  *)
    echo >&2 "unsupported Rust target for Sentry debug upload: $rust_target"
    exit 1
    ;;
esac

if [[ ! -e "$check_path" ]]; then
  echo >&2 "debug information not found: $check_path"
  exit 1
fi

sentry_cli_version="3.6.2"
sentry_org="a8c"
sentry_project="day-one-cli"
system="$(uname -s)"
architecture="$(uname -m)"
case "$system:$architecture" in
  Darwin:arm64)
    sentry_cli_asset="sentry-cli-Darwin-arm64"
    sentry_cli_sha256="5a497deb1e388cc6445c09ddd6d2da4fc2aae8295405d6393c2e0ee635ca3687" # gitleaks:allow -- published SHA-256 checksum
    ;;
  Darwin:x86_64)
    sentry_cli_asset="sentry-cli-Darwin-x86_64"
    sentry_cli_sha256="efe0a5289cdd0ea8ff727b1228a1bea6c840f2da38152e8f9f5ced05bd6659cd" # gitleaks:allow -- published SHA-256 checksum
    ;;
  Linux:aarch64)
    sentry_cli_asset="sentry-cli-Linux-aarch64"
    sentry_cli_sha256="ff112ecf694b7d6b3629a6228ed4e3f7a0d51401bdf48a5051a79d8749dccd06" # gitleaks:allow -- published SHA-256 checksum
    ;;
  Linux:x86_64)
    sentry_cli_asset="sentry-cli-Linux-x86_64"
    sentry_cli_sha256="3a4bbf2c0d06378d4e59b337647483751a0a2b1603db5fd4991847d0cfd6478c" # gitleaks:allow -- published SHA-256 checksum
    ;;
  MINGW*:x86_64|MSYS*:x86_64|CYGWIN*:x86_64)
    sentry_cli_asset="sentry-cli-Windows-x86_64.exe"
    sentry_cli_sha256="5c90cb0045cef3d3c36113c2aa21a7dcae11627d2d6e3098b679dea5b6681be3" # gitleaks:allow -- published SHA-256 checksum
    ;;
  *)
    echo >&2 "unsupported host for Sentry CLI: $system $architecture"
    exit 1
    ;;
esac

sentry_cli_dir="$(mktemp -d)"
trap 'rm -rf "$sentry_cli_dir"' EXIT
sentry_cli="$sentry_cli_dir/$sentry_cli_asset"
curl --proto '=https' --tlsv1.2 --retry 3 -fsSL \
  -o "$sentry_cli" \
  "https://github.com/getsentry/sentry-cli/releases/download/$sentry_cli_version/$sentry_cli_asset"
if command -v sha256sum >/dev/null; then
  actual_sha256="$(sha256sum "$sentry_cli" | awk '{print $1}')"
else
  actual_sha256="$(shasum -a 256 "$sentry_cli" | awk '{print $1}')"
fi
if [[ "$actual_sha256" != "$sentry_cli_sha256" ]]; then
  echo >&2 "Sentry CLI checksum mismatch"
  exit 1
fi
chmod +x "$sentry_cli"

"$sentry_cli" debug-files check "$check_path"

if [[ "$mode" == "--no-upload" ]]; then
  source_bundle_dir="$sentry_cli_dir/source-bundles"
  mkdir -p "$source_bundle_dir"
  "$sentry_cli" debug-files bundle-sources --output "$source_bundle_dir" "$check_path"
  if ! find "$source_bundle_dir" -name '*.src.zip' -print -quit | grep -q .; then
    echo >&2 "Sentry CLI did not create a source bundle"
    exit 1
  fi
  echo "Debug information and source bundle prepared without uploading"
else
  "$sentry_cli" debug-files upload \
    --include-sources \
    --org "$sentry_org" \
    --project "$sentry_project" \
    --wait-for 300 \
    "${upload_paths[@]}"
fi

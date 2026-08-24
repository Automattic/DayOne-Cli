#!/usr/bin/env bash

set -euo pipefail

rust_target="${1:?usage: build-sign-notarize.sh <rust-target> <asset-name> [cargo flags...]}"
asset_name="${2:?usage: build-sign-notarize.sh <rust-target> <asset-name> [cargo flags...]}"
shift 2

release_tag="${BUILDKITE_TAG:-}"
build_sha="${BUILDKITE_COMMIT:-}"
build_sha="${build_sha//[[:space:]]/}"
if [[ ! "$build_sha" =~ ^[0-9a-fA-F]{40}$ ]]; then
  build_sha="unknown"
fi
if [[ "$release_tag" == v* ]]; then
  version="${release_tag#v}"
  if [ -z "$version" ]; then
    echo >&2 "release tag must include a version after 'v'"
    exit 1
  fi
  sentry_dsn="${DAYONE_SENTRY_DSN:-}"
  release_build_sha="${BUILDKITE_COMMIT:-}"
  release_build_sha="${release_build_sha//[[:space:]]/}"
  if [ -z "$release_build_sha" ]; then
    echo >&2 "BUILDKITE_COMMIT is required for release builds"
    exit 1
  fi
  if [[ ! "$release_build_sha" =~ ^[0-9a-fA-F]{40}$ ]]; then
    echo >&2 "BUILDKITE_COMMIT must be a 40-character Git commit SHA"
    exit 1
  fi
  build_sha="$release_build_sha"
  if [ -z "${sentry_dsn//[[:space:]]/}" ]; then
    echo >&2 "DAYONE_SENTRY_DSN is required for release builds"
    exit 1
  fi
  sed -i.bak "s/^version = \".*\"/version = \"$version\"/" Cargo.toml
  sed -i.bak "/^name = \"dayone\"$/{n;s/^version = \".*\"/version = \"$version\"/;}" Cargo.lock
  rm -f Cargo.toml.bak Cargo.lock.bak
  if ! grep -q "^version = \"$version\"$" Cargo.toml; then
    echo >&2 "Cargo.toml version was not updated to $version"
    exit 1
  fi
  if ! grep -A1 '^name = "dayone"$' Cargo.lock | grep -q "^version = \"$version\"$"; then
    echo >&2 "Cargo.lock dayone version was not updated to $version"
    exit 1
  fi
  echo "Cargo.toml and Cargo.lock version set to $version"
  export CARGO_PROFILE_RELEASE_DEBUG=true
  export CARGO_PROFILE_RELEASE_SPLIT_DEBUGINFO=packed
  export CARGO_PROFILE_RELEASE_STRIP=symbols
fi

echo "--- :rust: install toolchain"
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -v -y --default-toolchain stable
source "$HOME/.cargo/env"
rustup target add "$rust_target"

echo "--- :hammer_and_wrench: build"
export DAYONE_BUILD_SHA="$build_sha"
cargo build --release --locked --target "$rust_target" "$@"

binary="target/$rust_target/release/dayone"
if [ ! -f "$binary" ]; then
  printf >&2 "expected binary at %s\n" "$binary"
  exit 1
fi

echo "--- :key: set up Ruby tooling"
install_gems

echo "--- :closed_lock_with_key: sign and notarize"
bundle exec fastlane configure_code_signing
bundle exec fastlane sign_and_notarize binary:"$binary"

if [[ "$release_tag" == v* ]]; then
  echo "--- :sentry: upload debug information and source context"
  ./scripts/upload-sentry-debug-files.sh "$rust_target" "$binary"
fi

echo "--- :package: tarball + sha256"

# dist/ is gitignored, and the step's `artifact_paths` glob picks it up even if a
# later command fails.
rm -rf dist
mkdir -p dist

# Buildkite artifacts carry bytes, not file modes, so a bare binary downloads
# without its executable bit. Tar records the mode; extraction restores it.
staging="$(mktemp -d)"
cp "$binary" "$staging/dayone"
chmod +x "$staging/dayone"
tar -czf "dist/$asset_name.tar.gz" -C "$staging" dayone
rm -rf "$staging"
( cd dist && shasum -a 256 "$asset_name.tar.gz" > "$asset_name.tar.gz.sha256" )

if [[ "$release_tag" == v* ]]; then
  echo "--- :rocket: upload signed binary to GitHub Release"
  gh auth status
  cp "$binary" "dist/$asset_name"
  chmod +x "dist/$asset_name"

  if ! gh release view "$release_tag" --repo Automattic/DayOne-Cli >/dev/null 2>&1; then
    release_flags=(--verify-tag --generate-notes)
    [[ "$release_tag" == *-* ]] && release_flags+=(--prerelease)
    # GitHub Actions or the other architecture may win this creation race.
    gh release create "$release_tag" --repo Automattic/DayOne-Cli "${release_flags[@]}" \
      || gh release view "$release_tag" --repo Automattic/DayOne-Cli >/dev/null
  fi
  gh release upload "$release_tag" "dist/$asset_name" \
    --repo Automattic/DayOne-Cli --clobber
  rm -f "dist/$asset_name"
fi

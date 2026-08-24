# npm Distribution Plan for `@dayone/cli`

## Overview

Distribute the `dayone` Rust binary via npm so that `npm i -g @dayone/cli` makes the `dayone` command available on the CLI. Must support macOS (arm64 + x64), Linux (x64 + arm64), and Windows (x64).

## How It Works

We publish **n+1 npm packages**: one per target variant, plus one main package that users actually install. With 7 variants, that's 8 packages total.

When a user runs `npm i -g @dayone/cli`, npm sees the `optionalDependencies` and downloads matching OS/arch packages. On Linux this includes both the standard package and the `-no-embeddings` fallback package for that architecture; `run.js` picks the best one at runtime.

The main package is tiny (just `package.json` + `run.js`, a few KB). Each platform package is just the binary. So users only download one binary's worth of data.

This is the standard pattern used by `@biomejs/biome`, `@esbuild/*`, `@swc/*`, and `@rollup/*` for distributing native binaries through npm.

## Package Structure

### Platform packages (one per target)

Each contains only the prebuilt binary for that platform.

| Package name | Rust target | npm `os` | npm `cpu` | Binary |
|---|---|---|---|---|
| `@dayone/cli-darwin-arm64` | `aarch64-apple-darwin` | `darwin` | `arm64` | `dayone` |
| `@dayone/cli-darwin-x64` | `x86_64-apple-darwin` | `darwin` | `x64` | `dayone` — **No embeddings**: ONNX Runtime has no prebuilt binaries for macOS x64, so this variant is built with `--no-default-features --features production-default,telemetry` (mirrors the Linux `-no-embeddings` packages). |
| `@dayone/cli-linux-arm64` | `aarch64-unknown-linux-gnu` | `linux` | `arm64` | `dayone` |
| `@dayone/cli-linux-arm64-no-embeddings` | `aarch64-unknown-linux-gnu` | `linux` | `arm64` | `dayone` |
| `@dayone/cli-linux-x64` | `x86_64-unknown-linux-gnu` | `linux` | `x64` | `dayone` |
| `@dayone/cli-linux-x64-no-embeddings` | `x86_64-unknown-linux-gnu` | `linux` | `x64` | `dayone` |
| `@dayone/cli-win32-x64` | `x86_64-pc-windows-msvc` | `win32` | `x64` | `dayone.exe` — **No embeddings**: built with `--no-default-features --features production-default,telemetry` so clean Windows systems do not need the VC++ runtime used by fastembed/ONNX. |

Example platform `package.json`:

```json
{
  "name": "@dayone/cli-darwin-arm64",
  "version": "0.1.0",
  "os": ["darwin"],
  "cpu": ["arm64"],
  "bin": {
    "dayone": "dayone"
  }
}
```

### Main package (`@dayone/cli`)

- Lists all platform packages as `optionalDependencies` (npm auto-skips non-matching platforms)
- Has a `run.js` shim as the `bin` entry point

```json
{
  "name": "@dayone/cli",
  "version": "0.1.0",
  "bin": {
    "dayone": "run.js"
  },
  "optionalDependencies": {
    "@dayone/cli-darwin-arm64": "0.1.0",
    "@dayone/cli-darwin-x64": "0.1.0",
    "@dayone/cli-linux-arm64": "0.1.0",
    "@dayone/cli-linux-arm64-no-embeddings": "0.1.0",
    "@dayone/cli-linux-x64": "0.1.0",
    "@dayone/cli-linux-x64-no-embeddings": "0.1.0",
    "@dayone/cli-win32-x64": "0.1.0"
  }
}
```

## `run.js` Shim

- ESM module (`"type": "module"` in package.json)
- Detects `process.platform` + `process.arch` to find the correct platform package
- On Linux, checks glibc version and prefers standard packages when glibc is `>= 2.39`, otherwise prefers `-no-embeddings` packages
- Resolves the binary path via `createRequire` + `require.resolve`
- Delegates via `execFileSync` with `stdio: 'inherit'`
- **Update notifier:** Uses `update-notifier@7` (ESM) to check for new versions
  - ~~Only shows notices when `process.stderr.isTTY` is true (suppressed in CI and agentic contexts)~~ Shows in all contexts including non-TTY (AI agents) — bypasses update-notifier's built-in `stdout.isTTY` guard
  - Output goes to **stderr** to preserve JSON stdout for machine consumers
  - Persists cached update so the notice shows on every run until the user updates (update-notifier's default is one-shot)
  - Compares against installed `package.json` version (not stale cached version) to prevent false downgrade notices
  - Also suppressed via `NO_UPDATE_NOTIFIER=1` env var (standard convention)
- **Signal forwarding:** Re-raises signals so parent sees proper signal exit (e.g. Ctrl-C)

## Agentic Invocation

The binary ends up on `$PATH` after global install, so agents (Claude Code, etc.) can call `dayone <subcommand>` directly. The ~50ms Node shim overhead is negligible for API-calling commands.

~~The TTY guard on update-notifier ensures agents get clean stdout/stderr without update notices.~~ Update notices go to stderr so agents get clean stdout. Agents see the update notice on stderr, which is intentional — they can prompt users to update.

## CI Build Matrix

Native builds on each platform (no cross-compilation) to avoid ONNX/fastembed linking issues.

```yaml
strategy:
  matrix:
    include:
      - target: aarch64-apple-darwin
        os: macos-latest            # arm64 runners
        npm_pkg: cli-darwin-arm64
      - target: x86_64-apple-darwin
        os: macos-15-intel          # Intel runner; no-embeddings (ONNX has no macOS x64 binaries)
        npm_pkg: cli-darwin-x64
      - target: x86_64-unknown-linux-gnu
        os: ubuntu-24.04            # standard linux package (embeddings enabled)
        npm_pkg: cli-linux-x64
      - target: x86_64-unknown-linux-gnu
        os: ubuntu-22.04
        npm_pkg: cli-linux-x64-no-embeddings
      - target: aarch64-unknown-linux-gnu
        os: ubuntu-24.04-arm
        npm_pkg: cli-linux-arm64
      - target: aarch64-unknown-linux-gnu
        os: ubuntu-22.04-arm
        npm_pkg: cli-linux-arm64-no-embeddings
      - target: x86_64-pc-windows-msvc
        os: windows-latest           # no-embeddings; avoids fastembed/ONNX VC++ runtime dependency
        npm_pkg: cli-win32-x64
```

Linux publishes two variants per architecture:

- Standard packages include embeddings.
- `-no-embeddings` packages are built on Ubuntu 22.04 with `--no-default-features --features production-default,telemetry` for glibc 2.35 compatibility.

Windows publishes one x64 variant without embeddings. Keyword search remains available; semantic embedding search is deferred until the CLI has a self-contained Windows embedding runtime.

## Release Workflow

Triggered by a version tag (e.g. `v0.1.0`):

1. **Build job** (matrix): stamp `Cargo.toml` version from tag, `cargo build --release --target <triple>` on each platform, upload binary as artifact. This ensures `dayone --version` reports the correct version.
2. **Publish job** (depends on build): download all artifacts, stamp version into each `package.json`, `npm publish` platform packages first, then main package
3. **GitHub Release** (optional): attach binaries to the tag for direct download

Keep build and publish as separate jobs so the same artifacts can be consumed by other distribution channels later.

## Linux Distro Compatibility

No per-distro builds needed. Linux users get runtime selection:

- glibc `>= 2.39`: prefer standard Linux package (embeddings enabled)
- older/unknown glibc: prefer `-no-embeddings` package

## Future Distribution Channels

Nothing about the npm approach locks out other channels. All options below consume the same CI build artifacts:

| Channel | Effort | Notes |
|---|---|---|
| **Homebrew tap** | Low | Formula pointing at GitHub release tarballs |
| **cargo-binstall** | Very low | Auto-discovers binaries from GitHub releases if naming convention matches |
| **Shell installer** (`curl \| sh`) | Low | Script that detects platform and downloads correct binary |
| **Chocolatey** | Low-medium | XML manifest + `.nupkg` packaging |
| **cargo-dist** | Medium | Can auto-generate Homebrew formula, installers, and release CI — but may conflict with custom npm publish workflow; best used for builds + GitHub Releases with a separate npm publish job on top |

## Adding New Targets Later

Adding a target is: one new CI matrix row + one new platform npm package + one new `optionalDependencies` entry. No structural changes.

## npm Authentication

Publishing uses npm OIDC trusted publishing from GitHub Actions. On npmjs.org, configure each of the eight packages with:

- Organization: `bloom`
- Repository: `DayOne-Cli`
- Workflow: `release-npm.yml`
- Environment: leave blank
- Allowed action: `npm publish`

The workflow has `id-token: write`, publishes with `--access public`, and every package declares the canonical GitHub repository URL required by npm. Trusted publishing supports private repositories, but npm provenance is unavailable until the repository is public.

Prerelease versions publish under the `next` dist-tag so release-candidate testing cannot replace the stable `latest` version.

## Open Decisions

- [ ] **Version sync:** Should npm version track `Cargo.toml` version, or be independent?
- [ ] **cargo-dist spike:** Worth testing if cargo-dist handles fastembed/ONNX builds cleanly — could simplify CI and auto-generate Homebrew/installers
- [ ] **GitHub Release:** Attach binaries to tags for direct download (enables cargo-binstall and shell installer for free)?

## Reference

- `@biomejs/biome` — same pattern (Rust binary via npm with platform packages and JS shim)

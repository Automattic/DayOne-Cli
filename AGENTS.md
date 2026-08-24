# AGENTS.md

## Purpose

This file tells coding agents where to look first for implementation examples and source-of-truth docs while working on `dayone-cli`.

## Primary References

### 1) CLI architecture and patterns (inspiration)

- Focus areas to mirror:
  - command tree conventions (`clap`-style UX)
  - auth command ergonomics
  - JSON-first output style
  - configuration/env precedence patterns

### 2) API docs (source of truth)

- Use official/public Day One API documentation when available.
- If your environment relies on private/internal documentation repositories, keep those links in a local untracked override file (`AGENTS.private.md`).

### 3) Web app behavior and data flow

- Check public client behavior and API call patterns where available.
- Keep private repository references in `AGENTS.private.md` only.

## Local Project Map

When implementing features, start here:

- CLI routing and command wiring: `src/cli/mod.rs`
- Auth command logic: `src/commands/auth_login.rs`
- User settings command logic: `src/commands/user_settings_get.rs`
- Profile commands: `src/commands/profile.rs`
- Memory reconciliation: `src/commands/memory_process.rs`
- HTTP API client code: `src/http/client.rs`
- SQLite store and profile/session persistence: `src/store/sqlite.rs`
- SQLite schema migration: `src/store/migrations/0001_init.sql`

## Endpoint and Environment Defaults

- Staging default URL: `https://stg.dayone.me`
- Production URL: `https://dayone.me`
- Runtime precedence:
  1. `--api-host`
  2. `DAYONE_API_HOST`
  3. active SQLite profile
  4. build default (staging, or production with `production-default` feature)

## Validation Checklist for New Changes

After implementing a change, verify:

1. `cargo test` passes
2. `auth login` still succeeds
3. `user-settings get` still succeeds with stored token
4. profile/env precedence behaves as expected

## Notes for Agents

- Prefer updating existing command patterns rather than creating one-off behavior.
- Keep output machine-readable JSON unless a human-readable mode is explicitly requested.
- Treat API docs and endpoint behavior as authoritative when docs and code differ.

## Local Private Overrides

- Keep this tracked `AGENTS.md` safe for public release.
- Put private repository links, internal notes, and environment-specific instructions in an untracked `AGENTS.private.md`.
- Local tooling can merge `AGENTS.md` with `AGENTS.private.md` when that private file exists.

## Cursor Cloud specific instructions

### Build / test / lint

- **Build:** `cargo build`
- **Test:** `cargo test --locked` (268+ tests; all self-contained with temp SQLite DBs, no network needed)
- **Lint:** `cargo clippy` (warnings exist in main; treat as advisory)
- **Format check:** `cargo fmt -- --check`
- **Run:** `cargo run -- <subcommand>` (e.g. `cargo run -- profile list`)

### Git hooks (optional)

Shared hooks live in `githooks/`. The pre-commit hook scans staged changes with Gitleaks when installed, requires Gitleaks when a supported `secrets-cli` file exists, and runs Clippy with warnings denied. Enable it with:

```bash
git config core.hooksPath githooks
```

`SKIP_CLIPPY=1 git commit ...` bypasses only Clippy.

### System dependency gotcha

The `fastembed` crate (ONNX runtime) links against `libstdc++` statically. On Ubuntu, the static library lives under `/usr/lib/gcc/x86_64-linux-gnu/13/` which `rust-lld` does not search by default. The update script creates a symlink at `/usr/lib/x86_64-linux-gnu/libstdc++.a` to fix this. If the build fails with `unable to find library -lstdc++`, re-run the update script or create the symlink manually.

### Running the CLI

The CLI requires a Day One account for any API-connected commands (`auth login`, `sync`, `user-settings get`, etc.). Offline-only commands that work without authentication: `profile list`, `profile set`, `--help`, `--version`.

At startup, the binary loads `KEY=value` assignments from `~/.config/dayone/secrets-cli` when present (dotenv format). Variables already set in the environment are not overwritten.

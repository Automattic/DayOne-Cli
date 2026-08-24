# DayOne CLI Code Taste

This document captures review patterns for `dayone-cli` beyond `cargo fmt`, `clippy`, and type checking. It is intentionally tuned for a Rust CLI with local SQLite state and a Day One API sync boundary.

## Architecture

- **Respect command → API/store layering.** CLI parsing belongs in `src/cli/mod.rs`. Command orchestration belongs in `src/commands/*`. HTTP contract code belongs in `src/http/*`. SQLite/profile/session persistence belongs in `src/store/*`. Avoid pushing clap parsing, stdout formatting, or process-exit concerns down into store/sync helpers.

- **Keep output JSON-first and machine-readable.** Commands should return structured JSON unless a human-readable mode is explicitly part of the command. Do not add incidental prose to stdout; warnings belong on stderr and structured warning fields should be preferred when callers need them.

- **Preserve runtime precedence.** Effective configuration order is: `--api-host`, `DAYONE_API_HOST`, active SQLite profile, build default. Startup dotenv loading from `~/.config/dayone/secrets-cli` must not override already-set environment variables.

- **Prefer narrowly typed command options and payload structs.** Avoid passing loosely shaped `serde_json::Value` deeper than necessary. Parse and validate at the boundary, then carry typed data through command/store/sync code.

- **Do not hide side effects.** Commands that mutate the local store, enqueue outbox rows, write keychain state, delete DB files, or call the network should make those side effects obvious in names and result shapes. Dry-run and force flags must be honored all the way down.

- **Do not ratchet tech debt to minimize a diff.** If adding a behavior requires one more special case in an awkward function, prefer the smallest cleanup that makes the new behavior fit the local model. Do not request broad rewrites unrelated to the PR.

## Rust patterns we prefer

- **Use `Result` with context instead of panics.** Production command paths should not `unwrap`, `expect`, or `panic!` on external input, filesystem state, DB rows, network responses, or API payloads. Tests and hard-coded invariants are the normal exceptions.

- **Borrow when practical; clone intentionally.** Avoid cloning large JSON payloads, rich-text bodies, embeddings, media bytes, or entry lists just to satisfy ownership. Clone small IDs/strings when it keeps code simple and clear.

- **Normalize IDs and dates at boundaries.** Entry IDs are uppercase 32-char hex where applicable. Date handling must be explicit about epoch milliseconds vs seconds, RFC3339 offsets, and all-day local-midnight shapes.

- **Use explicit `Option`/`Result` handling over sentinel values.** Avoid empty strings, zero timestamps, or default JSON objects as meaning-bearing fallbacks unless the API contract requires them and the code names that contract.

- **Preserve unknown fields unless intentionally reserved.** Entry, journal, moment, and comment payloads often carry future server fields. Round-trip unknown data through `extra_fields_json`/raw payloads unless a field is intentionally stripped.

- **Keep shell scripts portable.** Scripts in this repo should work on macOS Bash 3.2 unless the shebang explicitly selects a newer shell. Avoid `${var,,}` and other newer Bash features.

## Anti-patterns we reject

- **No broad catch-and-continue around sync writes.** A sync phase may classify non-fatal remote conditions, but local persistence failures, cursor advancement failures, and outbox lease state changes must not be swallowed.

- **Do not advance cursors before durable persistence.** If a page is considered synced before local rows are stored, the next sync can skip data permanently.

- **Do not clobber pending outbox payloads.** Processing leases, retries, reconciliations, and ID remaps must not mutate queued payload snapshots unless the mutation is the explicit reconciliation step being tested.

- **Do not leak secrets or PII in logs/errors.** Tokens, private keys, shared message keys, decrypted content, email addresses, home paths, and raw API response bodies near auth/crypto paths must be scrubbed before telemetry or diagnostics.

- **Do not couple tests to incidental formatting.** Tests should assert semantic JSON, DB rows, outbox state, or API request shape rather than brittle whitespace/prose unless the command explicitly promises that output.

- **Delete code, don't comment it out.** Git history is the record. Commented-out Rust or shell code adds noise and misleads future readers.

## Testing

- **Use the existing temp SQLite test style.** New store/sync/command behavior should be covered with isolated temp DBs/fake clients, not the user's real `~/.config/dayone` state.

- **Test behavior at the cheapest meaningful layer.** Pure parsers, merge logic, payload builders, and conflict resolution belong in unit tests. Full CLI tests are useful when clap parsing, env/profile precedence, stdout/stderr, or process exit semantics matter.

- **Assert API and outbox contracts precisely.** When a command enqueues work or calls a fake API client, tests should verify the method/path/payload shape, local DB effect, and retry/defer classification where relevant.

- **Keep regression tests around data-loss edges.** Rich text round-trips, unknown field preservation, deleted/tombstoned rows, cursor pagination, comment ID remaps, and encrypted payload behavior need concrete before/after assertions.

## Observability

- **Telemetry fixes must preserve signal, not just quiet noise.** Suppression predicates should be narrow and tests should prove adjacent unexpected errors still report. Sentry/error messages should be categorical; put diagnostic values in scrubbed fields rather than message strings.

# Risk Map

> Maps file paths to risk levels for the DayOne CLI adversarial review system.
> Risk level determines review intensity and push eligibility.
>
> - **critical**: Could cause data loss, key loss, encryption/decryption failure, schema corruption, or silent sync failure.
> - **high**: Could break auth, API contract compatibility, local persistence, outbox processing, command side effects, or privacy boundaries.
> - **medium**: Could cause incorrect user-visible CLI/TUI behavior, search/listing mistakes, or operational friction.
> - **low**: Tooling, docs, tests, and cosmetic code where production user data is unlikely to be affected.

## critical

```
src/sync/crypto.rs
src/store/encryption.rs
src/store/migrations/**
src/store/sqlite.rs
src/sync/engine.rs
src/sync/phases/**
src/entry/body.rs
src/entry/rich_text.rs
src/entry/attachments.rs
```

**Why:** Crypto errors make journals unreadable or leak plaintext. Schema and SQLite round-trip errors can corrupt or drop local data. Sync/outbox errors silently lose edits or create duplicate/conflicting remote state. Entry body, rich text, and media metadata determine what is uploaded and whether Day One clients can read it.

## high

```
src/http/**
src/config.rs
src/env_util.rs
src/commands/auth_*.rs
src/commands/sync*.rs
src/commands/entry_*.rs
src/commands/comment*.rs
src/commands/journal_*.rs
src/commands/context_item_*.rs
src/commands/daily_chat_*.rs
src/commands/memory_process.rs
src/comment_codec.rs
src/models/**
src/store/**
src/sync/**
src/telemetry/**
```

**Why:** These areas define the API contract, profile/env precedence, auth/session behavior, local persistence, outbox mutations, comments/shared-entry behavior, memory reconciliation, and telemetry privacy boundaries.

## medium

```
src/cli/**
src/commands/list*.rs
src/commands/search*.rs
src/commands/user_settings_*.rs
src/commands/profile.rs
src/commands/embeddings*.rs
src/convert/**
src/tui/**
src/http/fake.rs
fixtures/**
e2e/**
```

**Why:** Bugs are visible to users and may produce incorrect output, but are usually recoverable unless they also touch a critical/high path.

## low

```
tests/**
docs/**
.agents/**
.pi/**
githooks/**
*.md
Cargo.toml
Cargo.lock
```

**Why:** Tests, docs, agent skills, and dependency metadata should still be reviewed, but they usually do not directly mutate user data. Dependency changes can be promoted manually when they add runtime or crypto/networking crates.

## Overlap rules

When a diff touches files across multiple risk levels, the **highest** level applies to the entire review. A PR touching both `src/sync/crypto.rs` and docs gets critical treatment.

## System understanding injection

Based on which files appear in the diff, inject corresponding sections from `references/system-understanding/`:

| Files touched | Inject section |
| --- | --- |
| `src/sync/crypto.rs`, `src/store/encryption.rs`, `*crypto*`, `*encryption*` | Crypto / E2EE / Key Storage |
| `src/sync/**`, `src/commands/sync*.rs`, `*outbox*` | Sync / Outbox |
| `src/store/**`, `src/store/migrations/**` | SQLite Store |
| `src/http/**`, `src/config.rs`, `src/env_util.rs`, `src/commands/auth_*.rs`, `src/commands/profile.rs`, `src/commands/user_settings_*.rs` | HTTP / Auth / Config |
| `src/entry/**`, `src/convert/**`, `*rich_text*`, `*attachment*`, `*moment*` | Entry / Rich Text / Media |
| `src/commands/daily_chat_*.rs`, `src/commands/memory_process.rs`, `*daily_chat*`, `*memory*`, `*context_item*` | Daily Chat / Memory |
| `src/commands/comment*.rs`, `src/comment_codec.rs`, `*comment*`, `*journal_create*`, `*shared*` | Comments / Shared Journals |
| `src/telemetry/**`, `*sentry*`, `*telemetry*` | Telemetry / Error Reporting |
| `src/cli/**`, command files, `src/tui/**` | CLI / TUI UX |
| `tests/**`, `e2e/**`, files containing `#[cfg(test)]` changes | Rust Testing |

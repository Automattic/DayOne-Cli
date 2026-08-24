# Task Index

All tasks extracted from [`docs/architecture.md`](../docs/architecture.md) and [`docs/code-structure.md`](../docs/code-structure.md). Each file contains a description, concrete steps, a t-shirt size estimate, and a list of dependencies.

**T-shirt sizes:** XS < 1 day · S 1–2 days · M 3–5 days · L 1–2 weeks · XL 2+ weeks

---

## Store

| # | Task | Size | Depends on |
|---|------|------|------------|
| [001](001-split-store-domain-modules.md) | Split `store/sqlite.rs` into domain modules | L | — |
| [002](002-store-error-types.md) | Introduce typed `StoreError` | M | 001 |
| [003](003-outbox-error-types.md) | Replace `NonRetryableOutboxError` with `OutboxProcessError` enum | S | 002 |
| [004](004-sqlite-wal-and-connection.md) | Enable WAL mode; evaluate connection pooling | S | — |
| [005](005-migration-versioning.md) | Add migration versioning; run migrations once per command | M | 004 |

## Domain models and type safety

| # | Task | Size | Depends on |
|---|------|------|------------|
| [006](006-typed-domain-models.md) | Define typed domain structs (`Entry`, `Journal`, etc.) | L | 001, 002 |
| [007](007-newtype-ids.md) | Introduce newtype IDs (`JournalId`, `EntryId`, `MomentId`) | M | 006 |

## Trait boundaries and testability

| # | Task | Size | Depends on |
|---|------|------|------------|
| [008](008-storage-traits.md) | Introduce storage traits (`OutboxStore`, `SyncStateStore`, `EntryReadStore`) | M | 006, 002 |
| [009](009-http-client-trait.md) | Unify HTTP clients behind a shared trait | S | — |

## Sync engine

| # | Task | Size | Depends on |
|---|------|------|------------|
| [010](010-sync-engine-phase-split.md) | Split `sync/engine.rs` into phase modules | L | 003, 008 |
| [011](011-sync-outbox-drain-ordering.md) | Fix sync ordering: drain outbox only after a full pull | S | — |
| [012](012-sync-feature-flags-presync.md) | Fetch feature flags and user profile as a pre-sync phase | M | 010 |
| [013](013-sync-content-keys-consolidate.md) | Consolidate `content_keys` and `named_crypto_keys` into one concept | M | — |
| [014](014-sync-push-retries-via-outbox.md) | Route conflict push retries through the outbox | M | 003, 010 |

## CLI

| # | Task | Size | Depends on |
|---|------|------|------------|
| [015](015-cli-dispatch-refactor.md) | Refactor CLI dispatch into per-noun helpers | M | — |

## Entry write

| # | Task | Size | Depends on |
|---|------|------|------------|
| [016](016-entry-write-extract-domain.md) | Extract entry domain logic from `entry_write.rs` into `src/entry/` | M | 006, 007 |
| [017](017-rich-text-promote-production.md) | Promote `rich_text.rs` out of `#[cfg(test)]` | S | 016 |
| [018](018-rtjson-markdown-library.md) | Build a first-class RTJson ↔ Markdown conversion library | XL | 017 |
| [019](019-attachment-type-collapse.md) | Collapse `AttachmentType` and `PlaceholderMomentType` into one enum | S | 016 |

## Encryption

| # | Task | Size | Depends on |
|---|------|------|------------|
| [020](020-encryption-master-key-keychain.md) | Store master key in system keychain | M | — |
| [021](021-encryption-journal-key-map.md) | Build an in-memory journal key map for sync duration | M | 013 |
| [022](022-encryption-canonicalize-user-keys.md) | Canonicalize user keys payload into internal struct | M | — |
| [023](023-encryption-remove-unused-params.md) | Remove unused `has_user_key`/`has_content_keys` params | XS | — |

## Embeddings

| # | Task | Size | Depends on |
|---|------|------|------------|
| [024](024-embeddings-chunked.md) | Implement chunk-based embeddings for long entries | M | 006, 001 |
| [025](025-embeddings-dedup-storage.md) | Eliminate duplicate `embedding_json` column | S | 024 |
| [026](026-embeddings-mutex-inference.md) | Investigate parallel embedding inference | S | — |

---

## Recommended starting order

Follow the dependency chain. The highest-leverage first moves:

1. **001** — store split (unlocks everything else in the store group)
2. **004** — WAL mode (independent, one-line win)
3. **011** — fix outbox drain ordering (independent, reduces conflicts today)
4. **023** — remove unused crypto params (trivial, quick win)
5. **002 → 003** — typed errors
6. **006 → 007** — typed domain models and IDs
7. **008 + 009** — storage and HTTP traits
8. **010** — sync engine phase split
9. **005** — migration versioning
10. **015 + 016 + 017 + 019** — CLI and entry write cleanup
11. **013 → 021 → 022** — encryption improvements
12. **020** — keychain storage
13. **024 → 025** — embeddings improvements
14. **012 + 014** — sync ordering refinements
15. **018** — RTJson/Markdown library (long-running, can start anytime)
16. **026** — embeddings inference investigation (low urgency)

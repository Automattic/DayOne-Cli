# Code Structure Review

This document critiques the current structure of `dayone-cli` from a software architecture and Rust idioms perspective. It is distinct from [`architecture.md`](architecture.md), which covers domain design and protocol concerns. This document is about *code organisation, maintainability, and testability*: what makes the codebase hard to work in today, and a concrete path toward fixing it.

The intent is honest and constructive. The codebase is working software. The problems here are the normal result of a codebase that grew faster than its structure could keep up.

> **Current status note**
>
> This document is a critique and refactoring roadmap, not an always-current architecture reference. Some of its original recommendations have already been partially implemented: store methods have been split across domain modules under `src/store/`, sync orchestration now delegates to `src/sync/phases/`, the HTTP layer has a `DayOneApiClient` trait, store operations use a `StoreError` type, and basic domain ID/model types exist under `src/models/`. Read this doc as "where the code still hurts and where it came from," then check the current source before treating any specific line-count or "no traits exist" claim as gospel. Docs, like bananas, brown quietly when unattended.

---

## Table of Contents

1. [The Gravity Wells: Two Files That Own Too Much](#the-gravity-wells-two-files-that-own-too-much)
2. [Limited Trait Boundaries: Most Things Are Concrete](#limited-trait-boundaries-most-things-are-concrete)
3. [Error Handling: Better, But Still Mixed](#error-handling-better-but-still-mixed)
4. [Partially Typed Domain Models](#partially-typed-domain-models)
5. [Partially Stringly-Typed Identifiers](#partially-stringly-typed-identifiers)
6. [Functions Too Large to Reason About](#functions-too-large-to-reason-about)
7. [Testability](#testability)
8. [Prioritised Remediation Order](#prioritised-remediation-order)

---

## The Gravity Wells: Two Files That Own Too Much

The original version of this critique called out two giant files: `src/store/sqlite.rs` and `src/sync/engine.rs`. Both have since been split substantially, which is excellent and annoyingly responsible.

The current shape is better:

```text
src/store/sqlite.rs          # Store type, connection setup, migrations
src/store/profiles.rs        # profiles and auth sessions
src/store/sync_state.rs      # cursors, locks, sync run/resource state
src/store/outbox.rs          # sync_outbox lifecycle
src/store/encryption.rs      # master-key storage and lookup
src/store/entries/           # entry rows, list queries, attachments, embeddings
src/store/comments.rs        # comments and reactions

src/sync/engine.rs           # sync orchestration
src/sync/phases/pull/        # pull resources and decrypt/persist logic
src/sync/phases/outbox/      # durable outbox draining and upload helpers
```

The gravity did not disappear; it moved. `Store` is still the central persistence type, and several phase modules remain dense because they encode a lot of protocol and domain behavior. But the worst "one file owns the universe" smell has been reduced.

The remaining structural risk is that every new persistence or sync behavior still tends to accrete onto `Store` methods and phase helpers. The next refactor should focus less on moving files around and more on defining smaller traits/interfaces around specific domains: entries, outbox, comments, encryption keys, and sync state.

---

## Limited Trait Boundaries: Most Things Are Concrete

Most command and sync code still takes a concrete `&Store`, and commands are standalone functions. The HTTP layer now has a useful `DayOneApiClient` trait, but the storage boundary remains mostly concrete.

This matters for two reasons: *testability* (covered separately below) and *flexibility* — if you want to swap a real database for an in-memory one during tests, the current storage design has no broad seam to insert the swap.

### The `Store` should have a trait

The most impactful trait to introduce is a storage trait (or a family of them, one per domain module). Here is a sketch for the outbox domain:

```rust
// src/store/outbox.rs
pub trait OutboxStore {
    fn enqueue_outbox_item(
        &self,
        resource: &str,
        operation: &str,
        object_id: &str,
        payload_json: &str,
    ) -> Result<()>;

    fn lease_outbox_items(
        &self,
        now_epoch_ms: i64,
        limit: usize,
    ) -> Result<Vec<OutboxItem>>;

    fn mark_outbox_item_succeeded(&self, id: &str, leased_payload_json: &str) -> Result<()>;
    fn mark_outbox_item_retry(&self, id: &str, next_attempt_epoch_ms: i64) -> Result<()>;
    fn mark_outbox_item_failed(&self, id: &str, error: &str) -> Result<()>;
}
```

The concrete `SqliteStore` implements this trait. The sync engine accepts `&dyn OutboxStore` instead of `&Store`. Tests pass a `MockOutboxStore`. The production binary is unchanged.

This pattern applies to each domain slice: `ProfileStore`, `EntryStore`, `SyncStateStore`, `EncryptionKeyStore`.

### The HTTP clients now share a trait

There are two HTTP client types — `DayOneClient` in `http/client.rs` and `SyncApiClient` in `sync/api.rs`. They now share the `DayOneApiClient` trait in `http/mod.rs`, with a fake test client available under `src/http/fake.rs`.

That is the right direction: code that only needs generic JSON/feed behavior can depend on the trait rather than a live network client. The remaining smell is mostly duplication and shape mismatch between the basic API client and the sync client, not total absence of an abstraction.

---

## Error Handling: Better, But Still Mixed

The CLI still uses `anyhow::Result<T>` heavily at command/application boundaries, which is fine for user-facing error reporting. Module boundaries are improving: the store has `StoreError`, and outbox processing has a typed `OutboxProcessError`.

The remaining risk is consistency. When callers need to distinguish retryable vs non-retryable, missing-key vs invalid-input, or user-error vs bug, those cases should be explicit enum variants at the module boundary — not string matching and not surprise downcasts.

### The outbox now has typed errors at its boundary

Outbox processing now defines `OutboxProcessError` under `src/sync/phases/outbox/`, with variants for non-retryable failures, missing encryption keys, store errors, and unexpected errors. That gives the drain loop a proper decision surface for retry/defer/fail behavior.

The next improvement is to keep tightening that boundary: when a new outbox failure mode appears, add a variant and force callers to handle it intentionally. The compiler is very good at nagging; let it nag.

### The store now has its own error type

The store layer now returns `StoreResult<T>` / `StoreError` from `src/store/error.rs`, so common persistence failures can be represented explicitly instead of being flattened immediately into `anyhow::Error`.

That is the right boundary: the CLI can still convert errors to `anyhow` at the application edge, while store/sync code can match specific variants such as `NotFound`, keychain failures, or invalid input.

The remaining improvement is not "invent a store error type" anymore; it is to keep pushing typed errors to module boundaries where callers actually need to make decisions, instead of smuggling control flow through strings or downcasts.

---

## Partially Typed Domain Models

This is the most distinctly Rust-relevant pressure point in the codebase. Rust's type system is its primary tool for correctness, and this codebase still opts out of it for a lot of domain data.

Basic typed models now exist under `src/models/` (`Entry`, `Journal`, `Moment`, comments, reactions, and ID newtypes), and several sync paths use them. But plenty of records — daily chat messages, context items, user settings, content keys, and many API-specific payload fragments — still travel through the codebase as raw JSON strings or `serde_json::Value`. Commands and sync phases still do many string-key field accesses like `record["fieldName"].as_str()`.

The directionally correct shape looks like:

```rust
struct Journal {
    id: JournalId,
    name: String,
    sort_method: EntryListSortMethod,
    is_encrypted: bool,
    // ...
}

struct Entry {
    id: EntryId,
    journal_id: JournalId,
    date: f64,             // epoch ms — the user-assigned entry date
    user_edit_date: String,
    body: Option<String>,
    rich_text_json: Option<Value>,
    tags: Vec<String>,
    // ...
}
```

Every field access is a runtime operation with no compiler assistance. A field name typo — `record["userEditDate"]` instead of `record["user_edit_date"]` — is a silent `None` at runtime, not a compile error. Renaming a field in the API response requires a grep-and-pray refactor; there is no place in the code that enumerates all known fields of an `Entry`.

### The fix: deserialise at the store boundary

The store is the right place to convert raw JSON into typed structs. Above the store, code works with domain types. Below the store, raw JSON is the wire/persistence format.

```rust
// src/store/entries.rs
impl Store {
    pub fn get_entry(&self, id: &EntryId) -> Result<Option<Entry>, StoreError> {
        let json_str = self.get_json_row_by_id("entries", id.as_str())?;
        json_str.map(|s| serde_json::from_str::<Entry>(&s)
            .map_err(StoreError::Json))
            .transpose()
    }
}
```

### Preserving unknown fields for round-tripping

A concern with full deserialisation is losing fields that newer clients added and this CLI doesn't know about. The correct Rust pattern is a catch-all `extra_fields` via the `#[serde(flatten)]` attribute:

```rust
#[derive(Debug, Serialize, Deserialize)]
pub struct Entry {
    pub id: EntryId,
    pub date: f64,
    pub user_edit_date: Option<String>,
    pub body: Option<String>,
    pub tags: Option<Vec<String>>,

    #[serde(flatten)]
    pub extra: std::collections::HashMap<String, Value>,
}
```

When the entry is serialised back out to send to the server, `extra` is flattened back into the JSON object. Fields the CLI doesn't know about are preserved verbatim. This is the safe, round-trip-preserving alternative to the current `data_json` blob — and it is more explicit: the known fields are visible in the struct definition.

### Why this matters

This is the class of change that makes every other improvement concrete. Trait boundaries (section 3) are most useful when they traffic in typed structs. Error handling (section 4) is safer when match arms work on known fields. Function decomposition (section 6) is cleaner when functions accept `&Entry` rather than `&Value`. Without typed models, the Rust type system is largely decorative for the domain layer.

---

## Partially Stringly-Typed Identifiers

Basic ID newtypes exist in `src/models/ids.rs` for journal IDs, entry IDs, moment IDs, and content key fingerprints. However, many store and command boundaries still accept `&str` or `String`, so there is still nothing consistently preventing you from passing a journal ID where an entry ID belongs.

The newtype pattern costs almost nothing and eliminates an entire class of bugs:

```rust
// src/types.rs
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct JournalId(String);

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct EntryId(String);

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct MomentId(String);
```

Store method signatures become:

```rust
pub fn upsert_entry_json_row(
    &self,
    journal_id: &JournalId,
    entry_id: &EntryId,
    data_json: &str,
) -> Result<(), StoreError>
```

Passing a `JournalId` where an `EntryId` is expected is a compile error, not a runtime bug discovered in production. The `impl Deref<Target = str>` and `impl AsRef<str>` implementations keep interop with string slices painless.

---

## Functions Too Large to Reason About

The worst giant functions have been trimmed or split, but the codebase still has dense modules where a lot of behavior stacks up.

### `run_sync` — mostly orchestration now

`src/sync/engine.rs::run_sync` is now mostly a coordinator: load session, acquire lock, create a run, fetch pre-sync resources, run the pull phase, drain the outbox, finish the run, release the lock. That is much better than the older mega-function.

The remaining complexity lives in the phase modules. That is an improvement, but not a finish line. Phase helpers should keep shrinking into named resource handlers and upload helpers with direct tests.

### `cli/mod.rs` — dispatch is split, but the module is still fat

`cli/mod.rs` now has per-noun dispatch helpers, which makes `run()` easier to scan. The module still owns the command tree, all CLI arg structs, dispatch helpers, output helpers, and a large test tail.

The next cleanup is still the same shape: move command-specific CLI arg structs closer to their command modules and use conversion impls so `cli/mod.rs` becomes a routing table rather than the place every flag goes to become immortal.

### `entry_write` — domain logic has moved out

The entry write command is now backed by domain modules under `src/entry/`:

```text
src/entry/attachments.rs
src/entry/body.rs
src/entry/persistence.rs
src/entry/rich_text.rs
```

That leaves `src/commands/entry_write.rs` as a much thinner command adapter. Future entry behavior should keep landing in the domain modules unless it is genuinely CLI-only.

---

## Testability

Test coverage is much better than the original critique implied: there are CLI parsing tests, store round-trip tests, sync engine tests, conflict tests, entry-body tests, and fake HTTP client support.

### What is still hard to test today

- Sync phases still generally require a concrete `&Store`, so many tests need real temporary SQLite databases instead of tiny in-memory fakes.
- The outbox drain loop is better isolated than before, but it still mixes store state, API behavior, retry decisions, and upload mechanics.
- Some domain behavior still lives behind raw JSON payloads, so tests often need bulky fixtures instead of typed builders.

### What testability looks like after the storage trait refactor

With `OutboxStore` as a trait, a test can provide a `Vec`-backed mock:

```rust
struct FakeOutboxStore {
    items: RefCell<VecDeque<OutboxItem>>,
}

impl OutboxStore for FakeOutboxStore {
    fn lease_outbox_items(&self, _now: i64, limit: usize) -> Result<Vec<OutboxItem>, StoreError> {
        let items: Vec<_> = self.items.borrow_mut().drain(..limit).collect();
        Ok(items)
    }
    // ...
}

#[tokio::test]
async fn test_outbox_retries_on_409() {
    let store = FakeOutboxStore::with_items(vec![make_entry_item()]);
    let client = FakeApiClient::returning(StatusCode::CONFLICT);
    let result = drain_sync_outbox(&store, &client, ...).await;
    assert_eq!(result.retry_count, 1);
}
```

This test runs in milliseconds, requires no SQLite database, requires no network, and is deterministic.

---

## Prioritised Remediation Order

Not everything needs to change at once. Some original roadmap items are already done or partially done, so the useful sequencing now looks more like this:

### Done / mostly done

- Split `store/sqlite.rs` into domain modules under `src/store/`.
- Split sync orchestration into `src/sync/engine.rs` plus `src/sync/phases/`.
- Add `StoreError` / `StoreResult` at the store boundary.
- Add `OutboxProcessError` for outbox processing decisions.
- Add initial domain structs and ID newtypes under `src/models/`.
- Move entry attachment/rich-text/body construction into `src/entry/` modules.

### Next Step 1 — Introduce narrower storage traits

Define focused traits such as `OutboxStore`, `SyncStateStore`, `EntryStore`, and `CommentStore`. Have sync phases depend on the smallest trait they need rather than the full concrete `Store`.

**Expected outcome:** Outbox and pull behavior can be tested with fast in-memory fakes instead of real SQLite setup everywhere.

### Next Step 2 — Continue typing domain payloads at boundaries

Keep expanding typed structs where they remove runtime field-name guesses: daily chat, context items, user settings, content keys, and key payloads. Preserve unknown fields with `#[serde(flatten)]` so the CLI remains forward-compatible with API changes.

**Expected outcome:** More field typos become compile errors, and payload shape changes fail near parse boundaries instead of three function calls later in the fog machine.

### Next Step 3 — Push ID newtypes through public/internal APIs

ID newtypes exist, but many functions still accept bare `&str` / `String`. Migrate method signatures gradually, starting with high-risk boundaries where journal IDs, entry IDs, and moment IDs appear together.

**Expected outcome:** A class of ID-confusion bugs becomes a compile error without needing one massive, miserable rewrite.

### Next Step 4 — Shrink dense phase helpers

The giant files were split, but some phase modules are still dense. Extract resource-specific pull/persist handlers and outbox upload helpers into smaller, named units with direct tests.

**Expected outcome:** Sync protocol changes become reviewable as small units instead of "hold the whole octopus in your head and pray."

---

*Last updated: June 2026*

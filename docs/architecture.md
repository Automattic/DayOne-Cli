# Architecture

This is a walkthrough of how `dayone-cli` is built — how commands are wired up, how entry data is modeled, and how sync works. It's meant to be a pleasant read for anyone who wants to understand the system before making changes, not a dry reference.

---

## Table of Contents

1. [The Big Picture](#the-big-picture)
2. [CLI Architecture](#cli-architecture)
3. [The Local Store](#the-local-store)
4. [The Date and Entry Model](#the-date-and-entry-model)
5. [The Sync Engine](#the-sync-engine)
6. [Encryption](#encryption)
7. [Embeddings and Semantic Search](#embeddings-and-semantic-search)
8. [Telemetry and Analytics](#telemetry-and-analytics)

---

## The Big Picture

`dayone-cli` is a **local-first** command-line client for Day One. Most commands read from and write to a local SQLite database, and a separate `sync` command reconciles that local state with the server.

The rough data flow for a write looks like this:

```
dayone entry write ...
        │
        ▼
  local SQLite DB  ───────► sync_outbox table
        │
        ▼ (next sync)
  sync engine reads outbox
        │
        ▼
  PUT /api/v3/sync/entries/:journal/:entry  (multipart)
        │
        ▼
  server response stored back in local SQLite
```

And for a read:

```
dayone list entries --journal-id <id>
        │
        ▼
  reads directly from local SQLite
  (no network call)
```

This means the CLI works fine offline for reads, and queues writes that get pushed on the next sync.

---

## CLI Architecture

### Entry point

`src/main.rs` is a thin async wrapper that calls `src/cli/mod.rs`:

```rust
#[tokio::main]
async fn main() -> std::process::ExitCode {
    crate::env_util::apply_shared_dayone_secrets_from_config();
    let mut telemetry = telemetry::TelemetryGuard::default();
    diagnostics::install_panic_hook();

    let result = cli::run(&mut telemetry).await;
    let sentry_event_id = match &result {
        Ok(()) => None,
        Err(err) => {
            eprintln!("{err:#}");
            telemetry::capture_anyhow(err)
        }
    };
    diagnostics::finish(&result, sentry_event_id.as_deref());

    if result.is_ok() {
        std::process::ExitCode::SUCCESS
    } else {
        std::process::ExitCode::FAILURE
    }
}
```

### The command tree

The command tree lives entirely in `src/cli/mod.rs`. It uses `clap`'s derive API. The shape is:

```
dayone
├── setup
├── auth
│   ├── login
│   ├── logout
│   ├── key-set
│   └── whoami
├── user-settings
│   └── get
├── profile
│   ├── list
│   └── set
├── journal
│   ├── create
│   └── update
├── entry
│   ├── write
│   └── delete
├── embeddings
│   └── recalculate-entries
├── daily-chat
│   └── add
├── memory
│   └── process
├── context-item
│   ├── put
│   └── delete
├── comment
│   ├── list
│   ├── write
│   ├── update
│   ├── delete
│   ├── react
│   └── unreact
├── list
│   ├── journals
│   ├── entries
│   ├── daily-chat
│   ├── daily-chat-messages
│   └── context-items
├── search
│   ├── context-items
│   └── entries
├── sync
├── sync-schedule
│   ├── status
│   ├── enable
│   ├── set-interval
│   └── disable
├── tui
└── doctor
```

Each top-level noun (`auth`, `entry`, `journal`, etc.) is a `clap::Args` struct wrapping a `Subcommand` enum. The `run()` function parses the CLI, loads config, resolves the active profile and API host, opens the profile's local store, then dispatches into a `commands/` module.

Dispatch is centralized in `run()` after config/profile/store setup:

```rust
// src/cli/mod.rs
let config_dir = resolve_config_dir()?;
let config = AppConfig::load_or_create(&config_dir)?;
let profile_name = cli.profile.as_deref().unwrap_or(&config.active_profile);
let profile_config = config.get_profile(profile_name)?;
let base_url = resolve_base_url(cli.api_host.as_deref(), &profile_config.base_url)?;
let store = Store::open_for_profile(&config_dir, profile_name, &base_url)?;

match cli.command {
    TopLevelCommand::Auth(cmd) => dispatch_auth(&base_url, cmd, &store).await?,
    TopLevelCommand::Entry(cmd) => dispatch_entry(&base_url, cmd, &store).await?,
    // ...
}
```

All output is printed as pretty-printed JSON via `print_json`. There is no other output format.

### API host resolution

Every networked command needs a base URL. Resolution happens in `resolve_base_url`:

```
Priority (highest to lowest):
  1. --api-host flag  (or DAYONE_API_HOST env var via clap's env= attribute)
  2. The selected profile's base_url from config.toml
  3. The default config's active profile: staging, or production with the `production-default` feature
```

`DAYONE_API_HOST` may be set by the shell or injected at process startup from `~/.config/dayone/secrets-cli` (dotenv format; existing environment variables are not overwritten).

The feature flag `production-default` flips the default active profile from staging to production when `config.toml` is first created. It is currently included in Cargo's default feature set, so ordinary `cargo build` / `cargo run` builds default to production unless default features are disabled.

### Command modules

Each subcommand dispatches to a function in `src/commands/`. The pattern is consistent:

- Accept a `&Store` and a `base_url: &str`
- Accept a typed `Args` struct (defined in the same module)
- Return a typed `Output` struct that derives `Serialize`
- The caller in `cli/mod.rs` prints it as JSON

The `cli/mod.rs` does a small amount of CLI-specific translation (e.g., mapping `AttachmentTypeCli` to the entry layer's `MediaType`) before calling the command. This keeps the command modules free of `clap` dependencies.

### HTTP clients

The basic API client lives in `src/http/client.rs` as `DayOneClient`. Sync uses `src/sync/api.rs` as `SyncApiClient`, which adds the web-style device headers and multipart/binary upload helpers needed by sync.

Both clients share the `DayOneApiClient` trait in `src/http/mod.rs`, so sync phases can be tested with fake clients instead of only real network calls.

> **Critique — the fat CLI module**
>
> `cli/mod.rs` is still large: it owns the command tree, CLI-only argument structs, dispatch helpers, JSON printing helpers, and a long tail of tests. The dispatch itself has been factored into per-noun helper functions (`dispatch_auth`, `dispatch_entry`, etc.), which is an improvement over one enormous `match`, but the module remains a lot to scan.
>
> A cleaner next step would be to move CLI arg structs into each command module and implement a `From<CliArgs> for CommandArgs` conversion, so `cli/mod.rs` only needs a one-liner per arm.

---

## The Local Store

### SQLite as the source of truth

Local state is stored under the config directory, which defaults to `dirs::config_dir()/dayone-cli` and can be overridden with `DAYONE_CONFIG_DIR`.

Current CLI profiles use separate SQLite databases:

```text
<config_dir>/profiles/<profile-name>/dayone.db
```

For example, on macOS the production profile is typically somewhere like:

```text
~/Library/Application Support/dayone-cli/profiles/production/dayone.db
```

Older/legacy callers may still open a shared database at `<config_dir>/dayone.db`, but normal CLI startup uses `Store::open_for_profile()` and the per-profile layout.

The `Store` struct in `src/store/sqlite.rs` wraps the database path and opens a new SQLite connection per operation. There is no connection pooling; each public method opens, operates, and closes.

The persistence API is split across domain-focused modules that add `impl Store` blocks:

```text
src/store/sqlite.rs          # Store type, connection setup, migrations
src/store/profiles.rs        # profiles and auth sessions
src/store/sync_state.rs      # cursors, locks, sync run/resource state
src/store/outbox.rs          # sync_outbox lifecycle
src/store/encryption.rs      # master-key storage and lookup
src/store/entries/           # entry rows, list queries, attachments, embeddings
src/store/comments.rs        # comments and reactions
```

### Schema migrations

Migrations run at store initialization inside `Store::initialize()`. They are embedded SQL files listed in the `MIGRATIONS` constant in `src/store/sqlite.rs` and recorded in a `schema_migrations` table.

Each migration uses a cheap read-only check first. If the version is missing, the store starts an immediate transaction, re-checks inside the transaction to handle concurrent openers, applies the SQL with `execute_batch`, records the version, and commits. In other words: no, every migration does **not** rerun every time anymore. Good. That would be silly little foot-gun confetti.

**Migration history:**

| File | What it adds |
|---|---|
| `0001_init.sql` | `profiles`, `auth_sessions` |
| `0002_sync.sql` | sync infrastructure + all content tables (journals, entries, templates, etc.) |
| `0003_encryption_keys.sql` | `encryption_keys` for the user's master key |
| `0004_outbox.sql` | `sync_outbox` + supporting indexes |
| `0005_entry_attachments.sql` | `entry_attachments` for local media tracking |
| `0006_entry_embeddings.sql` | `entry_embeddings` + `entry_embeddings_vec` (sqlite-vec virtual table) |
| `0007_entries_schema.sql` | moves entries from a mostly JSON-only table to a fuller SQL schema with indexed fields |
| `0008_outbox_tuple_index.sql` | adds an outbox tuple uniqueness/indexing improvement |
| `0009_unread_entries_deleted_at.sql` | adds deleted-at tracking for unread entries |
| `0010_comments.sql` | adds comments and comment reactions |
| `0011_reset_journals_cursor.sql` | resets journal sync cursor state for a migration-safe re-pull |
| `0012_drop_decrypted_text.sql` | removes the legacy CLI-synthetic `decrypted_text` fallback from entry storage |
| `0013_analytics.sql` | stores local analytics state |
| `0014_rescrub_decrypted_text.sql` | re-scrubs `decrypted_text` after the revert window |

### The JSON blob pattern

Most synced content tables still follow the JSON blob pattern:

```sql
CREATE TABLE journals (
  id TEXT PRIMARY KEY,
  user_edit_date TEXT,
  updated_at TEXT,
  deleted_at TEXT,
  is_deleted INTEGER NOT NULL DEFAULT 0,
  data_json TEXT NOT NULL
);
```

The API payload is stored in `data_json`. The structured columns (`updated_at`, `deleted_at`, `user_edit_date`, and similar fields) are extracted for indexing, filtering, sorting, and conflict resolution, but the blob is still kept for round-tripping API fields. Entry rows intentionally exclude explicitly dropped local-only synthetic fields such as the historical `decrypted_text` fallback.

This makes it easy to evolve the API response without schema changes — new fields just appear inside `data_json`. The `entries` table is the notable exception: migration `0007_entries_schema.sql` extracts common entry fields (`date`, `title`, `body`, `tags_json`, `moments_json`, etc.) into columns for faster list/search paths while still preserving `data_json` and extra JSON fields for compatibility, minus explicitly dropped local artifacts.

### Profiles and sessions

At the app-config level, a **profile** is a named base URL stored in `config.toml`. Two built-in profiles, `staging` and `production`, are included in the default config. The active profile acts as the default endpoint for all networked commands, and `--profile <name>` can override it for one invocation.

Inside each per-profile SQLite database, the store seeds a matching `profiles` row and marks it active. This is partly historical — older shared-DB layouts needed multiple profile rows in one database — but command code still uses profile IDs when tying auth sessions and encryption keys to an endpoint.

An **auth session** is tied to a profile and stores the bearer token + a cached user JSON blob. One session per profile at most, overwritten on re-login.

```
profiles
  id  name       base_url                is_active
  1   staging    https://stg.dayone.me   0
  2   production https://dayone.me       1

auth_sessions
  profile_id   token   token_created_at   user_json
  2            <tok>   2026-03-01T...      {...}
```

> **Critique — one connection per operation**
>
> Opening a new SQLite connection for every store method call is simple and correct, but carries overhead for workflows that call multiple store methods in sequence (e.g., sync, which does many reads and writes). A shared `Connection` (or a connection pooled via `r2d2` or similar) would reduce that overhead significantly, especially for the sync engine.
>
> WAL mode **is** enabled on connection open via `PRAGMA journal_mode=WAL`, with `PRAGMA synchronous=NORMAL`. That means readers and writers get better concurrency than SQLite's default rollback journal mode, at the cost of the normal `dayone.db-wal` and `dayone.db-shm` sidecar files.

> **Critique — migrations are versioned, but still tied to store open**
>
> The old "no migration version table" problem is fixed: `schema_migrations` records applied versions, and already-applied migrations are cheap read-only checks. The remaining tradeoff is that initialization still happens when a `Store` opens. That is fine for normal CLI startup, but long-term the migration/init boundary should stay explicit and boring so future commands do not accidentally pay surprise startup costs. Boring migration systems are good migration systems. Excitement belongs elsewhere.

---

## The Date and Entry Model

This section describes how entries are modeled, how time is represented, and how rich text and attachments are handled.

### Entry identity

Entry IDs are 32-character uppercase hex strings, generated from a SHA-256 hash of `(now_epoch_ms + pid + random_nonce)`. Non-hex IDs (like `strava-1000116162`) are also accepted for integration use cases where the caller controls the ID.

### Timestamps: a two-timestamp world

Each entry carries two distinct timestamps:

| Field | Meaning |
|---|---|
| `date` (epoch ms, f64) | The date the user *assigned to* the entry — the "entry date" displayed in the app. This is not necessarily when the entry was created; a user writing in 2026 can backdate an entry to 2024. |
| `user_edit_date` (ISO 8601) | When the entry was *last edited* — used for conflict detection and `editDate` sort |
| creation date (ISO 8601) | When the entry was *actually created* — a separate field distinct from the user-assigned `date`. This records the real moment of entry creation regardless of what `date` is set to. |

The `date` field is a floating-point epoch millisecond stored as a JSON number. This is Day One's original date format; newer fields prefer ISO 8601 strings. The format must remain stable in this form to support old clients. The `user_edit_date` is an ISO 8601 string (`updated_at` serves the same purpose for non-entry resources).

The local SQLite schema mirrors this by extracting both into columns alongside the `data_json` blob:

```sql
entries (
  id TEXT PRIMARY KEY,
  journal_id TEXT NOT NULL,
  user_edit_date TEXT,   -- extracted for sort + conflict detection
  updated_at TEXT,       -- server-assigned timestamp
  deleted_at TEXT,
  is_deleted INTEGER,
  data_json TEXT NOT NULL
)
```

> **Critique — JSON blob hides all entry fields from queries**
>
> The current schema extracts only a handful of columns (`user_edit_date`, `updated_at`, `deleted_at`, `is_deleted`) and stores everything else in `data_json`. Every other field — tags, location, body, timezone, activity, etc. — requires parsing the JSON blob to access.
>
> A better approach: add an explicit column for every known entry field, and put any remaining/unknown fields from the sync feed into an `extra_fields` JSON blob. When writing back to the server, combine the known columns with `extra_fields` so that fields added by newer clients are not silently dropped. This round-trip preservation is important — the CLI should never be a lossy pass-through for data it doesn't know about.

### Entry listing and sort methods

Entries can be sorted by either `entryDate` (the `date` field) or `editDate` (the `user_edit_date` field). The CLI picks the sort method from:

1. `--sort-method` CLI flag
2. The journal's own `sort_method` setting (fetched from local SQLite)
3. Default: `entryDate`

This mirrors how the Day One app respects per-journal sort configuration.

### Rich text: the `richTextJSON` field

Day One entries can carry both a plain-text `body` and a structured `richTextJSON` field. The JSON format is a document model:

```json
{
  "contents": [
    { "attributes": { "line": { "header": 1 } }, "text": "Title\n\n" },
    { "embeddedObjects": [{ "type": "photo", "identifier": "IMG-ABCD1234" }] },
    { "attributes": {}, "text": "Body text" }
  ],
  "meta": {
    "version": 1,
    "created": { "version": 1746805027, "platform": "webapp" },
    "small-lines-removed": true
  }
}
```

The `contents` array alternates between text nodes (with optional formatting attributes) and embedded object nodes (media references). The CLI writes this format when creating or updating entries.

When the CLI updates an existing entry created by the native app, it tries to preserve rich text fidelity:
- If the update is a pure append, the CLI appends a new text node to the existing `contents` array.
- If the update modifies existing text in an entry that already has meaningful rich text (i.e., not a CLI-created entry), the CLI preserves the existing `richTextJSON` verbatim to avoid corrupting formatted nodes or embedded references.

The `src/entry/rich_text.rs` module contains the production document builder used for CLI-created entries, including placeholder parsing and embedded object construction.

> **Status — rich text building is no longer test-only**
>
> Older versions kept rich text helpers behind test-only abstractions and built production rich text inline. Current code uses `src/entry/rich_text.rs` in production, which is the better shape. The remaining improvement is to keep growing typed RTJson helpers instead of letting raw `serde_json::json!` snippets creep back in like raccoons through a dog door.

> **Critique — no robust RTJson ↔ Markdown conversion library**
>
> Converting between RTJson and Markdown is currently ad hoc and undertested. This needs to be developed into a first-class, TDD'd Rust library with comprehensive round-trip coverage. The test suite should be built from real rich content examples from the web app — actual RTJson documents and their expected Markdown representations — not synthetic constructs. The conversion logic handles all the cases where the two formats have different expressive power (inline formatting, embedded objects, nested structure), and each of those cases needs an explicit test with real content as the input. This library is a good candidate to expose as a standalone Rust crate alongside the D1 binary format library.

### Attachments

The codebase uses the term **attachments** for media items (images, video, audio, PDFs). The server API refers to these as "moments" in several endpoint paths and field names — this is legacy naming that should be treated as a codec detail, not a domain concept. Prefer "attachment" everywhere in new code.

Each attachment has:
- A unique ID (same 32-char hex format as entry IDs)
- A type: `image`, `video`, `audio`, or `pdfAttachment`
- A content type (MIME)
- An MD5 hash of the file body
- File size in bytes

Attachments appear in two distinct places with different scopes:

1. **The entry's `moments` array inside the encrypted body** — this is the list of attachments that are part of the entry content. This array is encrypted along with the entry body before being sent to the server. If an attachment appears in this array but is not referenced by a placeholder in the markdown or rich text, it is automatically displayed at the end of the entry by the web editor and other clients.

2. **The `entry_attachments` table** — this tracks attachment metadata from the *outer*, unencrypted attachment descriptor that is sent to the server alongside the encrypted blob. This is the server's only visibility into what attachments an entry has; it uses this metadata to associate media uploads with their entries and track upload state. This table must be kept in sync with the moments array in the entry body.

The entry body references attachments via placeholder syntax in the markdown or rich text. The placeholder format uses the `dayone-moment:` scheme:

```
![](dayone-moment://IMG-ABCD1234)              ← image (uses double-slash, no type segment)
![](dayone-moment:/video/VID-ABCD1234)         ← video
![](dayone-moment:/audio/AUD-ABCD1234)         ← audio
![](dayone-moment:/pdfAttachment/PDF-ABCD1234) ← PDF
```

The format is `dayone-moment:` followed by the type path and the attachment ID. Images are a historical exception — they use `://` with no type segment rather than `:/image/`. All other types follow the `:/type/id` pattern.

When writing a new entry with attachments, the CLI resolves unbound placeholder markers (those without an ID) to the provided attachment files, in order. Unmatched attachments get appended at the end.

> **Status — attachment type modeling is centralized**
>
> Older versions duplicated attachment and placeholder type enums. Current code uses `MediaType` from `src/entry/attachments.rs` across staging, placeholder parsing, and rich text embed construction. Keep it that way; duplicate media enums are how tiny inconsistencies breed in the walls.

---

## The Sync Engine

Sync is the most complex part of the codebase. Here's a walkthrough of what happens when you run `dayone sync`.

### Overview

**Current implementation order:**

```
dayone sync
     │
     ▼
src/commands/sync.rs        (thin wrapper; calls engine)
     │
     ▼
src/sync/engine.rs          (orchestration: lock, run lifecycle, phase calls)
     │
  ┌──┴───────────────────────────────────────────────────────────┐
  │  1. load auth session                                         │
  │  2. acquire sync lock                                         │
  │  3. create sync run                                           │
  │  4. fetch pre-sync resources (user profile + feature flags)   │
  │  5. pull resources (cursor-based feeds + singletons)          │
  │  6. drain sync_outbox                                         │
  │  7. finish sync run and release sync lock                     │
  └──────────────────────────────────────────────────────────────┘
```

The heavy lifting lives in phase modules rather than directly in the top-level engine:

```text
src/sync/phases/pull/      # pre-sync resources, cursor resources, entries, crypto-aware pulls
src/sync/phases/outbox/    # durable outbox draining and upload helpers
```

### The sync lock

Before doing anything, the engine acquires a process-level lock stored in the `sync_lock` table. If another process is already syncing (or a previous sync crashed while holding the lock), the engine returns immediately with `locked: true`. The lock has a 15-minute TTL to handle crash recovery.

### Pulling resources

Resources are pulled in a fixed sequence. There are two pull patterns:

**Cursor resources** — paginated feeds with a server-side cursor. The engine stores the cursor in `sync_cursors` and resumes from where it left off on the next run. Examples: `journals`, `entries:<journal_id>`, `content_keys`, `notifications`.

```
GET /api/v6/sync/journals?cursor=<stored_cursor>
     │
     ▼
for each item in response:
    upsert into local SQLite
advance cursor
```

The engine pages until the response has no next cursor, up to a hard limit of 10,000 pages to prevent infinite loops.

**Singleton resources** — endpoints that return a single document. The engine fetches and upserts on every sync. Examples: `user_key`, `user_profile`, `user_settings`, `feature_flags`.

### The resource pull sequence

Resource order matters because some resources depend on earlier key material or feature flags.

Pre-sync phase:

1. `user_profile` — keeps the cached profile fresh; failure is recorded but does not necessarily abort sync
2. `feature_flags` — required input for feature-gated resources; fetched from `/v2/feature-flags`

Main pull phase:

1. `user_key` — the authoritative encrypted user identity key (needed to create and unlock journal vaults); fetched from `/users/key` and stored in `user_identity_key`
2. `content_keys` — asymmetric key pairs used to encrypt non-journal/non-entry entities (daily chat, context items); fetched from the cursor-based `/v4/sync/changes/contentKeys` feed and stored in the `content_keys` table
3. `named_crypto_keys` — the optional legacy content-key bundle from `/v4/sync/named/cryptoKeys`; stored separately in `user_keys` for content-key features
4. `journals` — the list of journals (decrypted in-place if an encryption key is set)
5. **entries per journal** — pulled in a loop over all sync-enabled journal IDs
6. `notifications`
7. `unread_entries`
8. `templates`
9. `user_settings`
10. `pbc_template_categories`, `pbc_templates`, `pbc_prompt_pack_gallery`
11. *(feature-gated)* `journal_hierarchy`
12. *(feature-gated)* `daily_chat_feed`, `daily_chat_settings`
13. *(feature-gated)* `context_library`

Feature-gated resources are only pulled when the corresponding feature flags are present. Because those flags are fetched in the pre-sync phase, the rest of sync can make pull/no-pull decisions from a single `FeatureFlags` value.

> **Status — user identity and content-key sources stay separate**
>
> `/users/key` is the source of truth for the user identity key. The named `cryptoKeys` response is an optional legacy content-key bundle and may include a duplicate `userKey` for old-client compatibility. Journal code uses the direct identity key; Daily Chat, Context Library, and similar features compose that identity key with the supplementary content keys they require.

### Entry sync in detail

Entries are the most interesting resource. They use the `/v2/sync/entries/:journal_id/feed` endpoint:

- On the **initial pull** (no stored cursor), `excludeDeleted=true` is sent to skip deleted entries.
- On **subsequent pulls**, the cursor is sent and the server returns only changes since that cursor.
- The journal's sync status in `journal_sync_info` transitions: `WAITING_INITIAL_SYNC` → `SYNCING` → `IDLE`.

Journal records themselves are also decrypted in-place during the journal pull (step 4 above) — encrypted journal fields are unwrapped using the vault key hierarchy before the journal is stored locally. Entries follow the same pattern: if the journal's content key is available, entry content is decrypted in-place before storage.

### The outbox: write-ahead for uploads

When you run `entry write`, the CLI does not immediately talk to the server. Instead:

1. The entry JSON is saved to the local `entries` table.
2. An outbox item is enqueued in `sync_outbox`:
   - `resource = "entry"`, `operation = "update"`
   - Payload contains `journal_id` and `entry_id`
3. Attachment records are saved to `entry_attachments`.
4. Additional outbox items are enqueued for each attachment (`resource = "original_media"`).

After the pull phase, the engine calls `drain_sync_outbox`. This processes pending outbox items in batches of 50.

The outbox handles retries with exponential backoff (up to 8 attempts before dead-lettering), a special "defer" path when encryption keys are missing, and a "non-retryable" path for permanent failures.

**Outbox item types:**

| resource | operation | What it does |
|---|---|---|
| `entry` | `update` | PUT entry content (multipart) to server |
| `original_media` | `create` | Upload original media file to storage URL |
| `context_item` | `update` | PUT context library item to server |
| `daily_chat_feed` | `update` | PUT daily chat record to server |
| `journal` | `create` | POST new journal to server; remaps local pending ID to server ID |

### Conflict detection and push retries

During a resource pull, if the server sends a record that is *older* than the local version (as determined by `user_edit_date` or `updated_at`), the pull phase queues a durable outbox update rather than overwriting the local version. The outbox drain later pushes that local version back up.

The conflict comparison is in `src/sync/conflicts.rs`:

```rust
pub fn local_is_newer(local: &Value, server: &Value) -> bool {
    match (extract_edit_timestamp(local), extract_edit_timestamp(server)) {
        (Some(local_ts), Some(server_ts)) => local_ts > server_ts,
        _ => false,
    }
}
```

`extract_edit_timestamp` accepts numeric epoch-millisecond values and RFC 3339 strings, normalizes them to epoch milliseconds, and then compares the parsed values. This is less fragile than the older string-only comparison path.

> **Critique — sync is split into phases, but the phase internals are still dense**
>
> `run_sync` is now mostly orchestration, which is a major improvement. The complexity moved into `src/sync/phases/pull/` and `src/sync/phases/outbox/`, where individual phase files still carry a lot of protocol detail: cursor handling, JSON parsing, encryption, conflict detection, upload retries, and resource-specific persistence.
>
> A cleaner next design would separate the **pull plan** (a declarative list of resources with their fetch and persist callbacks) from the **execution engine** (cursor progression, run logging, error recording). This would make it possible to unit-test individual resource pull behavior without running the entire sync.

> **Status — feature flags and user profile are now a pre-sync phase**
>
> The older design fetched feature flags deep in the pull sequence. Current sync fetches user profile and feature flags before the main pull phase via `fetch_pre_sync_resources()`. That separates the "what do we need to pull?" decision from the pull execution. Tiny architecture win; we take those.

> **Status — conflict retries route through the outbox**
>
> The older design accumulated conflict push retries in memory during the pull phase. Current code queues local-newer conflicts back into `sync_outbox`, which gives conflict recovery the same durability and retry/backoff behavior as normal writes. That is the right shape: if sync crashes, the push intent is still on disk instead of evaporating like a startup roadmap.

---

## Encryption

Day One supports end-to-end encryption for journals and some AI features. The CLI supports this with a "D1" binary encryption format.

### The master key

When you run `dayone auth key-set`, you store a **master key** — a string like `D1-<user_id>-<passphrase-words>`. The CLI prefers the system keychain via the `keyring` crate and stores a `keychain` sentinel in the `encryption_keys` table. If secure storage is unavailable or verification fails, it falls back to storing the key in SQLite. Login can also persist the key automatically when the login payload includes `master_key` or `e2ee_display_key`.

The master key is turned into a 32-byte symmetric key via PBKDF2-SHA256:

```
symmetric_key = PBKDF2_HMAC_SHA256(
    password = passphrase_words (joined),
    salt     = user_id,
    rounds   = 100_000,
    length   = 32 bytes
)
```

### The D1 binary format

Encrypted payloads use a custom binary format with a `D1` magic prefix. There are three modes:

| Mode | How the content key is protected |
|---|---|
| Mode 0 | AES-256-GCM with the symmetric key directly |
| Mode 1 | AES-256-GCM with a per-item content key, itself RSA-OAEP encrypted to a public key (stored alongside the ciphertext as "locked key") |
| Mode 2 | Same as mode 1 but the payload is gzip-compressed before encryption |

Binary structure for mode 1/2:
```
[D1][schema=1][mode][32-byte fingerprint][sig_len u16][signature?][wrapped_key][iv 12B][ciphertext+tag][md5 16B]
```

The D1 format implementation is a good candidate to expose as a standalone Rust crate with a comprehensive public API for parsing and building D1 payloads. The `docs/` folder contains two HTML-based debugger tools useful during development: `d1-helper.html` for exploring D1 binary blobs and `string-encodings.html` for inspecting raw binary and string encodings. These should be referenced in the library's documentation.

### Content keys

The user has one or more **content keys** — asymmetric RSA key pairs. The private side of each content key is locked by the user key. Content keys are used to encrypt entities that are *not* journals or entries: daily chat records, context library items, and similar. They are identified by a fingerprint.

### Journal and entry encryption

Encrypted journals use RSA key hierarchies:

1. The user has a **user private key** (RSA), itself encrypted by the master-derived symmetric key.
2. Each encrypted journal has a **vault** — a set of **grants**, each containing a **vault key** RSA-encrypted to the user's public key.
3. The vault key unlocks **per-journal content keys** (RSA key pairs). These journal-level content keys encrypt the journal record itself and its individual entries.

On sync, the engine:
1. Decrypts the user private key using the stored master key.
2. For each journal, finds a grant encrypted to the user's public key and decrypts the vault key.
3. Uses the vault key to decrypt the journal's content private keys.
4. Decrypts the journal record itself with the journal content key.
5. Decrypts each entry's content using the appropriate content private key (matched by fingerprint).

When writing an entry to an encrypted journal, the engine encrypts using the journal's *active* content public key (the newest one, identified by `activeContentKeyFingerprint` in the user keys payload).

### Daily chat and context item encryption

These use the user-level content keys (not the journal vault hierarchy). Each daily chat record has a `shared_message_key` — a 32-byte symmetric key, itself RSA-encrypted to the user's active content public key (stored as a D1 mode 1 payload). Individual message `content` fields are AES-256-GCM encrypted using that shared key (D1 mode 0).

Context field values in daily chat are also encrypted individually using D1 mode 1 to the active content public key.

> **Status — master key prefers system keychain with SQLite fallback**
>
> The old "SQLite only" key storage concern has been addressed. `src/store/encryption.rs` writes master keys to the OS keychain when possible and stores a SQLite sentinel so the store knows where to read from later. SQLite remains a fallback for headless or unavailable-keychain environments.
>
> There is still room to keep encryption setup boring: sync should continue minimizing repeated key derivation and should keep decrypted key material scoped to command execution, not durable storage. Secret-handling code is where we do not want surprise confetti.

> **Critique — dual-path key extraction with fragile pattern matching**
>
> `extract_user_encrypted_private_key` in `crypto.rs` tries seven different field paths to find the encrypted private key in the user keys payload (`encryptedPrivateKey`, `encrypted_private_key`, `userKey.encryptedPrivateKey`, inside a base64-decoded `blob`, etc.). This is an accumulation of workarounds for different API versions and response shapes.
>
> The right fix is to canonicalize the user keys payload into a single internal struct as soon as it arrives from the server, rather than treating the raw API JSON as the canonical form everywhere. Doing so would make all downstream code simpler and would surface format changes at parse time rather than at decrypt time.

> **`build_crypto_context_from_sources` signature**
>
> `build_crypto_context_from_sources` takes the master key, authoritative user identity key, and optional content-key bundle separately. This keeps key provenance explicit while still allowing content-key features to unlock their private keys with the user identity key.

---

## Embeddings and Semantic Search

The CLI generates local vector embeddings for entries during sync and uses them for semantic search. This is an offline feature — no embeddings are sent to the server.

### The embedding model

Embeddings are generated using the `all-MiniLM-L6-v2` model via the `fastembed` crate. This is a compact 384-dimension sentence embedding model that runs entirely locally via ONNX Runtime. The model files are downloaded on first use.

The model is initialized once per process and held in a `OnceLock<Mutex<TextEmbedding>>`. The `Mutex` serializes embedding calls since ONNX Runtime's inference is not `Send + Sync` in all configurations.

### Embedding lifecycle

**Generation:**
```
sync
  │
  ▼
for each new/changed entry in feed:
    compute_entry_embedding(record)    ← hashes searchable text, runs inference
        │
        ▼
    store in entry_embeddings table
    store float vector in entry_embeddings_vec (sqlite-vec virtual table)
```

**Recalculation:** `embeddings recalculate-entries` re-runs inference on existing entries without requiring a sync, useful if the model changes.

**Source text:** The text fed to the model is assembled from whichever fields are present: `title`, `body`, or `payload.body`. This handles both the CLI-created format and the server-synced format. Currently the entire assembled text is embedded as a single vector.

> **Critique — whole-entry embeddings become watered-down for long entries**
>
> Embedding the entire entry body as a single vector means the embedding is an average of all the content. For long entries, this dilutes the signal from any individual topic or passage, making semantic search less precise.
>
> A better approach: chunk the markdown field into windows of N words with Y padding words of overlap on either side. Each chunk gets its own embedding and stores the word-range it covers. Short entries may produce a single chunk; long entries produce several. This keeps embeddings focused and searchable at a sub-entry level.
>
> Future extensions: generate embeddings for individual attachments (photo captions, audio transcripts), for tags, and for rich content in the RTJson that isn't represented in the markdown (e.g., workout types from Apple Health suggestions).

**Change detection:** Before embedding inference, the CLI computes a SHA-256 hash of the current searchable text and compares it against stored `source_hash`. If the hash matches, inference and upsert are skipped. Entries are only re-embedded when the source text hash changes.

### Storage

```sql
entry_embeddings (
  entry_id TEXT PRIMARY KEY,
  journal_id TEXT,
  source_hash TEXT,        -- SHA-256 of the text that was embedded
  embedding_json TEXT,     -- the float vector, JSON-serialized
  dimension INTEGER,
  updated_at TEXT
)

entry_embeddings_vec USING vec0(   -- sqlite-vec virtual table
  embedding float[384],
  entry_id TEXT PRIMARY KEY,
  +journal_id TEXT          -- auxiliary column for filtered KNN
)
```

`entry_embeddings_vec` uses **sqlite-vec** (`vec0` virtual table), which provides performant vector indexing and KNN queries directly inside SQLite without an external vector database. The `entry_embeddings` table is the durable store; `entry_embeddings_vec` is the queryable KNN index. The migration script backfills the vec table from the regular table on first open.

### Querying

`search entries --query "..."` embeds the query string (using the `query:` prefix for the asymmetric MiniLM model) and runs a KNN search against `entry_embeddings_vec` with a configurable similarity threshold. Results are filtered by `journal_id` if `--journal-id` is provided.

> **Critique — embedding JSON stored twice**
>
> Each embedding is stored twice: once as a JSON array in `entry_embeddings.embedding_json` and once as a binary float vector in `entry_embeddings_vec`. The json column appears to be kept as a backup or for portability, but it means every write doubles the storage for each embedding. At 384 floats × 8 bytes = ~3KB per entry, this adds up for large journals. Consider whether the JSON copy is needed, or whether the vec table alone is sufficient.

> **Critique — Mutex-serialized inference**
>
> Embedding calls hold a `Mutex<TextEmbedding>` for the duration of inference. This serializes all embedding requests in a process. For the current batch-style usage (generate embeddings during sync) this is fine. But if the CLI ever moves toward a daemon or server model where multiple requests could arrive concurrently, the mutex becomes a bottleneck. ONNX Runtime does support thread-safe inference sessions in some configurations; investigating that would allow parallel embedding.

## Telemetry and Analytics

`src/consent.rs` implements opt-in for Tracks and Sentry. Consent is recorded in `config.toml` with a disclosure version and decision timestamp. The `telemetry enable`, `disable`, and `status` commands operate on configuration without opening a profile database or starting a collector.

Interactive commands prompt when a collector could run. Unattended commands continue with reporting off until consent is recorded. Endpoint, environment, and cached account settings determine whether collection is otherwise enabled.

### Error monitoring (Sentry)

`src/telemetry.rs` initializes Sentry after configuration, store initialization, and consent resolution. The default `telemetry` Cargo feature includes Sentry. Main retains its client guard through error reporting. The `before_send` callback rechecks consent and filters sensitive fields. Errors marked as `telemetry::UserError` are excluded from reporting.

### Product analytics (Automattic Tracks)

`src/analytics/` sends usage events to Automattic Tracks with the prefix `dayone_cli_`. It rechecks consent before recording events and starting a flush. Flush runs at the end of an invocation, with a time budget of one request timeout plus 500 ms. Analytics failures do not fail the command:

```
handler ──track(event, props)──▶ analytics_events (SQLite queue)
                                        │
run() end / `dayone sync` ──flush──▶ drain queue ──POST batch──▶ /rest/v1.1/tracks/record
                                        │ (2xx → delete rows; else bump attempts & retry)
```

- **Transport** (`tracks.rs`) sends batches to `https://public-api.wordpress.com/rest/v1.1/tracks/record` as `{ "commonProps": { "_ui", "_ut", "_rt" }, "events": [...] }`. Delivery is enabled for the production Day One API; `DAYONE_TRACKS_ENDPOINT` selects a test endpoint for other environments. Consent is required in either case. Event and property names must match `^[a-z_][a-z0-9_]*$`.
- **Queue** (`store/analytics.rs`, migration `0013_analytics.sql`) stores up to 500 events per profile in SQLite. Each flush considers up to 50 events and drops events older than 30 days.
- **Identity** (`identity.rs`) is a pseudonymous installation ID (`_ut = anon`), generated after consent and stored under `[analytics]`. It remains installation-scoped after sign-in. Changing consent clears the ID. During initialization, Tracks discards events at or before the current agreement timestamp; if cleanup fails, Tracks stays disabled for that invocation.
- **Events** map to Day One Web's vocabulary where they apply (`user_sign_in`, `entry_create`, `journal_create`, `entry_comment_added`, …), plus a generic `command_run` lifecycle event fired once per invocation with `command`, `subcommand`, `outcome`, and `duration_ms`.

### Opt-out

Both collectors honor `DO_NOT_TRACK=1` and `DAYONE_TELEMETRY=0`. Tracks also checks the cached `track_usage_statistics` account setting at startup. A missing account setting permits Tracks only when consent, endpoint, and environment checks also allow it. Tracks event properties are restricted by an allowlist and exclude journal content, titles, bodies, emails, and tokens. Sentry error filtering has different guarantees; see [Telemetry](telemetry.md).

---

*Last updated: September 2026*

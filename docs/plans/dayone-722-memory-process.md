# DAYONE-722 Memory Process Plan

## Goal
Implement a new CLI workflow that is not daily-chat branded:
- `dayone memory process --json <payload> | --json-file <path>`
- It triggers the same memory processing pattern as DayOne-Web (`/labs/ai/daily-chat/ephemeral/memory-updates` loop with `start`/`continuation`, local memory tool application), then returns machine-readable JSON.

## Key References To Mirror
- Day One Web memory flow docs: `docs/features/daily-chat.md` (DayOne-Web)
- API caller behavior (payload cleaning + loop): `src/data/api_callers/DailyChatAPICaller.ts` (DayOne-Web)
- Memory controller semantics (bookmark, context window, skip rules): `src/data/controllers/InHouseMemoriesController.ts` (DayOne-Web)
- Existing CLI command wiring: `src/cli/mod.rs`
- Existing command module exports: `src/commands/mod.rs`
- HTTP client trait and fake client: `src/http/mod.rs`, `src/http/client.rs`, `src/http/fake.rs`
- Skills index + existing skill style: `docs/skills.md`, `skills/dayone-daily-chat-add/SKILL.md`

## Proposed Implementation

### 1) Add new top-level memory command
- Update `src/cli/mod.rs`:
  - Add `TopLevelCommand::Memory(MemoryCommand)`.
  - Add `MemorySubcommand::Process(MemoryProcessCliArgs)` with `--json` / `--json-file` input.
  - Add dispatch function that resolves base URL + auth session and prints JSON output.
- Update `src/commands/mod.rs` to export `memory_process`.

### 2) Implement `memory_process` command module
- Add `src/commands/memory_process.rs` with:
  - Request input model for conversation + optional metadata (`date`, `timezone`, `locale`, `existing_summary`).
  - Validation and normalization modeled after Web behavior:
    - Exclude `meta` messages from memory-check payload.
    - Preserve tool messages/tool metadata for memory endpoint.
    - Strip `memory_changes` before sending.
  - Optional bookmark/context logic for input conversations:
    - Compute `new_messages` and up to 4 `context_messages` (Web parity).
    - Skip remote call when no new user message is present.
  - Endpoint driver:
    - `POST /labs/ai/daily-chat/ephemeral/memory-updates`.
    - Loop on `status: processing` using `continuation` until `complete`.
    - Execute returned `in_house_memory` tool calls locally against SQLite context items (create/update/delete), collecting `memoryChanges` for output.
  - Output JSON includes processed counts, applied memory changes, and updated summary (if any).

### 3) Extend HTTP testability for fixture-driven integration tests
- Enhance fake API support in `src/http/fake.rs`:
  - Capture POST payloads for assertions.
  - Support sequenced responses for the same key to model `processing -> continuation -> complete`.
- Keep production HTTP behavior unchanged in `src/http/client.rs`.

### 4) Add primary Rust integration tests + fixtures
- Add fixture directory (new): `tests/fixtures/memory-process/`
  - Conversation payload fixtures (with user+assistant+tool+meta mixes).
  - API response fixtures for multi-round memory-updates (`processing` then `complete`).
  - Expected output fixtures for stable assertions.
- Add integration tests (new): `tests/memory_process.rs`
  - CLI-level or command-level integration around temp SQLite.
  - Assert:
    - payload cleaning and skip conditions,
    - continuation loop behavior,
    - persisted context-library memory mutations,
    - final JSON output shape.
- Add focused unit tests in command module for normalization/bookmark/context-window edge cases.

### 5) Add a new skill and docs entry
- Add new skill directory/file:
  - `skills/dayone-memory-process/SKILL.md`
- Update skills index:
  - `docs/skills.md`
- Skill describes trigger terms and exact command usage for processing AI conversations.

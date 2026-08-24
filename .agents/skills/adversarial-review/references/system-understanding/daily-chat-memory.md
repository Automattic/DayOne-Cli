## Daily Chat / Memory / Context Items

**Files:** `src/commands/daily_chat_*.rs`, `src/commands/memory_process.rs`, context-item command/sync code.

### Chat encryption model

- Current daily-chat messages use a long-lived `shared_message_key` stored on the chat object.
- Legacy chats may lack `shared_message_key` and use content-key-based message encryption. Code must handle both.
- `shared_message_key`, decrypted messages, tool-call internals, and memory contents must not leak to server payloads, logs, or telemetry unless the API explicitly expects the scrubbed/serialized form.

### Message handling

- Chat message modes: 1=online, 2=log, 3=offline.
- Tool calls should not be synced as normal user/assistant messages.
- Meta role messages may be persisted/synced differently than messages sent to ephemeral model APIs.
- Message caps exclude tool calls and server errors may use localization keys rather than English text.

### Memory reconciliation

- `last_memory_check_message_id` must not be clobbered by unrelated chat saves. Use read-modify-write patterns that preserve newer metadata.
- Memory metadata updates should avoid bumping user edit dates when that would create unnecessary sync conflicts.
- Context items need expired/deleted filtering where user-visible memory/search output is built.

### V2 feed structure

- Feeds can interleave chats and messages as separate typed items. Build state by assembling messages with their parent chats rather than trusting an always-populated chat history field.

### What to watch for

- Save paths that overwrite chat metadata fetched by another process or sync step.
- Tool calls, hydrated memories, shared keys, or decrypted content included in API payloads/telemetry.
- Missing filters that revive expired/deleted memories.
- Tests that don't cover continuation/bookmark preservation.

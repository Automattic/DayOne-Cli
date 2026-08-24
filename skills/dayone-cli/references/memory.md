# memory process

Run a conversation payload through Day One memory reconciliation and persist the resulting context-item changes locally.

```bash
dayone memory process [--json <payload> | --json-file <path>]
```

## Invocation intent (default behavior)

When invoked, treat the target as the **current AI conversation in progress** unless the user explicitly provides a different payload.

- Build the payload from the active conversation transcript (latest user + assistant turns in this chat).
- Prefer the current memory bookmark (`last_memory_check_message_id`) from the active thread state when available.
- Do **not** reuse stale payload files from older chats unless the user explicitly asks for replay/backfill.
- If no explicit payload is provided, synthesize `--json` from the current chat state and run `dayone memory process --json '<payload>'`.

## Required follow-up

After every successful reconciliation run:

```bash
dayone sync
```

`sync` flushes the queued context-library changes from the local outbox to the server.

## Input payload shape

```json
{
  "conversation": {
    "messages": [
      { "id": "m1", "role": "user", "content": "..." },
      { "id": "m2", "role": "assistant", "content": "..." }
    ],
    "last_memory_check_message_id": null
  },
  "existing_summary": null,
  "date": "2026-04-08",
  "timezone": "UTC",
  "locale": "en-US"
}
```

## Notes

- The command filters out `meta` messages from memory processing input.
- `context_messages` uses up to four messages immediately before the bookmark (excluding the bookmark message).
- Changes are persisted as context-library items and queued for sync.
- See [context-item.md](context-item.md) for direct CRUD on the resulting context items.

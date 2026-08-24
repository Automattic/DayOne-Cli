# context-item

Create/update Context Library items, or write a tombstone (soft-delete).

```bash
dayone context-item put    [--json <inline> | --json-file <path>]
dayone context-item delete --id <id> --updated-at <ISO-8601> [--deleted-at <ISO-8601>]
```

| Subcommand | Purpose |
|------------|---------|
| `put` | Upsert one context item via JSON payload. |
| `delete` | Soft-delete one item by id (writes a tombstone update). |

## `put` flags

| Flag | Required | Description |
|------|----------|-------------|
| `--json` | conditional | Inline JSON payload object |
| `--json-file` | conditional | JSON payload file path |
| `--api-host` | no | Override endpoint for this call |

## `delete` flags

| Flag | Required | Description |
|------|----------|-------------|
| `--id` | yes | Context item id |
| `--updated-at` | yes | ISO-8601 timestamp used for write ordering |
| `--deleted-at` | no | Optional ISO-8601 tombstone timestamp (defaults to `--updated-at`) |
| `--api-host` | no | Override endpoint for this call |

## Examples

```bash
dayone context-item put --json '{"id":"ctx-1","content":"RD...","category":"RD...","updated_at":"2026-03-11T16:57:53.000Z"}'
dayone context-item put --json-file /tmp/context-item.json
dayone context-item delete --id ctx-1 --updated-at 2026-03-11T17:00:00.000Z
```

## Notes

- `delete` reads the local row by id to include required fields in the tombstone write. If the id is not found locally, run `dayone sync` first.
- Writes are local-first: `put` / `delete` persist locally with `needs_sync=1` and queue an outbox item.
- Run `dayone sync` after writes to push queued context-library operations.
- Verify with `dayone list context-items` (after sync).
- Output includes `operation`, `queued`, `synced`, and `outbox_id`.

> [!CAUTION]
> `context-item delete` is destructive from the user's perspective. The calling tool MUST require explicit human confirmation.

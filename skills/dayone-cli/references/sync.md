# sync

Pull journals, entries, chats, comments, and Context Library items from the Day One API into local SQLite, and flush queued writes from the durable outbox.

```bash
dayone sync
```

| Flag | Required | Description |
|------|----------|-------------|
| `--api-host` | no | Override endpoint for this call |

```bash
dayone sync
DAYONE_API_HOST=https://dayone.me dayone sync
dayone --api-host https://dayone.me sync
```

> [!CAUTION]
> Sync can be long-running and may download large datasets on a fresh account.

## What sync flushes

Every write command queues an outbox item; `sync` is what actually delivers it:

- `entry write` (including backdated `--date`), `entry delete`
- `comment write`, `comment update`, `comment delete`, `comment react`, `comment unreact`
- `context-item put`, `context-item delete`
- `daily-chat add`
- `journal create`
- `memory process` (queues context-item changes)

Attachments on entries upload in two phases during sync: entry association first, then the original media payload.

## Recommended write/verify pattern

```bash
dayone sync                # 1. pull latest server state
# ... write command(s) ...
dayone sync                # 2. push queued changes
dayone list ...            # optional verification (after sync)
```

For comments, use `dayone comment list --refresh` after the second `sync` to fetch the canonical server-assigned IDs.

# comment

Manage comments on shared journal entries. All writes are local-first and queued for sync.

```bash
dayone comment list   --journal-id <id> --entry-id <id> [--limit <n>] [--offset <n>] [--cursor <token>] [--refresh]
dayone comment write  --journal-id <id> --entry-id <id> --body <text>
dayone comment update --journal-id <id> --entry-id <id> --comment-id <id> --body <text>
dayone comment delete --journal-id <id> --entry-id <id> --comment-id <id>
dayone comment react   --journal-id <id> --entry-id <id> --comment-id <id> [--reaction <like>]
dayone comment unreact --journal-id <id> --entry-id <id> --comment-id <id>
```

| Subcommand | Purpose |
|------------|---------|
| `list` | List locally stored comments for an entry. |
| `write` | Create a comment locally and queue it for sync. |
| `update` | Update an existing comment locally and queue it for sync. |
| `delete` | Soft-delete a comment locally and queue it for sync. |
| `react` | Add the current user's reaction (default: `like`) to a comment. |
| `unreact` | Remove the current user's reaction from a comment. |

## Shared flags

| Flag | Required | Description |
|------|----------|-------------|
| `--journal-id` | yes | Journal id that owns the entry |
| `--entry-id` | yes | Entry id that owns the comment(s) |
| `--api-host` | no | Override endpoint for this call |

## Subcommand-specific flags

`list`:

| Flag | Description |
|------|-------------|
| `--limit` | Maximum comments to return |
| `--offset` | Offset for offset-based paging |
| `--cursor` | Cursor token for cursor-based paging |
| `--refresh` | Fetch the latest comment feed from the server before listing |

`write` / `update`: `--body` (required) — comment body text.

`update` / `delete` / `react` / `unreact`: `--comment-id` (required).

`react`: `--reaction` (optional; default `like`).

## ID remapping after first sync

Write commands return temporary local IDs. During create reconciliation, sync remaps them to canonical server IDs. **Always read IDs from `comment list --refresh` after `sync`** before chaining `update` / `react` / `delete` calls.

## Recommended verification sequence

```bash
dayone sync
dayone comment write --journal-id <id> --entry-id <id> --body "Great memory."
dayone sync
dayone comment list --journal-id <id> --entry-id <id> --refresh
# now use the canonical comment-id from the refreshed list for any follow-ups:
dayone comment update --journal-id <id> --entry-id <id> --comment-id <canonical> --body "Great memory! Thanks."
dayone comment react   --journal-id <id> --entry-id <id> --comment-id <canonical>
dayone comment delete  --journal-id <id> --entry-id <id> --comment-id <canonical>
```

> [!CAUTION]
> `comment delete` is destructive from the user's perspective. The calling tool MUST require explicit human confirmation.

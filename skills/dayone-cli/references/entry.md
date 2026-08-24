# entry

Create/update entry content or soft-delete an entry in a journal.

```bash
dayone entry write --journal-id <journal-id> [--body <text> | --body-stdin] \
                   [--entry-id <entry-id>] [--date <YYYY-MM-DD|RFC3339>] [--all-day] \
                   [--attach <path> ...] [--attach-type <image|video|audio|pdfAttachment> ...]
dayone entry delete --journal-id <journal-id> --entry-id <entry-id>
```

| Subcommand | Purpose |
|------------|---------|
| `write` | Create or update one entry in a journal; prints JSON result. |
| `delete` | Soft-delete one entry (writes a tombstone update and queues sync). |

## `write` flags

| Flag | Required | Description |
|------|----------|-------------|
| `--journal-id` | yes | Target journal id |
| `--body` | conditional | Entry body text (required if `--body-stdin` is not used) |
| `--body-stdin` | conditional | Read body from stdin (required if `--body` is not used) |
| `--entry-id` | no | Optional entry id; auto-generated 32-char hex when omitted |
| `--date` | no | Entry timestamp as `YYYY-MM-DD` (local midnight) or RFC3339 datetime |
| `--all-day` | no | Mark as all-day. If `--date` is omitted, defaults to today's local midnight. With RFC3339 `--date`, the date is preserved and time-of-day is ignored. |
| `--attach` | no | Local file path to attach (repeatable) |
| `--attach-type` | no | Type per `--attach`; inferred from extension when omitted |
| `--api-host` | no | Override endpoint for this call |

`write` updates `body` and regenerates `richTextJSON` from the new body text.

## `delete` flags

| Flag | Required | Description |
|------|----------|-------------|
| `--journal-id` | yes | Journal id that owns the entry |
| `--entry-id` | yes | Entry id to soft-delete |
| `--api-host` | no | Override endpoint for this call |

> [!CAUTION]
> `entry delete` is destructive from the user's perspective. The calling tool MUST require explicit human confirmation.

## Examples

```bash
dayone entry write --journal-id <id> --body "Today was great."
printf '%s' 'Draft from stdin' | dayone entry write --journal-id <id> --body-stdin
dayone entry write --journal-id <id> --date 2026-03-31 --body "Daily recap"
dayone entry write --journal-id <id> --date 2026-03-31T09:30:00Z --body "Standup notes"
dayone entry write --journal-id <id> --date 2026-03-31T09:30:00Z --all-day --body "Daily summary"
dayone entry write --journal-id <id> --body "Trip" --attach /path/photo.jpg --attach /path/receipt.pdf
dayone entry delete --journal-id <id> --entry-id <entry-id>
```

## Output

Local-first JSON including `entry_id`, `journal_id`, `queued`, `synced`, and `outbox_id`. Attachments are queued with the entry and uploaded during `dayone sync`.

## Safe update workflow (appending to an existing entry)

1. `dayone sync`
2. Read the existing body from local data (`dayone list entries --journal-id <id> --fields id,date,body`).
3. Append the new text to the existing body in memory.
4. `dayone entry write --journal-id <id> --entry-id <entry-id> --body-stdin` (piping the merged body).
5. `dayone sync` to push.
6. Verify both `body` and (when relevant) rich-text rendering in a client after sync.

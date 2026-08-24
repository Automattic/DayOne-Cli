# list

Read synced local state as machine-readable JSON. `list` never hits the network.

> [!IMPORTANT]
> **Run `dayone sync` at the start of the session before the first `list` call**, unless the user explicitly opts out ("from cache", "use local data", "skip sync"). The CLI does not expose a "last synced at" timestamp, so the safest default is to sync first. See [../SKILL.md#read-freshness](../SKILL.md) for the full rule.

For interactive browsing instead of JSON, see [tui.md](tui.md). For keyword/semantic search, see [search.md](search.md).

```bash
dayone list <command> [flags]
```

| Subcommand | Purpose |
|------------|---------|
| `journals [--include-deleted]` | List synced journals. |
| `entries --journal-id <id> [--sort <asc\|desc>] [--sort-method <entryDate\|editDate>] [--fields <f[,f...]>]` | List entries for a journal. |
| `daily-chat` | List daily chat records. |
| `daily-chat-messages --daily-chat-id <id>` | List messages in a daily chat. |
| `context-items` | List Context Library items. |

## Journals

- `list journals` excludes deleted journals by default.
- Use `--include-deleted` to include them.

## Entry ordering

- `--sort` accepts `asc` or `desc`. Default is `desc`.
- `--sort-method` accepts `entryDate` or `editDate`. Default follows the journal's `sort_method` (`entryDate` if unknown).
- Tie-breaker is `id DESC` (or `ASC` when `--sort asc`).

> [!IMPORTANT]
> **`list entries` and `search entries` order rows with different flag names.** Do not copy the `search` shape over.
>
> | Command | direction flag | key flag |
> |---------|----------------|----------|
> | `dayone list entries`   | `--sort asc\|desc` | `--sort-method entryDate\|editDate` |
> | `dayone search entries` | `--direction asc\|desc` | `--sort relevancy\|entryDate\|editDate` |
>
> On `list entries`, `--sort entryDate` is rejected (`entryDate` is not in `[asc, desc]`) and `--direction` does not exist.

## Entry field projection

- `--fields` accepts a comma-separated list of top-level entry keys (e.g. `id,date,tags,body`).
- Absent keys are returned as `null`.
- Omitting `--fields` returns full entry objects.

## Examples

```bash
dayone list journals
dayone list journals --include-deleted
dayone list entries --journal-id <journal-id>
dayone list entries --journal-id <journal-id> --sort asc
# newest entries by entry date (the most common ask):
dayone list entries --journal-id <journal-id> --sort desc --sort-method entryDate
# oldest first by last edit:
dayone list entries --journal-id <journal-id> --sort asc  --sort-method editDate
dayone list entries --journal-id <journal-id> --fields id,date,tags,body
dayone list daily-chat
dayone list daily-chat-messages --daily-chat-id <daily-chat-id>
dayone list context-items
```

> [!NOTE]
> Newly created journals show a `pending-…` id until the outbox is flushed. After `dayone sync` the server returns a numeric id. Always use the resolved id from `list journals` when scoping `list entries --journal-id …`.

## Output shape

`list journals` / `list entries` / `list daily-chat` / `list context-items`:

```json
{
  "ok": true,
  "count": 12,
  "limit": null, "offset": null, "next_cursor": null, "has_more": false,
  "items": [ /* one resource per element */ ]
}
```

`list daily-chat-messages` uses a **different** envelope (no `items` key — it's `messages`):

```json
{ "ok": true, "daily_chat_id": "...", "count": 5, "messages": [ /* per-message JSON */ ] }
```

Entry `date` fields are epoch milliseconds — convert with `(.date / 1000 | todate)`.

`jq` recipes:

```bash
dayone list journals | jq '.items[] | {id, name}'
dayone list entries --journal-id <id> --sort desc --sort-method entryDate --fields id,date,tags,body \
  | jq '.items[] | {id, date: (.date / 1000 | todate), tags, body: .body[:200]}'
dayone list daily-chat-messages --daily-chat-id 2026-05-13 | jq '.messages[]'
```

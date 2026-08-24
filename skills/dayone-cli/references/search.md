# search

Keyword + semantic search over local Day One data. Reads SQLite only — never hits the network at query time.

> [!IMPORTANT]
> **Run `dayone sync` at the start of the session before the first `search` call**, unless the user explicitly opts out ("from cache", "use local data", "skip sync"). Without it, queries like *"when did I last run?"* may miss recent entries written from the web or mobile clients. See [../SKILL.md#read-freshness](../SKILL.md) for the full rule.

```bash
dayone search context-items --query <text> [--threshold <number>] [--limit <n>] [--category <name>]
dayone search entries [--query <text>] [--threshold <number>] [--limit <n>] [filters...] \
                      [--sort <relevancy|entryDate|editDate>] [--direction <asc|desc>]
```

| Subcommand | Purpose |
|------------|---------|
| `context-items` | Search context library item content. |
| `entries` | Search entries with optional query + Day One Web–parity filters. |

## `search entries` filters (web parity)

Repeatable filters may be passed multiple times.

| Flag | Repeatable | Description |
|------|------------|-------------|
| `--journal-id <id>` | yes | Restrict to one or more journal ids. |
| `--tag <name>` | yes | Match entries containing any requested tag. |
| `--date-from <YYYY-MM-DD\|RFC3339\|epoch-ms>` | no | Inclusive lower date bound. |
| `--date-to <YYYY-MM-DD\|RFC3339\|epoch-ms>` | no | Inclusive upper date bound. |
| `--favorite` | no | Only starred/favorite entries. |
| `--has-checklist` | no | Entries with checklist-like rich text/markdown content. |
| `--place <name>` | yes | Match place names (`location.placeName`). |
| `--media <image\|video\|audio\|pdfAttachment>` | yes | Match entries with the requested media types in `moments`. |
| `--prompt-id <id>` | yes | Match `promptID` / `promptId`. |
| `--template-id <id>` | yes | Match `templateID` / `templateId`. |
| `--creation-device <name>` | yes | Match creation device from `clientMeta`. |
| `--weather-code <code>` | yes | Match weather code. |
| `--music-artist <name>` | yes | Match music artist. |
| `--activity <name>` | yes | Match activity value. |

## Sorting

| Flag | Values | Default | Notes |
|------|--------|---------|-------|
| `--sort` | `relevancy`, `entryDate`, `editDate` | `relevancy` | Relevancy uses merged keyword + semantic score when query is present. |
| `--direction` | `asc`, `desc` | `desc` | Applies to the selected sort key. |

> [!IMPORTANT]
> **`search entries` and `list entries` order rows with different flag names.** Do not copy this shape over to `list entries`.
>
> | Command | key flag | direction flag |
> |---------|----------|----------------|
> | `dayone search entries` | `--sort relevancy\|entryDate\|editDate` | `--direction asc\|desc` |
> | `dayone list entries`   | `--sort-method entryDate\|editDate`     | `--sort asc\|desc` |
>
> On `list entries`, `--sort entryDate --direction desc` is invalid — see [list.md](list.md#entry-ordering).

## Query behavior

- `--query <text>` runs keyword + semantic search.
- Blank query is supported on `search entries` only when at least one non-query filter is set (**wildcard mode**, matching Day One Web).
- For `search entries`, at least one of `--query` or a non-query filter must be provided.
- `--threshold` is only meaningful when semantic query scoring is active.

## Output shape

```json
{
  "ok": true,
  "query": "running",
  "threshold": 0.3,
  "limit": 5,
  "count": 3,
  "effective_params": { /* search entries only — normalized params, parsed date bounds, wildcard-mode state, etc. */ },
  "items": [
    { "id": "...", "similarity": 0.81, "item": { /* full entry or context-item JSON */ } }
  ]
}
```

> [!IMPORTANT]
> - Top level is an **object**, never a bare array. Use `.items[]`, not `.[]`.
> - Each hit wraps the actual entry/context-item under `.item`. Reach for `.items[].item.body`, **not** `.items[].body`.
> - Entry `date` (inside `.item`) is **epoch milliseconds** — convert with `(.item.date / 1000 | todate)` for RFC3339.

`jq` recipes:

```bash
# Recent entries matching "running" with date + first 200 chars of body:
dayone search entries --query "running" --sort entryDate --direction desc --limit 5 \
  | jq '.items[] | {id, similarity, date: (.item.date / 1000 | todate), body: .item.body[:200]}'

# Just the bodies (newline-separated):
dayone search entries --query "running" --limit 5 | jq -r '.items[].item.body'

# Top context-item hits:
dayone search context-items --query "tennis" --limit 5 \
  | jq '.items[] | {id, similarity, content: .item.content[:200]}'
```

## Examples

```bash
# Context item search
dayone search context-items --query "playing tennis" --limit 20

# Basic entry search
dayone search entries --query "tennis" --limit 10

# Multi-filter web-parity search
dayone search entries \
  --journal-id <journal-a> \
  --journal-id <journal-b> \
  --tag travel \
  --tag morning \
  --date-from 2026-04-01 \
  --date-to 2026-04-10 \
  --favorite \
  --has-checklist \
  --place "Yosemite Valley" \
  --media image \
  --media pdfAttachment \
  --prompt-id <prompt-id> \
  --template-id <template-id> \
  --creation-device "iPhone 15 Pro" \
  --weather-code clear-day \
  --music-artist Radiohead \
  --activity Walking \
  --sort editDate \
  --direction asc

# Wildcard mode (blank query + filters)
dayone search entries --favorite --tag travel --sort entryDate --direction desc
```

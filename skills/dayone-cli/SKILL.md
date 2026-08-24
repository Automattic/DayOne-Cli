---
name: dayone-cli
description: "Read, write, search, and sync a Day One journal from the command line via the `dayone` binary — including journal entries, comments on shared entries, daily chats, and account settings. Use whenever the user wants to do anything with their Day One journal or is asking for something that can be found in a journal or memory system: 'add a journal entry', 'write today's entry', 'log today', 'append a note to today', 'search my journal for X', 'what did I write about X', 'when did I last X', 'show my journals', 'list yesterday's entries', 'sync Day One', 'comment on a shared entry', 'react to a comment', 'append to today's daily chat'. Local-first: `list`, `search`, and `tui` read only the local SQLite store, so ALWAYS run `dayone sync` before the first read in a session unless the user explicitly opts out ('from cache', 'use local data', 'skip sync'). Prefer this skill over hand-rolled shell pipelines whenever any Day One operation is requested."
allowed-tools: "Bash(dayone:*)"
---

# dayone

The `dayone` binary is a local-first CLI for the Day One service. Everything below — auth, writes, search, sync — runs against a per-profile SQLite store; writes are queued in an outbox and pushed to the server by `dayone sync`.

Use this top-level page as the index. Open the matching `references/<topic>.md` only when you need the full flag table or command-specific workflow.

## Command tree

| Group | What it does | Reference |
|-------|--------------|-----------|
| `auth` | `login`, `logout`, `key-set`, `whoami` | [references/auth.md](references/auth.md) |
| `profile` | `list`, `set` (switch staging/production endpoint) | [references/profile.md](references/profile.md) |
| `sync` | (terminal command) Pull remote state, flush local outbox | [references/sync.md](references/sync.md) |
| `sync-schedule` | `status`, `enable`, `set-interval`, `disable` automatic background sync | [references/sync-schedule.md](references/sync-schedule.md) |
| `entry` | `write`, `delete` journal entries | [references/entry.md](references/entry.md) |
| `journal` | `create`, `update` journals | [references/journal.md](references/journal.md) |
| `comment` | `list`, `write`, `update`, `delete`, `react`, `unreact` on shared entries | [references/comment.md](references/comment.md) |
| `daily-chat add` | (terminal command) Append one message to a date-based Daily Chat | [references/daily-chat.md](references/daily-chat.md) |
| `context-item` | `put`, `delete` Context Library items | [references/context-item.md](references/context-item.md) |
| `memory process` | (terminal command) Reconcile a conversation into context items | [references/memory.md](references/memory.md) |
| `list` | `journals`, `entries`, `daily-chat`, `daily-chat-messages`, `context-items` | [references/list.md](references/list.md) |
| `search` | `entries` (with web-parity filters), `context-items` | [references/search.md](references/search.md) |
| `tui` | (terminal command) Read-only terminal browser for journals, entries, daily chats | [references/tui.md](references/tui.md) |
| `user-settings get` | (terminal command) Fetch account settings as JSON | [references/user-settings.md](references/user-settings.md) |

> [!IMPORTANT]
> Every row that lists subcommands above is a **group** — the command takes the form `dayone <group> <subcommand> [flags]`. For example:
> - `dayone search` alone is invalid → use `dayone search entries --query "X"` or `dayone search context-items --query "X"`.
> - `dayone list` alone is invalid → use `dayone list entries --journal-id <id>` etc.
> - `dayone auth` alone is invalid → use `dayone auth login --email ...` etc.
>
> Rows marked `(terminal command)` take flags directly (e.g. `dayone sync`, `dayone tui`).

Discover any subcommand and its flags with `--help`:

```bash
dayone --help
dayone auth --help
dayone auth login --help
```

## Common one-liners

Copy-paste shapes for the most frequent tasks. **Use these first** before drilling into `references/` — references only matter when you need a flag not shown here.

```bash
# ── Always sync first before any read (see "Read freshness" below) ──
dayone sync

# ── Recommended desktop setup: keep local data fresh automatically ──
dayone sync-schedule enable --interval-minutes 30
dayone sync-schedule status

# ── Read (local SQLite, instant) ──
dayone list journals
dayone list entries --journal-id <journal-id> --sort desc --sort-method entryDate
dayone list entries --journal-id <journal-id> --fields id,date,tags,body
dayone search entries --query "running"
dayone search entries --query "running" --journal-id <journal-id> --limit 10
dayone search entries --tag travel --favorite                # wildcard mode (no --query)
dayone search context-items --query "tennis"

# ── Write (local-first; follow with `dayone sync` to push) ──
dayone entry write --journal-id <journal-id> --body "Today was great."
printf '%s' 'Draft from stdin' | dayone entry write --journal-id <journal-id> --body-stdin
dayone daily-chat add --message "Quick thought for today."
dayone journal update --journal-id <journal-id> --name "Renamed" --color "#E74C3C"
dayone sync

# ── Auth & state ──
printf '%s' '<password>' | dayone auth login --email you@example.com --password-stdin
dayone auth whoami
dayone profile list
dayone profile set staging
```

## Output shapes & `jq` recipes

> [!IMPORTANT]
> Every command prints a JSON **object** to stdout, never a bare array. Three common `jq` mistakes:
> 1. `jq '.[] | …'` fails with `Cannot index boolean with string "x"` — top level is `{ ok, …, items: […] }`. Use `.items[]` (or `.messages[]` for `list daily-chat-messages` — that one uses a `messages` key instead).
> 2. For `search`, each hit wraps the real entry/context-item under `.item`. Use `.items[].item.body`, **not** `.items[].body`. Hit envelope: `{ id, similarity, item }`.
> 3. Entry `date` is **epoch milliseconds** — `(.date / 1000 | todate)` or, inside a `search` hit, `(.item.date / 1000 | todate)` for RFC3339.

Canonical recipe covering all three (adapt and reuse):

```bash
dayone search entries --query "running" --sort entryDate --direction desc --limit 5 \
  | jq '.items[] | {id, similarity, date: (.item.date / 1000 | todate), body: .item.body[:200]}'
```

Write commands (`entry write`, `comment write`, `context-item put`, `daily-chat add`, `memory process`) return a flat object with `ok`, the resource id (`entry_id`, `comment_id`, …), `queued`, `synced`, `outbox_id` — no `items` wrapper.

Full envelopes and per-command recipes live in the references: [list.md](references/list.md#output-shape), [search.md](references/search.md#output-shape).

## Endpoint precedence (applies to every command)

In order of priority:

1. `--api-host <url>` flag on the command
2. `DAYONE_API_HOST` environment variable
3. Active SQLite profile (`dayone profile list` shows it)

## Read freshness

`list`, `search`, and `tui` read **only** local SQLite — never the network — and the CLI exposes no "last synced at" timestamp, so freshness can't be checked programmatically.

> [!IMPORTANT]
> **Run `dayone sync` once at the start of any session that will read journal data**, before the first `list`/`search`/`tui` call. Skip only when the user explicitly opts out ("from cache", "use local data", "skip sync"). Within a session no need to re-sync between successive reads unless a write happened in between (in which case re-sync to flush the outbox).

Incremental sync on an established account is fast; the "long-running" warning in [references/sync.md](references/sync.md) mostly applies to fresh accounts and first syncs. For normal desktop use, recommend `dayone sync-schedule enable --interval-minutes 30` so local reads stay fresh without manual syncs.

## Local-first write model

Every write command — `entry write`, `entry delete`, `comment write|update|delete|react|unreact`, `context-item put|delete`, `daily-chat add`, `journal create`, `journal update`, `memory process` — persists locally with `needs_sync=1` and enqueues a durable outbox item. **Run `dayone sync` after writes** to push to the server.

Recommended write flow:

```bash
dayone sync            # 1. pull latest server state
# ... your write commands ...
dayone sync            # 2. push queued changes
# optional: verify with `dayone list ...` or `dayone comment list --refresh`
```

> [!CAUTION]
> Newly created journals are assigned a `pending-…` id until the outbox is flushed. After `dayone sync` the server returns a numeric id. Commands scoped by journal (e.g. `list entries --journal-id …`) need the **resolved** id from `list journals`. Using the `pending-…` id silently returns no rows.

## Output and safety rules

- Output is JSON-first for every command. Pipe it through `jq` for human reading.
- `list`, `search`, and `tui` read local SQLite only — they never hit the network.
- Never log or echo `--password`, `--key`, encryption keys, or API tokens. Prefer `--password-stdin` / `--key-stdin` over the flag forms.
- The CLI does **not** prompt for confirmation on destructive operations. The calling tool MUST require explicit human confirmation before:
  - `auth logout --force`
  - `entry delete`
  - `comment delete`
  - `context-item delete`
  - Any command run against a production profile or `--api-host https://dayone.me`
- Prefer `--api-host` for one-off calls and `profile set` for persistent environment selection.

# daily-chat

Append one user message to a date-based Daily Chat. Read daily chats with `list daily-chat` / `list daily-chat-messages` ([references/list.md](list.md)).

```bash
dayone daily-chat add [--message <text> | --message-stdin] [--date <YYYY-MM-DD>]
```

| Flag | Required | Description |
|------|----------|-------------|
| `--message` | conditional | Message body (required if `--message-stdin` is not used) |
| `--message-stdin` | conditional | Read message body from stdin |
| `--date` | no | Target a specific local day. Defaults to today's local date when omitted. |
| `--api-host` | no | Override endpoint for this call |

## Behavior

- If no chat exists for the target date, a new Daily Chat object is created locally first.
- If a chat exists for the date, the command appends a `role=user` message to `chat_history`.
- Writes are local-first and queue an outbox item (`daily_chat_feed:update`).
- Run `dayone sync` after `daily-chat add` to push queued changes to the server.

## Examples

```bash
dayone daily-chat add --message "Today I finally shipped the feature."
printf '%s' 'A note from stdin' | dayone daily-chat add --message-stdin
dayone daily-chat add --date 2026-03-17 --message "Backfilled note"
```

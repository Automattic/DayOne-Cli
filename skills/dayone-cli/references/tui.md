# tui

```bash
dayone tui
```

Read-only, full-screen three-pane terminal browser backed by the local synced SQLite store. The TUI never writes to the server and never reaches the network.

> [!IMPORTANT]
> **Run `dayone sync` before launching `dayone tui`**, unless the user explicitly opts out ("from cache", "use local data", "skip sync"). What the TUI shows is exactly what was on disk at launch — entries added from the web or mobile after the last sync will not appear. See [../SKILL.md#read-freshness](../SKILL.md) for the full rule.

For machine-readable output, use [list.md](list.md) instead. For full-text/semantic search, use [search.md](search.md).

## Layout

Three vertical panes at a 1/6, 2/6, 3/6 width split, plus a header and a footer.

| Pane | Role |
|------|------|
| Sidebar (left) | Non-deleted journals, followed by a `DAILY CHAT` entry point. |
| Entries (middle) | Entries in the selected journal, or dates with daily chats. |
| Content (right) | Markdown-rendered entry body, or the day's chat transcript. |

Each entry row spans up to four lines:

1. Date in `Weekday, Mon DD, YYYY`
2. Single-line body preview (markdown markers stripped)
3. Tags (`#tag1 #tag2`) and media count (`N media`), only when present
4. Dim divider between rows (omitted after the last row)

Deleted journals are filtered out at query level — there is no flag to include them.

## Header

```
dayone tui   profile: production   user: Alex Example <alex@example.com>
```

If no session is present, the header shows `user: (not signed in)`.

## Keybindings

| Key | Action |
|-----|--------|
| `↑` / `↓` or `k` / `j` | Move selection (scroll the right pane when focused there). |
| `Enter` / `→` / `l` | Open the current selection and advance focus one pane right. |
| `Esc` / `←` / `h` / `Backspace` | Return focus one pane left; selection is preserved. |
| `Tab` / `Shift+Tab` | Cycle focus between panes. |
| `g` / `G` / `Home` / `End` | Jump to top / bottom of the focused list. |
| `PgUp` / `PgDn` | Scroll the right pane by 10 lines. |
| Mouse click | Select the clicked row and focus that pane. |
| Mouse wheel | Scroll the right pane. |
| `Ctrl+Y` | Copy the current entry's raw markdown to the system clipboard (via `pbcopy` / `wl-copy` / `xclip` / `xsel` / `clip`). |
| `Ctrl+C` | Quit. |

Plain `q` is a NoOp (not a quit key) so the letter can be typed freely. `Cmd`-based shortcuts on macOS are intercepted by terminal emulators before reaching the TUI, so only `Ctrl+` chords are bound.

## Daily Chat

`DAILY CHAT` in the sidebar surfaces every day that has chat history, sorted most-recent-first. Selecting a date renders the transcript as an alternating `you` / `assistant` exchange with markdown applied per message.

## Data source

- Reads from the active profile's database at `{config_dir}/profiles/{profile}/dayone.db`.
- Respects the same `--profile` / `DAYONE_API_HOST` precedence as every other subcommand.
- No network access.

## Do not use the TUI when

- You need machine-readable JSON output → use [list.md](list.md).
- You need to write, edit, or delete entries → use [entry.md](entry.md).
- You need full-text or semantic search → use [search.md](search.md).
- You are running in a non-interactive environment (CI, `!` shell, etc.). The TUI requires a real TTY and will exit with `failed to enable raw mode` otherwise.

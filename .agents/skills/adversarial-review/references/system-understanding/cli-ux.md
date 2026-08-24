## CLI / TUI UX

**Files:** `src/cli/**`, command modules, `src/tui/**`.

- Keep clap UX consistent: kebab-case command names, clear required flags, mutually exclusive options rejected at parse/normalize time, and help text that documents web/API parity flags.
- Keep stdout machine-readable JSON unless a command explicitly chooses human output. Warnings and progress belong on stderr.
- Offline-capable commands should use local SQLite state. API-connected commands require auth and should fail clearly when no token/session exists.
- Profile/env/API-host precedence must stay consistent across commands.
- TUI refreshes should invalidate stale caches, preserve selection/scroll when appropriate, and react to store writes from other connections.

### What to watch for

- Adding a one-off output shape for a single command.
- Parse-time validation implemented only after side effects have already happened.
- TUI state refresh bugs where selected entry/body/day gets stale after sync or external writes.
- Tests that assert only clap accepts a command but not normalized conflict behavior.

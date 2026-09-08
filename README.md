# Day One CLI

Command-line access to your Day One account, journals, entries, daily chat, and context items.

**License:** [GNU GPL v2 or later (`GPL-2.0-or-later`)](LICENSE-NOTICE)

The CLI is designed for scriptable workflows and machine-readable output (JSON-first).

## Supported platforms

Public releases support:

- macOS on Apple silicon and Intel
- Linux on arm64 and x64 (glibc 2.35 or newer)
- Windows on x64

The npm installer requires Node.js LTS. Direct binary installs do not require Node.js.

## Installation

### npm (recommended)

```bash
npm install -g @dayone/cli
dayone --version
```

Upgrade later with:

```bash
npm install -g @dayone/cli@latest
```

### GitHub Release binary

Download the binary for your platform from [GitHub Releases](https://github.com/Automattic/DayOne-Cli/releases/latest):

| Platform | Release file |
| --- | --- |
| macOS Apple silicon | `dayone-darwin-arm64` |
| macOS Intel | `dayone-darwin-x64` |
| Linux arm64 | `dayone-linux-arm64-no-embeddings` (glibc 2.35+) or `dayone-linux-arm64` (glibc 2.39+) |
| Linux x64 | `dayone-linux-x64-no-embeddings` (glibc 2.35+) or `dayone-linux-x64` (glibc 2.39+) |
| Windows x64 | `dayone-win32-x64.exe` |

The npm launcher chooses the compatible Linux build automatically. For direct installs on glibc 2.35–2.38, choose the `-no-embeddings` file.

On macOS or Linux, rename the download to `dayone`, make it executable, and place it on `PATH`:

```bash
RELEASE_FILE=dayone-darwin-arm64 # choose the file for your platform above
mkdir -p ~/.local/bin
install -m 0755 "$HOME/Downloads/$RELEASE_FILE" ~/.local/bin/dayone
export PATH="$HOME/.local/bin:$PATH"
dayone --version
```

Add the `export PATH=...` line to your shell startup file if `~/.local/bin` was not already on `PATH`.

On Windows, rename the download to `dayone.exe` and move it into a directory on `PATH`. To upgrade a direct install, replace that binary with the file from the newer release.

If an older, unrelated `dayone` binary still resolves first, locate it with `which dayone` (`where.exe dayone` on Windows) and remove it before installing this CLI.

## AI Agent Skills (Optional)

This repository ships a single agent skill at [`skills/dayone-cli/`](skills/dayone-cli/SKILL.md) for AI agent integrations like Claude Code, Cursor, and similar. The top-level [`SKILL.md`](skills/dayone-cli/SKILL.md) is the entry point, with per-command-group references under [`skills/dayone-cli/references/`](skills/dayone-cli/references/).

Install:

```bash
npx skills add bloom/dayone-cli-skills
```

## Quick Start

1) Run guided setup:

```bash
dayone setup
```

Setup also asks whether to set an encryption key now; you can skip and run `dayone auth key-set` later.

2) Or run login only:

```bash
dayone auth login --email you@example.com
```

Password from stdin:

```bash
# Prompt for password without echo; it will not be stored in shell history
read -r -s -p 'Day One password: ' DAYONE_PASSWORD && printf '%s' "$DAYONE_PASSWORD" | dayone auth login --email you@example.com --password-stdin
unset DAYONE_PASSWORD
```

3) Verify current identity and token:

```bash
dayone auth whoami
dayone user-settings get
```

4) Sync remote data locally:

```bash
dayone sync
```

Recommended: keep local data fresh by scheduling automatic sync every ~30 minutes:

```bash
dayone sync-schedule enable --interval-minutes 30
dayone sync-schedule status
```

This installs a per-user schedule using the native scheduler for your OS (macOS LaunchAgent, Linux systemd user timer, or Windows Task Scheduler) and does not require admin privileges for the normal current-user setup.

5) Start using data:

```bash
dayone list journals
dayone list entries --journal-id <journal-id> --limit 20
dayone list entries --journal-id <journal-id> --limit 20 --offset 20
dayone list entries --journal-id <journal-id> --fields id,date,tags,body
dayone list entries --journal-id <journal-id> --sort-method editDate
```

## Common Commands

### Sync scheduling

Journal and entry workflows are local-first. Listing, searching, and reading entries use the local SQLite store; journal, entry, comment-body, Daily Chat, and context-item changes queue work for `dayone sync`. Authentication, user settings, sync, refreshed comment listings, and comment reactions use the network directly. For normal desktop use, we recommend scheduling automatic sync about every 30 minutes:

```bash
dayone sync-schedule enable --interval-minutes 30
```

Manage the schedule:

```bash
dayone sync-schedule status
dayone sync-schedule set-interval --interval-minutes 15
dayone sync-schedule disable
```

`sync-schedule` uses per-user schedulers: LaunchAgent on macOS, systemd user timers on Linux, and Task Scheduler on Windows. Output is JSON, like other commands.

### Entries

Write an entry:

```bash
dayone entry write --journal-id <journal-id> --body "My new entry"
```

Read or update an existing entry:

```bash
dayone entry read --journal-id <journal-id> --entry-id <entry-id>
dayone entry write --journal-id <journal-id> --entry-id <entry-id> --body "Updated body"
```

Writes are local-first: they update the profile's SQLite store and queue an outbox item. Run `dayone sync` (or enable `sync-schedule`) to send them to Day One.

Write an all-day entry (time ignored on display):

```bash
dayone entry write --journal-id <journal-id> --date 2026-03-31T09:30:00Z --all-day --body "Daily summary"
```

Write an entry with attachments (repeat `--attach`):

```bash
dayone entry write --journal-id <journal-id> --body "Trip notes" --attach /path/to/photo.jpg --attach /path/to/itinerary.pdf
```

Place media inline with Day One Web placeholders:

```bash
dayone entry write --journal-id <journal-id> --body $'Trip notes\n\n![](dayone-moment://)\n\nMore notes' --attach /path/to/photo.jpg
```

Use existing moment IDs directly:

```bash
dayone entry write --journal-id <journal-id> --body $'Look back\n\n![](dayone-moment://AFC7155130AF46E497A7C8D1C70D7731)'
```

Optionally pin attachment types explicitly:

```bash
dayone entry write --journal-id <journal-id> --body "Voice memo" --attach /path/to/memo.m4a --attach-type audio
```

Write from stdin:

```bash
printf '%s' 'My new entry from stdin' | dayone entry write --journal-id <journal-id> --body-stdin
```

Soft-delete an entry (requires both journal and entry ids):

```bash
dayone entry delete --journal-id <journal-id> --entry-id <entry-id>
```

List entries with projected fields (smaller JSON payloads):

```bash
dayone list entries --journal-id <journal-id> --fields id,date,tags,body
```

`--fields` accepts a comma-separated list of top-level entry keys. When omitted, full entry objects are returned. Missing keys are returned as `null`.

`--sort-method` accepts `entryDate` or `editDate`. When omitted, the command follows the journal's `sort_method` (defaulting to `entryDate` if unavailable). For cursor pagination, reuse the same `--sort` and `--sort-method` values between pages; changing either can skip or duplicate entries.

Paginate entries with offset mode:

```bash
dayone list entries --journal-id <journal-id> --limit 20 --offset 40
```

Paginate entries with cursor mode (opaque cursor token):

```bash
dayone list entries --journal-id <journal-id> --limit 20
# copy `next_cursor` from JSON output
dayone list entries --journal-id <journal-id> --limit 20 --cursor "<next_cursor>"
```

### Journals

Create a journal from flags:

```bash
dayone journal create --name "CLI Journal" --color "#2f5d62" --conceal true
```

Create from JSON:

```bash
dayone journal create --json '{"name":"JSON Journal","color":"#5f4b8b","hide_all_entries":true}'
```

`--json`/`--json-file` accept only editable metadata fields (`name`, `description`/`description_v2`, `color`, `sort_method`, `hide_on_this_day`, `hide_all_entries`, `hide_streaks`, `conceal`, `add_location_to_new_entries`, `comments_disabled`, `template_id`, `preset_id`). Any other field — notably the end-to-end `encryption` vault and server-managed fields like `participants`, `owner_id`, or `state` — is silently dropped, so a full journal JSON can be passed without corrupting managed data. The encryption vault is **not** user-settable; use `--e2e` to have it generated.

Create a shared journal (shared journals must be end-to-end encrypted):

```bash
dayone journal create --name "Shared Space" --shared --e2e
```

Shared journals require end-to-end encryption. Use `--e2e` to have the CLI generate the encrypted payload; it requires `dayone auth key-set` and a prior `dayone sync` so the CLI has your encryption key and synced user key material.

### Daily Chat

List daily chats:

```bash
dayone list daily-chat
```

Append a message:

```bash
dayone daily-chat add --message "Today I shipped a feature"
```

### Context Items

List synced context items:

```bash
dayone list context-items
```

Search context items:

```bash
dayone search context-items --query "playing tennis" --limit 20
```

## Encryption Key (for decrypt-enabled sync resources)

Shortcut during onboarding (login + key set):

```bash
dayone setup
```

Set key interactively:

```bash
dayone auth key-set
```

Set key from stdin:

```bash
printf '%s' 'your-encryption-key' | dayone auth key-set --key-stdin
```

Updating the encryption key resets stored sync cursors so the next `sync` re-pulls resources from the beginning.

## Profiles, credentials, and local data

Each profile has its own API host, authentication session, and SQLite database. List or switch profiles with:

```bash
dayone profile list
dayone profile set production
dayone --profile production sync
```

Authentication tokens are stored in the profile database. End-to-end encryption master keys are written to the operating system's credential store and verified there. If secure storage is unavailable or verification fails, the CLI stores the key in the profile database instead; `auth key-set` reports this as `"storage":"sqlite"`. On headless macOS, operations that need Keychain approval fail rather than opening an invisible prompt or silently changing storage. Rerun them interactively. Protect the profile directory as sensitive data, and do not delete it to fix a prompt.

Upgrading the CLI does not remove profile data. `DAYONE_CONFIG_DIR` changes where configuration and profile databases are loaded, so changing it can make an existing login appear to be missing.

## Configuration

Default config path uses your OS config directory (for example, `~/.config/dayone-cli` on many systems).

Override config directory:

```bash
DAYONE_CONFIG_DIR=/path/to/config dayone user-settings get
```

### Shared secrets file (CLI)

On startup, before parsing arguments, the CLI loads `KEY=value` lines from `~/.config/dayone/secrets-cli` when that file exists (dotenv format, via the `dotenvy` crate). Values already set in the process environment are left unchanged, so explicit `export` / CI secrets take precedence. Use this for local CLI-only variables such as `DAYONE_API_HOST` without checking them into the repo.

### Telemetry and analytics

Telemetry is **off until you opt in**, regardless of locale or location. An interactive invocation that could collect telemetry asks for consent; pressing Enter declines. Unattended commands continue with telemetry off and never use piped input to request consent. Notices go to stderr so stdout remains machine-readable JSON. Existing installations must opt in too: seeing an older notice is not consent.

Manage the choice locally with `dayone telemetry status`, `dayone telemetry enable`, or `dayone telemetry disable`. The CLI stores the decision, disclosure version, and timestamp in `config.toml`. No reporting client starts during argument parsing, configuration loading, or consent resolution. Full collection details and outstanding release-review items are in [Telemetry](docs/telemetry.md).

The CLI uses Sentry for unexpected errors and Automattic Tracks for product-usage analytics. When enabled, Sentry reports can include the command, profile category (`production`, `staging`, or `custom`), OS/architecture, release, environment, and error chain. Before sending, the CLI removes Sentry's user and server-name fields, removes HTTP response bodies, and scrubs home paths, URLs, email-shaped strings, and long token-shaped strings from common message fields. Other server-provided error text can still appear, so use one of the opt-outs below if you do not want reports sent.

Tracks events contain command and event categories, booleans, a signed-in flag, version/platform information, subscription tier, outcomes, counts, and durations. They do not include journal content, titles, bodies, emails, or tokens. The ingestion service also derives approximate location from your IP address. This is pseudonymous personal data, not anonymous data. CLI analytics are **not linked to your Day One account**: every event is attributed only to a pseudonymous installation ID, before and after you sign in. Your Day One user id is never used as the analytics identity and is never sent. Tracks is disabled for staging and custom API endpoints unless `DAYONE_TRACKS_ENDPOINT` explicitly selects a development or test endpoint. Events are queued locally and flushed at the end of an invocation. A stalled Tracks request can delay exit by up to about 2.5 seconds, but analytics cannot change a command's result; unsent events are retried on a later run.

Both are disabled together by `dayone telemetry disable` or either of these (read from the environment, so they can live in `secrets-cli`):

- `DO_NOT_TRACK=1` (per [consoledonottrack.com](https://consoledonottrack.com/))
- `DAYONE_TELEMETRY=0` (also accepts `false`/`off`/`no`/`disabled`)

Analytics additionally respects your account's **Usage Statistics** setting (`track_usage_statistics`) once synced. The installation ID is created only when consent and the Tracks settings permit collection, and is stored in `config.toml` under `[analytics]`.

## Troubleshooting and support

### Authentication or Keychain failures

Run login and Keychain operations from an interactive terminal so the operating system can display authorization prompts:

```bash
dayone auth whoami
dayone auth login --email you@example.com
dayone auth key-set
```

If a Keychain operation requires interaction or appears stuck, cancel it, unlock the system credential store, and rerun it interactively. Avoid repeatedly deleting credentials or the SQLite database; that can strand encrypted local state.

### Queued or failed sync changes

Inspect the outbox without making a network request:

```bash
dayone outbox list
dayone outbox list --payload
```

`--payload` can contain journal text and private identifiers. Inspect it locally and share it only through a private support channel. Clearing an item prevents that queued change from reaching the server:

```bash
dayone outbox clear --id <outbox-id>
dayone outbox clear --failed
```

Local entry data is not deleted by `outbox clear`, but the cleared operation will not sync unless another local edit queues it again.

### Collecting diagnostics

`doctor` prints a privacy-safe JSON report of the CLI build, platform, configuration health, SQLite availability, aggregate local-data counts, session/key-storage state, sync metadata, and known local problems. It does not contact the Day One API, migrate or repair the database, or read Keychain secrets:

```bash
dayone doctor > dayone-doctor.json
```

The top-level `status` and `check_counts` support quick triage. Checks report `pass`, `finding`, `fail`, or `not_run`: findings need user context, while failures identify concrete breakage. Individual findings use stable codes plus impact and suggested action; configuration details distinguish an actual config file from built-in fallback defaults.

Raw journal and entry IDs are excluded by default. For deliberate local investigation, `dayone doctor --include-private-identifiers` includes them and marks the report private; journal names and entry content remain excluded.

To reproduce a command failure with structured timing and subsystem checkpoints, write a local JSONL trace:

```bash
dayone --trace-file ~/Desktop/dayone-trace.jsonl sync
```

The file must not already exist. Traces are capped at 1 MiB, never contain raw command arguments or payloads, and are never uploaded automatically. A random invocation ID links any matching Sentry error without identifying the account. Telemetry opt-outs remain in effect.

Do not send your profile database, `secrets-cli`, authentication tokens, encryption keys, or `outbox list --payload` output. Contact [Day One Support](https://dayone.me/support) with the doctor report or trace and the normal command error.

## Launch scope and limitations

The first supported release covers npm and GitHub binary distribution, authentication and profiles, local-first sync, journal and entry workflows, comments, supported media, Daily Chat, context items, search, and the terminal UI.

Important limits:

- Existing-entry updates are safe only for content the CLI can represent. Do not use the CLI to edit entries containing unsupported features such as drawings, highlighting, pinned state, extended embeds, AI metadata, or route data.
- Some sync failures require manual outbox inspection; not every conflict is automatically resolved.
- macOS Intel, Windows x64, and Linux `-no-embeddings` builds do not provide semantic embedding search. Keyword search remains available.
- Windows x64 is supported, but Windows code signing is not part of the first release; SmartScreen may warn on a downloaded binary.
- Homebrew, Cargo publishing, package marketplaces, MCP, location editing, and full native-app feature parity are not launch features.

## Output and notes

- Command output is structured JSON.
- `sync` progress logs are written to stderr with `[sync]` prefixes.
- Auth/session data is persisted locally in SQLite.
- Encryption master keys use the system credential store when available. SQLite fallback occurs when secure storage is unavailable or verification fails; headless authorization requirements fail instead.
- Attachment uploads are local-first: `entry write` queues metadata and originals; `sync` uploads entry metadata first, then original binaries.
- Attachment constraints: max `500MB` per file, with supported families image/video/audio/pdf.

## License

GNU GPL v2 or later (`GPL-2.0-or-later`). See [`LICENSE-NOTICE`](LICENSE-NOTICE) for the project grant and [`LICENSE`](LICENSE) for the complete GPLv2 terms.

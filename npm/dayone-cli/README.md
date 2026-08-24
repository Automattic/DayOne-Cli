# Day One CLI

Command-line access to Day One with JSON-first output and a local-first SQLite store.

## Supported platforms

- macOS on Apple silicon and Intel (Intel supports keyword search only; semantic embedding search is not available)
- Linux on arm64 and x64 (glibc 2.35 or newer; the launcher selects the compatibility build below glibc 2.39)
- Windows on x64 (keyword search only; semantic embedding search is not available)

## Install or upgrade

Node.js LTS is required for the npm launcher:

```bash
npm install -g @dayone/cli@latest
dayone --version
```

Standalone binaries are also available from [GitHub Releases](https://github.com/Automattic/DayOne-Cli/releases/latest). Homebrew, Cargo publishing, package marketplaces, and Windows code signing are not part of the first public release.

## Quick start

```bash
dayone setup
dayone auth whoami
dayone sync
dayone list journals
dayone list entries --journal-id <journal-id> --limit 20
```

Journal and entry workflows are local-first: listing, searching, and reading entries use the active profile's SQLite database, while journal and entry changes queue work for `dayone sync`. Authentication, user settings, sync, refreshed comment listings, and comment reactions use the network directly.

```bash
dayone entry write --journal-id <journal-id> --body "My new entry"
dayone entry read --journal-id <journal-id> --entry-id <entry-id>
dayone entry write --journal-id <journal-id> --entry-id <entry-id> --body "Updated body"
dayone sync
```

Enable optional per-user background sync:

```bash
dayone sync-schedule enable --interval-minutes 30
```

## Privacy controls

The CLI uses Sentry for error diagnostics and Automattic Tracks for product-usage analytics. Tracks is disabled for staging and custom API endpoints unless `DAYONE_TRACKS_ENDPOINT` explicitly selects a development or test endpoint. Disable both reporting systems with either environment variable:

```bash
DO_NOT_TRACK=1 dayone sync
DAYONE_TELEMETRY=0 dayone sync
```

These values can also be placed in `~/.config/dayone/secrets-cli`.

## Troubleshooting

Inspect locally stored diagnostic and outbox state:

```bash
dayone doctor
dayone outbox list
```

The commands read local state, but the CLI may still flush queued analytics when it exits. Use `DO_NOT_TRACK=1` for a fully offline invocation.

`dayone outbox list --payload` can include journal text and private identifiers. Do not share payloads, the profile database, authentication tokens, or encryption keys publicly.

Contact [Day One Support](https://dayone.me/support) with the CLI version, operating system, failing command without entry text, and exact error.

## Documentation

The [full README](https://github.com/Automattic/DayOne-Cli#readme) covers direct binary installation, profiles and Keychain behavior, supported workflows, media, telemetry, recovery, and launch limitations.

## License

GNU GPL v2 or later (`GPL-2.0-or-later`).

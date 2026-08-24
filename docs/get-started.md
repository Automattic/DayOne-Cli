# Get started

This guide gets a public Day One CLI install authenticated and synced. See the [main README](../README.md) for supported platforms, direct GitHub binary installation, privacy controls, troubleshooting, and launch limitations.

## Install

The recommended install uses Node.js LTS and npm:

```bash
npm install -g @dayone/cli
dayone --version
```

Upgrade with the same package:

```bash
npm install -g @dayone/cli@latest
```

You can instead install a standalone binary from [GitHub Releases](https://github.com/Automattic/DayOne-Cli/releases/latest). Direct binaries do not require Node.js.

## Authenticate

Run guided setup:

```bash
dayone setup
```

Or log in directly:

```bash
dayone auth login --email you@example.com
```

For a non-echoing password prompt that avoids shell history:

```bash
read -r -s -p 'Day One password: ' DAYONE_PASSWORD
printf '%s' "$DAYONE_PASSWORD" | dayone auth login --email you@example.com --password-stdin
unset DAYONE_PASSWORD
```

Verify the stored session:

```bash
dayone auth whoami
dayone user-settings get
```

## Sync and use local data

```bash
dayone sync
dayone list journals
dayone list entries --journal-id <journal-id> --limit 20
```

Journal and entry workflows are local-first. Listing, searching, and reading entries use the profile's SQLite database; journal and entry changes queue local outbox work for the next sync. Authentication, user settings, sync, refreshed comment listings, and comment reactions use the network directly.

```bash
dayone entry write --journal-id <journal-id> --body "First CLI entry"
dayone sync
```

For regular desktop use, enable a per-user background schedule:

```bash
dayone sync-schedule enable --interval-minutes 30
dayone sync-schedule status
```

## Profiles

The built-in `production` and `staging` profiles keep separate sessions and databases. Public account usage should normally use `production`.

```bash
dayone profile list
dayone profile set production
dayone --profile production sync
```

Changing `DAYONE_CONFIG_DIR` selects a different configuration and profile-data location. If an existing login appears missing, check that environment variable before logging in again.

## Build from source (contributors)

Building from source is a development workflow, not a supported public distribution channel:

```bash
cargo build
cargo test --locked
cargo run -- --help
```

Production releases are distributed through npm and GitHub Releases; Cargo publishing is not part of the first public launch.

## Next reads

- [Main README](../README.md)
- [Entry body and media format](entry-body-format.md)
- [Architecture](architecture.md)
- [Release procedure](releasing.md)

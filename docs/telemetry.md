# Day One CLI telemetry

Telemetry is optional and off by default. Declining does not limit CLI features. Usage analytics and error reports help us understand how the CLI is used and diagnose problems.

## Enable or disable telemetry

Interactive commands ask `Enable optional telemetry? [y/N]`. Press Enter or answer `no` to decline. Scripts and pipelines continue with telemetry off until you opt in.

```sh
dayone telemetry status
dayone telemetry enable
dayone telemetry disable
```

`enable` shows the telemetry notice and saves your consent. `disable` turns off both analytics and error reporting. These commands work without an account. Your choice is saved in `config.toml` and applies to all profiles in that configuration directory.

To override saved consent, set `DO_NOT_TRACK=1` or `DAYONE_TELEMETRY=0` in your environment. You can also save either setting in `~/.config/dayone/secrets-cli`. Environment variables cannot grant consent. The account's Usage Statistics setting controls Tracks only; use the CLI controls above to disable both services.

Disabling telemetry also affects running commands. Requests already in flight may finish, and data already sent is not deleted by this command.

## Usage analytics: Automattic Tracks

Analytics can include:

- Command categories, success or failure, and duration.
- CLI version, operating system, and architecture.
- Sign-in status and subscription tier.
- Counts and flags, such as whether a created journal is shared.
- A random installation identifier and approximate location derived from the source IP address.

The identifier is pseudonymous personal data. It is not linked to your Day One account ID. Analytics excludes journal, entry, and comment content, titles, email addresses, and authentication tokens.

Each profile can store up to 500 unsent analytics events locally. Events older than 30 days are discarded when the CLI next flushes analytics. Re-enabling telemetry uses a new installation identifier and does not upload events from before the current consent decision.

## Error reports: Sentry

Error reports can include command and profile categories, CLI version, operating system, architecture, error messages, and stack traces. If you request a diagnostic trace, its identifier may also be included.

The CLI removes Sentry user and server-name fields and HTTP response bodies. It filters home paths, URLs, email-shaped strings, and long token-shaped strings from common error fields. Filtering can miss sensitive text, including other server-provided error messages. Disable telemetry if you do not want to send these reports.

Both services receive the source IP address used for the request.

For questions, contact [Day One Support](https://dayone.me/support). Do not include journal content, credentials, or your profile database in a support request.

# Day One CLI telemetry

Telemetry is optional and off until you agree, everywhere the CLI runs. Declining does not restrict CLI functionality. Usage analytics and error reporting help the team understand feature usage and diagnose reliability problems.

## Your choice

On an interactive invocation that could collect telemetry, the CLI explains the collection and asks `Enable optional telemetry? [y/N]`. Press Enter or answer `no` to decline. Only `yes` or `y` grants consent. If input ends or the decision cannot be saved, telemetry stays off.

In scripts, CI, and pipelines, the command continues with telemetry off until you have recorded consent. The CLI does not consume piped command input for the consent prompt. It prints a one-time notice to stderr; stdout remains machine-readable JSON.

You can manage the choice without contacting either reporting service:

```sh
dayone telemetry status
dayone telemetry enable
dayone telemetry disable
```

`enable` prints this collection's disclosure and records agreement. `disable` withdraws consent for both analytics and error reporting. These commands work without an account or a profile database. The choice applies across profiles in the same configuration directory.

The CLI saves the choice, disclosure version, and decision timestamp in `config.toml` under `[analytics]`. The old default-on notice is not consent. An agreement to an outdated disclosure must be renewed; a refusal remains a refusal. Changing language, timezone, or location never enables telemetry.

These environment variables override a saved agreement and disable both reporting systems:

- `DO_NOT_TRACK=1`
- `DAYONE_TELEMETRY=0` (also accepts `false`, `off`, `no`, or `disabled`)

They can be saved in `~/.config/dayone/secrets-cli`. Setting `DAYONE_TELEMETRY=1` is not consent and cannot override a saved refusal. The account's Usage Statistics setting (`track_usage_statistics`), once cached locally, also disables Tracks when false. It does not control Sentry.

Running commands recheck the saved decision before recording analytics, starting a flush, or submitting a Sentry event. Withdrawal cannot recall a request already in flight or erase events already delivered to the services.

## Product usage analytics: Automattic Tracks

Events describe command categories, supported feature usage, success or failure, and duration. They can include:

- A random installation identifier, stable between consent changes.
- CLI version, operating system, and architecture.
- Whether the user is signed in and a subscription-tier category.
- Counts, durations, and booleans such as whether a created journal is shared.
- Approximate location derived at ingestion from the source IP address.

The installation identifier and approximate location are pseudonymous personal data, not anonymous data. The CLI does not send the Day One account ID as its analytics identity or link the installation identifier to that ID. Analytics event fields exclude journal, entry, and comment content, titles, email addresses, and authentication tokens.

The CLI creates an installation identifier only after consent and only when Tracks is otherwise enabled. Tracks is disabled for staging and custom API hosts unless `DAYONE_TRACKS_ENDPOINT` explicitly selects a test endpoint. An endpoint override does not bypass consent.

Unsent events are stored in a per-profile SQLite queue, capped at 500 rows. At the next permitted initialization, events at or before the current agreement's timestamp are discarded, so granting consent does not authorize uploading earlier activity. Events older than 30 days are discarded during a flush. Cleanup requires a CLI invocation; files on an unused installation are not automatically erased after 30 days.

Changing the consent decision removes the stored installation identifier. Enabling reporting again starts a new identifier when Tracks next runs and excludes events queued under the earlier decision.

## Error reporting: Sentry

When configured in the build or environment, Sentry reports unexpected errors and panics after consent. It can include command and profile categories, CLI version, operating system, architecture, error chains, and stack traces. An explicitly requested diagnostic invocation can also supply a diagnostic invocation identifier.

Sentry is not initialized during argument parsing, configuration loading, or consent resolution. `profile`, `telemetry`, `doctor`, `--help`, and `--version` do not initialize a reporting client.

The CLI removes Sentry user and server-name fields, strips HTTP response bodies, and filters home paths, URLs, email-shaped strings, and long token-shaped strings from common error fields. Expected errors marked as user errors are not reported. Filtering is not a guarantee that every sensitive string is removed: other server-provided error text can remain. Disable telemetry if you do not want these reports sent.

## Retention and release review

This document is also the draft for a future hosted telemetry page. The following require confirmation before release and publication as a final privacy disclosure:

- **Tracks:** the reported company-wide retention period is 36 months. Confirm its application to CLI events and derived location fields.
- **Sentry:** confirm the project's actual event retention. A platform default is not a verified project setting.
- **Source IP and ingestion logs:** both services receive a network connection. Confirm where raw IP addresses are retained, including server or ingestion logs, and for how long. Do not interpret omission from an event payload as proof that the backend retains no IP address.
- **Legal review:** approve the wording, consent purposes, processing arrangements, and instructions for exercising data-protection rights before launch.

For questions, contact [Day One Support](https://dayone.me/support). Do not include journal content, credentials, or the profile database in a support request.

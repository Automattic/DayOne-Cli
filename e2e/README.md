# Day One Web Screenshot E2E

This folder contains Playwright-based screenshot checks for the entry regression
scenarios listed below.

## What it captures

- Plain text entry view
- Formatted markdown entry view
- Plain text entry with image at end view
- Formatted markdown entry with image at end view
- Inline media rendering entry view

## Prerequisites

1. Node.js 20+.
2. Install dependencies and browser runtime:

   ```bash
   cd e2e
   npm install
   npx playwright install chromium
   ```

   The screenshot fixture flow also expects media tooling on `PATH`:

   - `ffmpeg`
   - `ffprobe`

3. Ensure the target entries are present in web data before capture:

   ```bash
   dayone sync
   ```

4. Create Playwright auth state (recommended once per environment):

   ```bash
   cd e2e
   npx playwright codegen https://stg.dayone.me --save-storage=.auth/staging-user.json
   ```

`npm run run:full` checks this file before running tests and exits with a clear
error if it is missing.
If screenshots redirect to `/login`, the session likely expired; regenerate the
storage state with the same command above.
The screenshot spec also attempts automatic re-login using `E2E_EMAIL` and
`E2E_PASSWORD` and then refreshes `PLAYWRIGHT_STORAGE_STATE`.

## Environment variables

Set these before running screenshots:

- `DAYONE_WEB_BASE_URL` (optional, default: `https://stg.dayone.me`)
- `PLAYWRIGHT_STORAGE_STATE` (optional, default: `.auth/staging-user.json`)
- `E2E_JOURNAL_ID` (required)

`npm run run:full` automatically loads variables from `e2e/.env` when present.
`npm run screenshots` and `npm run run:full` both discover screenshot target URLs
from `dayone list entries` automatically.
Entry routes use the web format `/journals/<journal-id>/<entry-id>`.

### Encrypted journals are required

`E2E_JOURNAL_ID` must point to an **end-to-end encrypted** journal. After the
prepare/sync step, `npm run run:full` runs `dayone list journals` and fails fast
unless the target journal has an encryption vault. This guarantees the suite
exercises the client-side crypto paths, including the client-side encryption of
original media before it is uploaded to S3. Set `E2E_ENCRYPTION_KEY` to the
journal's encryption key (already wired through `auth key-set` in
`scenarios/prepare.commands.txt`). Create an encrypted journal with
`dayone journal create --e2e` if you do not have one.

## Live entry conflict smoke test

The optional conflict smoke test mutates staging and is skipped unless explicitly
enabled. It uses three isolated CLI config dirs to simulate two clients and a
fresh verifier:

1. Client A creates and syncs an entry.
2. Client B syncs, edits the same entry, and syncs.
3. Client A edits its stale local copy and syncs.
4. The verifier syncs fresh data and asserts Client A's newer body won.

Run it with:

```bash
cd e2e
E2E_LIVE_CONFLICT=1 npm run conflict:e2e
```

Required environment variables: `E2E_JOURNAL_ID`, `E2E_EMAIL`, and
`E2E_PASSWORD`. Set `E2E_ENCRYPTION_KEY` too when the target journal requires
end-to-end encryption.

## Single-command flow (prepare + sync + screenshots)

If you want one command that runs CLI prep steps first (for example: `entry write`,
entry update, and `dayone sync`) and then captures screenshots:

1. Edit `scenarios/prepare.commands.txt` with real `dayone ...` commands.

2. Run the full flow:

   ```bash
   cd e2e
   npm run run:full
   ```

Notes:

- Commands in `prepare.commands.txt` run from the repository root.
- Image attachments use the committed fixture at `e2e/fixtures/sample-image.png`.
- You can override with `E2E_PREPARE_COMMANDS` (newline-separated commands) or
  `E2E_PREPARE_COMMANDS_FILE`.
- Use `npm run run:full -- --dry-run` to print commands without executing.

## Outputs

- PNG screenshots: `e2e/artifacts/screenshots/`
- Playwright artifacts/traces: `e2e/test-results/`

## Notes

- This suite is intentionally local-only. It uses a private test account, and its
  logs and screenshots can contain account data, so it must not run in public CI.
- If authentication expires, regenerate `PLAYWRIGHT_STORAGE_STATE`.

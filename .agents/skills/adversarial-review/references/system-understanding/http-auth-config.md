## HTTP / Auth / Config

**Files:** `src/http/**`, `src/config.rs`, `src/env_util.rs`, auth/profile/user-settings commands.

### API host precedence

Runtime precedence is:

1. `--api-host`
2. `DAYONE_API_HOST`
3. active SQLite profile
4. build default (`https://stg.dayone.me`, or production with `production-default` feature)

Do not introduce a new lookup path that bypasses this order.

### Secret loading

- At startup, `~/.config/dayone/secrets-cli` dotenv values are loaded only for variables not already set in the environment.
- Never print tokens, passwords, master keys, private keys, or raw auth responses to stdout/stderr/telemetry.

### Auth/session contract

- Bearer tokens must use the `Bearer` scheme and reject blank values.
- Session storage must remain scoped to base URL/profile so staging/production tokens do not cross.
- Auth setup/login flows need clear user-facing errors but structured command outputs where applicable.

### HTTP client contract

- Use typed methods and request builders where possible. Keep endpoint path construction explicit and tested.
- Preserve response parsing around JSON-only vs multipart/payload responses; payload-length overflow and truncated payloads should remain hard errors.
- Fake API clients should exercise the same method/path/payload contracts production code expects.

### What to watch for

- Profile/env precedence regressions.
- Silent fallback from auth failures to anonymous requests.
- Paths that concatenate unvalidated IDs or query strings by hand when URL encoding is required.
- Tests that update web parity expectations to match unreleased server behavior rather than released API behavior.

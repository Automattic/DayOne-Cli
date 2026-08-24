# user-settings

```bash
dayone user-settings <command> [flags]
```

| Subcommand | Purpose |
|------------|---------|
| `get` | Return the authenticated user's account-level settings as JSON. |

## `get`

```bash
dayone user-settings get
```

| Flag | Required | Description |
|------|----------|-------------|
| `--api-host` | no | Override endpoint for this call |

```bash
dayone user-settings get
DAYONE_API_HOST=https://dayone.me dayone user-settings get
dayone --api-host https://dayone.me user-settings get
```

Requires an active session. See [auth.md](auth.md) for `auth login`.

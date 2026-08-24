# profile

```bash
dayone profile <command> [flags]
```

| Subcommand | Purpose |
|------------|---------|
| `list` | Show saved profiles and the active profile. |
| `set <profile>` | Set the active profile (and optionally bind/update its `--api-host`). |

Built-in profiles:

- `production` → `https://dayone.me`

```bash
dayone profile list
dayone profile set production --api-host https://dayone.me
dayone profile set my-env --api-host https://my.dayone.endpoint
```

Each profile keeps its own SQLite database at `{config_dir}/profiles/{profile}/dayone.db`. Switching profiles never touches the other profile's data.

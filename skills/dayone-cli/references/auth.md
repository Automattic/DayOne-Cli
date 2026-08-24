# auth

```bash
dayone auth <command> [flags]
```

| Subcommand | Purpose |
|------------|---------|
| `login` | Authenticate with email/password and store a session token. |
| `logout` | Erase the local session (and with `--force` the entire local DB). |
| `key-set` | Store the encryption key used by sync for decrypt-enabled resources. |
| `whoami` | Show the currently authenticated user for the selected endpoint. |

See the parent [SKILL.md](../SKILL.md) for endpoint precedence and credential-safety rules.

## `login`

```bash
dayone auth login --email <email>
```

| Flag | Required | Description |
|------|----------|-------------|
| `--email` | yes | Account email |
| `--password-stdin` | no | Read password from stdin (CI/script-safe) |
| `--api-host` | no | Override endpoint for this call |

```bash
dayone auth login --email you@example.com
printf '%s' 'your-password' | dayone auth login --email you@example.com --password-stdin
dayone --api-host https://dayone.me auth login --email you@example.com
```

If the login response includes an encryption master key, the CLI stores it automatically (OS keychain when available, SQLite fallback otherwise).

## `logout`

Without `--force` the command only warns — nothing is deleted.

```bash
dayone auth logout            # warns only
dayone auth logout --force    # deletes the local SQLite DB
```

| Flag | Required | Description |
|------|----------|-------------|
| `--force` | no | Actually erase local data. Irreversible. |
| `--api-host` | no | Override endpoint for this call |

> [!CAUTION]
> `--force` is irreversible. Any entries written locally but not yet synced are permanently lost. The calling tool MUST require explicit human confirmation.

## `key-set`

```bash
dayone auth key-set [--key <value> | --key-stdin]
```

| Flag | Required | Description |
|------|----------|-------------|
| `--key` | no | Encryption key as a flag (least secure — visible in shell history) |
| `--key-stdin` | no | Read encryption key from stdin (preferred for scripts) |
| `--api-host` | no | Override endpoint for this call |

```bash
dayone auth key-set                                       # interactive prompt
printf '%s' 'your-encryption-key' | dayone auth key-set --key-stdin
dayone auth key-set --key 'your-encryption-key'           # avoid when possible
```

## `whoami`

```bash
dayone auth whoami
```

Returns the authenticated user for the active endpoint, or an error when no session exists.

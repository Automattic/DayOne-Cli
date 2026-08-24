# sync-schedule

Manage automatic background `dayone sync` for the active profile.

Recommended setup for normal desktop use:

```bash
dayone sync-schedule enable --interval-minutes 30
dayone sync-schedule status
```

| Subcommand | Description |
|------------|-------------|
| `status` | Show whether automatic sync is registered and enabled. |
| `enable --interval-minutes <N>` | Register or update automatic sync. Defaults to 30 minutes. |
| `set-interval --interval-minutes <N>` | Update the sync interval. Equivalent to enable/update. |
| `disable` | Disable and remove the schedule. |

`--interval-minutes` accepts `1..=1439`. Use about `30` minutes for normal use unless you need faster sync.

## Backend behavior

The command uses the native per-user scheduler for the operating system:

- macOS: LaunchAgent in `~/Library/LaunchAgents/`.
- Linux: systemd user service/timer in `~/.config/systemd/user/`.
- Windows: Task Scheduler task for the current user.

The normal current-user setup is designed to avoid admin/root privileges. Linux timers may only run while the user systemd manager is active unless lingering is enabled outside the CLI.

## Output shape

All subcommands print a JSON object:

```json
{
  "ok": true,
  "backend": "launchd",
  "registered": true,
  "enabled": true,
  "interval_minutes": 30,
  "command": ["/absolute/path/to/dayone", "--profile", "production", "--api-host", "https://dayone.me", "sync"],
  "path": "/Users/me/Library/LaunchAgents/com.dayone.cli.sync.plist",
  "details": null
}
```

`command` is the exact sync command the scheduler should run. `path` is backend-specific and may be `null` on Windows.

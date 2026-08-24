# journal

Create or update a journal. Both are local-first writes: they persist to the
local SQLite store and queue an outbox item — **run `dayone sync` afterward** to
push to the server.

```bash
dayone journal create [--name <text>] [--description <text>] [--color <hex>] \
                      [--e2e | --plaintext] [--shared] \
                      [--sort-method <m>] [--hide-on-this-day <bool>] [--hide-all-entries <bool>] \
                      [--conceal <bool>] [--add-location-to-new-entries <bool>] \
                      [--comments-disabled <bool>] [--template-id <id>] [--preset-id <id>] \
                      [--json <obj> | --json-file <path>]

dayone journal update --journal-id <id> [--name <text>] [--description <text>] [--color <hex>] \
                      [--sort-method <m>] [--hide-on-this-day <bool>] [--hide-all-entries <bool>] \
                      [--conceal <bool>] [--add-location-to-new-entries <bool>] \
                      [--comments-disabled <bool>] [--template-id <id>] [--preset-id <id>] \
                      [--json <obj> | --json-file <path>]
```

| Subcommand | Purpose |
|------------|---------|
| `create` | Create a new journal (plaintext or end-to-end encrypted); prints JSON result. |
| `update` | Modify an existing journal's metadata; prints JSON result. |

> [!NOTE]
> `--json`/`--json-file` accept an **allowlist** of editable metadata fields:
> `name`, `description` (`description_v2`), `color`, `sort_method`,
> `hide_on_this_day`, `hide_all_entries`, `hide_streaks`, `conceal`,
> `add_location_to_new_entries`, `comments_disabled`, `template_id`, `preset_id`.
> Anything else is **silently dropped** — so you can dump a full journal JSON in
> and only its editable fields apply. In particular the E2E `encryption` vault and
> server-managed fields (`participants`, `owner_id`, `state`, `invite_list`, …)
> are never settable; the vault is generated internally from `--e2e`. (`create`
> additionally honors `id`, `is_shared`, and the `"encryption": "plaintext"`
> opt-out marker as construction inputs.)

## `create` flags

| Flag | Required | Description |
|------|----------|-------------|
| `--name` | yes (flag or JSON) | Journal name (non-empty) |
| `--description` | no | Journal description (stored as `description_v2`) |
| `--color` | no | Hex color, e.g. `#E74C3C` |
| `--e2e` | no | Create as end-to-end encrypted (requires `dayone auth key-set` + a prior `dayone sync`) |
| `--plaintext` / `--no-e2e` | no | Force a plaintext journal |
| `--shared` | no | Create as a shared journal (requires E2E) |
| `--sort-method` | no | e.g. `entryDate` |
| `--hide-on-this-day` | no | bool |
| `--hide-all-entries` | no | bool |
| `--conceal` | no | bool |
| `--add-location-to-new-entries` | no | bool |
| `--comments-disabled` | no | bool |
| `--template-id` / `--preset-id` | no | Associate a template / preset |
| `--json` / `--json-file` | no | Extra journal fields as a JSON object — only editable fields are kept (see note above); `--e2e` controls encryption |
| `--api-host` | no | Override endpoint for this call |

When neither `--e2e` nor `--plaintext` is given, the journal is encrypted if the
active profile has an encryption key configured, otherwise plaintext.

## `update` flags

| Flag | Required | Description |
|------|----------|-------------|
| `--journal-id` | yes | Id of the journal to update |
| `--name` | no | New name (non-empty if provided) |
| `--description` | no | New description (stored as `description_v2`) |
| `--color` | no | Hex color |
| `--sort-method` | no | e.g. `entryDate` |
| `--hide-on-this-day` | no | bool |
| `--hide-all-entries` | no | bool |
| `--conceal` | no | bool |
| `--add-location-to-new-entries` | no | bool |
| `--comments-disabled` | no | bool |
| `--template-id` / `--preset-id` | no | Associate a template / preset |
| `--json` / `--json-file` | no | Editable journal fields as a JSON object — non-editable keys are dropped (see note above) |
| `--api-host` | no | Override endpoint for this call |

At least one field must be provided. Convenience flags take precedence over the
same key supplied in `--json`. `update` only changes the fields you pass; it
leaves the rest of the journal (including its `encryption` vault) untouched.

## Examples

```bash
# Plaintext journal
dayone journal create --name "Work Log" --plaintext
dayone sync

# E2E journal (needs an encryption key + prior sync)
dayone journal create --name "Private" --e2e
dayone sync

# Rename + recolor an existing journal
dayone journal update --journal-id 105442888546 --name "DH Research" --color "#E74C3C"
dayone sync

# Set multiple fields from JSON (only editable fields apply; others dropped)
dayone journal update --journal-id 105442888546 --json '{"conceal": true, "hide_on_this_day": true}'
dayone sync
```

> [!CAUTION]
> A newly created journal gets a `pending-…` id until the outbox is flushed.
> After `dayone sync` the server returns a numeric id — use that resolved id
> (from `dayone list journals`) for `journal update` and other journal-scoped
> commands.

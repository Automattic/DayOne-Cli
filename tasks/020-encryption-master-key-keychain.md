# 020 — Store the master encryption key in the system keychain

**Source:** architecture.md § Encryption — critique: master key stored in SQLite, not the system keychain  
**Size:** M  
**Depends on:** nothing (independent; the SQLite fallback path remains for non-keychain systems)

---

## Problem

The master key (stored via `dayone auth key-set`) is saved in plaintext in the `encryption_keys` SQLite table. Anyone with read access to `~/.config/dayone-cli/dayone.db` can retrieve it. The OS provides a secure credential store (macOS Keychain, Linux Secret Service / `libsecret`) that is specifically designed for this purpose — credentials are encrypted at rest and require user authentication to access.

## Goal

On macOS, store the master key in Keychain instead of SQLite. On Linux, store it in Secret Service (via `libsecret`) if available. Fall back to the SQLite table only when no secure store is accessible. Existing SQLite-stored keys should be migrated to the keychain on next `key-set` or `sync`.

## Concrete steps

1. Add the `keyring` crate to `Cargo.toml` (the `keyring` crate provides a unified interface to macOS Keychain, Linux Secret Service, and Windows Credential Manager with a single API):
   ```toml
   [dependencies]
   keyring = "2"
   ```

2. Define a keychain service name and account naming convention:
   - Service: `"dayone-cli"`
   - Account: `"master-key:<profile_id>"` or `"master-key:<base_url>"`

3. Update `src/commands/auth_key_set.rs`:
   - After validating the key, attempt to store it via `keyring::Entry::new("dayone-cli", account)?.set_password(&master_key)`.
   - If keychain storage succeeds, store a sentinel `"keychain"` value in the SQLite `encryption_keys` table to indicate the real key is in the keychain.
   - If keychain storage fails (e.g., no keychain available), fall back to storing the key directly in SQLite as before, with a log warning.

4. Update `store/encryption.rs` (or wherever `get_encryption_key_for_profile` is defined):
   - If the stored value is the sentinel `"keychain"`, retrieve the actual key from `keyring::Entry::new(...)?.get_password()`.
   - Otherwise, return the stored value directly (backward-compatible fallback).

5. Add a migration path: the first time `get_encryption_key_for_profile` is called after this change, if the stored value is a real key (not the sentinel), offer to migrate it to the keychain (or do so automatically and update the stored value to the sentinel).

6. Update `dayone auth key-set` output JSON to indicate where the key was stored (`"storage": "keychain"` or `"storage": "sqlite"`).

7. Test on macOS: run `dayone auth key-set`, verify the key appears in Keychain Access under service `"dayone-cli"`. Run `dayone sync`, verify decryption works.

8. Test the fallback: simulate keychain unavailability (e.g., build without the keychain feature or run in a Docker container) and confirm the key is stored in SQLite instead.

9. Run `cargo test --locked`.

## Notes

- The `keyring` crate compiles on all major platforms and handles the platform differences internally.
- On CI (Linux without a desktop session), the keyring crate returns an error. The fallback to SQLite ensures CI and headless deployments continue to work.
- The sentinel approach avoids removing the SQLite column (backward compatibility), but a future migration could clean up SQLite entries that have been moved to the keychain.

## Definition of done

- On macOS, `dayone auth key-set` stores the key in Keychain.
- `dayone sync` on macOS reads the key from Keychain.
- The SQLite fallback works on systems without a keychain.
- `cargo test --locked` passes.

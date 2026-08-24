# 013 — Consolidate `content_keys` and `named_crypto_keys` into a single concept

**Source:** architecture.md § The Sync Engine — critique: content_keys and named_crypto_keys fetched and stored separately  
**Size:** M  
**Depends on:** nothing (independent of other tasks, but cleaner after 010)

---

## Problem

The sync engine fetches key material from two different endpoints and stores it in two different places:

1. `content_keys` — fetched from `/v4/sync/changes/contentKeys` (a cursor-based feed), stored in the `content_keys` table.
2. `named_crypto_keys` — fetched from `/v4/sync/named/cryptoKeys` (a singleton endpoint), stored in the `user_keys` table — the same table used for the user's private key.

These two endpoints expose the same underlying key material through different API shapes. The result is duplicated fetch logic, two storage destinations, and two code paths in `crypto.rs` for retrieving what is conceptually the same thing: the user's content key pairs.

## Goal

Treat content keys as a single concept. Fetch from one canonical source (determine which endpoint is more current and reliable — likely the cursor feed for incremental updates). Store in one table. Remove the duplicate fetch and storage path.

## Concrete steps

1. Audit both endpoints against the server API to determine which one is canonical:
   - Does `/v4/sync/named/cryptoKeys` contain data not present in the cursor feed?
   - Is the cursor feed a complete representation of all content keys?
   - Determine the authoritative source.

2. If the cursor feed (`content_keys`) is canonical:
   - Remove the `named_crypto_keys` singleton fetch from the pull sequence.
   - Remove the separate upsert into `user_keys` that `named_crypto_keys` was performing.
   - Confirm that all `crypto.rs` key lookups that read from `user_keys` (for content key purposes) are updated to read from `content_keys` instead.

3. If `/v4/sync/named/cryptoKeys` is canonical (e.g., it returns more recent data than the cursor feed):
   - Replace the cursor feed fetch with the singleton endpoint.
   - Store results from the singleton in a single table.
   - Remove the cursor-based `content_keys` fetch.

4. Update `src/sync/crypto.rs::build_crypto_context` to read content keys from the single remaining source.

5. Remove whichever of `content_keys` or `user_keys` tables is no longer used (add a migration to drop it, using task 005's migration versioning).

6. Test against staging: confirm that encrypted entries are still decrypted correctly after this change, and that new entries can be encrypted.

7. Run `cargo test --locked`.

## Notes

- This is a correctness-sensitive change. The key material must be equivalent between the two sources before removing either. Validate against a test account with encrypted journals.
- If the two endpoints return genuinely different data (e.g., one is a subset of the other for backward compatibility), document this finding and keep both — but at least consolidate the storage destination.

## Definition of done

- Content keys are fetched from one endpoint and stored in one table.
- The duplicate fetch call is removed from the pull sequence.
- Encryption and decryption work correctly on staging.
- `cargo test --locked` passes.

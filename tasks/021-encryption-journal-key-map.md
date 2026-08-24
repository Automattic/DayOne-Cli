# 021 — Build an in-memory journal key map for the duration of sync

**Source:** architecture.md § Encryption — critique: re-decrypts journal keys on every operation  
**Size:** M  
**Depends on:** 013 (content keys consolidation; the key map should read from the canonical key source)

---

## Problem

The current sync implementation re-decrypts journal keys on demand during each operation that needs them. For a sync run over a journal with thousands of entries, this means the same RSA decryption (unlock vault → derive journal content key) is repeated for every entry decryption. RSA operations are expensive relative to the AES operations that follow.

Additionally, there is no persistent local table of locked (RSA-encrypted) journal keys indexed by fingerprint. When a sync run starts, the engine must build the decryptor from scratch every time by fetching and parsing the user keys payload.

## Goal

At sync start, unlock all journal content keys once, store them in a `HashMap<Fingerprint, RsaPrivateKey>` held in memory for the duration of the command. Any entry decryption or encryption operation looks up the key from the map instead of re-deriving it. Optionally, persist a table of locked (RSA-encrypted) journal keys to make the per-sync unlock step fast.

## Concrete steps

1. Define a `JournalKeyMap` struct in `src/sync/crypto.rs` (or `src/sync/keys.rs`):
   ```rust
   pub struct JournalKeyMap {
       keys: HashMap<String, RsaPrivateKey>, // fingerprint → content private key
   }

   impl JournalKeyMap {
       pub fn build(store: &Store, master_key: &str) -> anyhow::Result<Self>;
       pub fn get(&self, fingerprint: &str) -> Option<&RsaPrivateKey>;
   }
   ```

2. Implement `JournalKeyMap::build`:
   - Derive the symmetric key from the master key (PBKDF2, already done in `build_crypto_context`).
   - Decrypt the user private key using the symmetric key.
   - For each journal's vault grants, find the grant encrypted to the user's public key, decrypt it to get the vault key.
   - Use the vault key to decrypt the journal's content key pairs.
   - Store each content private key in the map, keyed by fingerprint.

3. In `run_sync`, call `JournalKeyMap::build(store, &master_key)` once at the top of the sync (after the user key is fetched but before the entry pull loop). Pass `&journal_key_map` to all decrypt/encrypt operations instead of the current `CryptoContext` or `EntriesDecryptor` structs.

4. Remove the `JournalDecryptor` and `EntriesDecryptor` structs from `engine.rs` (or demote them to implementation details inside the key map builder).

5. (Optional) Add a persistent `journal_content_keys` table to the store:
   ```sql
   CREATE TABLE journal_content_keys (
       fingerprint TEXT PRIMARY KEY,
       journal_id TEXT NOT NULL,
       locked_private_key_blob TEXT NOT NULL  -- RSA-encrypted private key, D1 format
   );
   ```
   After `JournalKeyMap::build`, persist any newly seen (fingerprint → locked key blob) pairs. On the next sync, load the locked blobs from the table instead of re-fetching from the user keys payload. This makes sync startup faster after the first run.

6. Run `cargo test --locked`. Test against staging: verify encrypted entries still decrypt correctly and new entries encrypt correctly.

## Notes

- The in-memory map is the high-value part of this task. The persistent table is an optimisation; do it only if sync startup time is measurably slow.
- The map should be cleared when the command exits (it lives on the stack, so this is automatic).

## Definition of done

- `JournalKeyMap` is built once per sync run.
- Entry decryption and encryption use the map for key lookups.
- No per-entry RSA decryption occurs inside the entry pull loop.
- `cargo test --locked` passes and staging sync works correctly.

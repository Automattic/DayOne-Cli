## Crypto / E2EE / Key Storage

**Files:** `src/sync/crypto.rs`, `src/store/encryption.rs`, auth/journal creation code that handles key material.

### Key hierarchy

- Master Key → unlocks User Key.
- User Key → unlocks Content Keys and Vault Key.
- Vault Key → unlocks Journal Key.
- Journal Key → unlocks Entry Key / Media Key.
- Content keys are NOT entry keys. Confusing them causes encryption/decryption failures.
- Entry keys and media keys are ephemeral. Each write generates fresh keys, locks them with the journal key, and discards the plaintext keys after use.

### D1 blob and signing invariants

- Encrypted data must be D1 blobs starting with bytes `0x44 0x31` (`D1` magic). Base64-encoded D1 blobs commonly start with `RD`.
- Fingerprints must be lowercased before comparison.
- User-key signatures sign a nonce with the user's private key.
- Asymmetric content-key signatures sign the `encrypted_key` with the **user's** private key, not the content key's private key.
- RSA-SHA256, SPKI/PEM format, 2048-bit modulus. Public key is required for asymmetric keys.

### Local storage model

- Local SQLite stores decrypted user content so list/search/TUI can work local-first. Encryption happens at the sync/API boundary before upload for E2E journals.
- The CLI stores its local database under the selected profile and uses a keychain-backed encryption key with SQLite fallback behavior. Changes to fallback, migration, or sentinel handling can strand existing users.
- Keychain errors should remain typed enough that auth/setup can distinguish missing keychain state from unrelated storage failures.

### Entry sync structure

Each entry push has two conceptual parts:

1. **Envelope** — metadata the server can read: edit dates, entry dates, feature flags, media metadata, md5s of encrypted media when applicable.
2. **Blob/content** — entry body, timezone, location, tags, and media references. For E2E journals this is encrypted before upload; for plaintext journals it is plain JSON.

Moment md5s have two meanings:

- Inside decrypted content: md5 of original media bytes.
- On the server envelope: md5 of uploaded/encrypted bytes. For E2E journals these can differ.

### Multiple user keys and content keys

- Accounts may have multiple user keys and content keys. Decryption should select by matching fingerprint, not by assuming the latest key is active.
- If multiple user keys exist, prefer the API-indicated key where available, but keep fallback matching robust.

### What to watch for

- Key unlock ordering changes.
- Fingerprint comparisons without lowercase normalization.
- Logging or telemetry of raw keys, shared message keys, decrypted content, ciphertext blobs, bearer tokens, or emails.
- Broad content-key error detection that can match its own refetch/failure path and loop forever.
- Changes that make plaintext/E2E journal creation defaults diverge from profile key state and explicit flags.

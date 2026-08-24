# 022 — Canonicalize the user keys payload into an internal struct at parse time

**Source:** architecture.md § Encryption — critique: dual-path key extraction with fragile pattern matching  
**Size:** M  
**Depends on:** nothing (independent; improves crypto.rs regardless of other changes)

---

## Problem

`extract_user_encrypted_private_key` in `crypto.rs` attempts to find the encrypted private key by trying seven different field paths in sequence:

- `encryptedPrivateKey`
- `encrypted_private_key`
- `userKey.encryptedPrivateKey`
- The key inside a base64-decoded `blob` field
- … and more

This is an accumulation of workarounds for different API versions and response shapes. Each workaround was added when a specific client or server version changed the field name. The result is fragile pattern matching that is hard to test, hard to extend, and silently ignores unexpected structures.

The deeper problem is treating the raw API JSON as the canonical form everywhere downstream. Any code that needs a field from the user keys payload reaches into the raw `Value` and applies its own field-path logic.

## Goal

Parse the user keys API response into a single internal `UserKeysPayload` struct as soon as it arrives from the server (at upsert time or at the start of sync). All downstream code reads from the typed struct, not the raw JSON. Field path ambiguities are resolved once, at parse time, with explicit fallback logic.

## Concrete steps

1. Define `UserKeysPayload` in `src/sync/crypto.rs` (or `src/models/crypto.rs`):
   ```rust
   #[derive(Debug, Deserialize)]
   pub struct UserKeysPayload {
       pub encrypted_private_key: String,  // resolved from all known field paths
       pub public_key: Option<String>,
       // add other fields as needed
   }

   impl UserKeysPayload {
       pub fn from_raw(raw: &serde_json::Value) -> anyhow::Result<Self> {
           // All seven field-path attempts live here and nowhere else
           let encrypted_private_key = Self::extract_encrypted_private_key(raw)
               .ok_or_else(|| anyhow::anyhow!("user keys payload missing encrypted private key"))?;
           Ok(Self { encrypted_private_key, ... })
       }

       fn extract_encrypted_private_key(raw: &Value) -> Option<String> {
           // try encryptedPrivateKey
           // try encrypted_private_key
           // try userKey.encryptedPrivateKey
           // try blob (base64-decoded)
           // return first that succeeds
       }
   }
   ```

2. In `build_crypto_context` (and everywhere else that reads from the user keys `Value`), call `UserKeysPayload::from_raw(&raw)` first. Use the typed fields for all subsequent operations.

3. Remove all scattered `raw["fieldName"].as_str()` accesses on the user keys payload from outside `UserKeysPayload::from_raw`.

4. Write tests for `UserKeysPayload::from_raw` using fixture JSON blobs representing each of the known API response shapes. There should be one fixture per known format variant.

5. Run `cargo test --locked`.

## Notes

- The seven field-path attempts do not need to disappear — they need to be *consolidated* into one place (`from_raw`) rather than scattered across functions that each reimplement the same search.
- This change surfaces format changes at parse time rather than at decrypt time, making format regressions detectable earlier.

## Definition of done

- `UserKeysPayload` struct exists with `from_raw` constructor.
- All known field path variants are handled in `from_raw`.
- No other function in `crypto.rs` or `engine.rs` reads raw field paths from the user keys payload.
- Tests cover each known payload shape.
- `cargo test --locked` passes.

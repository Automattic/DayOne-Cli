# 023 — Remove unused `has_user_key` / `has_content_keys` parameters from `build_crypto_context`

**Source:** architecture.md § Encryption — critique: has_user_key and has_content_keys parameters are unused  
**Size:** XS  
**Depends on:** nothing (trivial cleanup, safe to do any time)

---

## Problem

`build_crypto_context` in `src/sync/crypto.rs` accepts `has_user_key: bool` and `has_content_keys: bool` as parameters. The function body immediately discards them with `let _ = (has_user_key, has_content_keys)`. These were presumably intended as short-circuit guards to skip key loading when keys are not present, but they were never wired up.

The parameters add noise to every call site without providing any benefit.

## Goal

Either use the parameters as intended or remove them. Given that the function currently works correctly without them, the simpler fix is removal.

## Concrete steps

1. Open `src/sync/crypto.rs` and locate `build_crypto_context`.

2. Remove the `has_user_key: bool` and `has_content_keys: bool` parameters from the function signature.

3. Delete the `let _ = (has_user_key, has_content_keys);` line.

4. Update all call sites of `build_crypto_context` to remove the corresponding arguments. Grep for `build_crypto_context` to find all usages.

5. Run `cargo build` and `cargo test --locked` to confirm there are no other references.

## Alternative (if the parameters were intended to be used)

If the intention was to short-circuit key loading when keys are not present, implement that:
```rust
if !has_user_key {
    return Ok(CryptoContext::empty());
}
```
Only do this if there is a real call site that passes `false` and expects an empty context. Otherwise, remove the parameters.

## Definition of done

- `build_crypto_context` no longer has `has_user_key` or `has_content_keys` parameters.
- All call sites are updated.
- `cargo test --locked` passes.

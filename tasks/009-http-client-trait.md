# 009 — Unify HTTP clients behind a shared trait

**Source:** code-structure.md § No Trait Boundaries — HTTP clients  
**Size:** S  
**Depends on:** nothing (independent)

---

## Problem

There are two HTTP client types in the codebase:
- `DayOneClient` in `src/http/client.rs` — used by auth and other non-sync commands
- `SyncApiClient` in `src/sync/api.rs` — used by the sync engine

Both have nearly identical auth header construction (`bearer_headers`, `bearer_web_headers`) and error handling (`ensure_success_status`). Neither is abstract — they are concrete structs with no trait, making it impossible to inject a fake HTTP client in tests. The duplication grows with every new endpoint that needs custom status handling.

## Goal

Define a shared `DayOneApiClient` trait that both concrete types implement. Extract the duplicated auth header and status-handling logic into shared free functions. Create a `FakeApiClient` for use in unit tests.

## Concrete steps

1. Define the trait in `src/http/mod.rs`:
   ```rust
   pub trait DayOneApiClient: Send + Sync {
       async fn get_json(&self, path: &str, query: &[(&str, &str)]) -> anyhow::Result<serde_json::Value>;
       async fn get_json_allow_304(&self, path: &str, query: &[(&str, &str)]) -> anyhow::Result<Option<serde_json::Value>>;
       async fn put_json(&self, path: &str, body: &serde_json::Value) -> anyhow::Result<serde_json::Value>;
       async fn put_json_allow_409(&self, path: &str, body: &serde_json::Value) -> anyhow::Result<Option<serde_json::Value>>;
       async fn post_json(&self, path: &str, body: &serde_json::Value) -> anyhow::Result<serde_json::Value>;
   }
   ```
   Add other methods (`put_entry_multipart`, `put_file_to_absolute_url`, `get_bytes`) as needed.

2. Extract shared helpers into `src/http/mod.rs` as free functions:
   ```rust
   pub fn bearer_auth_header(token: &str) -> (HeaderName, HeaderValue) { ... }
   pub fn ensure_success(resp: &Response) -> anyhow::Result<()> { ... }
   ```

3. Implement `DayOneApiClient` on both `DayOneClient` and `SyncApiClient`. Remove the duplicated helper code from each.

4. Update `sync/engine.rs` to accept `impl DayOneApiClient` instead of `SyncApiClient` directly.

5. Create `src/http/fake.rs` (test-only):
   ```rust
   #[cfg(test)]
   pub struct FakeApiClient {
       responses: HashMap<String, serde_json::Value>,
   }

   #[cfg(test)]
   impl DayOneApiClient for FakeApiClient {
       async fn get_json(&self, path: &str, _query: &[(&str, &str)]) -> anyhow::Result<serde_json::Value> {
           self.responses.get(path).cloned().ok_or_else(|| anyhow::anyhow!("no fixture for {path}"))
       }
       // ...
   }
   ```

6. Run `cargo test --locked`.

## Notes

- `async` in traits requires `async-trait` or Rust 1.75+ RPITIT. Prefer the native feature if the project's MSRV supports it; otherwise add `async-trait` as a dev/test dependency.

## Definition of done

- `DayOneApiClient` trait exists in `src/http/mod.rs`.
- Both `DayOneClient` and `SyncApiClient` implement it.
- Duplicated header/error helpers are consolidated.
- `sync/engine.rs` uses the trait, not the concrete `SyncApiClient`.
- `FakeApiClient` exists for test use.
- `cargo test --locked` passes.

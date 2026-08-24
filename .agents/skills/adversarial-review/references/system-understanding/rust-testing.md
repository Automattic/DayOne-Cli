## Rust Testing

- `cargo test --locked` is the main validation suite. Tests are self-contained and should not require network access.
- Prefer temp SQLite stores and fake API clients. Never use or mutate the user's real Day One config/store in tests.
- Unit tests are appropriate for pure parsing, merge/conflict logic, payload construction, rich-text conversion, and store helpers.
- CLI/integration tests are appropriate when clap parsing, env/profile precedence, stdout/stderr, exit status, or binary behavior matters.
- Property/fuzz-style tests exist for JSON round-trips and rich text storage; preserve their invariants when modifying serialization.

### What to watch for

- Tests that would pass if the production function returned any non-error value.
- Tests that assert on inputs/mocks rather than persisted rows, output JSON, or request payloads.
- Missing negative tests for mutually exclusive flags, invalid IDs, malformed cursors, conflict responses, and auth/session absence.
- Leaked env vars or global state between tests.

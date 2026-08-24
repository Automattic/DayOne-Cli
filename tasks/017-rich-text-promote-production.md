# 017 — Promote `rich_text.rs` from `#[cfg(test)]` to production use

**Source:** code-structure.md § Untyped Domain Models; architecture.md § The Date and Entry Model — critique: rich text is partially a test-only abstraction  
**Size:** S  
**Depends on:** 016 (entry domain extraction; rich_text should live in src/entry/ after that refactor)

---

## Problem

`src/rich_text.rs` is entirely gated behind `#[cfg(test)]`. The `RichTextDocument`, `RichTextNode` types, and the `build_from_body()` builder function only compile in test mode. They are never used in production.

Meanwhile, `entry_write.rs` constructs rich text for production writes using raw `serde_json::json!` macros inline. The test helpers in `rich_text.rs` and the production code in `entry_write.rs` are supposed to test the same format — but they are separate implementations that can silently diverge.

## Goal

Remove the `#[cfg(test)]` gate from `src/rich_text.rs` (or from `src/entry/rich_text.rs` after task 016). Use the `build_from_body()` builder in production entry writes. Delete the inline `serde_json::json!` rich text construction from `entry_write.rs`.

## Concrete steps

1. If task 016 has landed: `src/rich_text.rs` is now `src/entry/rich_text.rs`. Remove the `#[cfg(test)]` gates from the type definitions and the `build_from_body` function. Keep the test helper constructors (e.g., `RichTextNode::text()`, `RichTextNode::embedded()`) available in tests.

2. If task 016 has not landed yet: remove the `#[cfg(test)]` gate from `src/rich_text.rs` directly. The module will be moved as part of task 016.

3. In `entry_write.rs` (or `entry/body.rs` after task 016), replace the inline `serde_json::json!` rich text construction with calls to `build_from_body()` and the typed builder API.

4. Verify that the rich text JSON output for a plain-text entry body matches what the old inline construction produced. A snapshot test comparing the two outputs is sufficient.

5. Run `cargo test --locked`. All existing rich text tests should still pass because they are testing the same builder; they now also run in non-test builds (which is fine).

## Notes

- The reason `rich_text.rs` was `#[cfg(test)]` in the first place may be that it was written as a test helper first and never promoted. This task corrects that.
- After this task, the rich text builder is the single source of truth for entry body construction in both test and production paths.

## Definition of done

- `#[cfg(test)]` is removed from `RichTextDocument`, `RichTextNode`, and `build_from_body`.
- Production entry writes use `build_from_body()`.
- Inline `serde_json::json!` rich text construction is removed from `entry_write.rs`.
- `cargo test --locked` passes.

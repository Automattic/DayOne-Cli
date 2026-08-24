# 016 — Extract entry domain logic from `entry_write.rs` into `src/entry/`

**Source:** code-structure.md § Functions Too Large to Reason About — entry_write.rs; § The Gravity Wells  
**Size:** M  
**Depends on:** 006 (typed domain models), 007 (newtype IDs)

---

## Problem

`src/commands/entry_write.rs` is 1,441 lines. The `execute()` function is the nominal entry point, but the file contains substantial domain logic that has nothing to do with CLI argument parsing:
- Attachment staging and validation
- `dayone-moment://` placeholder parsing and resolution
- Rich text document construction
- Entry JSON assembly
- Outbox enqueueing for media uploads

These are domain concepts that other commands (or future write paths) would want to reuse. Instead they are locked inside a command module with no way to call them without wiring through the full CLI argument stack.

## Goal

Create a `src/entry/` module containing the domain logic for building and writing entries. `entry_write.rs` becomes a thin adapter: parse CLI args, call into `entry::`, print output.

## Concrete steps

1. Create the `src/entry/` module with three sub-modules:

   **`src/entry/attachments.rs`** — move out:
   - `StagedAttachment` struct
   - `stage_attachment()` function (resolves file path, reads MIME, computes MD5, determines dimensions/duration)
   - `PlaceholderMomentType` enum and placeholder parsing logic
   - The attachment-to-placeholder binding logic (resolve unbound placeholders → attachment files in order)

   **`src/entry/body.rs`** — move out:
   - `assemble_entry_json()` or equivalent (builds the full entry JSON object from typed inputs)
   - The `richTextJSON` construction using structured inputs (currently done with inline `serde_json::json!` macros)
   - Moment array assembly from `StagedAttachment` list

   **`src/entry/rich_text.rs`** — promote `src/rich_text.rs` from `#[cfg(test)]` to production (see also task 017):
   - `RichTextDocument`, `RichTextNode` types
   - `build_from_body()` builder function
   - This is the production implementation; the test scaffolding stays in tests

2. Update `src/commands/entry_write.rs::execute()` to:
   - Call `entry::attachments::stage_attachments(...)` instead of inline attachment logic
   - Call `entry::body::assemble_entry_json(...)` instead of inline JSON assembly
   - Call `entry::rich_text::build_from_body(...)` instead of inline `serde_json::json!` macros
   - Remain focused on: arg translation, calling domain functions, calling store upsert, enqueueing outbox, returning output

3. The resulting `execute()` should be under 80 lines.

4. Add unit tests in `src/entry/body.rs` and `src/entry/attachments.rs` for the extracted logic. These tests should not require a database — they test pure transformation functions.

5. Run `cargo test --locked`.

## Notes

- `AttachmentType` and `PlaceholderMomentType` enums are near-identical (see also task 020). They can be collapsed as part of this extraction.
- The `StagedAttachment` type currently uses `String` for its `moment_id` field. After task 007, this should become `MomentId`.
- The `entry/rich_text.rs` promotion may be done as part of this task or as the dedicated task 017 — coordinate to avoid merge conflicts.

## Definition of done

- `src/entry/` module exists with `attachments.rs`, `body.rs`, `rich_text.rs`.
- `entry_write.rs` is under 120 lines.
- Extracted logic has unit tests that pass without a database.
- `cargo test --locked` passes.

# 018 — Build a first-class RTJson ↔ Markdown conversion library

**Source:** architecture.md § The Date and Entry Model — critique: no robust RTJson ↔ Markdown conversion library  
**Size:** XL  
**Depends on:** 017 (rich_text.rs must be promoted to production before this library builds on top of it)

---

## Problem

Conversion between Day One's `richTextJSON` format and Markdown is currently ad hoc and undertested. The conversion logic handles cases where the two formats have different expressive power — inline formatting, embedded objects, nested structure — but each case lacks an explicit test with real content as the input.

This matters because:
- The CLI reads and writes both formats. Incorrect conversion silently corrupts entry content.
- The test suite uses synthetic constructs rather than real RTJson documents from the web app.
- There is no round-trip test ensuring Markdown → RTJson → Markdown produces the same output.

## Goal

Build a TDD'd, comprehensively tested RTJson ↔ Markdown conversion library. It should live either as a standalone module in this repo (`src/entry/convert.rs`) or as a separate Rust crate (`dayone-rtjson`) that can be published independently.

## Concrete steps

### Phase 1: test corpus (do this first)

1. Collect at least 20 real RTJson documents from the Day One web app covering:
   - Plain text with multiple paragraphs
   - Headings (h1, h2, h3)
   - Bold, italic, bold-italic inline formatting
   - Unordered and ordered lists (single-level and nested)
   - Blockquotes
   - Code blocks and inline code
   - Hyperlinks
   - Embedded images (`dayone-moment://...` placeholders)
   - Embedded video, audio, PDF
   - A long entry (1000+ words) to test multi-paragraph fidelity
   - An entry with mixed formatting (bold inside a list, etc.)

2. For each document, manually write the expected Markdown output. Save both as fixture files in `tests/fixtures/rtjson/` and `tests/fixtures/markdown/`.

### Phase 2: RTJson → Markdown

3. Create `src/entry/convert.rs` (or the standalone crate). Define:
   ```rust
   pub fn rtjson_to_markdown(doc: &RichTextDocument) -> String;
   ```

4. Write a test for each fixture pair *before* implementing the conversion:
   ```rust
   #[test]
   fn rtjson_to_markdown_heading() {
       let doc = load_fixture_rtjson("heading.json");
       let expected = load_fixture_md("heading.md");
       assert_eq!(rtjson_to_markdown(&doc), expected);
   }
   ```

5. Implement `rtjson_to_markdown` to make each test pass in turn.

### Phase 3: Markdown → RTJson

6. Define:
   ```rust
   pub fn markdown_to_rtjson(markdown: &str) -> RichTextDocument;
   ```

7. Write round-trip tests: `rtjson_to_markdown(markdown_to_rtjson(md)) == md` for the Markdown fixtures.

8. Write lossless-round-trip tests for RTJson: `markdown_to_rtjson(rtjson_to_markdown(doc))` should produce a document that renders identically, even if the internal JSON differs.

### Phase 4: integration

9. Update `entry_write.rs` to use `markdown_to_rtjson` when building entries from a Markdown body.

10. If this library becomes a separate crate, publish it to crates.io and reference it in `docs/architecture.md` alongside the D1 format library.

## Notes

- **XL estimate is intentional.** The value of this library comes from the test corpus breadth, not the implementation complexity. Phase 1 (collecting and writing fixtures) is the majority of the work.
- Inline formatting (bold, italic) is where most edge cases live. Markdown has `**bold**` and `_italic_`; RTJson has attribute objects. Nesting (`**_bold italic_**`) requires careful handling.
- Embedded objects (images, video) have no standard Markdown representation. Define a convention (e.g., `![](dayone-moment://ID)`) and test it explicitly.
- The conversion does not need to be perfectly lossless for all RTJson documents (the web app may produce constructs the CLI does not generate), but it must be lossless for documents the CLI creates.

## Definition of done

- 20+ fixture pairs exist in `tests/fixtures/`.
- `rtjson_to_markdown` passes all fixture tests.
- `markdown_to_rtjson` produces valid RTJson that round-trips back to equivalent Markdown.
- `entry_write.rs` uses the conversion library.
- `cargo test --locked` passes.

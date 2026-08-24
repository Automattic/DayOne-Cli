# RTJson ↔ Markdown Conversion Library — Design Spec

**Date:** 2026-04-01
**Linear:** DAYONE-704
**Branch:** feat/DAYONE-704-rtjson-markdown-library
**Status:** Approved, ready for implementation

---

## Problem

Conversion between Day One's `richTextJSON` format and Markdown is currently ad hoc and undertested. The logic lives inline in `entry_write.rs` (2700+ lines), handles only headings in the Markdown→RTJson direction, has no RTJson→Markdown direction at all, and has no fixture-based tests using real content.

This matters because:
- The CLI reads and writes both formats. Incorrect conversion silently corrupts entry content.
- Tests use synthetic constructs, not real RTJson from the web app.
- There is no round-trip test ensuring Markdown → RTJson → Markdown produces equivalent output.

---

## Goal

Build a TDD'd, comprehensively tested `src/convert/` module providing:

```rust
pub fn markdown_to_rtjson(markdown: &str) -> ConversionResult;
pub fn rtjson_to_markdown(doc: &RtjDocument) -> String;
```

Backed by 21 real RTJson fixture files pulled from staging.

---

## Module Structure

```
src/convert/
  mod.rs           — public API
  model.rs         — RtjDocument, RtjNode, Line, Span + serde impls
  to_markdown.rs   — RtjDocument → Vec<Line> → String
  from_markdown.rs — &str (comrak events) → Vec<Line> → RtjDocument

tests/fixtures/
  rtjson/          — 01.json … 21.json  (real RTJson from staging)
  markdown/        — 01.md  … 21.md     (expected Markdown, hand-authored)
```

`src/rich_text.rs` (currently `#[cfg(test)]`-only) is deleted — replaced by production types in `model.rs`. The ad hoc functions `append_markdown_text_nodes` and `parse_markdown_heading_line` in `entry_write.rs` are removed.

---

## Core Types (`model.rs`)

```rust
pub struct RtjDocument {
    pub meta: Option<RtjMeta>,
    pub contents: Vec<RtjNode>,   // "contents" matches real API; accepts "nodes" alias on deserialize
}

pub struct RtjMeta {
    pub version: Option<u32>,
    pub created: Option<RtjMetaCreated>,
    // extra fields preserved via serde flatten
}

pub enum RtjNode {
    Text(RtjTextNode),
    Embedded(RtjEmbeddedNode),
}

pub struct RtjTextNode {
    pub text: String,
    pub attributes: Option<RtjTextAttrs>,
}

pub struct RtjTextAttrs {
    pub bold: bool,
    pub italic: bool,
    pub strikethrough: bool,
    pub inline_code: bool,
    pub highlight: bool,          // highlightedColor in RTJson
    pub link_url: Option<String>,
    pub autolink: bool,
    pub line: Option<RtjLineAttrs>,
    pub cursor_placement: bool,   // preserved on round-trip, not converted to Markdown
    // unknown attributes preserved via serde flatten extra: Map<String, Value>
}

pub struct RtjLineAttrs {
    pub header: Option<u8>,              // 1–6
    pub list_style: Option<ListStyle>,   // bulleted | numbered | checkbox
    pub indent_level: u32,
    pub checked: Option<bool>,
    pub list_index: Option<u32>,
    pub quote: bool,
    pub code_block: bool,
}

pub enum ListStyle { Bulleted, Numbered, Checkbox }

// The intermediate conversion type — the seam between both directions
pub struct Line {
    pub line_attrs: RtjLineAttrs,
    pub spans: Vec<Span>,
}

pub struct Span {
    pub text: String,
    pub bold: bool,
    pub italic: bool,
    pub strikethrough: bool,
    pub inline_code: bool,
    pub highlight: bool,
    pub link_url: Option<String>,
    pub autolink: bool,
}

// Return type of markdown_to_rtjson
pub struct ConversionResult {
    pub document: RtjDocument,
    pub has_tables: bool,   // true when any table node was produced
}
```

### `highlightedColor` default

When `==text==` Markdown is converted to RTJson, `highlightedColor` is set to Day One's default yellow. The exact hex value (`ffc107cc`) is confirmed from fixture 08 during implementation and stored as `convert::DEFAULT_HIGHLIGHT_COLOR`.

### Unknown attributes

`RtjTextAttrs` preserves unknown keys via a `#[serde(flatten)] extra: serde_json::Map<String, Value>` field. This ensures documents with `pageLink`, future attributes, or any other unknown keys survive a parse→serialize round-trip without data loss, even though those attributes are not represented in `Span` and are dropped during Markdown conversion.

---

## Intermediate: Line Model

A `Line` represents one logical line of RTJson output:
- `line_attrs` — the block-level context (header level, list style, etc.)
- `spans` — the inline content (text + formatting)

Both conversion directions convert through `Vec<Line>`:

```
RtjDocument  →  Vec<Line>  →  Markdown string
Markdown     →  Vec<Line>  →  RtjDocument
```

This makes each stage independently testable and makes round-trip fidelity explicit: a round-trip only needs to preserve `Lines`, not exact RTJson node boundaries.

---

## Conversion Rules

### RTJson → Markdown

| RTJson | Markdown |
|--------|----------|
| `header: 1–6` | `# ` … `###### ` prefix |
| `listStyle: bulleted` | `- ` (2 spaces per `indentLevel`) |
| `listStyle: numbered` | `1. ` (using `listIndex` if present) |
| `listStyle: checkbox` | `- [ ] ` / `- [x] ` |
| `quote: true` | `> ` prefix |
| `codeBlock: true` | fenced ` ``` ` block |
| `bold` | `**text**` |
| `italic` | `_text_` |
| `bold + italic` | `**_text_**` |
| `strikethrough` | `~~text~~` |
| `inlineCode` | `` `text` `` |
| `highlightedColor` | `==text==` (color lost) |
| `linkURL` | `[text](url)` |
| `autolink` | `<url>` |
| `photo / video / audio / pdfAttachment` | `![](dayone-moment://IDENTIFIER)` |
| `externalVideo / externalAudio` | `![](dayone-external:/video/<base64url>)` / `![](dayone-external:/audio/<base64url>)` |
| `horizontalLineRule` | `---` |
| `table` | use `markdown` field if present; otherwise reconstruct GFM from `rows` |
| `renderableCodeBlock` | fenced ` ``` ` block around `contents` |
| Unknown embedded types | omitted |
| `cursorPlacement`, `pageLink` | omitted (no Markdown representation) |

### Markdown → RTJson

Uses **comrak** with GFM extensions enabled (`strikethrough`, `highlight`, `table`).

comrak AST events map to `Vec<Line>` which serializes to RTJson nodes. Line attributes are applied to all nodes within the line (spec-compliant). Tables produce an embedded `table` node with:
- `rows` built from comrak's table AST
- `markdown` field populated with canonical/reconstructed GFM generated from the parsed table AST (the original source substring/formatting is not preserved)

The `has_tables` flag on `ConversionResult` is set to `true` when any table node is produced, signalling to `entry_write.rs` to set feature flag `0x20000`.

---

## Dependency

Add to `Cargo.toml`:

```toml
comrak = "0.x"
```

comrak chosen over pulldown-cmark because it provides first-class `==highlight==` support and GFM table parsing without pre-processing hacks. Both crates are actively maintained (comrak: 18 open issues, committed 2026-03-31; pulldown-cmark: 79 open issues, committed 2026-03-22).

---

## Test Strategy

### Fixture pipeline

21 real RTJson fixtures in `tests/fixtures/rtjson/` were pulled from the Day One staging environment using the CLI (`sync` + `list entries`). Corresponding Markdown fixtures in `tests/fixtures/markdown/` are hand-authored during Phase 1 of implementation, one per RTJson file.

### Fixture coverage

| # | Content |
|---|---------|
| 01 | Plain text, single paragraph |
| 02 | Multiple paragraphs |
| 03–05 | Headings h1, h2, h3 |
| 06 | Bold, italic, bold-italic, strikethrough |
| 07 | Inline code + code block |
| 08 | Highlight (`==text==`) |
| 09 | Hyperlink + autolink |
| 10 | Bulleted list (single-level) |
| 11 | Bulleted list (nested, `indentLevel`) |
| 12 | Numbered list |
| 13 | Checkbox list (checked + unchecked) |
| 14 | Blockquote |
| 15 | Embedded photo |
| 16 | Embedded video + audio |
| 17 | Embedded PDF |
| 18 | External video (YouTube, `preview` node) |
| 19 | Horizontal rule |
| 20 | Table (with header row) |
| 21 | Mixed: bold inside a list, link in a blockquote |

### Test structure

```rust
// Fixture-driven: one test per pair
#[test]
fn rtjson_to_markdown_01_plain_text() {
    let doc = load_fixture_rtjson("01.json");
    let expected = load_fixture_md("01.md");
    assert_eq!(rtjson_to_markdown(&doc), expected);
}

// Round-trip: Markdown → RTJson → Markdown must equal original
#[test]
fn roundtrip_markdown_06_inline_formatting() {
    let md = load_fixture_md("06.md");
    let result = markdown_to_rtjson(&md);
    assert_eq!(rtjson_to_markdown(&result.document), md);
}

// RTJson structural round-trip: rtjson → markdown → rtjson renders identically
#[test]
fn roundtrip_rtjson_15_photo() {
    let doc = load_fixture_rtjson("15.json");
    let md = rtjson_to_markdown(&doc);
    let result = markdown_to_rtjson(&md);
    assert_eq!(rtjson_to_markdown(&result.document), md);
}
```

---

## Integration with `entry_write.rs`

The conversion library replaces:
- `append_markdown_text_nodes` — deleted
- `parse_markdown_heading_line` — deleted

Call site becomes:

```rust
use crate::convert::markdown_to_rtjson;

// 1. Resolve dayone-moment:// placeholders (unchanged)
let body = resolve_body_dayone_moment_placeholders(&body, new_moments);

// 2. Convert Markdown → RTJson
let result = markdown_to_rtjson(&body);

// 3. Inject remaining new_moments as embedded nodes (unchanged)
// 4. Set feature flag 0x20000 if result.has_tables
```

Placeholder resolution happens before conversion. `markdown_to_rtjson` receives a body with identifiers already in place.

---

## Implementation Phases

### Phase 1 — Markdown fixture files
Author `tests/fixtures/markdown/01.md` … `21.md` by hand, one per RTJson fixture. This is the majority of the design work — getting the expected output right for each edge case.

### Phase 2 — RTJson → Markdown (TDD)
Write a failing test per fixture pair, then implement `rtjson_to_markdown` to make each pass in turn.

### Phase 3 — Markdown → RTJson (TDD)
Write round-trip tests, then implement `markdown_to_rtjson`.

### Phase 4 — Integration
Update `entry_write.rs`. Delete `rich_text.rs`. Ensure `cargo test --locked` passes.

---

## Definition of Done

- 21 fixture pairs in `tests/fixtures/`
- `rtjson_to_markdown` passes all fixture tests
- `markdown_to_rtjson` produces valid RTJson that round-trips back to equivalent Markdown
- `entry_write.rs` uses the conversion library; ad hoc functions removed
- `cargo test --locked` passes

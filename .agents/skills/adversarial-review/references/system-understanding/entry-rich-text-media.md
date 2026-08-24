## Entry / Rich Text / Media

**Files:** `src/entry/**`, `src/convert/**`, attachment handling, entry write/delete commands.

### Rich text and markdown conversion

- Day One rich text JSON must preserve formatted spans, embedded nodes, line attributes, tables, and unknown node fields where possible.
- Append-only updates should preserve existing rich text and embedded nodes instead of rebuilding stale placeholders. Wholesale rewrites may rebuild content, but must not duplicate media embeds.
- Markdown round-trips intentionally canonicalize some constructs; tests should assert stable semantics, not incidental whitespace unless promised.

### Entry content construction

- Existing payload fields, moments, tags, timezone/location, dates, and unknown data should survive local edits unless the command explicitly replaces them.
- All-day entries use local-midnight shapes; RFC3339 instants and epoch milliseconds must not be confused.
- Date conversion for pre-epoch instants needs floor division semantics, not truncation toward zero.

### Media and attachments

- Attachment staging infers type/content type, thumbnails, dimensions, and md5 metadata used later by sync.
- Placeholder resolution must not consume an attachment when the placeholder type/id mismatches; otherwise later placeholders can bind to the wrong file.
- Original media sync waits until the entry has been associated remotely.

### Deletion

- Entry delete sets tombstones and enqueues sync work. It must reject journal/entry mismatches and preserve enough payload shape for sync.
- Deleting an entry should cancel pending original-media outbox items for that entry without accidentally matching other entry IDs.

### What to watch for

- Rebuilds that drop unknown rich-text fields or embedded nodes.
- Media md5/content-type/dimension regressions.
- Attachment queues that race ahead of entry creation.
- Tests that assert only that output exists, not that the round-trip content is preserved.

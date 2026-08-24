# 019 — Collapse `AttachmentType` and `PlaceholderMomentType` into a single enum

**Source:** architecture.md § The Date and Entry Model — critique: duplicate AttachmentType / PlaceholderMomentType enums  
**Size:** S  
**Depends on:** 016 (entry domain extraction; both enums will live in src/entry/ after that refactor)

---

## Problem

`entry_write.rs` defines two enums with nearly identical variants and a conversion path between them:

- `AttachmentType` — used for the attachment metadata sent in the unencrypted outer descriptor to the server (`image`, `video`, `audio`, `pdfAttachment`)
- `PlaceholderMomentType` — used for the `dayone-moment://` placeholder syntax in the entry body

The conversion between them is manual and brittle. Any new media type must be added to both enums plus the conversion. The two concepts are actually the same: the type of a media item, expressed in slightly different string representations.

## Goal

Merge the two enums into one. Use a single `MediaType` (or `MomentType`) enum throughout the entry write path. Use `Display` or a dedicated method for the different string representations (API format vs. placeholder path segment).

## Concrete steps

1. Define a single `MediaType` enum (in `src/entry/attachments.rs` after task 016, or in `entry_write.rs` before it):
   ```rust
   #[derive(Debug, Clone, Copy, PartialEq, Eq)]
   pub enum MediaType {
       Image,
       Video,
       Audio,
       PdfAttachment,
   }

   impl MediaType {
       /// String used in the server API payload (`type` field in attachment descriptor).
       pub fn api_type_str(&self) -> &'static str {
           match self {
               Self::Image        => "image",
               Self::Video        => "video",
               Self::Audio        => "audio",
               Self::PdfAttachment => "pdfAttachment",
           }
       }

       /// Path segment used in `dayone-moment://` placeholder URLs.
       /// Images use a special case (no segment).
       pub fn placeholder_path_segment(&self) -> Option<&'static str> {
           match self {
               Self::Image         => None,   // images use `dayone-moment://ID` directly
               Self::Video         => Some("video"),
               Self::Audio         => Some("audio"),
               Self::PdfAttachment => Some("pdfAttachment"),
           }
       }
   }
   ```

2. Delete `PlaceholderMomentType` and `AttachmentType`.

3. Update all call sites that used either enum to use `MediaType`. The conversion between the two is now implicit — they are the same type.

4. Update the `AttachmentTypeCli` → `AttachmentType` mapping in `cli/mod.rs` (or the dispatch helper after task 015) to map to `MediaType` instead.

5. Run `cargo test --locked`.

## Notes

- The image placeholder exception (`dayone-moment://ID` with no type segment, using `://` instead of `:/type/ID`) is a historical quirk. It is expressed cleanly via `placeholder_path_segment` returning `None` for images, rather than duplicating the logic.

## Definition of done

- `AttachmentType` and `PlaceholderMomentType` are deleted.
- `MediaType` is the single enum for media item types.
- All call sites use `MediaType`.
- `cargo test --locked` passes.

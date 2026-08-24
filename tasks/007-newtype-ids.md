# 007 — Introduce newtype IDs (`JournalId`, `EntryId`, `MomentId`)

**Source:** code-structure.md § Stringly-Typed Identifiers  
**Size:** M  
**Depends on:** 006 (typed domain models should use newtype IDs from the start)

---

## Problem

Journal IDs, entry IDs, moment/attachment IDs, content key fingerprints, and profile IDs are all represented as `&str` or `String`. Nothing prevents passing a journal ID where an entry ID is expected; the compiler cannot help. These ID strings also carry implicit format constraints (32-char uppercase hex for most entity IDs, non-hex for integration IDs like `strava-*`) that are nowhere encoded in the type.

## Goal

Introduce newtype wrappers for the primary ID types. Passing the wrong ID kind becomes a compile error.

## Concrete steps

1. Create `src/types.rs` with the newtype definitions:
   ```rust
   use serde::{Deserialize, Serialize};
   use std::fmt;
   use std::ops::Deref;

   macro_rules! id_type {
       ($name:ident) => {
           #[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
           #[serde(transparent)]
           pub struct $name(pub String);

           impl $name {
               pub fn new(s: impl Into<String>) -> Self { Self(s.into()) }
               pub fn as_str(&self) -> &str { &self.0 }
           }

           impl fmt::Display for $name {
               fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { self.0.fmt(f) }
           }

           impl Deref for $name {
               type Target = str;
               fn deref(&self) -> &str { &self.0 }
           }

           impl AsRef<str> for $name {
               fn as_ref(&self) -> &str { &self.0 }
           }

           impl From<String> for $name {
               fn from(s: String) -> Self { Self(s) }
           }

           impl From<&str> for $name {
               fn from(s: &str) -> Self { Self(s.to_owned()) }
           }
       };
   }

   id_type!(JournalId);
   id_type!(EntryId);
   id_type!(MomentId);
   id_type!(ContentKeyFingerprint);
   ```

2. Declare `mod types;` in `src/lib.rs` (or `main.rs`) and `pub use types::*;` from `src/lib.rs` if a lib crate exists, or re-export from a common location.

3. Update `src/models/entry.rs` and `src/models/journal.rs` (from task 006) to use these types for their ID fields:
   ```rust
   pub struct Entry {
       pub uuid: EntryId,
       pub journal_id: JournalId,
       // ...
   }
   ```

4. Update store method signatures to use typed IDs. Work through one domain module at a time:
   - `store/entries.rs` — `entry_id: &EntryId`, `journal_id: &JournalId`
   - `store/outbox.rs` — `object_id: &str` can stay generic (the outbox object_id field holds different ID types); document this.
   - `store/profiles.rs` — `profile_id: i64` stays as-is (internal SQLite row ID, not a domain ID).

5. Update command modules and sync engine call sites. Because the newtype implements `From<&str>` and `Deref<Target = str>`, most call sites change mechanically — pass `entry_id.as_str()` where a `&str` is still expected by lower-level functions.

6. Run `cargo test --locked`. Fix any compile errors.

## Notes

- `#[serde(transparent)]` ensures the newtype serialises as a plain string, preserving JSON compatibility.
- The `Deref<Target = str>` impl means newtypes can be used anywhere `&str` is accepted implicitly (e.g., as hash map keys, in format strings).
- `profile_id: i64` should remain as-is for now — it is an internal SQLite row ID, not a domain identifier sent over the wire.

## Definition of done

- `JournalId`, `EntryId`, `MomentId`, `ContentKeyFingerprint` newtypes exist in `src/types.rs`.
- Store entry/journal methods use typed IDs in their signatures.
- Domain model structs (`Entry`, `Journal`) use typed ID fields.
- `cargo test --locked` passes.

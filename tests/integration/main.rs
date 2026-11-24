//! Single integration-test binary.
//!
//! Every file under `tests/*.rs` is linked into its own executable, and each
//! one statically links the whole dependency tree, so splitting these suites
//! across many files meant many large binaries per build. Keep new
//! integration suites as modules here instead of adding top-level files.

mod auth_logout;
mod convert;
mod diagnostics;
mod entry_attachments;
mod entry_read;
mod memory_process;
mod outbox;
mod profile;
#[cfg(all(feature = "embeddings", unix))]
mod search;
mod sync;
mod telemetry_consent;
mod updates;

fn sha256_hex(input: &[u8]) -> String {
    use sha2::{Digest, Sha256};

    Sha256::digest(input)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

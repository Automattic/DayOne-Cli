mod analytics;
mod comments;
mod encryption;
pub(crate) mod entries;
mod error;
mod journal_id;
mod json_rows;
mod outbox;
mod profiles;
mod sync_state;
pub(crate) mod traits;

pub(crate) use profiles::{user_id_from_session_json, user_id_from_value};
pub(crate) use sync_state::{JournalMetadataDiagnostic, SyncResourceRunSave};

pub mod sqlite;
pub(crate) use encryption::ENCRYPTION_KEY_KEYCHAIN_SENTINEL;
#[allow(unused_imports)]
pub use encryption::EncryptionKeyStorage;
#[cfg(test)]
pub(crate) use encryption::InMemorySecureMasterKeyStore;
pub use outbox::{OutboxItem, OutboxItemDetails};

// Re-export for callers that prefer `crate::store::Store` over `crate::store::sqlite::Store`.
pub use error::{StoreError, StoreResult};
#[allow(unused_imports)]
pub use sqlite::Store;

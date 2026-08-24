#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("row not found: {table}/{id}")]
    NotFound { table: String, id: String },
    #[error("invalid input: {0}")]
    InvalidInput(String),
    #[error("io error ({context}): {source}")]
    IoContext {
        context: String,
        #[source]
        source: std::io::Error,
    },
    #[error("database error: {0}")]
    Sqlite(#[source] rusqlite::Error),
    #[error("database error ({context}): {source}")]
    SqliteContext {
        context: String,
        #[source]
        source: rusqlite::Error,
    },
    #[error("serialization error: {0}")]
    Json(#[from] serde_json::Error),
    #[error(
        "stored auth session for profile {profile_id} has no valid user id; run `dayone auth logout --force`, then log in again"
    )]
    InvalidAuthSessionUserId { profile_id: i64 },
    #[error(
        "profile {profile_id} is already authenticated as a different account; run `dayone auth logout --force` before logging in with another account"
    )]
    AuthSessionAccountMismatch { profile_id: i64 },
    #[error(
        "keychain access requires user interaction, but this command is running without an \
         interactive terminal. Rerun this command once from a local Terminal and approve the \
         keychain prompt (choose \"Always Allow\"). Future unattended commands can then access \
         the keychain."
    )]
    KeychainInteractionRequired,
    #[error("keychain unavailable: {0}")]
    KeychainUnavailable(String),
    #[error("keychain error: {0}")]
    Keychain(String),
    #[error(
        "encryption key for profile {profile_id} is set to keychain mode but no keychain entry exists; it may have been stored under a previous local store location. Run `dayone auth key-set` to re-provision it"
    )]
    MissingKeychainMasterKey { profile_id: i64 },
}

pub type StoreResult<T> = Result<T, StoreError>;

impl From<rusqlite::Error> for StoreError {
    fn from(error: rusqlite::Error) -> Self {
        crate::diagnostics::record_sqlite_failure(
            "operation",
            "unknown",
            std::time::Duration::ZERO,
            &error,
        );
        Self::Sqlite(error)
    }
}

impl StoreError {
    pub(crate) fn invalid_input(message: impl Into<String>) -> Self {
        Self::InvalidInput(message.into())
    }

    pub(crate) fn io_context(context: impl Into<String>, source: std::io::Error) -> Self {
        Self::IoContext {
            context: context.into(),
            source,
        }
    }

    pub(crate) fn sqlite_context(context: impl Into<String>, source: rusqlite::Error) -> Self {
        crate::diagnostics::record_sqlite_failure(
            "operation",
            "unknown",
            std::time::Duration::ZERO,
            &source,
        );
        Self::SqliteContext {
            context: context.into(),
            source,
        }
    }
}

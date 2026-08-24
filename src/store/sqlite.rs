use rusqlite::{
    Connection,
    ffi::{SQLITE_OK, sqlite3, sqlite3_api_routines, sqlite3_auto_extension},
};
use std::fs;
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use crate::store::encryption::{SecureMasterKeyStore, system_secure_master_key_store};
use crate::store::profiles::upsert_profile;
use crate::store::{StoreError, StoreResult};

// Re-export from constants for backward compatibility with existing callers.
pub use crate::constants::{PRODUCTION_URL, STAGING_URL};

const DEFAULT_PROFILE_NAME: &str = if cfg!(feature = "production-default") {
    "production"
} else {
    "staging"
};

static SQLITE_VEC_REGISTRATION: OnceLock<std::result::Result<(), String>> = OnceLock::new();
const SQLITE_BUSY_TIMEOUT: Duration = Duration::from_secs(5);

// sqlite-vec is linked via the `sqlite-vec` crate; we declare the extension entry point with
// SQLite's actual callback signature so `sqlite3_auto_extension` accepts it without transmute.
// (The `sqlite-vec` Rust crate exposes `sqlite3_vec_init` as a zero-arg `fn()` for tests only.)
#[link(name = "sqlite_vec0", kind = "static")]
unsafe extern "C" {
    fn sqlite3_vec_init(
        db: *mut sqlite3,
        pz_err_msg: *mut *mut std::os::raw::c_char,
        p_api: *const sqlite3_api_routines,
    ) -> std::os::raw::c_int;
}

#[derive(Debug, Clone)]
pub struct Profile {
    pub id: i64,
    pub name: String,
    pub base_url: String,
    #[cfg_attr(not(test), allow(dead_code))]
    pub is_active: bool,
}

#[derive(Debug, Clone)]
pub struct AuthSession {
    pub profile_id: i64,
    pub token: String,
    pub token_created_at: String,
    pub user_json: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JournalSyncStatus {
    WaitingInitialSync,
    Syncing,
    Idle,
}

#[derive(Debug, Clone, Copy)]
pub enum EntryListSort {
    Asc,
    Desc,
}

#[derive(Debug, Clone, Copy)]
pub enum EntryListSortMethod {
    EntryDate,
    EditDate,
}

#[derive(Debug, Clone, Default)]
pub struct ListPageRequest {
    pub limit: Option<usize>,
    pub offset: Option<usize>,
    pub cursor: Option<String>,
}

#[derive(Debug, Clone, Copy)]
pub enum ListJsonTable {
    DailyChatFeed,
    ContextLibraryItems,
}

impl ListJsonTable {
    pub(crate) fn as_sql_identifier(self) -> &'static str {
        match self {
            Self::DailyChatFeed => "daily_chat_feed",
            Self::ContextLibraryItems => "context_library_items",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ListPageResult {
    pub rows: Vec<String>,
    pub has_more: bool,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone)]
pub struct EntryAttachmentUpsert {
    pub journal_id: String,
    pub entry_id: String,
    pub moment_id: String,
    pub file_path: String,
    pub moment_type: String,
    pub content_type: String,
    pub file_size_bytes: i64,
    pub md5_body: String,
    pub width: Option<i64>,
    pub height: Option<i64>,
    pub duration_seconds: Option<f64>,
    pub thumbnail_content_type: Option<String>,
    pub thumbnail_md5: Option<String>,
    pub thumbnail_width: Option<i64>,
    pub thumbnail_height: Option<i64>,
    pub thumbnail_file_size_bytes: Option<i64>,
    pub thumbnail_bytes: Option<Vec<u8>>,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct EntryAttachmentRow {
    pub journal_id: String,
    pub entry_id: String,
    pub moment_id: String,
    pub file_path: String,
    pub moment_type: String,
    pub content_type: String,
    pub file_size_bytes: i64,
    pub md5_body: String,
    pub width: Option<i64>,
    pub height: Option<i64>,
    pub duration_seconds: Option<f64>,
    pub thumbnail_content_type: Option<String>,
    pub thumbnail_md5: Option<String>,
    pub thumbnail_width: Option<i64>,
    pub thumbnail_height: Option<i64>,
    pub thumbnail_file_size_bytes: Option<i64>,
    pub thumbnail_bytes: Option<Vec<u8>>,
    pub entry_associated: bool,
    pub original_uploaded: bool,
    pub last_error: Option<String>,
}

impl JournalSyncStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::WaitingInitialSync => "WAITING_INITIAL_SYNC",
            Self::Syncing => "SYNCING",
            Self::Idle => "IDLE",
        }
    }
}

/// Optional seed data for a single profile, used when opening a per-profile
/// database. When provided to `initialize()`, only this profile is seeded
/// (instead of the default staging + production pair).
pub struct ProfileSeed {
    pub name: String,
    pub base_url: String,
}

pub struct Store {
    db_path: PathBuf,
    pub(crate) secure_master_key_store: Arc<dyn SecureMasterKeyStore>,
    pub(crate) is_profile_store: bool,
}

impl Store {
    #[expect(
        dead_code,
        reason = "CLI opens profile-specific stores; legacy callers may use this"
    )]
    pub fn open_default() -> StoreResult<Self> {
        let app_dir = if let Ok(override_dir) = std::env::var("DAYONE_CONFIG_DIR") {
            PathBuf::from(override_dir)
        } else {
            let config_dir = dirs::config_dir()
                .ok_or_else(|| StoreError::invalid_input("failed to resolve config directory"))?;
            config_dir.join("dayone-cli")
        };
        fs::create_dir_all(&app_dir).map_err(|err| {
            StoreError::io_context(
                format!("failed to create config directory {}", app_dir.display()),
                err,
            )
        })?;
        let db_path = app_dir.join("dayone.db");
        Self::open_at(db_path)
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn open_at(path: impl Into<PathBuf>) -> StoreResult<Self> {
        Self::open_at_with_secure_master_key_store(path, system_secure_master_key_store())
    }

    /// Open (or create) a per-profile database at
    /// `{config_dir}/profiles/{profile_name}/dayone.db`.
    ///
    /// The database is seeded with a single profile row matching the given
    /// name and base_url, rather than the default staging + production pair.
    pub fn open_for_profile(
        config_dir: &std::path::Path,
        profile_name: &str,
        base_url: &str,
    ) -> StoreResult<Self> {
        use crate::config::AppConfig;
        use crate::http::normalize_base_url;

        let db_path = AppConfig::profile_db_path(config_dir, profile_name)
            .map_err(|e| StoreError::invalid_input(e.to_string()))?;
        let normalized_url =
            normalize_base_url(base_url).map_err(|e| StoreError::invalid_input(e.to_string()))?;
        let seed = ProfileSeed {
            name: profile_name.to_owned(),
            base_url: normalized_url,
        };
        Self::open_with_seed(db_path, system_secure_master_key_store(), Some(seed))
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn open_at_with_secure_master_key_store(
        path: impl Into<PathBuf>,
        secure_master_key_store: Arc<dyn SecureMasterKeyStore>,
    ) -> StoreResult<Self> {
        Self::open_with_seed(path, secure_master_key_store, None)
    }

    /// Opens a store whose master keys live in an in-memory secure store, so
    /// tests can set/read encryption keys without touching the real keychain.
    #[cfg(test)]
    pub(crate) fn open_at_with_in_memory_master_keys(
        path: impl Into<PathBuf>,
    ) -> StoreResult<Self> {
        Self::open_at_with_secure_master_key_store(
            path,
            Arc::new(crate::store::encryption::InMemorySecureMasterKeyStore::default()),
        )
    }

    fn open_with_seed(
        path: impl Into<PathBuf>,
        secure_master_key_store: Arc<dyn SecureMasterKeyStore>,
        seed: Option<ProfileSeed>,
    ) -> StoreResult<Self> {
        ensure_sqlite_vec_registered()?;
        let db_path = path.into();
        if let Some(parent) = db_path.parent() {
            fs::create_dir_all(parent).map_err(|err| {
                StoreError::io_context(
                    format!(
                        "failed to create parent directories for {}",
                        db_path.display()
                    ),
                    err,
                )
            })?;
            #[cfg(unix)]
            if seed.is_some() {
                set_owner_only_permissions(parent, 0o700, "database directory")?;
            }
        }
        #[cfg(unix)]
        fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(&db_path)
            .map_err(|err| {
                StoreError::io_context(
                    format!("failed to open sqlite database at {}", db_path.display()),
                    err,
                )
            })?;
        #[cfg(unix)]
        set_owner_only_permissions(&db_path, 0o600, "sqlite database")?;
        let store = Self {
            db_path,
            secure_master_key_store,
            is_profile_store: seed.is_some(),
        };
        store.initialize(seed)?;
        Ok(store)
    }

    pub fn erase_local_data(&self) -> StoreResult<()> {
        self.delete_all_master_keys_best_effort();
        // Remove WAL and SHM side-files first; ignore not-found errors.
        let _ = fs::remove_file(self.db_path.with_extension("db-wal"));
        let _ = fs::remove_file(self.db_path.with_extension("db-shm"));
        fs::remove_file(&self.db_path).map_err(|err| {
            StoreError::io_context(
                format!("failed to remove database at {}", self.db_path.display()),
                err,
            )
        })?;
        Ok(())
    }

    fn initialize(&self, seed: Option<ProfileSeed>) -> StoreResult<()> {
        let mut conn = self.connect()?;

        // Run migrations outside the profile-setup transaction so each
        // migration is individually atomic and recoverable on retry.
        run_migrations(&mut conn)?;

        let tx = conn.transaction()?;

        match seed {
            Some(s) => {
                // Per-profile DB: seed exactly one profile and mark it active.
                let profile = upsert_profile(&tx, &s.name, &s.base_url)?;
                tx.execute(
                    "UPDATE profiles SET is_active = CASE WHEN id = ?1 THEN 1 ELSE 0 END, updated_at = CURRENT_TIMESTAMP",
                    rusqlite::params![profile.id],
                )?;
            }
            None => {
                // Legacy / shared DB: seed both default profiles.
                upsert_profile(&tx, "staging", STAGING_URL)?;
                upsert_profile(&tx, "production", PRODUCTION_URL)?;

                let active_count: i64 = tx.query_row(
                    "SELECT COUNT(*) FROM profiles WHERE is_active = 1",
                    [],
                    |row| row.get(0),
                )?;
                if active_count == 0 {
                    tx.execute(
                        "UPDATE profiles SET is_active = CASE WHEN name = ?1 THEN 1 ELSE 0 END, updated_at = CURRENT_TIMESTAMP",
                        rusqlite::params![DEFAULT_PROFILE_NAME],
                    )?;
                }
            }
        }

        tx.commit()?;
        Ok(())
    }

    pub(crate) fn connect(&self) -> StoreResult<Connection> {
        let started = std::time::Instant::now();
        let conn = Connection::open(&self.db_path).map_err(|error| {
            crate::diagnostics::record_sqlite_failure(
                "open",
                "database",
                started.elapsed(),
                &error,
            );
            StoreError::SqliteContext {
                context: format!(
                    "failed to open sqlite database at {}",
                    self.db_path.display()
                ),
                source: error,
            }
        })?;
        conn.busy_timeout(SQLITE_BUSY_TIMEOUT).map_err(|error| {
            crate::diagnostics::record_sqlite_failure(
                "open",
                "database",
                started.elapsed(),
                &error,
            );
            StoreError::SqliteContext {
                context: "failed to configure sqlite busy timeout".to_owned(),
                source: error,
            }
        })?;
        configure_sqlite_connection(&conn)?;
        crate::diagnostics::record_sqlite_success("open", "database", 1, started.elapsed());
        Ok(conn)
    }

    pub(crate) fn db_path(&self) -> &std::path::Path {
        &self.db_path
    }
}

pub(crate) const MIGRATIONS: &[(i64, &str)] = &[
    (1, include_str!("migrations/0001_init.sql")),
    (2, include_str!("migrations/0002_sync.sql")),
    (3, include_str!("migrations/0003_encryption_keys.sql")),
    (4, include_str!("migrations/0004_outbox.sql")),
    (5, include_str!("migrations/0005_entry_attachments.sql")),
    (6, include_str!("migrations/0006_entry_embeddings.sql")),
    (7, include_str!("migrations/0007_entries_schema.sql")),
    (8, include_str!("migrations/0008_outbox_tuple_index.sql")),
    (
        9,
        include_str!("migrations/0009_unread_entries_deleted_at.sql"),
    ),
    (10, include_str!("migrations/0010_comments.sql")),
    (
        11,
        include_str!("migrations/0011_reset_journals_cursor.sql"),
    ),
    (12, include_str!("migrations/0012_drop_decrypted_text.sql")),
    (13, include_str!("migrations/0013_analytics.sql")),
    (
        14,
        include_str!("migrations/0014_rescrub_decrypted_text.sql"),
    ),
    (15, include_str!("migrations/0015_user_identity_key.sql")),
    (16, include_str!("migrations/0016_analytics_identity.sql")),
    (
        17,
        include_str!("migrations/0017_journal_metadata_diagnostics.sql"),
    ),
];

fn run_migrations(conn: &mut Connection) -> StoreResult<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_migrations (
            version INTEGER PRIMARY KEY,
            applied_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
        );",
    )
    .map_err(|err| StoreError::sqlite_context("failed to create schema_migrations table", err))?;

    // Internal invariant: migration versions must be sequential starting from 1.
    // Enforced in debug builds only to avoid repeated runtime work on every DB open.
    debug_assert!(
        MIGRATIONS
            .iter()
            .enumerate()
            .all(|(i, &(version, _))| version == (i as i64) + 1),
        "migration versions must be sequential starting from 1"
    );

    for &(version, sql) in MIGRATIONS {
        run_migration(conn, version, sql)?;
    }
    Ok(())
}

/// Apply a single migration if not already recorded.  The common "already
/// migrated" path uses a cheap read-only check and never acquires a write
/// lock, avoiding contention when multiple processes open the same DB
/// concurrently.  Only when a version appears missing do we start an
/// IMMEDIATE transaction (acquiring the write lock up front), re-check
/// inside it to handle races, then apply and record the migration.
fn run_migration(conn: &mut Connection, version: i64, sql: &str) -> StoreResult<()> {
    // Fast read-only check — no write lock needed for the common case.
    let already_applied: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM schema_migrations WHERE version = ?1)",
            rusqlite::params![version],
            |row| row.get(0),
        )
        .map_err(|err| {
            StoreError::sqlite_context(format!("failed to check migration version {version}"), err)
        })?;

    if already_applied {
        return Ok(());
    }

    // Version appears missing — acquire write lock and re-check to handle
    // concurrent openers that may have applied it between our read and now.
    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(|err| {
            StoreError::sqlite_context(
                format!("failed to begin transaction for migration {version}"),
                err,
            )
        })?;

    let still_missing: bool = !tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM schema_migrations WHERE version = ?1)",
            rusqlite::params![version],
            |row| row.get::<_, bool>(0),
        )
        .map_err(|err| {
            StoreError::sqlite_context(
                format!("failed to re-check migration version {version}"),
                err,
            )
        })?;

    if !still_missing {
        return Ok(());
    }

    tx.execute_batch(sql)
        .map_err(|err| StoreError::sqlite_context(format!("migration {version:04} failed"), err))?;

    tx.execute(
        "INSERT INTO schema_migrations (version) VALUES (?1)",
        rusqlite::params![version],
    )
    .map_err(|err| {
        StoreError::sqlite_context(format!("failed to record migration version {version}"), err)
    })?;

    tx.commit().map_err(|err| {
        StoreError::sqlite_context(format!("failed to commit migration {version}"), err)
    })?;

    Ok(())
}

fn configure_sqlite_connection(conn: &Connection) -> StoreResult<()> {
    let journal_mode: String =
        match conn.query_row("PRAGMA journal_mode=WAL;", [], |row| row.get(0)) {
            Ok(mode) => mode,
            Err(err) if is_sqlite_busy_error(&err) => {
                // WAL mode is persistent in the database file. If another process
                // is briefly holding a write lock while we open, fall back to
                // reading the current mode instead of failing fast.
                conn.query_row("PRAGMA journal_mode;", [], |row| row.get(0))
                    .map_err(|query_err| {
                        StoreError::sqlite_context(
                            "failed to read sqlite journal mode after WAL set was busy/locked",
                            query_err,
                        )
                    })?
            }
            Err(err) => {
                return Err(StoreError::sqlite_context(
                    "failed to set sqlite WAL journal mode",
                    err,
                ));
            }
        };

    if journal_mode.to_lowercase() != "wal" {
        return Err(StoreError::invalid_input(format!(
            "failed to enable WAL journal mode (effective mode: {journal_mode})"
        )));
    }

    conn.execute("PRAGMA synchronous=NORMAL;", [])
        .map_err(|err| {
            StoreError::sqlite_context("failed to set sqlite synchronous pragma", err)
        })?;

    let synchronous: i64 = conn
        .query_row("PRAGMA synchronous;", [], |row| row.get(0))
        .map_err(|err| {
            StoreError::sqlite_context("failed to read sqlite synchronous pragma", err)
        })?;

    if synchronous != 1 {
        return Err(StoreError::invalid_input(format!(
            "sqlite PRAGMA synchronous is not NORMAL as requested (value: {synchronous})"
        )));
    }

    Ok(())
}

fn is_sqlite_busy_error(err: &rusqlite::Error) -> bool {
    match err {
        rusqlite::Error::SqliteFailure(inner, _) => {
            inner.code == rusqlite::ErrorCode::DatabaseBusy
                || inner.code == rusqlite::ErrorCode::DatabaseLocked
        }
        _ => false,
    }
}

#[cfg(unix)]
fn set_owner_only_permissions(
    path: &std::path::Path,
    mode: u32,
    description: &str,
) -> StoreResult<()> {
    crate::util::set_owner_only_permissions(path, mode).map_err(|error| {
        StoreError::io_context(
            format!("failed to secure {description} at {}", path.display()),
            error,
        )
    })
}

fn ensure_sqlite_vec_registered() -> StoreResult<()> {
    let init = SQLITE_VEC_REGISTRATION.get_or_init(|| {
        // Register sqlite-vec for all future SQLite connections in this process.
        let rc = unsafe { sqlite3_auto_extension(Some(sqlite3_vec_init)) };
        if rc == SQLITE_OK {
            Ok(())
        } else {
            Err(format!("sqlite3_auto_extension failed with rc={rc}"))
        }
    });

    match init {
        Ok(()) => Ok(()),
        Err(msg) => Err(StoreError::invalid_input(format!(
            "failed to register sqlite-vec extension: {msg}"
        ))),
    }
}

#[cfg(test)]
mod tests;

use rusqlite::{OptionalExtension, params};
#[cfg(target_os = "macos")]
use std::io::IsTerminal;

use crate::store::profiles::{short_hash, user_id_from_session_json};
use crate::store::sqlite::{Profile, Store};
use crate::store::{StoreError, StoreResult};

pub const ENCRYPTION_KEY_KEYCHAIN_SENTINEL: &str = "keychain";
const MASTER_KEY_SERVICE: &str = "dayone-cli";
const MASTER_KEY_ACCOUNT_PREFIX: &str = "master-key:";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EncryptionKeyStorage {
    Keychain,
    Sqlite,
}

impl EncryptionKeyStorage {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Keychain => "keychain",
            Self::Sqlite => "sqlite",
        }
    }
}

pub(crate) trait SecureMasterKeyStore: Send + Sync {
    fn set_master_key(&self, account: &str, master_key: &str) -> StoreResult<()>;
    fn get_master_key(&self, account: &str) -> StoreResult<Option<String>>;
    fn delete_master_key(&self, account: &str) -> StoreResult<()>;
}

pub(crate) struct SystemSecureMasterKeyStore;

#[cfg(target_os = "macos")]
const ERR_SEC_AUTH_FAILED: i32 = -25293;
#[cfg(target_os = "macos")]
const ERR_SEC_INTERACTION_NOT_ALLOWED: i32 = -25308;

fn map_keyring_error(err: keyring::Error, interaction_disabled: bool) -> StoreError {
    #[cfg(target_os = "macos")]
    if interaction_disabled
        && let keyring::Error::PlatformFailure(inner) = &err
        && inner
            .downcast_ref::<security_framework::base::Error>()
            .is_some_and(|err| {
                matches!(
                    err.code(),
                    ERR_SEC_AUTH_FAILED | ERR_SEC_INTERACTION_NOT_ALLOWED
                )
            })
    {
        return StoreError::KeychainInteractionRequired;
    }

    match err {
        keyring::Error::NoStorageAccess(inner) => {
            StoreError::KeychainUnavailable(inner.to_string())
        }
        keyring::Error::NoEntry => {
            StoreError::Keychain("No matching entry found in secure storage".to_owned())
        }
        other => StoreError::Keychain(other.to_string()),
    }
}

#[cfg(target_os = "macos")]
static KEYCHAIN_OPERATION_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(target_os = "macos")]
fn keychain_interaction_is_available() -> bool {
    keychain_interaction_is_available_for(
        std::io::stdin().is_terminal()
            || std::io::stdout().is_terminal()
            || std::io::stderr().is_terminal(),
        std::env::var_os("SSH_CONNECTION").is_some() || std::env::var_os("SSH_TTY").is_some(),
    )
}

#[cfg(target_os = "macos")]
fn keychain_interaction_is_available_for(terminal_is_available: bool, ssh_session: bool) -> bool {
    terminal_is_available && !ssh_session
}

struct KeychainOperationGuard {
    // Drop the interaction guard before releasing the process-wide operation
    // lock so another keychain call cannot start while UI policy is restoring.
    #[cfg(target_os = "macos")]
    _interaction_lock: Option<security_framework::os::macos::keychain::KeychainUserInteractionLock>,
    #[cfg(target_os = "macos")]
    _operation_lock: std::sync::MutexGuard<'static, ()>,
    #[cfg(target_os = "macos")]
    interaction_disabled: bool,
}

impl KeychainOperationGuard {
    fn acquire() -> StoreResult<Self> {
        #[cfg(target_os = "macos")]
        return Self::acquire_for_interactivity(keychain_interaction_is_available());

        #[cfg(not(target_os = "macos"))]
        Ok(Self {})
    }

    fn interaction_disabled(&self) -> bool {
        #[cfg(target_os = "macos")]
        return self.interaction_disabled;

        #[cfg(not(target_os = "macos"))]
        false
    }

    #[cfg(target_os = "macos")]
    fn acquire_for_interactivity(interactive: bool) -> StoreResult<Self> {
        use security_framework::os::macos::keychain::SecKeychain;

        let operation_lock = KEYCHAIN_OPERATION_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let interaction_lock = if !interactive
            && SecKeychain::user_interaction_allowed()
                .map_err(|err| StoreError::KeychainUnavailable(err.to_string()))?
        {
            Some(
                SecKeychain::disable_user_interaction()
                    .map_err(|err| StoreError::KeychainUnavailable(err.to_string()))?,
            )
        } else {
            None
        };
        Ok(Self {
            _interaction_lock: interaction_lock,
            _operation_lock: operation_lock,
            interaction_disabled: !interactive,
        })
    }
}

impl SecureMasterKeyStore for SystemSecureMasterKeyStore {
    fn set_master_key(&self, account: &str, master_key: &str) -> StoreResult<()> {
        let guard = KeychainOperationGuard::acquire()?;
        let interaction_disabled = guard.interaction_disabled();
        let entry = keyring::Entry::new(MASTER_KEY_SERVICE, account)
            .map_err(|err| map_keyring_error(err, interaction_disabled))?;
        entry
            .set_password(master_key)
            .map_err(|err| map_keyring_error(err, interaction_disabled))
    }

    fn get_master_key(&self, account: &str) -> StoreResult<Option<String>> {
        let guard = KeychainOperationGuard::acquire()?;
        let interaction_disabled = guard.interaction_disabled();
        let entry = keyring::Entry::new(MASTER_KEY_SERVICE, account)
            .map_err(|err| map_keyring_error(err, interaction_disabled))?;
        match entry.get_password() {
            Ok(value) => Ok(Some(value)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(err) => Err(map_keyring_error(err, interaction_disabled)),
        }
    }

    fn delete_master_key(&self, account: &str) -> StoreResult<()> {
        let guard = KeychainOperationGuard::acquire()?;
        let interaction_disabled = guard.interaction_disabled();
        let entry = keyring::Entry::new(MASTER_KEY_SERVICE, account)
            .map_err(|err| map_keyring_error(err, interaction_disabled))?;
        match entry.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(err) => Err(map_keyring_error(err, interaction_disabled)),
        }
    }
}

/// Returns the production secure store. On macOS, keychain UI remains enabled
/// for terminal commands, but headless commands disable it so authorization
/// requirements fail immediately instead of waiting forever on an invisible
/// prompt.
pub(crate) fn system_secure_master_key_store() -> std::sync::Arc<dyn SecureMasterKeyStore> {
    std::sync::Arc::new(SystemSecureMasterKeyStore)
}

/// In-memory secure master key store for tests, so tests that need a master key
/// never touch the real system keychain.
#[cfg(test)]
#[derive(Default)]
pub(crate) struct InMemorySecureMasterKeyStore {
    values: std::sync::Mutex<std::collections::HashMap<String, String>>,
}

#[cfg(test)]
impl SecureMasterKeyStore for InMemorySecureMasterKeyStore {
    fn set_master_key(&self, account: &str, master_key: &str) -> StoreResult<()> {
        self.values
            .lock()
            .expect("in-memory key store mutex should lock")
            .insert(account.to_owned(), master_key.to_owned());
        Ok(())
    }

    fn get_master_key(&self, account: &str) -> StoreResult<Option<String>> {
        Ok(self
            .values
            .lock()
            .expect("in-memory key store mutex should lock")
            .get(account)
            .cloned())
    }

    fn delete_master_key(&self, account: &str) -> StoreResult<()> {
        self.values
            .lock()
            .expect("in-memory key store mutex should lock")
            .remove(account);
        Ok(())
    }
}

fn legacy_secure_master_key_account(profile_id: i64) -> String {
    format!("{MASTER_KEY_ACCOUNT_PREFIX}{profile_id}")
}

fn legacy_secure_master_key_account_with_db_hash(
    profile_id: i64,
    db_path: &std::path::Path,
) -> String {
    let db_hash = short_hash(&db_path.to_string_lossy());
    format!("{MASTER_KEY_ACCOUNT_PREFIX}{db_hash}:{profile_id}")
}

fn legacy_secure_master_key_account_with_db_and_base_hash(
    profile: &Profile,
    db_path: &std::path::Path,
) -> String {
    let db_hash = short_hash(&db_path.to_string_lossy());
    let base_hash = short_hash(&profile.base_url);
    format!(
        "{MASTER_KEY_ACCOUNT_PREFIX}{db_hash}:{base_hash}:{}",
        profile.id
    )
}

fn legacy_secure_master_key_account_without_store_id(profile: &Profile) -> String {
    let base_hash = short_hash(&profile.base_url);
    format!("{MASTER_KEY_ACCOUNT_PREFIX}{base_hash}:{}", profile.id)
}

fn secure_master_key_account(profile: &Profile, store_id: &str) -> String {
    let base_hash = short_hash(&profile.base_url);
    let store_hash = short_hash(store_id);
    format!(
        "{MASTER_KEY_ACCOUNT_PREFIX}{store_hash}:{base_hash}:{}",
        profile.id
    )
}

/// Uses the server user id so the account remains stable when the database path changes.
/// The `uid:` prefix distinguishes it from legacy account formats.
fn secure_master_key_account_stable(profile: &Profile, server_user_id: &str) -> String {
    let base_hash = short_hash(&profile.base_url);
    let user_hash = short_hash(server_user_id);
    format!("{MASTER_KEY_ACCOUNT_PREFIX}uid:{base_hash}:{user_hash}")
}

fn secure_master_key_account_candidates(
    db_path: &std::path::Path,
    profile: &Profile,
    store_id: Option<&str>,
) -> Vec<String> {
    let mut candidates: Vec<String> = Vec::new();
    if let Some(id) = store_id {
        candidates.push(secure_master_key_account(profile, id));
    }
    candidates.push(legacy_secure_master_key_account_with_db_and_base_hash(
        profile, db_path,
    ));
    candidates.push(legacy_secure_master_key_account_with_db_hash(
        profile.id, db_path,
    ));
    candidates.push(legacy_secure_master_key_account_without_store_id(profile));
    candidates.push(legacy_secure_master_key_account(profile.id));
    let mut unique: Vec<String> = Vec::new();
    for candidate in candidates {
        if !unique.iter().any(|existing| existing == &candidate) {
            unique.push(candidate);
        }
    }
    unique
}

impl Store {
    fn profile_for_id(&self, profile_id: i64) -> StoreResult<Profile> {
        let conn = self.connect()?;
        conn.query_row(
            "SELECT id, name, base_url, is_active FROM profiles WHERE id = ?1 LIMIT 1",
            params![profile_id],
            |row| {
                Ok(Profile {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    base_url: row.get(2)?,
                    is_active: row.get::<_, i64>(3)? == 1,
                })
            },
        )
        .optional()?
        .ok_or_else(|| StoreError::NotFound {
            table: "profiles".to_owned(),
            id: profile_id.to_string(),
        })
    }

    /// The stable, server-assigned user id for a profile's persisted session, if
    /// one exists. Used to key the master key in secure storage independently of
    /// the local DB path. Returns `None` before login (no session yet) so callers
    /// fall back to the legacy path-derived account.
    fn server_user_id_for_profile(&self, profile_id: i64) -> StoreResult<Option<String>> {
        let conn = self.connect()?;
        let Some(user_json) = conn
            .query_row(
                "SELECT user_json FROM auth_sessions WHERE profile_id = ?1 LIMIT 1",
                params![profile_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?
        else {
            return Ok(None);
        };
        user_id_from_session_json(&user_json)?
            .map(Some)
            .ok_or(StoreError::InvalidAuthSessionUserId { profile_id })
    }

    fn secure_master_key_account_for_profile(&self, profile_id: i64) -> StoreResult<String> {
        let profile = self.profile_for_id(profile_id)?;
        // Prefer a stable account keyed by the server user id so the master key
        // survives DB path changes (DAYONE-1100). Fall back to the legacy
        // path-derived account only when no session/user id is available yet;
        // `get_encryption_key_for_profile` then migrates any legacy-account key
        // onto this stable account on the next unlock.
        if let Some(user_id) = self.server_user_id_for_profile(profile_id)? {
            return Ok(secure_master_key_account_stable(&profile, &user_id));
        }
        let store_id = self.db_path().to_string_lossy();
        Ok(secure_master_key_account(&profile, &store_id))
    }

    /// Report where the master key is configured without reading key material
    /// or prompting the system keychain.
    pub fn encryption_key_storage_for_profile(
        &self,
        profile_id: i64,
    ) -> StoreResult<Option<EncryptionKeyStorage>> {
        let conn = self.connect()?;
        conn.query_row(
            "SELECT master_key = ?2 FROM encryption_keys WHERE profile_id = ?1",
            params![profile_id, ENCRYPTION_KEY_KEYCHAIN_SENTINEL],
            |row| {
                Ok(if row.get::<_, bool>(0)? {
                    EncryptionKeyStorage::Keychain
                } else {
                    EncryptionKeyStorage::Sqlite
                })
            },
        )
        .optional()
        .map_err(Into::into)
    }

    pub fn set_encryption_key_for_profile(
        &self,
        profile_id: i64,
        master_key: &str,
    ) -> StoreResult<EncryptionKeyStorage> {
        let trimmed = master_key.trim();
        if trimmed.is_empty() {
            return Err(StoreError::invalid_input("encryption key cannot be empty"));
        }
        if trimmed == ENCRYPTION_KEY_KEYCHAIN_SENTINEL {
            return Err(StoreError::invalid_input(
                "encryption key cannot use reserved value 'keychain'",
            ));
        }
        let account = self.secure_master_key_account_for_profile(profile_id)?;
        match self
            .secure_master_key_store
            .set_master_key(&account, trimmed)
        {
            Ok(()) => {
                match self.secure_master_key_store.get_master_key(&account) {
                    Ok(Some(stored)) => {
                        if stored != trimmed {
                            eprintln!(
                                "[keychain] secure storage verification failed for profile {profile_id}; using sqlite fallback (mismatched value after write)"
                            );
                            let _ = self.secure_master_key_store.delete_master_key(&account);
                            self.set_sqlite_encryption_key_for_profile(profile_id, trimmed)?;
                            return Ok(EncryptionKeyStorage::Sqlite);
                        }
                    }
                    Ok(None) => {
                        eprintln!(
                            "[keychain] secure storage verification failed for profile {profile_id}; using sqlite fallback (entry missing after write)"
                        );
                        let _ = self.secure_master_key_store.delete_master_key(&account);
                        self.set_sqlite_encryption_key_for_profile(profile_id, trimmed)?;
                        return Ok(EncryptionKeyStorage::Sqlite);
                    }
                    Err(err) => {
                        eprintln!(
                            "[keychain] secure storage verification failed for profile {profile_id}; using sqlite fallback ({err})"
                        );
                        let _ = self.secure_master_key_store.delete_master_key(&account);
                        self.set_sqlite_encryption_key_for_profile(profile_id, trimmed)?;
                        return Ok(EncryptionKeyStorage::Sqlite);
                    }
                }
                if let Err(sentinel_err) = self.set_sqlite_encryption_key_for_profile(
                    profile_id,
                    ENCRYPTION_KEY_KEYCHAIN_SENTINEL,
                ) {
                    if let Err(rollback_err) =
                        self.secure_master_key_store.delete_master_key(&account)
                    {
                        eprintln!(
                            "[keychain] failed to rollback keychain entry after sqlite sentinel write failure for profile {profile_id} ({rollback_err})"
                        );
                    }
                    return Err(StoreError::Keychain(format!(
                        "failed to persist encryption key sentinel in sqlite for profile {profile_id}: {sentinel_err}"
                    )));
                }
                Ok(EncryptionKeyStorage::Keychain)
            }
            Err(StoreError::KeychainUnavailable(reason)) => {
                eprintln!(
                    "[keychain] secure storage unavailable for profile {profile_id}; using sqlite fallback ({reason})"
                );
                self.set_sqlite_encryption_key_for_profile(profile_id, trimmed)?;
                Ok(EncryptionKeyStorage::Sqlite)
            }
            Err(err) => Err(err),
        }
    }

    pub fn get_encryption_key_for_profile(&self, profile_id: i64) -> StoreResult<Option<String>> {
        let Some(stored_value) = self.get_stored_encryption_key_for_profile(profile_id)? else {
            return Ok(None);
        };
        if stored_value == ENCRYPTION_KEY_KEYCHAIN_SENTINEL {
            let profile = self.profile_for_id(profile_id)?;
            let account = self.secure_master_key_account_for_profile(profile_id)?;
            if let Some(value) = self.secure_master_key_store.get_master_key(&account)? {
                return Ok(Some(value));
            }
            for legacy_account in
                secure_master_key_account_candidates(self.db_path(), &profile, None)
            {
                if legacy_account == account {
                    continue;
                }
                if let Some(value) = self
                    .secure_master_key_store
                    .get_master_key(&legacy_account)?
                {
                    if let Err(err) = self
                        .secure_master_key_store
                        .set_master_key(&account, &value)
                    {
                        eprintln!(
                            "[keychain] failed to migrate legacy account for profile {profile_id} ({err})"
                        );
                    } else if let Err(err) = self
                        .secure_master_key_store
                        .delete_master_key(&legacy_account)
                    {
                        eprintln!(
                            "[keychain] failed to delete legacy account after migration for profile {profile_id} ({err})"
                        );
                    }
                    return Ok(Some(value));
                }
            }
            return Err(StoreError::MissingKeychainMasterKey { profile_id });
        }

        // Backward-compatible migration path for existing plaintext SQLite rows.
        if let Ok(account) = self.secure_master_key_account_for_profile(profile_id)
            && self
                .secure_master_key_store
                .set_master_key(&account, &stored_value)
                .is_ok()
            && let Err(err) = self
                .set_sqlite_encryption_key_for_profile(profile_id, ENCRYPTION_KEY_KEYCHAIN_SENTINEL)
        {
            eprintln!(
                "[keychain] failed to write sqlite sentinel during migration for profile {profile_id} ({err})"
            );
        }
        Ok(Some(stored_value))
    }

    pub(crate) fn delete_all_master_keys_best_effort(&self) {
        if let Ok(profiles) = self.list_profiles() {
            let store_id = self.db_path().to_string_lossy();
            for profile in profiles {
                let mut accounts =
                    secure_master_key_account_candidates(self.db_path(), &profile, Some(&store_id));
                // Also clear the stable (server-user-id) account (DAYONE-1100),
                // which the candidate list can't derive on its own.
                match self.server_user_id_for_profile(profile.id) {
                    Ok(Some(user_id)) => {
                        accounts.push(secure_master_key_account_stable(&profile, &user_id));
                    }
                    Ok(None) => {}
                    Err(err) => eprintln!(
                        "[keychain] failed to derive stable master key account during local erase ({err})"
                    ),
                }
                for account in accounts {
                    if let Err(err) = self.secure_master_key_store.delete_master_key(&account) {
                        eprintln!(
                            "[keychain] failed to delete a master key account during local erase ({err})"
                        );
                    }
                }
            }
        }
    }

    fn set_sqlite_encryption_key_for_profile(
        &self,
        profile_id: i64,
        value: &str,
    ) -> StoreResult<()> {
        let conn = self.connect()?;
        conn.execute(
            r#"
            INSERT INTO encryption_keys (profile_id, master_key, created_at, updated_at)
            VALUES (?1, ?2, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)
            ON CONFLICT(profile_id) DO UPDATE SET
              master_key = excluded.master_key,
              updated_at = CURRENT_TIMESTAMP
            "#,
            params![profile_id, value],
        )?;
        Ok(())
    }

    fn get_stored_encryption_key_for_profile(
        &self,
        profile_id: i64,
    ) -> StoreResult<Option<String>> {
        let conn = self.connect()?;
        conn.query_row(
            "SELECT master_key FROM encryption_keys WHERE profile_id = ?1 LIMIT 1",
            params![profile_id],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(StoreError::from)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;
    use crate::store::sqlite::Store;

    struct TestSecureMasterKeyStore {
        fail_set: bool,
        fail_set_with_keychain_error: bool,
        fail_get: bool,
        fail_delete: bool,
        forced_get_value: Option<String>,
        values: Mutex<HashMap<String, String>>,
    }

    impl TestSecureMasterKeyStore {
        fn available() -> Arc<Self> {
            Arc::new(Self {
                fail_set: false,
                fail_set_with_keychain_error: false,
                fail_get: false,
                fail_delete: false,
                forced_get_value: None,
                values: Mutex::new(HashMap::new()),
            })
        }

        fn unavailable() -> Arc<Self> {
            Arc::new(Self {
                fail_set: true,
                fail_set_with_keychain_error: false,
                fail_get: false,
                fail_delete: false,
                forced_get_value: None,
                values: Mutex::new(HashMap::new()),
            })
        }

        fn with_set_keychain_error() -> Arc<Self> {
            Arc::new(Self {
                fail_set: false,
                fail_set_with_keychain_error: true,
                fail_get: false,
                fail_delete: false,
                forced_get_value: None,
                values: Mutex::new(HashMap::new()),
            })
        }

        fn with_delete_failure() -> Arc<Self> {
            Arc::new(Self {
                fail_set: false,
                fail_set_with_keychain_error: false,
                fail_get: false,
                fail_delete: true,
                forced_get_value: None,
                values: Mutex::new(HashMap::new()),
            })
        }

        fn with_mismatched_read(value: &str) -> Arc<Self> {
            Arc::new(Self {
                fail_set: false,
                fail_set_with_keychain_error: false,
                fail_get: false,
                fail_delete: false,
                forced_get_value: Some(value.to_owned()),
                values: Mutex::new(HashMap::new()),
            })
        }

        fn insert(&self, account: &str, value: &str) {
            self.values
                .lock()
                .expect("test key store mutex should lock")
                .insert(account.to_owned(), value.to_owned());
        }
    }

    impl SecureMasterKeyStore for TestSecureMasterKeyStore {
        fn set_master_key(&self, account: &str, master_key: &str) -> StoreResult<()> {
            if self.fail_set {
                return Err(StoreError::KeychainUnavailable(
                    "secure store unavailable".to_owned(),
                ));
            }
            if self.fail_set_with_keychain_error {
                return Err(StoreError::Keychain(
                    "secure store write failed unexpectedly".to_owned(),
                ));
            }
            self.values
                .lock()
                .expect("test key store mutex should lock")
                .insert(account.to_owned(), master_key.to_owned());
            Ok(())
        }

        fn get_master_key(&self, account: &str) -> StoreResult<Option<String>> {
            if self.fail_get {
                return Err(StoreError::KeychainUnavailable(
                    "secure store read unavailable".to_owned(),
                ));
            }
            if let Some(value) = &self.forced_get_value {
                return Ok(Some(value.clone()));
            }
            Ok(self
                .values
                .lock()
                .expect("test key store mutex should lock")
                .get(account)
                .cloned())
        }

        fn delete_master_key(&self, account: &str) -> StoreResult<()> {
            if self.fail_delete {
                return Err(StoreError::KeychainUnavailable(
                    "secure store delete unavailable".to_owned(),
                ));
            }
            self.values
                .lock()
                .expect("test key store mutex should lock")
                .remove(account);
            Ok(())
        }
    }

    fn test_store_path(name: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time should be valid")
            .as_nanos();
        std::env::temp_dir().join(format!("dayone-cli-{name}-{unique}.db"))
    }

    fn test_store_with_secure_store(
        name: &str,
        secure_store: Arc<dyn SecureMasterKeyStore>,
    ) -> Store {
        let path = test_store_path(name);
        Store::open_at_with_secure_master_key_store(path, secure_store)
            .expect("store should open with test secure store")
    }

    /// Persist a minimal auth session so `server_user_id_for_profile` resolves,
    /// i.e. simulate a logged-in profile with the given server user id.
    fn insert_test_session(store: &Store, profile_id: i64, server_user_id: &str) {
        let conn = store.connect().expect("conn should open");
        conn.execute(
            "INSERT INTO auth_sessions (profile_id, token, token_created_at, user_json)
             VALUES (?1, 'test-token', '2026-01-01T00:00:00Z', ?2)
             ON CONFLICT(profile_id) DO UPDATE SET user_json = excluded.user_json",
            params![profile_id, format!("{{\"id\":\"{server_user_id}\"}}")],
        )
        .expect("session insert should work");
    }

    #[test]
    fn encryption_key_uses_sqlite_fallback_when_secure_store_is_unavailable() {
        let secure_store = TestSecureMasterKeyStore::unavailable();
        let store = test_store_with_secure_store("enc-key-fallback", secure_store.clone());
        let profile = store
            .get_active_profile()
            .expect("active profile query should work")
            .expect("active profile should exist");

        let storage = store
            .set_encryption_key_for_profile(profile.id, "k123")
            .expect("key should save");
        assert_eq!(storage, EncryptionKeyStorage::Sqlite);
        assert_eq!(
            store
                .encryption_key_storage_for_profile(profile.id)
                .expect("storage source should load"),
            Some(EncryptionKeyStorage::Sqlite)
        );
        let loaded = store
            .get_encryption_key_for_profile(profile.id)
            .expect("key should load");
        assert_eq!(loaded.as_deref(), Some("k123"));
    }

    #[test]
    fn encryption_key_prefers_secure_store_and_writes_sentinel() {
        let secure_store = TestSecureMasterKeyStore::available();
        let store = test_store_with_secure_store("enc-key-sentinel", secure_store.clone());
        let profile = store
            .get_active_profile()
            .expect("active profile query should work")
            .expect("active profile should exist");

        let storage = store
            .set_encryption_key_for_profile(profile.id, "k-keychain")
            .expect("key should save to secure store");
        assert_eq!(storage, EncryptionKeyStorage::Keychain);
        assert_eq!(
            store
                .encryption_key_storage_for_profile(profile.id)
                .expect("storage source should load"),
            Some(EncryptionKeyStorage::Keychain)
        );

        let conn = store.connect().expect("conn should open");
        let raw: Option<String> = conn
            .query_row(
                "SELECT master_key FROM encryption_keys WHERE profile_id = ?1",
                params![profile.id],
                |row| row.get(0),
            )
            .optional()
            .expect("query should work");
        assert_eq!(raw.as_deref(), Some(ENCRYPTION_KEY_KEYCHAIN_SENTINEL));
    }

    #[test]
    fn encryption_key_falls_back_to_sqlite_on_secure_store_mismatch() {
        let secure_store = TestSecureMasterKeyStore::with_mismatched_read("wrong-key");
        let store = test_store_with_secure_store("enc-key-mismatch", secure_store);
        let profile = store
            .get_active_profile()
            .expect("active profile query should work")
            .expect("active profile should exist");

        let storage = store
            .set_encryption_key_for_profile(profile.id, "expected-key")
            .expect("key should save using sqlite fallback");
        assert_eq!(storage, EncryptionKeyStorage::Sqlite);

        let conn = store.connect().expect("conn should open");
        let raw: Option<String> = conn
            .query_row(
                "SELECT master_key FROM encryption_keys WHERE profile_id = ?1",
                params![profile.id],
                |row| row.get(0),
            )
            .optional()
            .expect("query should work");
        assert_eq!(raw.as_deref(), Some("expected-key"));
    }

    #[test]
    fn encryption_key_returns_error_when_secure_store_write_fails_unexpectedly() {
        let secure_store = TestSecureMasterKeyStore::with_set_keychain_error();
        let store = test_store_with_secure_store("enc-key-set-hard-failure", secure_store);
        let profile = store
            .get_active_profile()
            .expect("active profile query should work")
            .expect("active profile should exist");

        let err = store
            .set_encryption_key_for_profile(profile.id, "expected-key")
            .expect_err("unexpected keychain error should not fallback to sqlite");
        assert!(matches!(err, StoreError::Keychain(_)));
    }

    #[test]
    fn sentinel_with_missing_keychain_entry_returns_error() {
        let secure_store = TestSecureMasterKeyStore::available();
        let store = test_store_with_secure_store("enc-key-missing-secure", secure_store);
        let profile = store
            .get_active_profile()
            .expect("active profile query should work")
            .expect("active profile should exist");
        store
            .set_sqlite_encryption_key_for_profile(profile.id, ENCRYPTION_KEY_KEYCHAIN_SENTINEL)
            .expect("sentinel row should save");

        let _err = store
            .get_encryption_key_for_profile(profile.id)
            .expect_err("missing keychain entry should error");
    }

    #[test]
    fn sentinel_with_missing_keychain_entry_returns_typed_error() {
        let secure_store = TestSecureMasterKeyStore::available();
        let store = test_store_with_secure_store("enc-key-missing-secure-typed", secure_store);
        let profile = store
            .get_active_profile()
            .expect("active profile query should work")
            .expect("active profile should exist");
        store
            .set_sqlite_encryption_key_for_profile(profile.id, ENCRYPTION_KEY_KEYCHAIN_SENTINEL)
            .expect("sentinel row should save");

        let err = store
            .get_encryption_key_for_profile(profile.id)
            .expect_err("missing keychain entry should error");
        assert!(
            matches!(err, StoreError::MissingKeychainMasterKey { profile_id } if profile_id == profile.id),
            "expected StoreError::MissingKeychainMasterKey"
        );
    }

    #[test]
    fn stable_account_hides_user_id_and_isolates_api_hosts() {
        let production = Profile {
            id: 1,
            name: "production".to_owned(),
            base_url: "https://dayone.me".to_owned(),
            is_active: true,
        };
        let staging = Profile {
            base_url: "https://stg.dayone.me".to_owned(),
            ..production.clone()
        };
        let user_id = "730889110";
        let production_account = secure_master_key_account_stable(&production, user_id);
        let staging_account = secure_master_key_account_stable(&staging, user_id);
        let other_user_account = secure_master_key_account_stable(&production, "different-user");

        assert_ne!(production_account, staging_account);
        assert_ne!(production_account, other_user_account);
        assert!(!production_account.contains(user_id));
    }

    // DAYONE-1100: the master key must survive a DB path change (backup restore,
    // machine migration, moved config dir) because it is keyed by the stable
    // server user id rather than the local DB path.
    #[test]
    fn master_key_under_stable_account_survives_db_path_change() {
        let secure_store = TestSecureMasterKeyStore::available();
        let uid = "730889110";

        // Store A (path 1): logged in, set the master key.
        let store_a = test_store_with_secure_store("enc-stable-a", secure_store.clone());
        let profile_a = store_a
            .get_active_profile()
            .expect("active profile query should work")
            .expect("active profile should exist");
        insert_test_session(&store_a, profile_a.id, uid);
        let storage = store_a
            .set_encryption_key_for_profile(profile_a.id, "master-key-abc")
            .expect("key should save to secure store");
        assert_eq!(storage, EncryptionKeyStorage::Keychain);

        // Store B: a DIFFERENT db path, same account (base_url + user id), sharing
        // the same keychain, carrying the sentinel + session a restored copy has.
        let store_b = test_store_with_secure_store("enc-stable-b", secure_store.clone());
        let profile_b = store_b
            .get_active_profile()
            .expect("active profile query should work")
            .expect("active profile should exist");
        assert_ne!(
            store_a.db_path(),
            store_b.db_path(),
            "the two stores must live at different db paths"
        );
        insert_test_session(&store_b, profile_b.id, uid);
        store_b
            .set_sqlite_encryption_key_for_profile(profile_b.id, ENCRYPTION_KEY_KEYCHAIN_SENTINEL)
            .expect("sentinel row should save");

        let loaded = store_b
            .get_encryption_key_for_profile(profile_b.id)
            .expect("key should load after a db path change");
        assert_eq!(
            loaded.as_deref(),
            Some("master-key-abc"),
            "path-independent stable account should resolve the key"
        );
    }

    #[test]
    fn stable_account_does_not_cross_user_boundaries() {
        let secure_store = TestSecureMasterKeyStore::available();
        let store = test_store_with_secure_store("enc-user-isolation", secure_store);
        let profile = store
            .get_active_profile()
            .expect("active profile query should work")
            .expect("active profile should exist");
        insert_test_session(&store, profile.id, "user-a");
        store
            .set_encryption_key_for_profile(profile.id, "user-a-key")
            .expect("user A key should save");
        store
            .connect()
            .expect("conn should open")
            .execute(
                "UPDATE auth_sessions SET user_json = '{\"id\":\"user-b\"}' WHERE profile_id = ?1",
                params![profile.id],
            )
            .expect("session update should work");

        assert!(matches!(
            store
                .get_encryption_key_for_profile(profile.id)
                .expect_err("user B must not read user A's key"),
            StoreError::MissingKeychainMasterKey { profile_id } if profile_id == profile.id
        ));
    }

    // Existing installs stored the key under the legacy path-derived account;
    // the first unlock after upgrade must find it and migrate it onto the stable
    // account so future path changes are safe.
    #[test]
    fn get_migrates_legacy_path_account_to_stable_account() {
        let secure_store = TestSecureMasterKeyStore::available();
        let store = test_store_with_secure_store("enc-migrate-stable", secure_store.clone());
        let profile = store
            .get_active_profile()
            .expect("active profile query should work")
            .expect("active profile should exist");
        let uid = "42";
        insert_test_session(&store, profile.id, uid);

        // Seed the key ONLY under the legacy path-derived account + set sentinel.
        let legacy_account =
            secure_master_key_account(&profile, &store.db_path().to_string_lossy());
        secure_store.insert(&legacy_account, "legacy-master-key");
        store
            .set_sqlite_encryption_key_for_profile(profile.id, ENCRYPTION_KEY_KEYCHAIN_SENTINEL)
            .expect("sentinel row should save");

        let loaded = store
            .get_encryption_key_for_profile(profile.id)
            .expect("key should load");
        assert_eq!(loaded.as_deref(), Some("legacy-master-key"));

        let stable_account = secure_master_key_account_stable(&profile, uid);
        assert_eq!(
            secure_store.get_master_key(&stable_account).expect("get"),
            Some("legacy-master-key".to_owned()),
            "key should be migrated onto the stable account"
        );
        assert_eq!(
            secure_store.get_master_key(&legacy_account).expect("get"),
            None,
            "legacy account should be cleared after migration"
        );
    }

    #[test]
    fn failed_stable_migration_keeps_the_legacy_key() {
        let secure_store = TestSecureMasterKeyStore::with_set_keychain_error();
        let store = test_store_with_secure_store("enc-migrate-failure", secure_store.clone());
        let profile = store
            .get_active_profile()
            .expect("active profile query should work")
            .expect("active profile should exist");
        insert_test_session(&store, profile.id, "42");
        let legacy_account =
            secure_master_key_account(&profile, &store.db_path().to_string_lossy());
        secure_store.insert(&legacy_account, "legacy-master-key");
        store
            .set_sqlite_encryption_key_for_profile(profile.id, ENCRYPTION_KEY_KEYCHAIN_SENTINEL)
            .expect("sentinel row should save");

        assert_eq!(
            store
                .get_encryption_key_for_profile(profile.id)
                .expect("legacy key should remain usable")
                .as_deref(),
            Some("legacy-master-key")
        );
        assert_eq!(
            secure_store
                .get_master_key(&legacy_account)
                .expect("legacy read should work")
                .as_deref(),
            Some("legacy-master-key")
        );
    }

    #[test]
    fn server_user_id_accepts_supported_session_shapes() {
        let store =
            test_store_with_secure_store("enc-uid-aliases", TestSecureMasterKeyStore::available());
        let profile = store
            .get_active_profile()
            .expect("active profile query should work")
            .expect("active profile should exist");
        insert_test_session(&store, profile.id, "initial");
        let conn = store.connect().expect("conn should open");

        for (user_json, expected) in [
            (r#"{"id":" 42 "}"#, "42"),
            (r#"{"id":null,"user_id":"43"}"#, "43"),
            (r#"{"id":"","userId":"44"}"#, "44"),
            (r#"{"id":45}"#, "45"),
            (r#"{"id":18446744073709551615}"#, "18446744073709551615"),
        ] {
            conn.execute(
                "UPDATE auth_sessions SET user_json = ?1 WHERE profile_id = ?2",
                params![user_json, profile.id],
            )
            .expect("session update should work");
            assert_eq!(
                store
                    .server_user_id_for_profile(profile.id)
                    .expect("session should parse")
                    .as_deref(),
                Some(expected)
            );
        }
    }

    #[test]
    fn session_without_user_id_does_not_select_a_legacy_account() {
        let store = test_store_with_secure_store(
            "enc-missing-session-user-id",
            TestSecureMasterKeyStore::available(),
        );
        let profile = store
            .get_active_profile()
            .expect("active profile query should work")
            .expect("active profile should exist");
        let conn = store.connect().expect("conn should open");
        conn.execute(
            "INSERT INTO auth_sessions (profile_id, token, token_created_at, user_json)
             VALUES (?1, 'tok', '2026-01-01T00:00:00Z', '{}')",
            params![profile.id],
        )
        .expect("session insert should work");

        let err = store
            .set_encryption_key_for_profile(profile.id, "master-key")
            .expect_err("session without a user id should fail");
        assert!(matches!(
            err,
            StoreError::InvalidAuthSessionUserId { profile_id } if profile_id == profile.id
        ));
    }

    #[test]
    fn malformed_session_does_not_silently_write_a_path_scoped_key() {
        let secure_store = TestSecureMasterKeyStore::available();
        let store = test_store_with_secure_store("enc-malformed-session", secure_store.clone());
        let profile = store
            .get_active_profile()
            .expect("active profile query should work")
            .expect("active profile should exist");
        let conn = store.connect().expect("conn should open");
        conn.execute(
            "INSERT INTO auth_sessions (profile_id, token, token_created_at, user_json)
             VALUES (?1, 'tok', '2026-01-01T00:00:00Z', 'not-json')",
            params![profile.id],
        )
        .expect("session insert should work");

        let err = store
            .set_encryption_key_for_profile(profile.id, "master-key")
            .expect_err("malformed session should fail");
        assert!(matches!(err, StoreError::Json(_)));
        assert!(
            secure_store
                .values
                .lock()
                .expect("test key store mutex should lock")
                .is_empty(),
            "no keychain account should be written"
        );
    }

    #[test]
    fn erase_local_data_deletes_stable_and_legacy_keychain_accounts() {
        let secure_store = TestSecureMasterKeyStore::available();
        let store = test_store_with_secure_store("enc-key-erase-stable", secure_store.clone());
        let profile = store
            .get_active_profile()
            .expect("active profile query should work")
            .expect("active profile should exist");
        let user_id = "730889110";
        insert_test_session(&store, profile.id, user_id);
        let stable_account = secure_master_key_account_stable(&profile, user_id);
        let legacy_account =
            secure_master_key_account(&profile, &store.db_path().to_string_lossy());
        secure_store.insert(&stable_account, "stable-key");
        secure_store.insert(&legacy_account, "legacy-key");

        store
            .erase_local_data()
            .expect("erase_local_data should succeed");

        assert_eq!(
            secure_store
                .get_master_key(&stable_account)
                .expect("stable read should work"),
            None
        );
        assert_eq!(
            secure_store
                .get_master_key(&legacy_account)
                .expect("legacy read should work"),
            None
        );
    }

    #[test]
    fn erase_local_data_deletes_known_keychain_accounts_best_effort() {
        let secure_store = TestSecureMasterKeyStore::with_delete_failure();
        let store = test_store_with_secure_store("enc-key-erase", secure_store);
        // Should not panic or fail even when keychain delete errors.
        store
            .erase_local_data()
            .expect("erase_local_data should still succeed");
    }

    #[test]
    fn migrates_legacy_plaintext_sqlite_key_to_keychain() {
        let secure_store = TestSecureMasterKeyStore::available();
        let store = test_store_with_secure_store("enc-key-migrate-plain", secure_store.clone());
        let profile = store
            .get_active_profile()
            .expect("active profile query should work")
            .expect("active profile should exist");
        store
            .set_sqlite_encryption_key_for_profile(profile.id, "legacy-plain")
            .expect("legacy row should save");

        let loaded = store
            .get_encryption_key_for_profile(profile.id)
            .expect("read should work");
        assert_eq!(loaded.as_deref(), Some("legacy-plain"));

        let account = store
            .secure_master_key_account_for_profile(profile.id)
            .expect("account should resolve");
        let secure_value = secure_store
            .get_master_key(&account)
            .expect("secure read should work");
        assert_eq!(secure_value.as_deref(), Some("legacy-plain"));
    }

    #[test]
    fn reads_legacy_account_and_migrates_to_current_account() {
        let secure_store = TestSecureMasterKeyStore::available();
        let store = test_store_with_secure_store("enc-key-legacy-account", secure_store.clone());
        let profile = store
            .get_active_profile()
            .expect("active profile query should work")
            .expect("active profile should exist");

        let legacy_account = legacy_secure_master_key_account(profile.id);
        secure_store.insert(&legacy_account, "legacy-key");
        store
            .set_sqlite_encryption_key_for_profile(profile.id, ENCRYPTION_KEY_KEYCHAIN_SENTINEL)
            .expect("sentinel row should save");

        let loaded = store
            .get_encryption_key_for_profile(profile.id)
            .expect("read should work");
        assert_eq!(loaded.as_deref(), Some("legacy-key"));

        let current_account = store
            .secure_master_key_account_for_profile(profile.id)
            .expect("account should resolve");
        let migrated = secure_store
            .get_master_key(&current_account)
            .expect("secure read should work");
        assert_eq!(migrated.as_deref(), Some("legacy-key"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn keychain_interaction_requires_local_terminal() {
        assert!(keychain_interaction_is_available_for(true, false));
        assert!(!keychain_interaction_is_available_for(false, false));
        assert!(!keychain_interaction_is_available_for(true, true));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn keychain_guard_disables_restores_and_serializes_interaction_policy() {
        use security_framework::os::macos::keychain::SecKeychain;
        use std::sync::{TryLockError, mpsc};

        assert!(
            SecKeychain::user_interaction_allowed().expect("keychain UI policy should be readable")
        );

        let (ready_tx, ready_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let headless = std::thread::spawn(move || {
            let _guard = KeychainOperationGuard::acquire_for_interactivity(false)
                .expect("headless keychain guard should initialize");
            assert!(
                !SecKeychain::user_interaction_allowed()
                    .expect("keychain UI policy should be readable")
            );
            ready_tx.send(()).expect("test receiver should remain");
            release_rx.recv().expect("test sender should remain");
        });
        ready_rx.recv().expect("headless guard should start");
        assert!(matches!(
            KEYCHAIN_OPERATION_LOCK.try_lock(),
            Err(TryLockError::WouldBlock)
        ));

        release_tx.send(()).expect("test receiver should remain");
        headless.join().expect("headless test thread should finish");
        let _interactive = KeychainOperationGuard::acquire_for_interactivity(true)
            .expect("interactive keychain guard should initialize");
        assert!(
            SecKeychain::user_interaction_allowed()
                .expect("interactive commands should retain keychain UI")
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn interaction_errors_are_actionable_only_when_ui_is_disabled() {
        for code in [ERR_SEC_AUTH_FAILED, ERR_SEC_INTERACTION_NOT_ALLOWED] {
            let err = map_keyring_error(
                keyring::Error::PlatformFailure(Box::new(
                    security_framework::base::Error::from_code(code),
                )),
                true,
            );
            assert!(matches!(err, StoreError::KeychainInteractionRequired));
            let message = err.to_string();
            assert!(message.contains("interactive terminal"));
            assert!(message.contains("Always Allow"));
        }

        let interactive_auth_failure = map_keyring_error(
            keyring::Error::PlatformFailure(Box::new(security_framework::base::Error::from_code(
                ERR_SEC_AUTH_FAILED,
            ))),
            false,
        );
        assert!(matches!(interactive_auth_failure, StoreError::Keychain(_)));

        let unrelated = map_keyring_error(
            keyring::Error::PlatformFailure(Box::new(security_framework::base::Error::from_code(
                -25309,
            ))),
            true,
        );
        assert!(matches!(unrelated, StoreError::Keychain(_)));
    }
}

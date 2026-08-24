use anyhow::Result;
use serde::Serialize;

use crate::store::sqlite::Store;

#[derive(Debug, Clone)]
pub struct AuthLogoutArgs {
    pub force: bool,
}

#[derive(Debug, Serialize)]
pub struct AuthLogoutOutput {
    pub ok: bool,
    pub base_url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

pub fn execute(store: &Store, base_url: &str, args: AuthLogoutArgs) -> Result<AuthLogoutOutput> {
    if !args.force {
        return Ok(AuthLogoutOutput {
            ok: false,
            base_url: base_url.to_owned(),
            warning: Some(
                "Logging out will delete all local data, including any entries that have not been synced.".to_owned(),
            ),
            hint: Some("Run `dayone auth logout --force` to confirm.".to_owned()),
        });
    }

    store.erase_local_data()?;

    Ok(AuthLogoutOutput {
        ok: true,
        base_url: base_url.to_owned(),
        warning: None,
        hint: None,
    })
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;
    use crate::store::sqlite::{STAGING_URL, Store};

    fn test_store_path(name: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time should be valid")
            .as_nanos();
        std::env::temp_dir().join(format!("dayone-cli-auth-logout-{name}-{unique}.db"))
    }

    fn store_at(path: &PathBuf) -> Store {
        let store = Store::open_at(path).expect("store should open");
        let profile = store
            .get_or_create_profile_for_base_url(STAGING_URL)
            .expect("profile should resolve");
        store
            .save_auth_session(profile.id, "tok", "2026-01-01T00:00:00Z", "{\"id\":1}")
            .expect("session should save");
        store
    }

    #[test]
    fn logout_without_force_returns_warning_and_does_nothing() {
        let path = test_store_path("logout-warn");
        let store = store_at(&path);
        assert!(path.exists(), "db should exist before logout");

        let out = execute(&store, STAGING_URL, AuthLogoutArgs { force: false })
            .expect("logout should succeed");

        assert!(!out.ok);
        assert!(out.warning.is_some());
        assert!(out.hint.is_some());
        assert!(
            path.exists(),
            "db should still exist after warning-only logout"
        );
    }

    #[test]
    fn logout_without_force_returns_correct_base_url() {
        let path = test_store_path("logout-warn-url");
        let store = store_at(&path);
        let out = execute(&store, STAGING_URL, AuthLogoutArgs { force: false })
            .expect("logout should succeed");
        assert_eq!(out.base_url, STAGING_URL);
    }

    #[test]
    fn logout_with_force_removes_db_file() {
        let path = test_store_path("logout-force");
        let store = store_at(&path);
        assert!(path.exists(), "db file should exist before logout");

        let out = execute(&store, STAGING_URL, AuthLogoutArgs { force: true })
            .expect("logout --force should succeed");

        assert!(out.ok);
        assert!(out.warning.is_none());
        assert!(
            !path.exists(),
            "db file should be removed after --force logout"
        );
    }

    #[test]
    fn logout_with_force_succeeds_when_no_session_exists() {
        let path = test_store_path("logout-force-empty");
        let store = Store::open_at(&path).expect("store should open");
        assert!(path.exists(), "db file should exist after open");

        let out = execute(&store, STAGING_URL, AuthLogoutArgs { force: true })
            .expect("logout --force with no session should not error");
        assert!(out.ok);
        assert!(
            !path.exists(),
            "db file should be removed after --force logout"
        );
    }
}

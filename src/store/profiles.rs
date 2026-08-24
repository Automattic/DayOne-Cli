use rusqlite::{Connection, OptionalExtension, params};
use sha2::{Digest, Sha256};

use crate::http::normalize_base_url;
use crate::store::sqlite::{AuthSession, Profile, Store};
use crate::store::{StoreError, StoreResult};

pub(crate) fn short_hash(value: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(value.as_bytes());
    let digest = hasher.finalize();
    format!("{:x}", digest)[0..12].to_owned()
}

pub(crate) fn user_id_from_session_json(user_json: &str) -> StoreResult<Option<String>> {
    let value: serde_json::Value = serde_json::from_str(user_json)?;
    Ok(user_id_from_value(&value))
}

pub(crate) fn user_id_from_value(value: &serde_json::Value) -> Option<String> {
    ["id", "user_id", "userId"]
        .iter()
        .find_map(|key| match value.get(key) {
            Some(serde_json::Value::String(id)) if !id.trim().is_empty() => {
                Some(id.trim().to_owned())
            }
            Some(serde_json::Value::Number(id)) if id.is_i64() || id.is_u64() => {
                Some(id.to_string())
            }
            _ => None,
        })
}

pub(crate) fn get_profile_by_name(conn: &Connection, name: &str) -> StoreResult<Option<Profile>> {
    conn.query_row(
        "SELECT id, name, base_url, is_active FROM profiles WHERE name = ?1 LIMIT 1",
        params![name],
        |row| {
            Ok(Profile {
                id: row.get(0)?,
                name: row.get(1)?,
                base_url: row.get(2)?,
                is_active: row.get::<_, i64>(3)? == 1,
            })
        },
    )
    .optional()
    .map_err(Into::into)
}

pub(crate) fn get_profile_by_base_url(
    conn: &Connection,
    base_url: &str,
) -> StoreResult<Option<Profile>> {
    conn.query_row(
        "SELECT id, name, base_url, is_active FROM profiles WHERE base_url = ?1 LIMIT 1",
        params![base_url],
        |row| {
            Ok(Profile {
                id: row.get(0)?,
                name: row.get(1)?,
                base_url: row.get(2)?,
                is_active: row.get::<_, i64>(3)? == 1,
            })
        },
    )
    .optional()
    .map_err(Into::into)
}

pub(crate) fn upsert_profile(
    conn: &Connection,
    name: &str,
    base_url: &str,
) -> StoreResult<Profile> {
    conn.execute(
        r#"
        INSERT INTO profiles (name, base_url, is_active, created_at, updated_at)
        VALUES (?1, ?2, 0, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)
        ON CONFLICT(name) DO UPDATE SET
          base_url = excluded.base_url,
          updated_at = CURRENT_TIMESTAMP
        "#,
        params![name, base_url],
    )?;
    get_profile_by_name(conn, name)?.ok_or_else(|| StoreError::NotFound {
        table: "profiles".to_owned(),
        id: name.to_owned(),
    })
}

impl Store {
    pub fn list_profiles(&self) -> StoreResult<Vec<Profile>> {
        let conn = self.connect()?;
        let mut stmt =
            conn.prepare("SELECT id, name, base_url, is_active FROM profiles ORDER BY name ASC")?;
        let rows = stmt.query_map([], |row| {
            Ok(Profile {
                id: row.get(0)?,
                name: row.get(1)?,
                base_url: row.get(2)?,
                is_active: row.get::<_, i64>(3)? == 1,
            })
        })?;

        let mut profiles = Vec::new();
        for profile in rows {
            profiles.push(profile?);
        }
        Ok(profiles)
    }

    pub fn get_active_profile(&self) -> StoreResult<Option<Profile>> {
        let conn = self.connect()?;
        conn.query_row(
            "SELECT id, name, base_url, is_active FROM profiles WHERE is_active = 1 LIMIT 1",
            [],
            |row| {
                Ok(Profile {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    base_url: row.get(2)?,
                    is_active: row.get::<_, i64>(3)? == 1,
                })
            },
        )
        .optional()
        .map_err(Into::into)
    }

    pub fn set_active_profile(
        &self,
        profile_name: &str,
        custom_base_url: Option<&str>,
    ) -> StoreResult<Profile> {
        let mut conn = self.connect()?;
        let tx = conn.transaction()?;

        let target_profile = if let Some(base_url) = custom_base_url {
            let normalized = normalize_base_url(base_url)
                .map_err(|err| StoreError::invalid_input(err.to_string()))?;
            match get_profile_by_base_url(&tx, &normalized)? {
                Some(existing) => existing,
                None => upsert_profile(&tx, profile_name, &normalized)?,
            }
        } else {
            get_profile_by_name(&tx, profile_name)?.ok_or_else(|| StoreError::NotFound {
                table: "profiles".to_owned(),
                id: profile_name.to_owned(),
            })?
        };

        tx.execute(
            "UPDATE profiles SET is_active = 0, updated_at = CURRENT_TIMESTAMP",
            [],
        )?;
        tx.execute(
            "UPDATE profiles SET is_active = 1, updated_at = CURRENT_TIMESTAMP WHERE id = ?1",
            params![target_profile.id],
        )?;
        tx.commit()?;
        self.get_active_profile()?
            .ok_or_else(|| StoreError::NotFound {
                table: "profiles".to_owned(),
                id: "active".to_owned(),
            })
    }

    pub fn get_or_create_profile_for_base_url(&self, base_url: &str) -> StoreResult<Profile> {
        let normalized = normalize_base_url(base_url)
            .map_err(|err| StoreError::invalid_input(err.to_string()))?;
        let mut conn = self.connect()?;
        let tx = conn.transaction()?;
        let existing = get_profile_by_base_url(&tx, &normalized)?;
        let profile = match existing {
            Some(profile) => profile,
            None => {
                let generated_name = format!("custom-{}", short_hash(&normalized));
                upsert_profile(&tx, &generated_name, &normalized)?
            }
        };
        tx.commit()?;
        Ok(profile)
    }

    /// Keep authentication attached to per-profile identity, even when the
    /// request URL is overridden. Legacy shared stores remain URL-keyed.
    pub fn resolve_profile_for_auth(&self, base_url: &str) -> StoreResult<Profile> {
        if !self.is_profile_store {
            return self.get_or_create_profile_for_base_url(base_url);
        }
        self.get_active_profile()?
            .ok_or_else(|| StoreError::NotFound {
                table: "profiles".to_owned(),
                id: "active".to_owned(),
            })
    }

    pub fn save_auth_session(
        &self,
        profile_id: i64,
        token: &str,
        token_created_at: &str,
        user_json: &str,
    ) -> StoreResult<()> {
        if token.trim().is_empty() {
            return Err(StoreError::invalid_input("cannot store empty auth token"));
        }

        let conn = self.connect()?;
        let incoming_user_id = user_id_from_session_json(user_json)?
            .ok_or(StoreError::InvalidAuthSessionUserId { profile_id })?;
        let existing_user_json = conn
            .query_row(
                "SELECT user_json FROM auth_sessions WHERE profile_id = ?1",
                params![profile_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        if let Some(existing_user_json) = existing_user_json {
            let existing_user_id = user_id_from_session_json(&existing_user_json)?
                .ok_or(StoreError::InvalidAuthSessionUserId { profile_id })?;
            if incoming_user_id != existing_user_id {
                return Err(StoreError::AuthSessionAccountMismatch { profile_id });
            }
        }

        conn.execute(
            r#"
            INSERT INTO auth_sessions (profile_id, token, token_created_at, user_json, created_at, updated_at)
            VALUES (?1, ?2, ?3, ?4, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)
            ON CONFLICT(profile_id) DO UPDATE SET
              token = excluded.token,
              token_created_at = excluded.token_created_at,
              user_json = excluded.user_json,
              updated_at = CURRENT_TIMESTAMP
            "#,
            params![profile_id, token.trim(), token_created_at, user_json],
        )?;
        Ok(())
    }

    /// Per-profile stores use their active profile's session so a runtime API
    /// host override does not change credential identity. Legacy shared stores
    /// retain exact-URL lookup.
    pub fn get_auth_session_for_base_url(
        &self,
        base_url: &str,
    ) -> StoreResult<Option<AuthSession>> {
        let normalized = normalize_base_url(base_url)
            .map_err(|err| StoreError::invalid_input(err.to_string()))?;
        let conn = self.connect()?;
        conn.query_row(
            r#"
            SELECT s.profile_id, s.token, s.token_created_at, s.user_json
            FROM auth_sessions s
            JOIN profiles p ON p.id = s.profile_id
            WHERE (?2 AND p.is_active = 1)
               OR (NOT ?2 AND p.base_url = ?1)
            ORDER BY s.updated_at DESC
            LIMIT 1
            "#,
            params![normalized, self.is_profile_store],
            |row| {
                Ok(AuthSession {
                    profile_id: row.get(0)?,
                    token: row.get(1)?,
                    token_created_at: row.get(2)?,
                    user_json: row.get(3)?,
                })
            },
        )
        .optional()
        .map_err(Into::into)
    }
}

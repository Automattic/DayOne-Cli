use anyhow::{Result, anyhow};
use serde::Serialize;
use std::path::Path;

use crate::config::{AppConfig, ProfileConfig};
use crate::http::normalize_base_url;

#[derive(Debug, Serialize)]
pub struct ProfileListOutput {
    pub ok: bool,
    pub profiles: Vec<ProfileRecord>,
}

#[derive(Debug, Serialize)]
pub struct ProfileSetOutput {
    pub ok: bool,
    pub active: ProfileRecord,
}

#[derive(Debug, Serialize)]
pub struct ProfileRecord {
    pub name: String,
    pub base_url: String,
    pub is_active: bool,
}

pub fn list(config: &AppConfig) -> Result<ProfileListOutput> {
    let profiles = config
        .profiles
        .iter()
        .map(|(name, pc)| ProfileRecord {
            name: name.clone(),
            base_url: pc.base_url.clone(),
            is_active: name == &config.active_profile,
        })
        .collect();
    Ok(ProfileListOutput { ok: true, profiles })
}

pub fn set(
    config_dir: &Path,
    config: &AppConfig,
    profile_name: &str,
    custom_base_url: Option<&str>,
) -> Result<ProfileSetOutput> {
    let name = profile_name.trim();
    if name.is_empty() {
        return Err(anyhow!("profile name cannot be empty"));
    }
    if !AppConfig::is_valid_profile_name(name) {
        return Err(anyhow!("invalid profile name: {name}"));
    }

    let mut config = config.clone();

    // A profile's API origin is part of its credential identity. Create a new
    // profile instead of rebinding an existing profile and its local data.
    if let Some(url) = custom_base_url {
        let normalized = normalize_base_url(url)?;
        if let Some(existing) = config.profiles.get(name) {
            if normalize_base_url(&existing.base_url)? != normalized {
                return Err(anyhow!(
                    "profile '{name}' already uses '{}'; create a new profile name for '{normalized}'",
                    existing.base_url
                ));
            }
        } else {
            config.profiles.insert(
                name.to_owned(),
                ProfileConfig {
                    base_url: normalized,
                },
            );
        }
    } else if !config.profiles.contains_key(name) {
        return Err(anyhow!("profile '{name}' not found"));
    }

    config.active_profile = name.to_owned();
    config.save(config_dir)?;

    let pc = config
        .profiles
        .get(name)
        .expect("profile just inserted/verified");
    Ok(ProfileSetOutput {
        ok: true,
        active: ProfileRecord {
            name: name.to_owned(),
            base_url: pc.base_url.clone(),
            is_active: true,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn test_dir(label: &str) -> TempDir {
        tempfile::Builder::new()
            .prefix(&format!("dayone-cli-profile-{label}-"))
            .tempdir()
            .expect("should create temp dir")
    }

    #[test]
    fn list_marks_active_correctly() {
        let config = AppConfig::default_config();
        let output = list(&config).unwrap();
        assert_eq!(output.ok, true);

        let active: Vec<_> = output.profiles.iter().filter(|p| p.is_active).collect();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].name, config.active_profile);

        let inactive: Vec<_> = output.profiles.iter().filter(|p| !p.is_active).collect();
        assert!(inactive.len() >= 1);
    }

    #[test]
    fn set_switches_to_existing_profile() {
        let dir = test_dir("set-existing");
        let config = AppConfig::default_config();
        config.save(dir.path()).unwrap();

        let output = set(dir.path(), &config, "production", None).unwrap();
        assert_eq!(output.ok, true);
        assert_eq!(output.active.name, "production");
        assert_eq!(output.active.is_active, true);

        // Verify persisted.
        let reloaded = AppConfig::load(dir.path()).unwrap();
        assert_eq!(reloaded.active_profile, "production");
    }

    #[test]
    fn set_creates_new_profile_with_url() {
        let dir = test_dir("set-new");
        let config = AppConfig::default_config();
        config.save(dir.path()).unwrap();

        let output = set(
            dir.path(),
            &config,
            "custom",
            Some("https://custom.example.com"),
        )
        .unwrap();
        assert_eq!(output.active.name, "custom");
        assert_eq!(output.active.base_url, "https://custom.example.com");

        let reloaded = AppConfig::load(dir.path()).unwrap();
        assert_eq!(reloaded.active_profile, "custom");
        assert!(reloaded.profiles.contains_key("custom"));
    }

    #[test]
    fn set_rejects_rebinding_existing_profile_url() {
        let dir = test_dir("set-rebind-url");
        let config = AppConfig::default_config();
        config.save(dir.path()).unwrap();

        let error = set(
            dir.path(),
            &config,
            "staging",
            Some("https://new-staging.example.com"),
        )
        .unwrap_err();
        assert!(error.to_string().contains("create a new profile name"));

        let reloaded = AppConfig::load(dir.path()).unwrap();
        assert_eq!(
            reloaded.profiles["staging"].base_url,
            crate::constants::STAGING_URL
        );
    }

    #[test]
    fn set_rejects_unknown_profile_without_url() {
        let dir = test_dir("set-unknown");
        let config = AppConfig::default_config();
        config.save(dir.path()).unwrap();

        let result = set(dir.path(), &config, "nonexistent", None);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not found"));
    }

    #[test]
    fn set_rejects_empty_name() {
        let dir = test_dir("set-empty");
        let config = AppConfig::default_config();

        let result = set(dir.path(), &config, "", None);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("empty"));
    }

    #[test]
    fn set_rejects_invalid_name() {
        let dir = test_dir("set-invalid");
        let config = AppConfig::default_config();

        let result = set(dir.path(), &config, "../escape", None);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("invalid"));
    }

    #[test]
    fn set_trims_whitespace() {
        let dir = test_dir("set-trim");
        let config = AppConfig::default_config();
        config.save(dir.path()).unwrap();

        let output = set(dir.path(), &config, "  staging  ", None).unwrap();
        assert_eq!(output.active.name, "staging");
    }
}

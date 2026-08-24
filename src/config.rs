use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;

use serde::{Deserialize, Serialize};

use crate::constants::{PRODUCTION_URL, STAGING_URL};

/// Per-profile configuration stored in `config.toml`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProfileConfig {
    pub base_url: String,
}

/// Install-level analytics configuration.
///
/// The anonymous id is generated once on first run and is stable across
/// profiles, mirroring the per-browser anonymous id used by Day One Web. It
/// identifies events fired before sign-in and is aliased to the Day One user
/// id on login.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct AnalyticsConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anonymous_id: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub notice_shown: bool,
}

/// Top-level application configuration persisted as TOML.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AppConfig {
    pub version: u32,
    pub active_profile: String,
    pub profiles: BTreeMap<String, ProfileConfig>,
    #[serde(default)]
    pub analytics: AnalyticsConfig,
}

const CONFIG_FILE_NAME: &str = "config.toml";

impl AppConfig {
    /// Returns the default config with staging + production profiles.
    /// The active profile is determined by the `production-default` feature flag.
    pub fn default_config() -> Self {
        let active = if cfg!(feature = "production-default") {
            "production"
        } else {
            "staging"
        };

        let mut profiles = BTreeMap::new();
        profiles.insert(
            "staging".to_owned(),
            ProfileConfig {
                base_url: STAGING_URL.to_owned(),
            },
        );
        profiles.insert(
            "production".to_owned(),
            ProfileConfig {
                base_url: PRODUCTION_URL.to_owned(),
            },
        );

        Self {
            version: 1,
            active_profile: active.to_owned(),
            profiles,
            analytics: AnalyticsConfig::default(),
        }
    }

    /// Load config from `{config_dir}/config.toml`.
    /// Returns the default config if the file does not exist.
    pub fn load(config_dir: &Path) -> Result<Self, ConfigError> {
        let path = config_dir.join(CONFIG_FILE_NAME);
        match fs::read_to_string(&path) {
            Ok(contents) => toml::from_str(&contents).map_err(|e| ConfigError::Parse(path, e)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default_config()),
            Err(e) => Err(ConfigError::Io(path, e)),
        }
    }

    /// Load config from `{config_dir}/config.toml`, persisting the default
    /// config to disk when the file does not yet exist.
    pub fn load_or_create(config_dir: &Path) -> Result<Self, ConfigError> {
        let path = config_dir.join(CONFIG_FILE_NAME);
        #[cfg(unix)]
        if path.exists() {
            secure_config_permissions(config_dir)?;
        }
        let config = Self::load(config_dir)?;
        if !path.exists() {
            config.save(config_dir)?;
        }
        Ok(config)
    }

    /// Print and persist the one-time telemetry disclosure.
    pub fn show_telemetry_notice_once(&mut self, config_dir: &Path) {
        if self.analytics.notice_shown {
            return;
        }
        eprintln!(
            "Day One CLI telemetry is enabled by default. It can send data about CLI use and reports about unexpected errors and crashes. Usage analytics is anonymous and is not linked to your Day One account. Telemetry does not include journal or entry content. Disable it with DO_NOT_TRACK=1 or DAYONE_TELEMETRY=0. You can save either setting in ~/.config/dayone/secrets-cli."
        );
        self.analytics.notice_shown = true;
        // Disclosure persistence must never make the user's command fail.
        let _ = self.save(config_dir);
    }

    /// Save config to `{config_dir}/config.toml`.
    ///
    /// Uses write-to-temp-then-rename with a backup to minimise the window
    /// where the config file could be missing. On Unix the rename is atomic;
    /// on Windows we back up the existing file first and restore it if the
    /// rename fails.
    pub fn save(&self, config_dir: &Path) -> Result<(), ConfigError> {
        fs::create_dir_all(config_dir).map_err(|e| ConfigError::Io(config_dir.to_owned(), e))?;
        #[cfg(unix)]
        set_owner_only_permissions(config_dir, 0o700)?;
        let path = config_dir.join(CONFIG_FILE_NAME);
        let contents =
            toml::to_string_pretty(self).map_err(|e| ConfigError::Serialize(path.clone(), e))?;

        // Use a per-call unique temp file name to avoid collisions from concurrent saves
        // (both cross-process via PID and intra-process via timestamp).
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let unique_suffix = format!("{}.{}", std::process::id(), timestamp);
        let tmp_path = config_dir.join(format!(".config.toml.{unique_suffix}.tmp"));
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        options.mode(0o600);
        use std::io::Write as _;
        options
            .open(&tmp_path)
            .and_then(|mut file| file.write_all(contents.as_bytes()))
            .map_err(|e| ConfigError::Io(tmp_path.clone(), e))?;

        // On Unix, rename atomically replaces the destination.
        // On Windows, rename fails if the destination exists, so we back up
        // the existing file and restore it if the rename fails.
        let backup_path = config_dir.join(format!(".config.toml.{unique_suffix}.bak"));
        let had_existing = path.exists();
        if had_existing {
            fs::rename(&path, &backup_path).map_err(|e| ConfigError::Io(path.clone(), e))?;
        }
        if let Err(e) = fs::rename(&tmp_path, &path) {
            // Restore backup so the old config isn't lost.
            if had_existing {
                let _ = fs::rename(&backup_path, &path);
            }
            let _ = fs::remove_file(&tmp_path);
            return Err(ConfigError::Io(path, e));
        }
        // Clean up the backup on success.
        if had_existing {
            let _ = fs::remove_file(&backup_path);
        }
        Ok(())
    }

    /// Returns the database directory for a given profile: `{config_dir}/profiles/{name}/`.
    pub fn profile_db_dir(config_dir: &Path, profile_name: &str) -> Result<PathBuf, ConfigError> {
        if !Self::is_valid_profile_name(profile_name) {
            return Err(ConfigError::InvalidProfileName(profile_name.to_owned()));
        }
        Ok(config_dir.join("profiles").join(profile_name))
    }

    /// Returns the database path for a given profile: `{config_dir}/profiles/{name}/dayone.db`.
    pub fn profile_db_path(config_dir: &Path, profile_name: &str) -> Result<PathBuf, ConfigError> {
        Self::profile_db_dir(config_dir, profile_name).map(|dir| dir.join("dayone.db"))
    }

    /// Validates that a profile name is safe for use as a filesystem directory name.
    ///
    /// Uses an allowlist approach: only ASCII alphanumerics, hyphens, underscores,
    /// and dots are permitted. Additionally rejects `.` and `..` to prevent
    /// directory traversal, trailing dots/spaces (invalid on Windows), and Windows
    /// reserved device names (e.g., `con`, `nul`, `com1`-`com9`, `lpt1`-`lpt9`)
    /// including when followed by an extension (e.g., `con.txt`).
    pub fn is_valid_profile_name(name: &str) -> bool {
        if name.is_empty() || name == "." || name == ".." {
            return false;
        }
        if name.ends_with('.') || name.ends_with(' ') {
            return false;
        }
        if !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
        {
            return false;
        }
        // Reject Windows reserved device names (case-insensitive), both bare
        // and when followed by an extension (e.g., `con.txt`).
        const WINDOWS_RESERVED: [&str; 22] = [
            "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7",
            "com8", "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
        ];
        let lowered = name.to_ascii_lowercase();
        let base = lowered.split('.').next().unwrap_or("");
        !WINDOWS_RESERVED.contains(&base)
    }

    /// Look up the active profile's config.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn active_profile_config(&self) -> Result<&ProfileConfig, ConfigError> {
        self.profiles
            .get(&self.active_profile)
            .ok_or_else(|| ConfigError::ProfileNotFound(self.active_profile.clone()))
    }

    /// Look up a profile by name.
    pub fn get_profile(&self, name: &str) -> Result<&ProfileConfig, ConfigError> {
        self.profiles
            .get(name)
            .ok_or_else(|| ConfigError::ProfileNotFound(name.to_owned()))
    }
}

#[cfg(unix)]
fn secure_config_permissions(config_dir: &Path) -> Result<(), ConfigError> {
    set_owner_only_permissions(config_dir, 0o700)?;
    set_owner_only_permissions(&config_dir.join(CONFIG_FILE_NAME), 0o600)
}

#[cfg(unix)]
fn set_owner_only_permissions(path: &Path, mode: u32) -> Result<(), ConfigError> {
    crate::util::set_owner_only_permissions(path, mode)
        .map_err(|error| ConfigError::Io(path.to_owned(), error))
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("failed to access config at {}: {}", .0.display(), .1)]
    Io(PathBuf, std::io::Error),

    #[error("failed to parse config at {}: {}", .0.display(), .1)]
    Parse(PathBuf, toml::de::Error),

    #[error("failed to serialize config for {}: {}", .0.display(), .1)]
    Serialize(PathBuf, toml::ser::Error),

    #[error("profile not found: {0}")]
    ProfileNotFound(String),

    #[error("invalid profile name: {0}")]
    InvalidProfileName(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    fn test_config_dir(name: &str) -> tempfile::TempDir {
        tempfile::Builder::new()
            .prefix(&format!("dayone-cli-config-{name}-"))
            .tempdir()
            .expect("should create temp dir")
    }

    #[test]
    fn default_config_has_staging_and_production() {
        let config = AppConfig::default_config();
        assert_eq!(config.version, 1);
        assert!(config.profiles.contains_key("staging"));
        assert!(config.profiles.contains_key("production"));
        assert_eq!(config.profiles["staging"].base_url, "https://stg.dayone.me");
        assert_eq!(config.profiles["production"].base_url, "https://dayone.me");
    }

    #[test]
    fn round_trip_toml() {
        let config = AppConfig::default_config();
        let serialized = toml::to_string_pretty(&config).unwrap();
        let deserialized: AppConfig = toml::from_str(&serialized).unwrap();
        assert_eq!(config, deserialized);
    }

    #[test]
    fn save_and_load() {
        let dir = test_config_dir("save-load");
        let config = AppConfig::default_config();
        config.save(dir.path()).unwrap();

        let loaded = AppConfig::load(dir.path()).unwrap();
        assert_eq!(config, loaded);
    }

    #[cfg(unix)]
    #[test]
    fn save_secures_config_file_and_directory() {
        let dir = test_config_dir("secure-permissions");
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o755)).unwrap();

        AppConfig::default_config().save(dir.path()).unwrap();

        assert_eq!(
            fs::metadata(dir.path()).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(dir.path().join("config.toml"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }

    #[test]
    fn load_missing_file_returns_default() {
        let dir = test_config_dir("missing");
        let loaded = AppConfig::load(dir.path()).unwrap();
        assert_eq!(loaded, AppConfig::default_config());
    }

    #[test]
    fn load_or_create_persists_default() {
        let dir = test_config_dir("load-or-create");
        assert!(!dir.path().join("config.toml").exists());

        let config = AppConfig::load_or_create(dir.path()).unwrap();
        assert_eq!(config, AppConfig::default_config());
        // File should now exist on disk.
        assert!(dir.path().join("config.toml").exists());

        // A subsequent plain load should return the same config.
        let reloaded = AppConfig::load(dir.path()).unwrap();
        assert_eq!(config, reloaded);
    }

    #[test]
    fn telemetry_notice_is_persisted_once() {
        let dir = test_config_dir("telemetry-notice");
        let mut config = AppConfig::load_or_create(dir.path()).unwrap();
        assert!(!config.analytics.notice_shown);

        config.show_telemetry_notice_once(dir.path());
        assert!(AppConfig::load(dir.path()).unwrap().analytics.notice_shown);

        config.show_telemetry_notice_once(dir.path());
        assert!(AppConfig::load(dir.path()).unwrap().analytics.notice_shown);
    }

    #[test]
    fn load_invalid_toml_returns_error() {
        let dir = test_config_dir("invalid");
        fs::write(dir.path().join("config.toml"), "not valid { toml").unwrap();
        assert!(AppConfig::load(dir.path()).is_err());
    }

    #[test]
    fn profile_db_path_layout() {
        let dir = PathBuf::from("home")
            .join("user")
            .join(".config")
            .join("dayone-cli");
        assert_eq!(
            AppConfig::profile_db_path(&dir, "staging").unwrap(),
            dir.join("profiles").join("staging").join("dayone.db")
        );
        assert_eq!(
            AppConfig::profile_db_dir(&dir, "production").unwrap(),
            dir.join("profiles").join("production")
        );
    }

    #[test]
    fn active_profile_config_found() {
        let config = AppConfig::default_config();
        let profile = config.active_profile_config().unwrap();
        assert!(!profile.base_url.is_empty());
    }

    #[test]
    fn get_profile_not_found() {
        let config = AppConfig::default_config();
        assert!(config.get_profile("nonexistent").is_err());
    }

    #[test]
    fn valid_profile_names() {
        // Allowed: alphanumeric, hyphens, underscores, dots
        assert!(AppConfig::is_valid_profile_name("staging"));
        assert!(AppConfig::is_valid_profile_name("my-profile"));
        assert!(AppConfig::is_valid_profile_name("user_1"));
        assert!(AppConfig::is_valid_profile_name("v2.0"));

        // Rejected: empty, traversal, separators, special chars
        assert!(!AppConfig::is_valid_profile_name(""));
        assert!(!AppConfig::is_valid_profile_name("."));
        assert!(!AppConfig::is_valid_profile_name(".."));
        assert!(!AppConfig::is_valid_profile_name("../other"));
        assert!(!AppConfig::is_valid_profile_name("a/b"));
        assert!(!AppConfig::is_valid_profile_name("a\\b"));
        assert!(!AppConfig::is_valid_profile_name("a:b"));
        assert!(!AppConfig::is_valid_profile_name("has space"));
        assert!(!AppConfig::is_valid_profile_name("a<b"));

        // Rejected: trailing dots (invalid on Windows)
        assert!(!AppConfig::is_valid_profile_name("name."));
        assert!(!AppConfig::is_valid_profile_name("test.."));

        // Rejected: Windows reserved device names (case-insensitive)
        assert!(!AppConfig::is_valid_profile_name("con"));
        assert!(!AppConfig::is_valid_profile_name("CON"));
        assert!(!AppConfig::is_valid_profile_name("nul"));
        assert!(!AppConfig::is_valid_profile_name("aux"));
        assert!(!AppConfig::is_valid_profile_name("com1"));
        assert!(!AppConfig::is_valid_profile_name("LPT1"));

        // Rejected: reserved names with extensions
        assert!(!AppConfig::is_valid_profile_name("con.txt"));
        assert!(!AppConfig::is_valid_profile_name("nul.db"));
    }

    #[test]
    fn profile_db_dir_rejects_invalid_name() {
        let dir = PathBuf::from("/tmp/test");
        let err = AppConfig::profile_db_dir(&dir, "../escape").unwrap_err();
        assert!(matches!(err, ConfigError::InvalidProfileName(_)));
    }

    #[test]
    fn load_or_create_does_not_clobber_existing() {
        let dir = test_config_dir("no-clobber");

        // Create and customise config.
        let mut config = AppConfig::load_or_create(dir.path()).unwrap();
        config.active_profile = "production".to_owned();
        config.save(dir.path()).unwrap();

        // A subsequent load_or_create must NOT overwrite the customised file.
        let reloaded = AppConfig::load_or_create(dir.path()).unwrap();
        assert_eq!(reloaded.active_profile, "production");
    }

    #[cfg(unix)]
    #[test]
    fn load_or_create_repairs_existing_config_permissions() {
        let dir = test_config_dir("repair-existing-permissions");
        AppConfig::default_config().save(dir.path()).unwrap();
        let path = dir.path().join(CONFIG_FILE_NAME);
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o755)).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();

        AppConfig::load_or_create(dir.path()).unwrap();

        assert_eq!(
            fs::metadata(dir.path()).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[cfg(unix)]
    #[test]
    fn load_or_create_refuses_symlinked_config_file() {
        use std::os::unix::fs::symlink;

        let dir = test_config_dir("symlink-config");
        let target = dir.path().join("target.toml");
        fs::write(
            &target,
            toml::to_string(&AppConfig::default_config()).unwrap(),
        )
        .unwrap();
        symlink(&target, dir.path().join(CONFIG_FILE_NAME)).unwrap();

        let error = AppConfig::load_or_create(dir.path()).unwrap_err();

        assert!(error.to_string().contains("symbolic link"));
    }

    #[test]
    fn save_creates_parent_directories() {
        let dir = test_config_dir("nested-parent");
        let nested = dir.path().join("a").join("b");
        let config = AppConfig::default_config();
        config.save(&nested).unwrap();

        let loaded = AppConfig::load(&nested).unwrap();
        assert_eq!(config, loaded);
    }
}

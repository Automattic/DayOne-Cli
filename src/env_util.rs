use std::path::{Path, PathBuf};

/// Loads `KEY=value` pairs from the user's `secrets-cli` file(s) (dotenv format) and
/// applies any that are not already set in the process environment.
///
/// Two locations are tried, in order, and the first hit per key wins:
///
/// 1. `~/.config/dayone/secrets-cli` — the documented, cross-platform path
///    (matches `README.md`, `AGENTS.md`, and `docs/get-started.md`). On Linux
///    this matches `dirs::config_dir()`'s default; on macOS and Windows it
///    differs and is the path most users expect after reading the docs.
/// 2. The platform-specific `dirs::config_dir().join("dayone")` (e.g.
///    `~/Library/Application Support/dayone` on macOS), so secrets shared
///    with the Day One desktop app are also picked up.
///
/// Parsing is delegated to the [`dotenvy`] crate (same family as `dotenv` /
/// `.env` loading). Existing environment variables are never overwritten.
pub fn apply_shared_dayone_secrets_from_config() {
    apply_shared_dayone_secrets_from_dirs(dirs::home_dir(), dirs::config_dir());
}

fn apply_shared_dayone_secrets_from_dirs(home: Option<PathBuf>, config: Option<PathBuf>) {
    let home_path = home.map(|h| h.join(".config").join("dayone").join("secrets-cli"));
    let config_path = config.map(|c| c.join("dayone").join("secrets-cli"));

    if let Some(path) = home_path.as_deref() {
        apply_shared_dayone_secrets_from_path(path);
    }
    if let Some(path) = config_path.as_deref()
        && home_path.as_deref() != Some(path)
    {
        apply_shared_dayone_secrets_from_path(path);
    }
}

fn apply_shared_dayone_secrets_from_path(path: &Path) {
    let Ok(iter) = dotenvy::from_path_iter(path) else {
        return;
    };
    for item in iter {
        let Ok((key, value)) = item else {
            continue;
        };
        if std::env::var_os(&key).is_none() {
            // SAFETY: Called synchronously at process startup before subcommands run.
            unsafe {
                std::env::set_var(key, value);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::{ENV_LOCK, with_env};

    #[test]
    fn secrets_cli_sets_unset_var() {
        with_env(&[("CLI_SECRETS_CLI_TEST", None)], || {
            let base =
                std::env::temp_dir().join(format!("dayone-cli-secrets-cli-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&base);
            std::fs::create_dir_all(base.join("dayone")).expect("mkdir");
            std::fs::write(
                base.join("dayone").join("secrets-cli"),
                "CLI_SECRETS_CLI_TEST=from_file\n",
            )
            .expect("write secrets-cli");

            apply_shared_dayone_secrets_from_path(&base.join("dayone").join("secrets-cli"));

            assert_eq!(
                std::env::var("CLI_SECRETS_CLI_TEST").expect("var set"),
                "from_file"
            );

            let _ = std::fs::remove_dir_all(&base);
        });
    }

    #[test]
    fn secrets_cli_loads_from_xdg_path_then_platform_path() {
        with_env(
            &[
                ("CLI_DUAL_XDG", None),
                ("CLI_DUAL_PLATFORM", None),
                ("CLI_DUAL_SHARED", None),
            ],
            || {
                let pid = std::process::id();
                let xdg_home = std::env::temp_dir().join(format!("dayone-cli-secrets-xdg-{pid}"));
                let platform_config =
                    std::env::temp_dir().join(format!("dayone-cli-secrets-plat-{pid}"));
                let _ = std::fs::remove_dir_all(&xdg_home);
                let _ = std::fs::remove_dir_all(&platform_config);

                std::fs::create_dir_all(xdg_home.join(".config").join("dayone"))
                    .expect("xdg mkdir");
                std::fs::create_dir_all(platform_config.join("dayone")).expect("platform mkdir");

                std::fs::write(
                    xdg_home.join(".config").join("dayone").join("secrets-cli"),
                    "CLI_DUAL_XDG=from_xdg\nCLI_DUAL_SHARED=xdg_wins\n",
                )
                .expect("write xdg");
                std::fs::write(
                    platform_config.join("dayone").join("secrets-cli"),
                    "CLI_DUAL_PLATFORM=from_platform\nCLI_DUAL_SHARED=platform_loses\n",
                )
                .expect("write platform");

                apply_shared_dayone_secrets_from_dirs(
                    Some(xdg_home.clone()),
                    Some(platform_config.clone()),
                );

                assert_eq!(std::env::var("CLI_DUAL_XDG").expect("xdg var"), "from_xdg");
                assert_eq!(
                    std::env::var("CLI_DUAL_PLATFORM").expect("platform var"),
                    "from_platform"
                );
                assert_eq!(
                    std::env::var("CLI_DUAL_SHARED").expect("shared var"),
                    "xdg_wins",
                    "XDG path is loaded first and must win for keys present in both files"
                );

                let _ = std::fs::remove_dir_all(&xdg_home);
                let _ = std::fs::remove_dir_all(&platform_config);
            },
        );
    }

    #[test]
    fn secrets_cli_does_not_override_existing() {
        let _guard = ENV_LOCK.lock().expect("env test lock");

        let base = std::env::temp_dir().join(format!(
            "dayone-cli-secrets-cli-override-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("dayone")).expect("mkdir");
        std::fs::write(
            base.join("dayone").join("secrets-cli"),
            "CLI_SECRETS_CLI_OVERRIDE=from_file\n",
        )
        .expect("write secrets-cli");

        unsafe {
            std::env::set_var("CLI_SECRETS_CLI_OVERRIDE", "preset");
        }
        apply_shared_dayone_secrets_from_path(&base.join("dayone").join("secrets-cli"));

        assert_eq!(
            std::env::var("CLI_SECRETS_CLI_OVERRIDE").expect("var set"),
            "preset"
        );

        unsafe {
            std::env::remove_var("CLI_SECRETS_CLI_OVERRIDE");
        }
    }
}

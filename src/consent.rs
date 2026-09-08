//! Worldwide opt-in for optional analytics and error reporting.

use std::io::{self, BufRead, IsTerminal, Write};
use std::path::Path;

use crate::config::{AppConfig, ConsentState};

// Increase this when the disclosed purposes or data change materially.
pub const DISCLOSURE_VERSION: u32 = 1;
// Use the published repository document until the website page is available.
const TELEMETRY_URL: &str = "https://github.com/Automattic/DayOne-Cli/blob/main/docs/telemetry.md";

pub fn print_disclosure(output: &mut impl Write) -> io::Result<()> {
    writeln!(
        output,
        "Day One CLI telemetry is optional. Usage analytics sent to Automattic Tracks includes a random installation ID, command categories, outcomes, version, device information, sign-in status, subscription tier, and approximate location derived from your IP address. The installation ID is pseudonymous personal data, not linked to your Day One account. Analytics excludes journal content. Sentry error reports contain filtered error details; HTTP response bodies are removed, but other server-provided error text can remain. You can withdraw consent with `dayone telemetry disable`, DO_NOT_TRACK=1, or DAYONE_TELEMETRY=0. Details: {TELEMETRY_URL}"
    )
}

/// Recheck before collection so withdrawal also affects a running command.
/// A missing or unreadable configuration fails closed. A later agreement
/// does not reactivate a process started under an older agreement.
pub fn still_permitted(config_dir: &Path, recorded_at_ms: i64) -> bool {
    crate::telemetry::is_enabled()
        && AppConfig::load(config_dir).is_ok_and(|config| {
            config.analytics.consent_granted()
                && config.analytics.consent_recorded_at_ms == Some(recorded_at_ms)
        })
}

/// Save before enabling any collector. A failed write leaves the in-memory
/// decision unchanged, so an unrecorded agreement cannot authorize collection.
pub fn save_decision(
    config: &mut AppConfig,
    config_dir: &Path,
    decision: ConsentState,
) -> anyhow::Result<()> {
    *config = AppConfig::update(config_dir, |updated| {
        updated.analytics.consent = decision;
        updated.analytics.consent_version = DISCLOSURE_VERSION;
        updated.analytics.consent_recorded_at_ms = Some(
            crate::util::now_epoch_ms().max(
                updated
                    .analytics
                    .consent_recorded_at_ms
                    .unwrap_or(0)
                    .saturating_add(1),
            ),
        );
        updated.analytics.notice_version = DISCLOSURE_VERSION;
        // A new decision starts a new identity and excludes prior queued events.
        updated.analytics.anonymous_id = None;
        Ok(())
    })?;
    Ok(())
}

/// Optional telemetry must not block the command, consume piped command input,
/// or treat a previous notice, EOF, or an invalid answer as consent.
pub fn resolve(config: &mut AppConfig, config_dir: &Path) {
    if !crate::telemetry::is_enabled()
        || config.analytics.consent_granted()
        || config.analytics.consent == ConsentState::Denied
    {
        return;
    }

    if io::stdin().is_terminal() && io::stderr().is_terminal() {
        match prompt(&mut io::stdin().lock(), &mut io::stderr().lock()) {
            Ok(Some(decision)) => {
                if save_decision(config, config_dir, decision).is_err() {
                    eprintln!("Telemetry remains off: the consent decision could not be saved.");
                }
            }
            Ok(None) | Err(_) => {
                eprintln!("Telemetry remains off: no decision was recorded.");
            }
        }
    } else if config.analytics.notice_version != DISCLOSURE_VERSION {
        // A versioned notice replaces the legacy 'anonymous' disclosure even
        // for installations that already have notice_shown=true.
        let mut stderr = io::stderr().lock();
        if writeln!(
            stderr,
            "Telemetry is off until you opt in with `dayone telemetry enable`."
        )
        .and_then(|_| print_disclosure(&mut stderr))
        .is_ok()
            && let Ok(updated) = AppConfig::update(config_dir, |updated| {
                updated.analytics.notice_version = DISCLOSURE_VERSION;
                Ok(())
            })
        {
            *config = updated;
        }
    }
}

fn prompt(input: &mut impl BufRead, output: &mut impl Write) -> io::Result<Option<ConsentState>> {
    print_disclosure(output)?;
    loop {
        write!(output, "Enable optional telemetry? [y/N]: ")?;
        output.flush()?;
        let mut line = String::new();
        if input.read_line(&mut line)? == 0 {
            return Ok(None);
        }
        match line.trim().to_ascii_lowercase().as_str() {
            "y" | "yes" => return Ok(Some(ConsentState::Granted)),
            "" | "n" | "no" => return Ok(Some(ConsentState::Denied)),
            _ => writeln!(
                output,
                "Enter yes or no. Telemetry remains off until you choose."
            )?,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_affirmative_answers_grant_consent() {
        for (answer, expected) in [
            ("yes\n", Some(ConsentState::Granted)),
            (" Y \n", Some(ConsentState::Granted)),
            ("no\n", Some(ConsentState::Denied)),
            ("\n", Some(ConsentState::Denied)),
            ("", None),
            ("maybe\n", None),
            ("maybe\nno\n", Some(ConsentState::Denied)),
        ] {
            assert_eq!(
                prompt(&mut answer.as_bytes(), &mut Vec::new()).unwrap(),
                expected
            );
        }
    }

    #[test]
    fn failed_persistence_cannot_grant_consent() {
        let dir = tempfile::tempdir().unwrap();
        let blocker = dir.path().join("not-a-directory");
        std::fs::write(&blocker, "blocked").unwrap();
        let mut config = AppConfig::default_config();
        let before = config.clone();
        assert!(save_decision(&mut config, &blocker, ConsentState::Granted).is_err());
        assert_eq!(config, before);
        assert!(!config.analytics.consent_granted());
    }

    #[test]
    fn profile_change_cannot_restore_a_stale_agreement() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = AppConfig::default_config();
        save_decision(&mut config, dir.path(), ConsentState::Granted).unwrap();
        save_decision(&mut config, dir.path(), ConsentState::Denied).unwrap();
        crate::commands::profile::set(dir.path(), "staging", None).unwrap();
        assert_eq!(
            AppConfig::load(dir.path()).unwrap().analytics.consent,
            ConsentState::Denied
        );
    }

    #[test]
    fn consent_change_preserves_profiles_added_since_config_was_loaded() {
        let dir = tempfile::tempdir().unwrap();
        let mut stale = AppConfig::load_or_create(dir.path()).unwrap();
        crate::commands::profile::set(dir.path(), "new-profile", Some("https://example.test"))
            .unwrap();
        save_decision(&mut stale, dir.path(), ConsentState::Denied).unwrap();
        assert!(
            AppConfig::load(dir.path())
                .unwrap()
                .profiles
                .contains_key("new-profile")
        );
    }

    #[test]
    fn choices_round_trip_with_version_and_timestamp() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = AppConfig::default_config();
        for decision in [ConsentState::Granted, ConsentState::Denied] {
            save_decision(&mut config, dir.path(), decision).unwrap();
            let loaded = AppConfig::load(dir.path()).unwrap();
            assert_eq!(loaded, config);
            assert_eq!(
                loaded.analytics.consent_granted(),
                decision == ConsentState::Granted
            );
            assert_eq!(loaded.analytics.consent_version, DISCLOSURE_VERSION);
            assert!(loaded.analytics.consent_recorded_at_ms.unwrap() > 0);
        }
    }
}

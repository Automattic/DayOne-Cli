//! Shared utilities for binary-crate tests.
//!
//! [`std::env::set_var`] / [`std::env::remove_var`] are `unsafe` precisely
//! because the process environment is shared mutable state. When `cargo test`
//! runs cases in parallel, mutating env vars from multiple test threads is
//! Undefined Behaviour unless serialised by a single global lock — module-
//! local locks are not enough because they don't synchronise across modules.
//!
//! Every test in this crate that mutates env vars MUST go through
//! [`with_env`] (or at minimum lock [`ENV_LOCK`] for the duration of the
//! mutation + read).

use std::ffi::OsString;
use std::sync::Mutex;

/// Crate-wide lock for tests that mutate process env vars.
pub static ENV_LOCK: Mutex<()> = Mutex::new(());

/// Run `f` with the named env vars set to the given values, restoring the
/// original values afterwards. The crate-wide [`ENV_LOCK`] is held for the
/// duration so concurrent tests in other modules cannot observe or interfere
/// with the temporary state.
///
/// `Some(value)` sets the variable; `None` removes it. Originals are
/// snapshotted via [`std::env::var_os`] so non-UTF-8 values round-trip
/// losslessly; using `var` here would silently drop such values on restore.
pub fn with_env<F: FnOnce()>(vars: &[(&str, Option<&str>)], f: F) {
    let _guard = ENV_LOCK.lock().expect("env lock");
    let saved: Vec<(String, Option<OsString>)> = vars
        .iter()
        .map(|(key, _)| ((*key).to_owned(), std::env::var_os(key)))
        .collect();
    for (key, value) in vars {
        unsafe {
            match value {
                Some(v) => std::env::set_var(key, v),
                None => std::env::remove_var(key),
            }
        }
    }
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
    for (key, original) in saved {
        unsafe {
            match original {
                Some(v) => std::env::set_var(&key, v),
                None => std::env::remove_var(&key),
            }
        }
    }
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}

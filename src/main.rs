mod analytics;
mod auth;
mod cli;
mod commands;
mod comment_codec;
mod comment_feed;
mod comment_flags;
mod config;
mod consent;
mod constants;
mod diagnostics;
mod entry;
mod entry_embeddings;
mod env_util;
mod http;
mod models;
mod store;
mod sync;
mod telemetry;
#[cfg(test)]
mod test_util;
mod tui;
mod updates;
mod util;

pub use dayone::convert;

#[tokio::main]
async fn main() -> std::process::ExitCode {
    if std::env::args_os().skip(1).eq([updates::WORKER_COMMAND]) {
        updates::refresh_worker().await;
        return std::process::ExitCode::SUCCESS;
    }
    // Hold the guard for the lifetime of `main` so Sentry's transport flushes
    // when `main` returns. `std::process::exit` would skip destructors.
    let mut telemetry = telemetry::TelemetryGuard::default();
    let result = if let Some(result) = cli::run_updates().await {
        result
    } else {
        crate::env_util::apply_shared_dayone_secrets_from_config();
        diagnostics::install_panic_hook();
        cli::run(&mut telemetry).await
    };
    let sentry_event_id = match &result {
        Ok(()) => None,
        Err(err) => {
            eprintln!("{err:#}");
            telemetry::capture_anyhow(err)
        }
    };
    diagnostics::finish(&result, sentry_event_id.as_deref());

    if result.is_ok() {
        std::process::ExitCode::SUCCESS
    } else {
        std::process::ExitCode::FAILURE
    }
}

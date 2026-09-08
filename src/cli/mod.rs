use anyhow::{Context, Result, anyhow};
use clap::{Args, Parser, Subcommand, ValueEnum};
use serde::Serialize;
use serde_json::Value;
use std::collections::HashSet;
use std::io::Write;
use std::path::PathBuf;
use std::time::Instant;

use crate::commands::{
    auth_key_set, auth_login, auth_logout, auth_setup, comment, context_item_write, daily_chat_add,
    doctor, embeddings, entry_delete, entry_read, entry_write, journal_create, journal_update,
    list, memory_process, outbox, profile, search, sync, sync_schedule, user_settings_get,
};
use crate::config::AppConfig;
use crate::http::normalize_base_url;
use crate::store::sqlite::{EntryListSort, EntryListSortMethod, Store};
use crate::tui;

#[derive(Debug, Parser)]
#[command(name = "dayone", about = "Day One CLI", version)]
struct Cli {
    /// Override the API host for this run without changing the selected profile.
    #[arg(long, global = true, env = "DAYONE_API_HOST")]
    api_host: Option<String>,

    /// Use a specific profile instead of the active one.
    #[arg(long, global = true)]
    profile: Option<String>,

    /// Write a privacy-safe JSONL diagnostic trace for this invocation.
    #[arg(long, global = true, value_name = "PATH")]
    trace_file: Option<PathBuf>,

    #[command(subcommand)]
    command: TopLevelCommand,
}

#[derive(Debug, Subcommand)]
enum TopLevelCommand {
    Setup,
    Auth(AuthCommand),
    UserSettings(UserSettingsCommand),
    Profile(ProfileCommand),
    /// Manage optional usage analytics and error reporting. No network access.
    Telemetry(TelemetryCommand),
    Journal(JournalCommand),
    Entry(EntryCommand),
    Embeddings(EmbeddingsCommand),
    DailyChat(DailyChatCommand),
    Memory(MemoryCommand),
    ContextItem(ContextItemCommand),
    Comment(CommentCommand),
    List(ListCommand),
    Search(SearchCommand),
    Sync(SyncCliArgs),
    /// Inspect and recover the local sync outbox (offline; no network access).
    Outbox(OutboxCommand),
    /// Manage automatic background sync scheduling for the active profile.
    SyncSchedule(SyncScheduleCommand),
    /// Launch the interactive terminal UI: browse journals, entries, and daily chats;
    /// create, edit, and delete entries with save prompts.
    Tui,
    /// Report environment and known local-state problems without modifying user data.
    Doctor(DoctorCliArgs),
}

#[derive(Debug, Args)]
struct TelemetryCommand {
    #[command(subcommand)]
    command: TelemetrySubcommand,
}

#[derive(Debug, Subcommand)]
enum TelemetrySubcommand {
    /// Agree to the telemetry disclosure and enable optional reporting.
    Enable,
    /// Withdraw consent for analytics and error reporting.
    Disable,
    /// Show the saved choice and environment override without collecting data.
    Status,
}

#[derive(Debug, Args)]
struct DoctorCliArgs {
    /// Include raw journal and entry IDs. The report is not safe to share without review.
    #[arg(long, default_value_t = false)]
    include_private_identifiers: bool,
}

#[derive(Debug, Subcommand)]
enum AuthSubcommand {
    Login(AuthLoginCliArgs),
    Logout(AuthLogoutCliArgs),
    KeySet(AuthKeySetCliArgs),
    Whoami,
}

#[derive(Debug, Args)]
struct AuthCommand {
    #[command(subcommand)]
    command: AuthSubcommand,
}

#[derive(Debug, Args)]
struct AuthLoginCliArgs {
    #[arg(long)]
    email: String,

    #[arg(long, default_value_t = false)]
    password_stdin: bool,
}

#[derive(Debug, Args)]
struct AuthLogoutCliArgs {
    #[arg(long, default_value_t = false)]
    force: bool,
}

#[derive(Debug, Args)]
#[group(multiple = false)]
struct AuthKeySetCliArgs {
    #[arg(long)]
    key: Option<String>,

    #[arg(long)]
    key_stdin: bool,
}

#[derive(Debug, Subcommand)]
enum UserSettingsSubcommand {
    Get,
}

#[derive(Debug, Args)]
struct UserSettingsCommand {
    #[command(subcommand)]
    command: UserSettingsSubcommand,
}

#[derive(Debug, Subcommand)]
enum ProfileSubcommand {
    List,
    Set(ProfileSetCliArgs),
}

#[derive(Debug, Args)]
struct ProfileCommand {
    #[command(subcommand)]
    command: ProfileSubcommand,
}

#[derive(Debug, Args)]
struct ProfileSetCliArgs {
    profile: String,

    #[arg(long)]
    api_host: Option<String>,
}

#[derive(Debug, Subcommand)]
enum EntrySubcommand {
    Write(EntryWriteCliArgs),
    /// Fetch a single entry from the local store as full JSON. No network.
    Read(EntryReadCliArgs),
    Delete(EntryDeleteCliArgs),
}

#[derive(Debug, Subcommand)]
enum DailyChatSubcommand {
    Add(DailyChatAddCliArgs),
}

#[derive(Debug, Subcommand)]
enum MemorySubcommand {
    Process(MemoryProcessCliArgs),
}

#[derive(Debug, Subcommand)]
enum JournalSubcommand {
    Create(JournalCreateCliArgs),
    /// Update an existing journal's metadata. Applies locally and queues a sync.
    Update(JournalUpdateCliArgs),
}

#[derive(Debug, Args)]
struct JournalCommand {
    #[command(subcommand)]
    command: JournalSubcommand,
}

#[derive(Debug, Args)]
struct JournalCreateCliArgs {
    #[arg(long, conflicts_with = "json_file")]
    json: Option<String>,

    #[arg(long)]
    json_file: Option<String>,

    #[arg(long)]
    name: Option<String>,

    #[arg(long)]
    description: Option<String>,

    #[arg(long)]
    color: Option<String>,

    #[arg(
        long,
        default_value_t = false,
        help = "Create as a shared journal (requires E2E encryption via --e2e or an encrypted payload supplied with --json/--json-file)."
    )]
    shared: bool,

    #[arg(
        long,
        default_value_t = false,
        help = "Generate E2E encryption payload (requires `dayone auth key-set` and a prior `dayone sync`)."
    )]
    e2e: bool,

    #[arg(
        long,
        alias = "no-e2e",
        default_value_t = false,
        conflicts_with = "e2e"
    )]
    plaintext: bool,

    #[arg(long)]
    sort_method: Option<String>,

    #[arg(long)]
    hide_on_this_day: Option<bool>,

    #[arg(long)]
    hide_all_entries: Option<bool>,

    #[arg(long)]
    conceal: Option<bool>,

    #[arg(long)]
    add_location_to_new_entries: Option<bool>,

    #[arg(long)]
    comments_disabled: Option<bool>,

    #[arg(long)]
    template_id: Option<String>,

    #[arg(long)]
    preset_id: Option<String>,
}

#[derive(Debug, Args)]
struct JournalUpdateCliArgs {
    #[arg(long = "journal-id", help = "Id of the journal to update.")]
    journal_id: String,

    #[arg(
        long,
        conflicts_with = "json_file",
        help = "Arbitrary journal fields as a JSON object."
    )]
    json: Option<String>,

    #[arg(long, help = "Path to a JSON file with journal fields to update.")]
    json_file: Option<String>,

    #[arg(long)]
    name: Option<String>,

    #[arg(long)]
    description: Option<String>,

    #[arg(long)]
    color: Option<String>,

    #[arg(long)]
    sort_method: Option<String>,

    #[arg(long)]
    hide_on_this_day: Option<bool>,

    #[arg(long)]
    hide_all_entries: Option<bool>,

    #[arg(long)]
    conceal: Option<bool>,

    #[arg(long)]
    add_location_to_new_entries: Option<bool>,

    #[arg(long)]
    comments_disabled: Option<bool>,

    #[arg(long)]
    template_id: Option<String>,

    #[arg(long)]
    preset_id: Option<String>,
}

#[derive(Debug, Subcommand)]
enum ContextItemSubcommand {
    Put(ContextItemPutCliArgs),
    Delete(ContextItemDeleteCliArgs),
}

#[derive(Debug, Subcommand)]
enum CommentSubcommand {
    List(CommentListCliArgs),
    Write(CommentWriteCliArgs),
    Update(CommentUpdateCliArgs),
    Delete(CommentDeleteCliArgs),
    React(CommentReactCliArgs),
    Unreact(CommentUnreactCliArgs),
}

#[derive(Debug, Args)]
struct ContextItemCommand {
    #[command(subcommand)]
    command: ContextItemSubcommand,
}

#[derive(Debug, Args)]
struct CommentCommand {
    #[command(subcommand)]
    command: CommentSubcommand,
}

#[derive(Debug, Args)]
struct EntryCommand {
    #[command(subcommand)]
    command: EntrySubcommand,
}

#[derive(Debug, Subcommand)]
enum EmbeddingsSubcommand {
    RecalculateEntries(RecalculateEntriesCliArgs),
}

#[derive(Debug, Args)]
struct EmbeddingsCommand {
    #[command(subcommand)]
    command: EmbeddingsSubcommand,
}

#[derive(Debug, Args)]
struct RecalculateEntriesCliArgs {
    #[arg(long)]
    journal_id: Option<String>,

    #[arg(long)]
    entry_id: Option<String>,
}

#[derive(Debug, Args)]
struct DailyChatCommand {
    #[command(subcommand)]
    command: DailyChatSubcommand,
}

#[derive(Debug, Args)]
struct MemoryCommand {
    #[command(subcommand)]
    command: MemorySubcommand,
}

#[derive(Debug, Args)]
struct DailyChatAddCliArgs {
    #[arg(
        long,
        required_unless_present = "message_stdin",
        conflicts_with = "message_stdin"
    )]
    message: Option<String>,

    #[arg(long)]
    message_stdin: bool,

    #[arg(long)]
    date: Option<String>,
}

#[derive(Debug, Args)]
#[group(required = true, multiple = false)]
struct MemoryProcessCliArgs {
    #[arg(long)]
    json: Option<String>,

    #[arg(long)]
    json_file: Option<String>,
}

#[derive(Debug, Args)]
struct EntryWriteCliArgs {
    #[arg(long)]
    journal_id: String,

    #[arg(
        long,
        required_unless_present = "body_stdin",
        conflicts_with = "body_stdin"
    )]
    body: Option<String>,

    #[arg(long)]
    body_stdin: bool,

    #[arg(long)]
    entry_id: Option<String>,

    #[arg(long)]
    date: Option<String>,

    #[arg(long, default_value_t = false)]
    all_day: bool,

    #[arg(long = "attach")]
    attachments: Vec<String>,

    #[arg(long = "attach-type", value_enum)]
    attachment_types: Vec<AttachmentTypeCli>,
}

#[derive(Debug, Args)]
struct EntryReadCliArgs {
    #[arg(long)]
    journal_id: String,

    #[arg(long)]
    entry_id: String,
}

#[derive(Debug, Args)]
struct EntryDeleteCliArgs {
    #[arg(long)]
    journal_id: String,

    #[arg(long)]
    entry_id: String,
}

#[derive(Debug, Args)]
#[group(required = true, multiple = false)]
struct ContextItemPutCliArgs {
    #[arg(long)]
    json: Option<String>,

    #[arg(long)]
    json_file: Option<String>,
}

#[derive(Debug, Args)]
struct ContextItemDeleteCliArgs {
    #[arg(long)]
    id: String,

    #[arg(long)]
    updated_at: String,

    #[arg(long)]
    deleted_at: Option<String>,
}

#[derive(Debug, Args)]
struct CommentListCliArgs {
    #[arg(long)]
    journal_id: String,
    #[arg(long)]
    entry_id: String,
    #[command(flatten)]
    pagination: ListPaginationCliArgs,
    #[arg(long, default_value_t = false)]
    refresh: bool,
}

#[derive(Debug, Args)]
struct CommentWriteCliArgs {
    #[arg(long)]
    journal_id: String,
    #[arg(long)]
    entry_id: String,
    #[arg(long)]
    body: String,
}

#[derive(Debug, Args)]
struct CommentUpdateCliArgs {
    #[arg(long)]
    journal_id: String,
    #[arg(long)]
    entry_id: String,
    #[arg(long)]
    comment_id: String,
    #[arg(long)]
    body: String,
}

#[derive(Debug, Args)]
struct CommentDeleteCliArgs {
    #[arg(long)]
    journal_id: String,
    #[arg(long)]
    entry_id: String,
    #[arg(long)]
    comment_id: String,
}

#[derive(Debug, Args)]
struct CommentReactCliArgs {
    #[arg(long)]
    journal_id: String,
    #[arg(long)]
    entry_id: String,
    #[arg(long)]
    comment_id: String,
    #[arg(long, default_value = "like")]
    reaction: String,
}

#[derive(Debug, Args)]
struct CommentUnreactCliArgs {
    #[arg(long)]
    journal_id: String,
    #[arg(long)]
    entry_id: String,
    #[arg(long)]
    comment_id: String,
}

#[derive(Debug, Subcommand)]
enum ListSubcommand {
    Journals(ListJournalsCliArgs),
    Entries(ListEntriesCliArgs),
    DailyChat(ListPaginationCliArgs),
    DailyChatMessages(ListDailyChatMessagesCliArgs),
    ContextItems(ListPaginationCliArgs),
}

#[derive(Debug, Args)]
struct ListCommand {
    #[command(subcommand)]
    command: ListSubcommand,
}

#[derive(Debug, Args)]
struct ListEntriesCliArgs {
    #[arg(long)]
    journal_id: String,

    #[command(flatten)]
    pagination: ListPaginationCliArgs,

    #[arg(long, value_enum, default_value_t = ListEntriesSortCli::Desc)]
    sort: ListEntriesSortCli,

    #[arg(long, value_enum)]
    sort_method: Option<ListEntriesSortMethodCli>,

    #[arg(long, value_delimiter = ',', value_name = "FIELD[,FIELD...]")]
    fields: Vec<String>,
}

#[derive(Debug, Args, Clone, Default)]
struct ListPaginationCliArgs {
    #[arg(
        long,
        value_parser = clap::builder::RangedU64ValueParser::<usize>::new()
            .range(1..i64::MAX as u64)
    )]
    limit: Option<usize>,

    #[arg(
        long,
        conflicts_with = "cursor",
        value_parser = clap::builder::RangedU64ValueParser::<usize>::new()
            .range(..=i64::MAX as u64)
    )]
    offset: Option<usize>,

    #[arg(long)]
    cursor: Option<String>,
}

#[derive(Debug, Args)]
struct ListJournalsCliArgs {
    #[command(flatten)]
    pagination: ListPaginationCliArgs,

    #[arg(long, default_value_t = false)]
    include_deleted: bool,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum ListEntriesSortCli {
    Asc,
    Desc,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum ListEntriesSortMethodCli {
    #[value(name = "entryDate")]
    EntryDate,
    #[value(name = "editDate")]
    EditDate,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum AttachmentTypeCli {
    Image,
    Video,
    Audio,
    #[value(name = "pdfAttachment")]
    PdfAttachment,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum SearchEntriesSortCli {
    Relevancy,
    #[value(name = "entryDate")]
    EntryDate,
    #[value(name = "editDate")]
    EditDate,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum SearchEntriesDirectionCli {
    Asc,
    Desc,
}

#[derive(Debug, Args)]
struct ListDailyChatMessagesCliArgs {
    #[arg(long)]
    daily_chat_id: String,
}

#[derive(Debug, Subcommand)]
enum SearchSubcommand {
    ContextItems(SearchContextItemsCliArgs),
    Entries(Box<SearchEntriesCliArgs>),
}

#[derive(Debug, Args)]
struct SearchCommand {
    #[command(subcommand)]
    command: SearchSubcommand,
}

#[derive(Debug, Args)]
struct SearchContextItemsCliArgs {
    #[arg(long)]
    query: String,

    #[arg(long, default_value_t = 0.3)]
    threshold: f64,

    #[arg(long)]
    limit: Option<usize>,

    #[arg(long)]
    category: Option<String>,
}

#[derive(Debug, Args)]
struct SearchEntriesCliArgs {
    #[arg(long)]
    query: Option<String>,

    #[arg(long, default_value_t = 0.3)]
    threshold: f64,

    #[arg(long)]
    limit: Option<usize>,

    #[arg(long = "journal-id")]
    journal_ids: Vec<String>,

    #[arg(long = "tag")]
    tags: Vec<String>,

    #[arg(long)]
    date_from: Option<String>,

    #[arg(long)]
    date_to: Option<String>,

    #[arg(long, default_value_t = false)]
    favorite: bool,

    #[arg(long, default_value_t = false)]
    has_checklist: bool,

    #[arg(long = "place")]
    places: Vec<String>,

    #[arg(long, value_enum)]
    media: Vec<AttachmentTypeCli>,

    #[arg(long = "prompt-id")]
    prompt_ids: Vec<String>,

    #[arg(long = "template-id")]
    template_ids: Vec<String>,

    #[arg(long = "creation-device")]
    creation_devices: Vec<String>,

    #[arg(long = "weather-code")]
    weather_codes: Vec<String>,

    #[arg(long = "music-artist")]
    music_artists: Vec<String>,

    #[arg(long = "activity")]
    activities: Vec<String>,

    #[arg(long, value_enum, default_value_t = SearchEntriesSortCli::Relevancy)]
    sort: SearchEntriesSortCli,

    #[arg(long, value_enum, default_value_t = SearchEntriesDirectionCli::Desc)]
    direction: SearchEntriesDirectionCli,
}

#[derive(Debug, Args)]
struct SyncCliArgs {
    #[arg(long, default_value_t = false)]
    ignore_cursors: bool,
}

#[derive(Debug, Subcommand)]
enum OutboxSubcommand {
    /// List all queued sync-outbox items with status and last error.
    List(OutboxListCliArgs),
    /// Remove stuck items from the local sync outbox. Cleared items will not
    /// be pushed to the server; local data is not modified.
    Clear(OutboxClearCliArgs),
}

#[derive(Debug, Args)]
struct OutboxCommand {
    #[command(subcommand)]
    command: OutboxSubcommand,
}

#[derive(Debug, Args)]
struct OutboxListCliArgs {
    /// Include each item's queued payload as parsed JSON.
    #[arg(long, default_value_t = false)]
    payload: bool,
}

#[derive(Debug, Args)]
#[group(required = true, multiple = false)]
struct OutboxClearCliArgs {
    /// Remove a single item by its outbox id (see `dayone outbox list`).
    #[arg(long)]
    id: Option<String>,

    /// Remove only dead-lettered items (status `failed`).
    #[arg(long)]
    failed: bool,

    /// Remove every outbox item, including pending ones.
    #[arg(long)]
    all: bool,
}

#[derive(Debug, Subcommand)]
enum SyncScheduleSubcommand {
    /// Show whether automatic sync is registered and enabled.
    Status,
    /// Register or update automatic sync for the current profile.
    Enable(SyncScheduleEnableCliArgs),
    /// Update the automatic sync interval.
    SetInterval(SyncScheduleEnableCliArgs),
    /// Disable and remove the automatic sync schedule.
    Disable,
}

#[derive(Debug, Args)]
struct SyncScheduleCommand {
    #[command(subcommand)]
    command: SyncScheduleSubcommand,
}

#[derive(Debug, Args)]
struct SyncScheduleEnableCliArgs {
    /// Sync interval in minutes. Recommended: 30 minutes for normal use.
    #[arg(
        long,
        default_value_t = 30,
        value_parser = clap::value_parser!(u32).range(1..=1439)
    )]
    interval_minutes: u32,
}

#[derive(Debug, Serialize)]
struct WhoAmIOutput {
    ok: bool,
    base_url: String,
    token_created_at: String,
    user: Value,
}

pub async fn run(telemetry: &mut crate::telemetry::TelemetryGuard) -> Result<()> {
    let cli = Cli::parse();
    let command_name = top_level_command_name(&cli.command);
    let subcommand = subcommand_label(&cli.command);
    crate::telemetry::set_command(command_name);

    let diagnostic_invocation_id = if let Some(path) = cli.trace_file.as_deref() {
        Some(crate::diagnostics::start_trace(
            path,
            command_name,
            subcommand,
        )?)
    } else if matches!(&cli.command, TopLevelCommand::Doctor(_)) {
        Some(crate::diagnostics::start_doctor(command_name, subcommand)?)
    } else {
        None
    };
    if let Some(id) = diagnostic_invocation_id.as_deref() {
        crate::telemetry::set_diagnostic_invocation_id(id);
    }

    // Doctor must be able to describe config and database failures, so it runs
    // before normal config creation, store migrations, and analytics setup.
    if let TopLevelCommand::Doctor(args) = cli.command {
        return doctor::execute(doctor::DoctorArgs {
            config_dir: resolve_config_dir().ok(),
            requested_profile: cli.profile,
            api_host_override: cli.api_host,
            include_private_identifiers: args.include_private_identifiers,
        });
    }

    let config_started = Instant::now();
    let config_dir = match resolve_config_dir() {
        Ok(path) => path,
        Err(error) => {
            crate::diagnostics::record_config(
                "error",
                "unknown",
                config_started.elapsed(),
                Some(error.as_ref()),
            );
            return Err(error);
        }
    };
    let mut config = match AppConfig::load_or_create(&config_dir) {
        Ok(config) => config,
        Err(error) => {
            crate::diagnostics::record_config(
                "error",
                "unknown",
                config_started.elapsed(),
                Some(&error),
            );
            return Err(error.into());
        }
    };
    if let TopLevelCommand::Telemetry(cmd) = cli.command {
        use crate::config::ConsentState;
        match cmd.command {
            TelemetrySubcommand::Enable => {
                crate::consent::print_disclosure(&mut std::io::stderr())?;
                crate::consent::save_decision(&mut config, &config_dir, ConsentState::Granted)?;
            }
            TelemetrySubcommand::Disable => {
                crate::consent::save_decision(&mut config, &config_dir, ConsentState::Denied)?;
            }
            TelemetrySubcommand::Status => {}
        }
        return print_json(&serde_json::json!({
            "ok": true,
            "consent": config.analytics.consent,
            "consent_version": config.analytics.consent_version,
            "consent_recorded_at_ms": config.analytics.consent_recorded_at_ms,
            "permitted": config.analytics.consent_granted() && crate::telemetry::is_enabled(),
            "disabled_by_environment": !crate::telemetry::is_enabled(),
        }));
    }

    // Profile commands remain local and do not initialize telemetry. Clone the
    // name so configuration can be updated during consent and analytics setup.
    let profile_name = cli
        .profile
        .clone()
        .unwrap_or_else(|| config.active_profile.clone());
    crate::telemetry::set_profile(&profile_name);
    // Profile commands only need config.toml — handle them before opening
    // the Store so they work even when the active profile's DB is missing.
    // They run before analytics is initialised (no store yet), so they are
    // intentionally not tracked.
    if let TopLevelCommand::Profile(cmd) = cli.command {
        let endpoint = cli
            .api_host
            .as_deref()
            .or_else(|| {
                config
                    .get_profile(&profile_name)
                    .ok()
                    .map(|profile| profile.base_url.as_str())
            })
            .map(crate::diagnostics::endpoint_class)
            .unwrap_or("unknown");
        crate::diagnostics::record_config("success", endpoint, config_started.elapsed(), None);
        return dispatch_profile(cmd, &config_dir, &config);
    }

    let profile_base_url = match config.get_profile(&profile_name) {
        Ok(profile) => profile.base_url.clone(),
        Err(error) => {
            crate::diagnostics::record_config(
                "error",
                "unknown",
                config_started.elapsed(),
                Some(&error),
            );
            return Err(error.into());
        }
    };
    let base_url = match resolve_base_url(cli.api_host.as_deref(), &profile_base_url) {
        Ok(base_url) => base_url,
        Err(error) => {
            crate::diagnostics::record_config(
                "error",
                "unknown",
                config_started.elapsed(),
                Some(error.as_ref()),
            );
            return Err(error);
        }
    };
    if let Err(error) = normalize_base_url(&profile_base_url) {
        crate::diagnostics::record_config(
            "error",
            "unknown",
            config_started.elapsed(),
            Some(error.as_ref()),
        );
        return Err(crate::store::StoreError::invalid_input(error.to_string()).into());
    }
    crate::diagnostics::record_config(
        "success",
        crate::diagnostics::endpoint_class(&base_url),
        config_started.elapsed(),
        None,
    );
    let store_started = Instant::now();
    let store = match Store::open_for_profile(&config_dir, &profile_name, &profile_base_url) {
        Ok(store) => {
            crate::diagnostics::record_store("open", "success", store_started.elapsed(), None);
            store
        }
        Err(error) => {
            crate::diagnostics::record_store(
                "open",
                "error",
                store_started.elapsed(),
                Some(&error),
            );
            return Err(error.into());
        }
    };

    if crate::analytics::would_collect(&store, &base_url) || crate::telemetry::would_collect() {
        crate::consent::resolve(&mut config, &config_dir);
    }
    // Main retains the guard until after error reporting. No Sentry client
    // exists during argument parsing, configuration, or consent failures.
    *telemetry = crate::telemetry::init(&config, &config_dir);
    crate::telemetry::set_command(command_name);
    crate::telemetry::set_profile(&profile_name);
    if let Some(id) = diagnostic_invocation_id.as_deref() {
        crate::telemetry::set_diagnostic_invocation_id(id);
    }

    // Initialise analytics (anonymous id, identity, opt-out gate) before
    // dispatch so handlers can record domain events.
    crate::analytics::init(&config_dir, &mut config, &store, &base_url);

    let started = Instant::now();
    let result = dispatch_command(cli.command, &config_dir, &profile_name, &base_url, &store).await;

    // Best-effort, time-bounded analytics: a once-per-invocation lifecycle
    // event followed by a queue flush. Neither can fail the command.
    crate::analytics::record_command_run(
        &store,
        command_name,
        subcommand,
        &result,
        started.elapsed(),
    );
    crate::analytics::flush(&store).await;

    result
}

async fn dispatch_command(
    command: TopLevelCommand,
    config_dir: &std::path::Path,
    profile_name: &str,
    base_url: &str,
    store: &Store,
) -> Result<()> {
    match command {
        TopLevelCommand::Setup => dispatch_setup(base_url, store).await?,
        TopLevelCommand::Auth(cmd) => dispatch_auth(base_url, cmd, store).await?,
        TopLevelCommand::UserSettings(cmd) => dispatch_user_settings(base_url, cmd, store).await?,
        TopLevelCommand::Profile(_) | TopLevelCommand::Telemetry(_) => {
            unreachable!("handled before dispatch")
        }
        TopLevelCommand::Journal(cmd) => dispatch_journal(base_url, cmd, store).await?,
        TopLevelCommand::Entry(cmd) => dispatch_entry(base_url, cmd, store).await?,
        TopLevelCommand::Embeddings(cmd) => dispatch_embeddings(cmd, store)?,
        TopLevelCommand::DailyChat(cmd) => dispatch_daily_chat(base_url, cmd, store).await?,
        TopLevelCommand::Memory(cmd) => dispatch_memory(base_url, cmd, store).await?,
        TopLevelCommand::ContextItem(cmd) => dispatch_context_item(base_url, cmd, store).await?,
        TopLevelCommand::Comment(cmd) => dispatch_comment(base_url, cmd, store).await?,
        TopLevelCommand::List(cmd) => dispatch_list(cmd, store)?,
        TopLevelCommand::Search(cmd) => dispatch_search(cmd, store)?,
        TopLevelCommand::Sync(args) => dispatch_sync(base_url, args, store).await?,
        TopLevelCommand::Outbox(cmd) => dispatch_outbox(base_url, cmd, store)?,
        TopLevelCommand::SyncSchedule(cmd) => {
            dispatch_sync_schedule(cmd, config_dir, profile_name, base_url)?
        }
        TopLevelCommand::Tui => tui::run(store, profile_name, base_url)?,
        TopLevelCommand::Doctor(_) => unreachable!("handled before config initialization"),
    }
    Ok(())
}

async fn dispatch_setup(base_url: &str, store: &Store) -> Result<()> {
    let output = auth_setup::execute(store, base_url, auth_setup::AuthSetupArgs {}).await?;
    print_json(&output)?;
    Ok(())
}

async fn dispatch_auth(base_url: &str, auth: AuthCommand, store: &Store) -> Result<()> {
    match auth.command {
        AuthSubcommand::Login(args) => {
            let output = auth_login::execute(
                store,
                base_url,
                auth_login::AuthLoginArgs {
                    email: args.email,
                    password_stdin: args.password_stdin,
                },
            )
            .await?;
            print_json(&output)?;
        }
        AuthSubcommand::Logout(args) => {
            // `auth logout --force` erases the local database (queue included),
            // so emit and flush the sign-out before the data is gone.
            if args.force {
                crate::analytics::track(store, crate::analytics::Event::UserSignOut, &[]);
                crate::analytics::flush(store).await;
            }
            let output = auth_logout::execute(
                store,
                base_url,
                auth_logout::AuthLogoutArgs { force: args.force },
            )?;
            print_json(&output)?;
        }
        AuthSubcommand::KeySet(args) => {
            let output = auth_key_set::execute(
                store,
                base_url,
                auth_key_set::AuthKeySetArgs {
                    key: args.key,
                    key_stdin: args.key_stdin,
                },
            )?;
            print_json(&output)?;
        }
        AuthSubcommand::Whoami => {
            let session = store
                .get_auth_session_for_base_url(base_url)?
                .ok_or_else(crate::telemetry::UserError::auth_required)?;
            let user = serde_json::from_str(&session.user_json)
                .context("failed to decode stored user payload as JSON")?;
            let output = WhoAmIOutput {
                ok: true,
                base_url: base_url.to_owned(),
                token_created_at: session.token_created_at,
                user,
            };
            print_json(&output)?;
        }
    }
    Ok(())
}

async fn dispatch_user_settings(
    base_url: &str,
    user_settings: UserSettingsCommand,
    store: &Store,
) -> Result<()> {
    match user_settings.command {
        UserSettingsSubcommand::Get => {
            let output = user_settings_get::execute(store, base_url).await?;
            print_json(&output)?;
        }
    }
    Ok(())
}

fn dispatch_profile(
    profile_cmd: ProfileCommand,
    config_dir: &std::path::Path,
    config: &AppConfig,
) -> Result<()> {
    match profile_cmd.command {
        ProfileSubcommand::List => {
            let output = profile::list(config)?;
            print_json(&output)?;
        }
        ProfileSubcommand::Set(args) => {
            let output = profile::set(config_dir, &args.profile, args.api_host.as_deref())?;
            print_json(&output)?;
        }
    }
    Ok(())
}

async fn dispatch_journal(
    base_url: &str,
    journal_cmd: JournalCommand,
    store: &Store,
) -> Result<()> {
    match journal_cmd.command {
        JournalSubcommand::Create(args) => {
            let output = journal_create::execute(
                store,
                base_url,
                journal_create::JournalCreateArgs {
                    json: args.json,
                    json_file: args.json_file,
                    name: args.name,
                    description: args.description,
                    color: args.color,
                    shared: args.shared,
                    e2e: args.e2e,
                    plaintext: args.plaintext,
                    sort_method: args.sort_method,
                    hide_on_this_day: args.hide_on_this_day,
                    hide_all_entries: args.hide_all_entries,
                    conceal: args.conceal,
                    add_location_to_new_entries: args.add_location_to_new_entries,
                    comments_disabled: args.comments_disabled,
                    template_id: args.template_id,
                    preset_id: args.preset_id,
                },
            )
            .await?;
            crate::analytics::track(
                store,
                crate::analytics::Event::JournalCreate,
                &[
                    ("shared", serde_json::json!(output.shared)),
                    ("encrypted", serde_json::json!(output.encrypted)),
                ],
            );
            print_json(&output)?;
        }
        JournalSubcommand::Update(args) => {
            let output = journal_update::execute(
                store,
                base_url,
                journal_update::JournalUpdateArgs {
                    journal_id: args.journal_id,
                    json: args.json,
                    json_file: args.json_file,
                    name: args.name,
                    description: args.description,
                    color: args.color,
                    sort_method: args.sort_method,
                    hide_on_this_day: args.hide_on_this_day,
                    hide_all_entries: args.hide_all_entries,
                    conceal: args.conceal,
                    add_location_to_new_entries: args.add_location_to_new_entries,
                    comments_disabled: args.comments_disabled,
                    template_id: args.template_id,
                    preset_id: args.preset_id,
                },
            )
            .await?;
            crate::analytics::track(store, crate::analytics::Event::JournalUpdate, &[]);
            print_json(&output)?;
        }
    }
    Ok(())
}

async fn dispatch_entry(base_url: &str, entry_cmd: EntryCommand, store: &Store) -> Result<()> {
    match entry_cmd.command {
        EntrySubcommand::Write(args) => {
            // An explicit `--entry-id` targets an existing entry (edit);
            // otherwise this creates a new one.
            let is_edit = args.entry_id.is_some();
            let output = entry_write::execute(
                store,
                base_url,
                entry_write::EntryWriteArgs {
                    journal_id: args.journal_id,
                    body: args.body,
                    body_stdin: args.body_stdin,
                    entry_id: args.entry_id,
                    date: args.date,
                    all_day: args.all_day,
                    attachments: args
                        .attachments
                        .into_iter()
                        .map(std::path::PathBuf::from)
                        .collect(),
                    attachment_types: args
                        .attachment_types
                        .into_iter()
                        .map(|kind| match kind {
                            AttachmentTypeCli::Image => entry_write::MediaType::Image,
                            AttachmentTypeCli::Video => entry_write::MediaType::Video,
                            AttachmentTypeCli::Audio => entry_write::MediaType::Audio,
                            AttachmentTypeCli::PdfAttachment => {
                                entry_write::MediaType::PdfAttachment
                            }
                        })
                        .collect(),
                },
            )
            .await?;
            let event = if is_edit {
                crate::analytics::Event::EntryEditFinish
            } else {
                crate::analytics::Event::EntryCreate
            };
            crate::analytics::track(
                store,
                event,
                &[("synced", serde_json::json!(output.synced))],
            );
            print_json(&output)?;
        }
        EntrySubcommand::Read(args) => {
            let output = entry_read::execute(
                store,
                entry_read::EntryReadArgs {
                    journal_id: args.journal_id,
                    entry_id: args.entry_id,
                },
            )?;
            print_json(&output)?;
        }
        EntrySubcommand::Delete(args) => {
            let output = entry_delete::execute(
                store,
                base_url,
                entry_delete::EntryDeleteArgs {
                    journal_id: args.journal_id,
                    entry_id: args.entry_id,
                },
            )?;
            crate::analytics::track(store, crate::analytics::Event::EntryDelete, &[]);
            print_json(&output)?;
        }
    }
    Ok(())
}

fn dispatch_embeddings(embeddings_cmd: EmbeddingsCommand, store: &Store) -> Result<()> {
    match embeddings_cmd.command {
        EmbeddingsSubcommand::RecalculateEntries(args) => {
            let output = embeddings::recalculate_entries(
                store,
                embeddings::RecalculateEntriesArgs {
                    journal_id: args.journal_id,
                    entry_id: args.entry_id,
                },
            )?;
            print_json(&output)?;
        }
    }
    Ok(())
}

async fn dispatch_daily_chat(
    base_url: &str,
    daily_chat_cmd: DailyChatCommand,
    store: &Store,
) -> Result<()> {
    match daily_chat_cmd.command {
        DailyChatSubcommand::Add(args) => {
            let output = daily_chat_add::execute(
                store,
                base_url,
                daily_chat_add::DailyChatAddArgs {
                    message: args.message,
                    message_stdin: args.message_stdin,
                    date: args.date,
                },
            )
            .await?;
            crate::analytics::track(store, crate::analytics::Event::DailyChatEntryUpdated, &[]);
            print_json(&output)?;
        }
    }
    Ok(())
}

async fn dispatch_memory(base_url: &str, memory_cmd: MemoryCommand, store: &Store) -> Result<()> {
    match memory_cmd.command {
        MemorySubcommand::Process(args) => {
            let output = memory_process::execute(
                store,
                base_url,
                memory_process::MemoryProcessArgs {
                    json: args.json,
                    json_file: args.json_file,
                },
            )
            .await?;
            print_json(&output)?;
        }
    }
    Ok(())
}

async fn dispatch_context_item(
    base_url: &str,
    context_item_cmd: ContextItemCommand,
    store: &Store,
) -> Result<()> {
    match context_item_cmd.command {
        ContextItemSubcommand::Put(args) => {
            let output = context_item_write::put(
                store,
                base_url,
                context_item_write::ContextItemPutArgs {
                    json: args.json,
                    json_file: args.json_file,
                },
            )
            .await?;
            print_json(&output)?;
        }
        ContextItemSubcommand::Delete(args) => {
            let output = context_item_write::delete(
                store,
                base_url,
                context_item_write::ContextItemDeleteArgs {
                    id: args.id,
                    updated_at: args.updated_at,
                    deleted_at: args.deleted_at,
                },
            )
            .await?;
            print_json(&output)?;
        }
    }
    Ok(())
}

async fn dispatch_comment(
    base_url: &str,
    comment_cmd: CommentCommand,
    store: &Store,
) -> Result<()> {
    match comment_cmd.command {
        CommentSubcommand::List(args) => {
            let output = comment::list(
                store,
                base_url,
                comment::CommentListArgs {
                    journal_id: args.journal_id,
                    entry_id: args.entry_id,
                    pagination: comment::CommentPaginationInput {
                        limit: args.pagination.limit,
                        offset: args.pagination.offset,
                        cursor: args.pagination.cursor,
                    },
                    refresh: args.refresh,
                },
            )
            .await?;
            print_json(&output)?;
        }
        CommentSubcommand::Write(args) => {
            let output = comment::write(
                store,
                base_url,
                comment::CommentWriteArgs {
                    journal_id: args.journal_id,
                    entry_id: args.entry_id,
                    body: args.body,
                },
            )
            .await?;
            crate::analytics::track(store, crate::analytics::Event::EntryCommentAdded, &[]);
            print_json(&output)?;
        }
        CommentSubcommand::Update(args) => {
            let output = comment::update(
                store,
                base_url,
                comment::CommentUpdateArgs {
                    journal_id: args.journal_id,
                    entry_id: args.entry_id,
                    comment_id: args.comment_id,
                    body: args.body,
                },
            )
            .await?;
            crate::analytics::track(store, crate::analytics::Event::EntryCommentEdited, &[]);
            print_json(&output)?;
        }
        CommentSubcommand::Delete(args) => {
            let output = comment::delete(
                store,
                base_url,
                comment::CommentDeleteArgs {
                    journal_id: args.journal_id,
                    entry_id: args.entry_id,
                    comment_id: args.comment_id,
                },
            )
            .await?;
            crate::analytics::track(store, crate::analytics::Event::EntryCommentDeleted, &[]);
            print_json(&output)?;
        }
        CommentSubcommand::React(args) => {
            let output = comment::react(
                store,
                base_url,
                comment::CommentReactionArgs {
                    journal_id: args.journal_id,
                    entry_id: args.entry_id,
                    comment_id: args.comment_id,
                    reaction: args.reaction,
                },
            )
            .await?;
            crate::analytics::track(
                store,
                crate::analytics::Event::EntryCommentReactionAdded,
                &[],
            );
            print_json(&output)?;
        }
        CommentSubcommand::Unreact(args) => {
            let output = comment::unreact(
                store,
                base_url,
                comment::CommentUnreactArgs {
                    journal_id: args.journal_id,
                    entry_id: args.entry_id,
                    comment_id: args.comment_id,
                },
            )
            .await?;
            crate::analytics::track(
                store,
                crate::analytics::Event::EntryCommentReactionDeleted,
                &[],
            );
            print_json(&output)?;
        }
    }
    Ok(())
}

fn dispatch_list(list_cmd: ListCommand, store: &Store) -> Result<()> {
    match list_cmd.command {
        ListSubcommand::Journals(args) => {
            let output = list::journals(
                store,
                normalize_list_pagination(&args.pagination)?,
                args.include_deleted,
            )?;
            print_json(&output)?;
        }
        ListSubcommand::Entries(args) => {
            let sort = match args.sort {
                ListEntriesSortCli::Asc => EntryListSort::Asc,
                ListEntriesSortCli::Desc => EntryListSort::Desc,
            };
            let sort_method =
                resolve_entry_list_sort_method(store, &args.journal_id, args.sort_method)?;
            let fields = normalize_projection_fields(&args.fields)?;
            let output = list::entries_by_journal_id(
                store,
                &args.journal_id,
                normalize_list_pagination(&args.pagination)?,
                sort,
                sort_method,
                fields.as_deref(),
            )?;
            print_json(&output)?;
        }
        ListSubcommand::DailyChat(args) => {
            let output = list::daily_chat(store, normalize_list_pagination(&args)?)?;
            print_json(&output)?;
        }
        ListSubcommand::DailyChatMessages(args) => {
            let output = list::daily_chat_messages_by_id(store, &args.daily_chat_id)?;
            print_json(&output)?;
        }
        ListSubcommand::ContextItems(args) => {
            let output = list::context_items(store, normalize_list_pagination(&args)?)?;
            print_json(&output)?;
        }
    }
    Ok(())
}

fn dispatch_search(search_cmd: SearchCommand, store: &Store) -> Result<()> {
    match search_cmd.command {
        SearchSubcommand::ContextItems(args) => {
            let output = search::context_items(
                store,
                search::SearchContextItemsArgs {
                    query: args.query,
                    threshold: args.threshold,
                    limit: args.limit,
                    category: args.category,
                },
            )?;
            print_json(&output)?;
        }
        SearchSubcommand::Entries(args) => {
            let args = *args;
            let output = search::entries(
                store,
                search::SearchEntriesArgs {
                    query: args.query.unwrap_or_default(),
                    threshold: args.threshold,
                    limit: args.limit,
                    journal_ids: args.journal_ids,
                    tags: args.tags,
                    date_from: args.date_from,
                    date_to: args.date_to,
                    favorite: args.favorite,
                    has_checklist: args.has_checklist,
                    places: args.places,
                    media: args
                        .media
                        .into_iter()
                        .map(|kind| match kind {
                            AttachmentTypeCli::Image => search::SearchMediaType::Image,
                            AttachmentTypeCli::Video => search::SearchMediaType::Video,
                            AttachmentTypeCli::Audio => search::SearchMediaType::Audio,
                            AttachmentTypeCli::PdfAttachment => {
                                search::SearchMediaType::PdfAttachment
                            }
                        })
                        .collect(),
                    prompt_ids: args.prompt_ids,
                    template_ids: args.template_ids,
                    creation_devices: args.creation_devices,
                    weather_codes: args.weather_codes,
                    music_artists: args.music_artists,
                    activities: args.activities,
                    sort_by: match args.sort {
                        SearchEntriesSortCli::Relevancy => search::SearchEntriesSortBy::Relevancy,
                        SearchEntriesSortCli::EntryDate => search::SearchEntriesSortBy::EntryDate,
                        SearchEntriesSortCli::EditDate => search::SearchEntriesSortBy::EditDate,
                    },
                    direction: match args.direction {
                        SearchEntriesDirectionCli::Asc => search::SearchSortDirection::Asc,
                        SearchEntriesDirectionCli::Desc => search::SearchSortDirection::Desc,
                    },
                },
            )?;
            print_json(&output)?;
        }
    }
    Ok(())
}

async fn dispatch_sync(base_url: &str, args: SyncCliArgs, store: &Store) -> Result<()> {
    let mut output = sync::execute(
        store,
        base_url,
        sync::SyncArgs {
            ignore_cursors: args.ignore_cursors,
        },
    )
    .await?;
    let fatal_error = output.fatal_error.clone();
    let failure = output.failure.take();
    let ok = output.ok;
    print_json(&output)?;
    if !ok {
        return Err(failure.unwrap_or_else(|| {
            anyhow!(
                "{}",
                fatal_error.unwrap_or_else(|| "sync failed with fatal error".to_owned())
            )
        }));
    }
    Ok(())
}

fn dispatch_outbox(base_url: &str, cmd: OutboxCommand, store: &Store) -> Result<()> {
    match cmd.command {
        OutboxSubcommand::List(args) => {
            let output = outbox::list(store, base_url, args.payload)?;
            print_json(&output)?;
        }
        OutboxSubcommand::Clear(args) => {
            let output = outbox::clear(
                store,
                base_url,
                outbox::OutboxClearArgs {
                    id: args.id,
                    failed_only: args.failed,
                    all: args.all,
                },
            )?;
            print_json(&output)?;
        }
    }
    Ok(())
}

fn dispatch_sync_schedule(
    cmd: SyncScheduleCommand,
    config_dir: &std::path::Path,
    profile_name: &str,
    base_url: &str,
) -> Result<()> {
    let args = sync_schedule::SyncScheduleArgs {
        config_dir: config_dir.to_path_buf(),
        profile_name: profile_name.to_owned(),
        base_url: base_url.to_owned(),
    };
    let output = match cmd.command {
        SyncScheduleSubcommand::Status => sync_schedule::status(args)?,
        SyncScheduleSubcommand::Enable(enable_args)
        | SyncScheduleSubcommand::SetInterval(enable_args) => sync_schedule::enable(
            args,
            sync_schedule::EnableArgs {
                interval_minutes: enable_args.interval_minutes,
            },
        )?,
        SyncScheduleSubcommand::Disable => sync_schedule::disable(args)?,
    };
    print_json(&output)?;
    Ok(())
}

/// Resolve the config directory.
///
/// Precedence: `DAYONE_CONFIG_DIR` env var, then platform default.
fn resolve_config_dir() -> Result<std::path::PathBuf> {
    if let Ok(override_dir) = std::env::var("DAYONE_CONFIG_DIR") {
        return Ok(std::path::PathBuf::from(override_dir));
    }
    let config_dir =
        dirs::config_dir().ok_or_else(|| anyhow!("failed to resolve config directory"))?;
    Ok(config_dir.join("dayone-cli"))
}

/// Resolve the API base URL.
///
/// Precedence: `--api-host` / `DAYONE_API_HOST`, then profile's base_url.
fn resolve_base_url(api_host_override: Option<&str>, profile_base_url: &str) -> Result<String> {
    if let Some(host) = api_host_override {
        return normalize_base_url(host);
    }
    Ok(profile_base_url.to_owned())
}

fn print_json<T: Serialize>(payload: &T) -> Result<()> {
    let mut stdout = std::io::stdout().lock();
    write_json_to_writer(&mut stdout, payload)
}

fn write_json_to_writer<W: Write, T: Serialize>(writer: &mut W, payload: &T) -> Result<()> {
    let json = serde_json::to_string_pretty(payload)?;
    match writeln!(writer, "{json}") {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::BrokenPipe => Ok(()),
        Err(err) => Err(err.into()),
    }
}

/// Stable kebab-case identifier for the top-level command, used as a
/// telemetry tag so error reports can be filtered per surface.
fn top_level_command_name(command: &TopLevelCommand) -> &'static str {
    match command {
        TopLevelCommand::Setup => "setup",
        TopLevelCommand::Auth(_) => "auth",
        TopLevelCommand::UserSettings(_) => "user-settings",
        TopLevelCommand::Profile(_) => "profile",
        TopLevelCommand::Telemetry(_) => "telemetry",
        TopLevelCommand::Journal(_) => "journal",
        TopLevelCommand::Entry(_) => "entry",
        TopLevelCommand::Embeddings(_) => "embeddings",
        TopLevelCommand::DailyChat(_) => "daily-chat",
        TopLevelCommand::Memory(_) => "memory",
        TopLevelCommand::ContextItem(_) => "context-item",
        TopLevelCommand::List(_) => "list",
        TopLevelCommand::Search(_) => "search",
        TopLevelCommand::Sync(_) => "sync",
        TopLevelCommand::Outbox(_) => "outbox",
        TopLevelCommand::SyncSchedule(_) => "sync-schedule",
        TopLevelCommand::Tui => "tui",
        TopLevelCommand::Doctor(_) => "doctor",
        TopLevelCommand::Comment(_) => "comment",
    }
}

/// Stable kebab-case identifier for the subcommand, attached to the
/// `command_run` analytics event. Returns `None` for commands with no
/// subcommand (e.g. `setup`, `sync`, `tui`, `doctor`).
fn subcommand_label(command: &TopLevelCommand) -> Option<&'static str> {
    match command {
        TopLevelCommand::Auth(cmd) => Some(match cmd.command {
            AuthSubcommand::Login(_) => "login",
            AuthSubcommand::Logout(_) => "logout",
            AuthSubcommand::KeySet(_) => "key-set",
            AuthSubcommand::Whoami => "whoami",
        }),
        TopLevelCommand::Telemetry(cmd) => Some(match cmd.command {
            TelemetrySubcommand::Enable => "enable",
            TelemetrySubcommand::Disable => "disable",
            TelemetrySubcommand::Status => "status",
        }),
        TopLevelCommand::UserSettings(cmd) => Some(match cmd.command {
            UserSettingsSubcommand::Get => "get",
        }),
        TopLevelCommand::Profile(cmd) => Some(match cmd.command {
            ProfileSubcommand::List => "list",
            ProfileSubcommand::Set(_) => "set",
        }),
        TopLevelCommand::Journal(cmd) => Some(match cmd.command {
            JournalSubcommand::Create(_) => "create",
            JournalSubcommand::Update(_) => "update",
        }),
        TopLevelCommand::Entry(cmd) => Some(match cmd.command {
            EntrySubcommand::Write(_) => "write",
            EntrySubcommand::Read(_) => "read",
            EntrySubcommand::Delete(_) => "delete",
        }),
        TopLevelCommand::Embeddings(cmd) => Some(match cmd.command {
            EmbeddingsSubcommand::RecalculateEntries(_) => "recalculate-entries",
        }),
        TopLevelCommand::DailyChat(cmd) => Some(match cmd.command {
            DailyChatSubcommand::Add(_) => "add",
        }),
        TopLevelCommand::Memory(cmd) => Some(match cmd.command {
            MemorySubcommand::Process(_) => "process",
        }),
        TopLevelCommand::ContextItem(cmd) => Some(match cmd.command {
            ContextItemSubcommand::Put(_) => "put",
            ContextItemSubcommand::Delete(_) => "delete",
        }),
        TopLevelCommand::Comment(cmd) => Some(match cmd.command {
            CommentSubcommand::List(_) => "list",
            CommentSubcommand::Write(_) => "write",
            CommentSubcommand::Update(_) => "update",
            CommentSubcommand::Delete(_) => "delete",
            CommentSubcommand::React(_) => "react",
            CommentSubcommand::Unreact(_) => "unreact",
        }),
        TopLevelCommand::List(cmd) => Some(match cmd.command {
            ListSubcommand::Journals(_) => "journals",
            ListSubcommand::Entries(_) => "entries",
            ListSubcommand::DailyChat(_) => "daily-chat",
            ListSubcommand::DailyChatMessages(_) => "daily-chat-messages",
            ListSubcommand::ContextItems(_) => "context-items",
        }),
        TopLevelCommand::Search(cmd) => Some(match cmd.command {
            SearchSubcommand::ContextItems(_) => "context-items",
            SearchSubcommand::Entries(_) => "entries",
        }),
        TopLevelCommand::Outbox(cmd) => Some(match cmd.command {
            OutboxSubcommand::List(_) => "list",
            OutboxSubcommand::Clear(_) => "clear",
        }),
        TopLevelCommand::SyncSchedule(cmd) => Some(match cmd.command {
            SyncScheduleSubcommand::Status => "status",
            SyncScheduleSubcommand::Enable(_) => "enable",
            SyncScheduleSubcommand::SetInterval(_) => "set-interval",
            SyncScheduleSubcommand::Disable => "disable",
        }),
        TopLevelCommand::Setup
        | TopLevelCommand::Sync(_)
        | TopLevelCommand::Tui
        | TopLevelCommand::Doctor(_) => None,
    }
}

fn normalize_projection_fields(raw_fields: &[String]) -> Result<Option<Vec<String>>> {
    if raw_fields.is_empty() {
        return Ok(None);
    }

    let mut seen = HashSet::new();
    let mut normalized = Vec::new();

    for field in raw_fields {
        let trimmed = field.trim();
        if trimmed.is_empty() {
            return Err(anyhow!(
                "`--fields` contains an empty field name; provide comma-separated non-empty names"
            ));
        }
        let normalized_field = trimmed.to_owned();
        if seen.insert(normalized_field.clone()) {
            normalized.push(normalized_field);
        }
    }

    Ok(Some(normalized))
}

fn normalize_list_pagination(raw: &ListPaginationCliArgs) -> Result<list::PaginationInput> {
    let cursor = raw.cursor.as_ref().map(|value| value.trim().to_owned());
    if cursor.as_deref().is_some_and(|value| value.is_empty()) {
        return Err(anyhow!("`--cursor` cannot be empty"));
    }
    Ok(list::PaginationInput {
        limit: raw.limit,
        offset: raw.offset,
        cursor,
    })
}

fn resolve_entry_list_sort_method(
    store: &Store,
    journal_id: &str,
    cli_override: Option<ListEntriesSortMethodCli>,
) -> Result<EntryListSortMethod> {
    if let Some(value) = cli_override {
        return Ok(match value {
            ListEntriesSortMethodCli::EntryDate => EntryListSortMethod::EntryDate,
            ListEntriesSortMethodCli::EditDate => EntryListSortMethod::EditDate,
        });
    }

    let journal_row = store.get_json_row_by_id("journals", journal_id)?;
    let Some(journal_json) = journal_row else {
        return Ok(EntryListSortMethod::EntryDate);
    };
    let sort_method = serde_json::from_str::<Value>(&journal_json)
        .ok()
        .and_then(|value| {
            value
                .get("sort_method")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
        });

    Ok(match sort_method.as_deref() {
        Some("editDate") => EntryListSortMethod::EditDate,
        _ => EntryListSortMethod::EntryDate,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;
    use std::io::{Error, ErrorKind};
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn test_store_path(name: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time should be valid")
            .as_nanos();
        std::env::temp_dir().join(format!("dayone-cli-cli-tests-{name}-{unique}.db"))
    }

    #[test]
    fn top_level_command_name_is_stable_kebab_case() {
        // Sanity-check representative mappings so telemetry tags stay aligned
        // with clap spellings, including kebab-case command names.
        let parsed = Cli::try_parse_from(["dayone", "auth", "logout"]).expect("parse");
        assert_eq!(top_level_command_name(&parsed.command), "auth");

        let parsed = Cli::try_parse_from(["dayone", "user-settings", "get"]).expect("parse");
        assert_eq!(top_level_command_name(&parsed.command), "user-settings");

        let parsed = Cli::try_parse_from(["dayone", "tui"]).expect("parse");
        assert_eq!(top_level_command_name(&parsed.command), "tui");
    }

    #[test]
    fn list_pagination_rejects_offset_and_cursor_together() {
        let parsed = Cli::try_parse_from([
            "dayone", "list", "journals", "--offset", "10", "--cursor", "abc",
        ]);
        assert!(parsed.is_err(), "offset and cursor should conflict");
    }

    #[test]
    fn list_pagination_accepts_cursor_mode() {
        let parsed = Cli::try_parse_from([
            "dayone",
            "list",
            "context-items",
            "--limit",
            "20",
            "--cursor",
            "abc",
        ]);
        assert!(parsed.is_ok(), "cursor mode should parse");
    }

    #[test]
    fn list_journals_accepts_include_deleted_flag() {
        let parsed = Cli::try_parse_from([
            "dayone",
            "list",
            "journals",
            "--include-deleted",
            "--limit",
            "20",
        ]);
        assert!(parsed.is_ok(), "journals include-deleted should parse");
    }

    #[test]
    fn list_pagination_enforces_sqlite_numeric_ranges() {
        let parse = |flag, value| Cli::try_parse_from(["dayone", "list", "journals", flag, value]);

        let max_limit = usize::try_from(i64::MAX - 1)
            .unwrap_or(usize::MAX)
            .to_string();
        let max_offset = usize::try_from(i64::MAX).unwrap_or(usize::MAX).to_string();

        assert!(parse("--limit", "1").is_ok());
        assert!(parse("--limit", "0").is_err());
        assert!(parse("--limit", &max_limit).is_ok());
        assert!(parse("--limit", "9223372036854775807").is_err());
        assert!(parse("--offset", &max_offset).is_ok());
        assert!(parse("--offset", "9223372036854775808").is_err());
    }

    #[test]
    fn clap_enforces_structural_argument_relationships() {
        let parse = |args: &[&str]| Cli::try_parse_from(args.iter().copied());

        assert!(parse(&["dayone", "auth", "key-set"]).is_ok());
        assert!(parse(&["dayone", "auth", "key-set", "--key", "x", "--key-stdin"]).is_err());
        assert!(
            parse(&[
                "dayone",
                "journal",
                "create",
                "--json",
                "{}",
                "--json-file",
                "payload.json",
            ])
            .is_err()
        );
        assert!(
            parse(&[
                "dayone",
                "journal",
                "update",
                "--journal-id",
                "journal-1",
                "--json",
                "{}",
                "--json-file",
                "payload.json",
            ])
            .is_err()
        );
        assert!(parse(&["dayone", "entry", "write", "--journal-id", "journal-1"]).is_err());
        assert!(
            parse(&[
                "dayone",
                "entry",
                "write",
                "--journal-id",
                "journal-1",
                "--body",
                "hello",
                "--body-stdin",
            ])
            .is_err()
        );
        assert!(parse(&["dayone", "daily-chat", "add", "--message", "hello"]).is_ok());
        assert!(parse(&["dayone", "daily-chat", "add"]).is_err());
        assert!(
            parse(&[
                "dayone",
                "daily-chat",
                "add",
                "--message",
                "hello",
                "--message-stdin",
            ])
            .is_err()
        );
        assert!(parse(&["dayone", "context-item", "put", "--json", "{}"]).is_ok());
        assert!(parse(&["dayone", "context-item", "put"]).is_err());
        assert!(
            parse(&[
                "dayone",
                "context-item",
                "put",
                "--json",
                "{}",
                "--json-file",
                "payload.json",
            ])
            .is_err()
        );
    }

    #[test]
    fn auth_logout_parses_without_flags() {
        let parsed = Cli::try_parse_from(["dayone", "auth", "logout"]);
        assert!(parsed.is_ok(), "auth logout should parse without flags");
    }

    #[test]
    fn auth_logout_parses_with_force_flag() {
        let parsed = Cli::try_parse_from(["dayone", "auth", "logout", "--force"]);
        assert!(parsed.is_ok(), "auth logout --force should parse");
    }

    #[test]
    fn setup_parses_without_flags() {
        let parsed = Cli::try_parse_from(["dayone", "setup"]);
        assert!(parsed.is_ok(), "setup should parse without flags");
    }

    #[test]
    fn search_entries_parses_with_optional_filters() {
        let parsed = Cli::try_parse_from([
            "dayone",
            "search",
            "entries",
            "--query",
            "ship log",
            "--journal-id",
            "journal-1",
            "--limit",
            "10",
        ]);
        assert!(parsed.is_ok(), "search entries command should parse");
    }

    #[test]
    fn search_entries_parses_with_web_parity_filters() {
        let parsed = Cli::try_parse_from([
            "dayone",
            "search",
            "entries",
            "--journal-id",
            "journal-1",
            "--journal-id",
            "journal-2",
            "--tag",
            "travel",
            "--tag",
            "morning",
            "--date-from",
            "2026-04-01",
            "--date-to",
            "2026-04-10",
            "--favorite",
            "--has-checklist",
            "--place",
            "Portland",
            "--media",
            "image",
            "--media",
            "pdfAttachment",
            "--prompt-id",
            "prompt-1",
            "--template-id",
            "template-1",
            "--creation-device",
            "iPhone 15 Pro",
            "--weather-code",
            "clear-day",
            "--music-artist",
            "Radiohead",
            "--activity",
            "Walking",
            "--sort",
            "editDate",
            "--direction",
            "asc",
        ]);
        assert!(
            parsed.is_ok(),
            "search entries parity flags should parse together"
        );
    }

    #[test]
    fn search_entries_help_lists_parity_flags_and_enum_values() {
        let mut command = Cli::command();
        let search = command
            .find_subcommand_mut("search")
            .expect("search subcommand should exist");
        let entries = search
            .find_subcommand_mut("entries")
            .expect("search entries subcommand should exist");

        let mut help_output = Vec::new();
        entries
            .write_long_help(&mut help_output)
            .expect("help should render");
        let help_text = String::from_utf8(help_output).expect("help output must be utf-8");

        for expected in [
            "--journal-id",
            "--tag",
            "--date-from",
            "--date-to",
            "--favorite",
            "--has-checklist",
            "--place",
            "--media",
            "--prompt-id",
            "--template-id",
            "--creation-device",
            "--weather-code",
            "--music-artist",
            "--activity",
            "--sort",
            "--direction",
            "relevancy",
            "entryDate",
            "editDate",
            "asc",
            "desc",
            "image",
            "video",
            "audio",
            "pdfAttachment",
        ] {
            assert!(
                help_text.contains(expected),
                "help text should contain '{expected}'"
            );
        }
    }

    #[test]
    fn journal_create_help_documents_shared_e2e_requirements() {
        let mut command = Cli::command();
        let journal = command
            .find_subcommand_mut("journal")
            .expect("journal subcommand should exist");
        let create = journal
            .find_subcommand_mut("create")
            .expect("journal create subcommand should exist");

        let mut help_output = Vec::new();
        create
            .write_long_help(&mut help_output)
            .expect("help should render");
        let help_text = String::from_utf8(help_output).expect("help output must be utf-8");
        let normalized_help_text = help_text.split_whitespace().collect::<Vec<_>>().join(" ");

        for expected in [
            "--shared",
            "requires E2E encryption",
            "--json/--json-file",
            "--e2e",
            "dayone auth key-set",
            "dayone sync",
        ] {
            assert!(
                normalized_help_text.contains(expected),
                "help text should contain '{expected}'"
            );
        }
    }

    #[test]
    fn outbox_list_parses_with_optional_payload_flag() {
        assert!(Cli::try_parse_from(["dayone", "outbox", "list"]).is_ok());
        assert!(Cli::try_parse_from(["dayone", "outbox", "list", "--payload"]).is_ok());
    }

    #[test]
    fn outbox_clear_parses_selectors_and_rejects_combinations() {
        assert!(
            Cli::try_parse_from(["dayone", "outbox", "clear"]).is_err(),
            "outbox clear should require a selector"
        );
        assert!(Cli::try_parse_from(["dayone", "outbox", "clear", "--failed"]).is_ok());
        assert!(Cli::try_parse_from(["dayone", "outbox", "clear", "--all"]).is_ok());
        assert!(Cli::try_parse_from(["dayone", "outbox", "clear", "--id", "entry:j:e"]).is_ok());
        assert!(
            Cli::try_parse_from(["dayone", "outbox", "clear", "--failed", "--all"]).is_err(),
            "--failed and --all should conflict"
        );
        assert!(
            Cli::try_parse_from(["dayone", "outbox", "clear", "--id", "x", "--failed"]).is_err(),
            "--id and --failed should conflict"
        );
    }

    #[test]
    fn sync_schedule_parses_status() {
        let parsed = Cli::try_parse_from(["dayone", "sync-schedule", "status"]);
        assert!(parsed.is_ok(), "sync-schedule status should parse");
    }

    #[test]
    fn sync_schedule_parses_enable_with_interval() {
        let parsed = Cli::try_parse_from([
            "dayone",
            "sync-schedule",
            "enable",
            "--interval-minutes",
            "15",
        ]);
        assert!(parsed.is_ok(), "sync-schedule enable should parse");
    }

    #[test]
    fn sync_schedule_enforces_supported_interval_boundaries() {
        let parse = |interval| {
            Cli::try_parse_from([
                "dayone",
                "sync-schedule",
                "enable",
                "--interval-minutes",
                interval,
            ])
        };

        for interval in ["1", "1439"] {
            assert!(
                parse(interval).is_ok(),
                "sync-schedule should accept interval {interval}"
            );
        }
        for interval in ["0", "1440"] {
            assert!(
                parse(interval).is_err(),
                "sync-schedule should reject interval {interval}"
            );
        }
    }

    #[test]
    fn sync_schedule_parses_set_interval() {
        let parsed = Cli::try_parse_from([
            "dayone",
            "sync-schedule",
            "set-interval",
            "--interval-minutes",
            "45",
        ]);
        assert!(parsed.is_ok(), "sync-schedule set-interval should parse");
    }

    #[test]
    fn sync_schedule_parses_disable() {
        let parsed = Cli::try_parse_from(["dayone", "sync-schedule", "disable"]);
        assert!(parsed.is_ok(), "sync-schedule disable should parse");
    }

    #[test]
    fn sync_embed_entries_flag_is_rejected() {
        let parsed = Cli::try_parse_from(["dayone", "sync", "--embed-entries"]);
        assert!(parsed.is_err(), "sync --embed-entries should be rejected");
    }

    #[test]
    fn entry_read_parses_with_required_args() {
        let parsed = Cli::try_parse_from([
            "dayone",
            "entry",
            "read",
            "--journal-id",
            "journal-1",
            "--entry-id",
            "entry-1",
        ]);
        assert!(parsed.is_ok(), "entry read should parse");
    }

    #[test]
    fn entry_read_requires_both_journal_and_entry_id() {
        let missing_entry_id =
            Cli::try_parse_from(["dayone", "entry", "read", "--journal-id", "journal-1"]);
        assert!(
            missing_entry_id.is_err(),
            "entry read should fail without --entry-id"
        );
        let missing_journal_id =
            Cli::try_parse_from(["dayone", "entry", "read", "--entry-id", "entry-1"]);
        assert!(
            missing_journal_id.is_err(),
            "entry read should fail without --journal-id"
        );
    }

    #[test]
    fn entry_delete_parses_with_required_args() {
        let parsed = Cli::try_parse_from([
            "dayone",
            "entry",
            "delete",
            "--journal-id",
            "journal-1",
            "--entry-id",
            "entry-1",
        ]);
        assert!(parsed.is_ok(), "entry delete should parse");
    }

    #[test]
    fn entry_delete_requires_both_journal_and_entry_id() {
        let missing_entry_id =
            Cli::try_parse_from(["dayone", "entry", "delete", "--journal-id", "journal-1"]);
        assert!(
            missing_entry_id.is_err(),
            "entry delete should fail without --entry-id"
        );
        let missing_journal_id =
            Cli::try_parse_from(["dayone", "entry", "delete", "--entry-id", "entry-1"]);
        assert!(
            missing_journal_id.is_err(),
            "entry delete should fail without --journal-id"
        );
    }

    #[test]
    fn entry_write_accepts_date_flag() {
        let parsed = Cli::try_parse_from([
            "dayone",
            "entry",
            "write",
            "--journal-id",
            "journal-1",
            "--body",
            "hello",
            "--date",
            "2026-03-30",
        ]);
        assert!(parsed.is_ok(), "entry write should parse --date");
    }

    #[test]
    fn entry_write_accepts_all_day_flag() {
        let parsed = Cli::try_parse_from([
            "dayone",
            "entry",
            "write",
            "--journal-id",
            "journal-1",
            "--body",
            "hello",
            "--all-day",
        ]);
        assert!(parsed.is_ok(), "entry write should parse --all-day");
    }

    #[test]
    fn embeddings_recalculate_entries_parses() {
        let parsed = Cli::try_parse_from([
            "dayone",
            "embeddings",
            "recalculate-entries",
            "--journal-id",
            "journal-1",
            "--entry-id",
            "entry-1",
        ]);
        assert!(
            parsed.is_ok(),
            "embeddings recalculate-entries should parse"
        );
    }

    #[test]
    fn memory_process_parses_with_json_file() {
        let parsed =
            Cli::try_parse_from(["dayone", "memory", "process", "--json-file", "payload.json"]);
        assert!(parsed.is_ok(), "memory process --json-file should parse");
    }

    #[test]
    fn memory_process_parses_with_inline_json() {
        let parsed =
            Cli::try_parse_from(["dayone", "memory", "process", "--json", "{\"messages\":[]}"]);
        assert!(parsed.is_ok(), "memory process --json should parse");
    }

    #[test]
    fn memory_process_requires_exactly_one_payload_source() {
        assert!(
            Cli::try_parse_from(["dayone", "memory", "process"]).is_err(),
            "memory process should require a payload source"
        );
        let parsed = Cli::try_parse_from([
            "dayone",
            "memory",
            "process",
            "--json",
            "{\"messages\":[]}",
            "--json-file",
            "payload.json",
        ]);
        assert!(
            parsed.is_err(),
            "memory process should reject --json with --json-file"
        );
    }

    #[test]
    fn list_entries_accepts_sort_method_flag() {
        let parsed = Cli::try_parse_from([
            "dayone",
            "list",
            "entries",
            "--journal-id",
            "journal-1",
            "--sort-method",
            "editDate",
        ]);
        assert!(parsed.is_ok(), "list entries should parse --sort-method");
    }

    #[test]
    fn comment_list_parses_with_refresh_and_pagination() {
        let parsed = Cli::try_parse_from([
            "dayone",
            "comment",
            "list",
            "--journal-id",
            "journal-1",
            "--entry-id",
            "entry-1",
            "--limit",
            "10",
            "--cursor",
            "abc123",
            "--refresh",
        ]);
        assert!(parsed.is_ok(), "comment list should parse");
    }

    #[test]
    fn comment_write_parses_required_flags() {
        let parsed = Cli::try_parse_from([
            "dayone",
            "comment",
            "write",
            "--journal-id",
            "journal-1",
            "--entry-id",
            "entry-1",
            "--body",
            "hello",
        ]);
        assert!(parsed.is_ok(), "comment write should parse");
    }

    #[test]
    fn comment_react_parses_with_default_reaction() {
        let parsed = Cli::try_parse_from([
            "dayone",
            "comment",
            "react",
            "--journal-id",
            "journal-1",
            "--entry-id",
            "entry-1",
            "--comment-id",
            "comment-1",
        ]);
        assert!(parsed.is_ok(), "comment react should parse");
    }

    #[test]
    fn resolve_entry_list_sort_method_defaults_to_entry_date() {
        let path = test_store_path("sort-method-default");
        let store = Store::open_at(&path).expect("store should open");

        let method = resolve_entry_list_sort_method(&store, "missing-journal", None)
            .expect("sort method should resolve");
        assert!(matches!(method, EntryListSortMethod::EntryDate));
    }

    #[test]
    fn resolve_entry_list_sort_method_uses_journal_setting() {
        let path = test_store_path("sort-method-journal");
        let store = Store::open_at(&path).expect("store should open");
        store
            .upsert_json_row(
                "journals",
                "journal-1",
                Some("2026-03-27T00:00:00.000Z"),
                None,
                r#"{"id":"journal-1","sort_method":"editDate"}"#,
            )
            .expect("journal row should save");

        let method = resolve_entry_list_sort_method(&store, "journal-1", None)
            .expect("sort method should resolve");
        assert!(matches!(method, EntryListSortMethod::EditDate));
    }

    #[derive(Default)]
    struct BrokenPipeWriter;

    impl std::io::Write for BrokenPipeWriter {
        fn write(&mut self, _buf: &[u8]) -> std::io::Result<usize> {
            Err(Error::new(ErrorKind::BrokenPipe, "broken pipe"))
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn write_json_to_writer_ignores_broken_pipe() {
        let mut writer = BrokenPipeWriter;
        let payload = serde_json::json!({ "ok": true });
        let result = write_json_to_writer(&mut writer, &payload);
        assert!(result.is_ok(), "broken pipe should be ignored");
    }

    #[derive(Default)]
    struct OtherIoErrorWriter;

    impl std::io::Write for OtherIoErrorWriter {
        fn write(&mut self, _buf: &[u8]) -> std::io::Result<usize> {
            Err(Error::new(ErrorKind::PermissionDenied, "permission denied"))
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn write_json_to_writer_returns_non_broken_pipe_errors() {
        let mut writer = OtherIoErrorWriter;
        let payload = serde_json::json!({ "ok": true });
        let err = write_json_to_writer(&mut writer, &payload).expect_err("should return io error");
        assert!(
            err.to_string().contains("permission denied"),
            "non-broken-pipe errors should be preserved"
        );
    }
}

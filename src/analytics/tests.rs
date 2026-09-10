use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Map, Value, json};

use super::tracks::EventSender;
use super::*;
use crate::store::sqlite::Store;

static TEST_DB_COUNTER: AtomicU64 = AtomicU64::new(0);

fn test_store() -> Store {
    // Combine the clock, a per-process counter, and the pid so concurrent test
    // threads never collide on the same temp path (`as_nanos()` alone is not
    // unique enough under parallel execution).
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("time should be valid")
        .as_nanos()
        + u128::from(TEST_DB_COUNTER.fetch_add(1, Ordering::Relaxed));
    let path = std::env::temp_dir().join(format!(
        "dayone-cli-analytics-{}-{unique}.db",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    Store::open_at(&path).expect("store should open")
}

/// Records the batch bodies it is asked to send and optionally fails, so the
/// drain routine can be exercised without touching the network.
struct StubSender {
    fail: bool,
    sent: Mutex<Vec<Value>>,
}

impl StubSender {
    fn new(fail: bool) -> Self {
        Self {
            fail,
            sent: Mutex::new(Vec::new()),
        }
    }

    fn sent(&self) -> Vec<Value> {
        self.sent.lock().expect("lock").clone()
    }
}

impl EventSender for StubSender {
    async fn send(&self, body: &Value) -> anyhow::Result<()> {
        self.sent.lock().expect("lock").push(body.clone());
        if self.fail {
            anyhow::bail!("stub failure");
        }
        Ok(())
    }
}

#[test]
fn custom_property_schema_allows_only_expected_keys_and_values() {
    let body = json!("private journal body");
    let synced = json!(true);
    let wrong_type = json!("true");
    let filtered = filter_custom_properties(
        Event::EntryCreate,
        [
            ("synced", &synced),
            ("body", &body),
            ("encrypted", &synced),
            ("synced", &wrong_type),
        ],
    );

    assert_eq!(
        filtered,
        Map::from_iter([("synced".to_owned(), json!(true))])
    );
    assert!(!filtered.values().any(|value| value == &body));
}

#[test]
fn event_names_round_trip_through_the_send_time_schema() {
    for event in Event::ALL {
        let name = event.full_name();
        assert_eq!(event_from_full_name(&name), Some(event));
    }
    assert_eq!(event_from_full_name("dayone_cli_future_event"), None);
}

#[test]
fn custom_property_schema_bounds_command_values() {
    assert!(custom_property_allowed(
        Event::CommandRun,
        "command",
        &json!("sync")
    ));
    assert!(custom_property_allowed(
        Event::CommandRun,
        "outcome",
        &json!("user_error")
    ));
    assert!(!custom_property_allowed(
        Event::CommandRun,
        "command",
        &json!("private journal body")
    ));
    assert!(!custom_property_allowed(
        Event::CommandRun,
        "duration_ms",
        &json!(-1)
    ));
}

#[test]
fn unsafe_subscription_is_omitted_before_queue_serialization() {
    let mut custom = Map::new();
    custom.insert("command".to_owned(), json!("sync"));
    let params = build_params(
        "dayone_cli_command_run",
        true,
        "0.1.0",
        "macos aarch64",
        identity::normalize_subscription_level("alice@example.com"),
        custom,
    );

    assert!(!params.contains_key("subscription_level"));
    assert!(
        !serde_json::to_string(&params)
            .expect("params should serialize")
            .contains("alice@example.com")
    );
}

#[test]
fn build_params_includes_globals_and_custom() {
    let mut custom = Map::new();
    custom.insert("command".to_owned(), json!("sync"));
    let params = build_params(
        "dayone_cli_command_run",
        true,
        "0.1.0",
        "macos aarch64",
        Some("Gold"),
        custom,
    );
    assert_eq!(params["_en"], json!("dayone_cli_command_run"));
    assert_eq!(params["is_signed_in"], json!(true));
    assert_eq!(params["device_info_app_version"], json!("0.1.0"));
    assert_eq!(params["device_info_os"], json!("macos aarch64"));
    assert_eq!(params["subscription_level"], json!("Gold"));
    assert_eq!(params["command"], json!("sync"));
}

#[test]
fn build_params_does_not_let_custom_clobber_reserved_keys() {
    let mut custom = Map::new();
    // A misbehaving caller tries to override reserved/global keys.
    custom.insert("_en".to_owned(), json!("evil_rename"));
    custom.insert("is_signed_in".to_owned(), json!(false));
    custom.insert("device_info_os".to_owned(), json!("spoofed"));
    custom.insert("command".to_owned(), json!("sync"));
    let params = build_params(
        "dayone_cli_command_run",
        true,
        "0.1.0",
        "macos aarch64",
        None,
        custom,
    );
    // Reserved/global keys win; only genuinely new custom keys are added.
    assert_eq!(params["_en"], json!("dayone_cli_command_run"));
    assert_eq!(params["is_signed_in"], json!(true));
    assert_eq!(params["device_info_os"], json!("macos aarch64"));
    assert_eq!(params["command"], json!("sync"));
}

#[test]
fn build_params_omits_subscription_when_absent() {
    let params = build_params(
        "dayone_cli_x",
        false,
        "0.1.0",
        "linux x86_64",
        None,
        Map::new(),
    );
    assert!(!params.contains_key("subscription_level"));
    assert_eq!(params["is_signed_in"], json!(false));
}

#[test]
fn build_event_object_stringifies_props_and_adds_timestamp() {
    let event = build_event_object(
        r#"{"_en":"dayone_cli_entry_create","is_signed_in":true,"device_info_app_version":"0.1.0","device_info_os":"macos aarch64"}"#,
        1234,
    )
    .expect("valid params should build an event object");

    assert_eq!(event["_en"], json!("dayone_cli_entry_create"));
    // Tracks stores properties as strings, so scalars are stringified.
    assert_eq!(event["is_signed_in"], json!("true"));
    assert_eq!(
        event["device_info_app_version"],
        json!(env!("CARGO_PKG_VERSION"))
    );
    assert_eq!(event["_ts"], json!("1234"));
}

#[test]
fn build_event_object_rejects_non_object_payload() {
    assert!(build_event_object("not json", 1).is_none());
    assert!(build_event_object("[1,2,3]", 1).is_none());
}

#[test]
fn build_event_object_requires_a_valid_event_name() {
    // Missing `_en`.
    assert!(build_event_object(r#"{"command":"sync"}"#, 1).is_none());
    // `_en` present but not a valid Tracks name.
    assert!(build_event_object(r#"{"_en":"Bad Name"}"#, 1).is_none());
}

#[test]
fn build_event_object_drops_disallowed_properties_from_queued_rows() {
    let event = build_event_object(
        r#"{"_en":"dayone_cli_command_run","is_signed_in":true,"device_info_app_version":"0.1.0","device_info_os":"macos aarch64","command":"sync","outcome":"success","duration_ms":2,"body":"private journal body","Bad Key":"x"}"#,
        7,
    )
    .expect("valid event name should build");
    assert_eq!(event["command"], json!("sync"));
    assert_eq!(event["outcome"], json!("success"));
    assert_eq!(event["duration_ms"], json!("2"));
    assert!(event.get("body").is_none());
    assert!(event.get("Bad Key").is_none());
    assert!(!event.to_string().contains("private journal body"));
}

#[test]
fn build_event_object_rejects_unknown_cli_events() {
    assert!(
        build_event_object(
            r#"{"_en":"dayone_cli_future_event","is_signed_in":true,"device_info_app_version":"0.1.0","device_info_os":"macos aarch64","body":"private"}"#,
            1,
        )
        .is_none()
    );
}

#[test]
fn build_event_object_rejects_invalid_globals_and_rebuilds_device_fields() {
    assert!(
        build_event_object(
            r#"{"_en":"dayone_cli_entry_delete","is_signed_in":"yes","device_info_app_version":"0.1.0","device_info_os":"macos aarch64"}"#,
            1,
        )
        .is_none()
    );

    let event = build_event_object(
        r#"{"_en":"dayone_cli_entry_delete","is_signed_in":true,"device_info_app_version":"private journal body","device_info_os":"alice@example.com","subscription_level":"alice@example.com"}"#,
        1,
    )
    .expect("valid globals should build");
    assert!(event.get("subscription_level").is_none());
    assert!(!event.to_string().contains("alice@example.com"));
    assert!(!event.to_string().contains("private journal body"));
    assert_eq!(
        event["device_info_app_version"],
        json!(env!("CARGO_PKG_VERSION"))
    );
    assert_eq!(
        event["device_info_os"],
        json!(format!(
            "{} {}",
            std::env::consts::OS,
            std::env::consts::ARCH
        ))
    );

    let event = build_event_object(
        r#"{"_en":"dayone_cli_entry_delete","is_signed_in":true,"device_info_app_version":"0.1.0","device_info_os":"macos aarch64","subscription_level":"gold"}"#,
        1,
    )
    .expect("known subscription should build");
    assert_eq!(event["subscription_level"], json!("Gold"));
}

#[test]
fn build_batch_body_sets_common_props_and_events() {
    let anon_id = "AAECAwQFBgcICQoLDA0ODxAR";
    let events = vec![
        build_event_object(
            r#"{"_en":"dayone_cli_entry_create","is_signed_in":false,"device_info_app_version":"0.1.0","device_info_os":"macos aarch64"}"#,
            222,
        )
        .expect("event should build"),
    ];
    let body = build_batch_body(anon_id, "anon", events);

    assert_eq!(body["commonProps"]["_ui"], json!(anon_id));
    assert_eq!(body["commonProps"]["_ut"], json!("anon"));
    assert!(
        body["commonProps"]["_rt"].is_string(),
        "request time should be set"
    );
    let events = body["events"]
        .as_array()
        .expect("events should be an array");
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["_en"], json!("dayone_cli_entry_create"));
    assert_eq!(events[0]["_ts"], json!("222"));
}

#[test]
fn build_event_object_drops_legacy_alias_events() {
    // The CLI no longer emits `_aliasUser`; any such row left in an upgraded
    // client's queue is dropped rather than sent.
    for raw in [
        r#"{"_en":"_aliasUser","anonId":"AAECAwQFBgcICQoLDA0ODxAR"}"#,
        r#"{"_en":"_aliasUser"}"#,
        r#"{"_en":"_aliasUser","anonId":42}"#,
    ] {
        assert!(build_event_object(raw, 1).is_none());
    }
}

#[test]
fn value_to_query_string_flattens_scalars() {
    assert_eq!(value_to_query_string(&json!("hello")), "hello");
    assert_eq!(value_to_query_string(&json!(true)), "true");
    assert_eq!(value_to_query_string(&json!(42)), "42");
    assert_eq!(value_to_query_string(&json!(null)), "");
}

#[tokio::test]
async fn drain_queue_scrubs_disallowed_properties_before_sending() {
    let store = test_store();
    store
        .enqueue_analytics_event(
            "dayone_cli_command_run",
            r#"{"_en":"dayone_cli_command_run","is_signed_in":true,"device_info_app_version":"0.1.0","device_info_os":"macos aarch64","command":"sync","outcome":"success","duration_ms":2,"body":"private journal body"}"#,
            crate::util::now_epoch_ms(),
            Some("AAECAwQFBgcICQoLDA0ODxAR"),
            Some("anon"),
        )
        .expect("enqueue");
    let sender = StubSender::new(false);

    drain_queue(&store, &sender, "AAECAwQFBgcICQoLDA0ODxAR").await;

    let sent = sender.sent();
    let event = &sent[0]["events"][0];
    assert_eq!(event["command"], json!("sync"));
    assert!(event.get("body").is_none());
    assert!(!sent[0].to_string().contains("private journal body"));
}

#[tokio::test]
async fn drain_queue_deletes_sent_events() {
    let store = test_store();
    let created_at = crate::util::now_epoch_ms();
    store
        .enqueue_analytics_event(
            "dayone_cli_entry_create",
            r#"{"_en":"dayone_cli_entry_create","is_signed_in":true,"device_info_app_version":"0.1.0","device_info_os":"macos aarch64"}"#,
            created_at,
            Some("AAECAwQFBgcICQoLDA0ODxAR"),
            Some("anon"),
        )
        .expect("enqueue");
    let sender = StubSender::new(false);
    let anon_id = "AAECAwQFBgcICQoLDA0ODxAR";

    drain_queue(&store, &sender, anon_id).await;

    assert_eq!(sender.sent().len(), 1, "one batch should have been sent");
    let body = &sender.sent()[0];
    assert_eq!(body["commonProps"]["_ui"], json!(anon_id));
    assert_eq!(body["commonProps"]["_ut"], json!("anon"));
    let events = body["events"].as_array().expect("events array");
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["_en"], json!("dayone_cli_entry_create"));
    assert_eq!(events[0]["_ts"], json!(created_at.to_string()));
    assert!(
        store.take_analytics_events(10).expect("take").is_empty(),
        "sent events should be removed from the queue"
    );
}

#[tokio::test]
async fn drain_queue_sends_all_events_in_one_batch() {
    let store = test_store();
    let created_at = crate::util::now_epoch_ms();
    for name in ["dayone_cli_entry_create", "dayone_cli_journal_create"] {
        store
            .enqueue_analytics_event(
                name,
                &format!(r#"{{"_en":"{name}","is_signed_in":false,"device_info_app_version":"0.1.0","device_info_os":"linux x86_64"}}"#),
                created_at,
                Some("AAECAwQFBgcICQoLDA0ODxAR"),
                Some("anon"),
            )
            .expect("enqueue");
    }
    let sender = StubSender::new(false);

    drain_queue(&store, &sender, "AAECAwQFBgcICQoLDA0ODxAR").await;

    assert_eq!(sender.sent().len(), 1, "events should be sent as one batch");
    let events = sender.sent()[0]["events"]
        .as_array()
        .expect("events array")
        .len();
    assert_eq!(events, 2, "both queued events should be in the batch");
    assert!(store.take_analytics_events(10).expect("take").is_empty());
}

#[tokio::test]
async fn drain_queue_discards_rows_without_the_current_consent_identity() {
    let store = test_store();
    let created_at = crate::util::now_epoch_ms();
    let anon_id = "AAECAwQFBgcICQoLDA0ODxAR";
    store
        .enqueue_analytics_event(
            "dayone_cli_entry_create",
            r#"{"_en":"dayone_cli_entry_create","is_signed_in":false}"#,
            created_at,
            Some(anon_id),
            Some("anon"),
        )
        .expect("enqueue current event");
    for (id, kind) in [
        (Some("YWJjZGVmZ2hpamtsbW5vcHFy"), Some("anon")),
        (None, None),
        (Some("invalid-id"), Some("anon")),
        // Match the current ID so checking only the ID cannot pass this test.
        (Some(anon_id), Some("dayone:user_id")),
        (Some(anon_id), None),
    ] {
        store
            .enqueue_analytics_event(
                "dayone_cli_user_sign_in",
                r#"{"_en":"dayone_cli_user_sign_in","is_signed_in":true}"#,
                created_at + 60_000,
                id,
                kind,
            )
            .expect("enqueue stale or unidentified event");
    }
    let sender = StubSender::new(false);
    drain_queue(&store, &sender, anon_id).await;
    let sent = sender.sent();
    let names: Vec<_> = sent
        .iter()
        .flat_map(|batch| batch["events"].as_array().unwrap())
        .map(|event| event["_en"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["dayone_cli_entry_create"]);
    assert!(store.take_analytics_events(10).expect("take").is_empty());

    // An entirely stale batch must be discarded without an empty request.
    store
        .enqueue_analytics_event(
            "dayone_cli_user_sign_in",
            r#"{"_en":"dayone_cli_user_sign_in","is_signed_in":true}"#,
            created_at + 60_000,
            None,
            None,
        )
        .expect("enqueue legacy event");
    drain_queue(&store, &sender, anon_id).await;
    assert_eq!(sender.sent().len(), 1);
    assert!(store.take_analytics_events(10).expect("take").is_empty());
}

#[tokio::test]
async fn drain_queue_retains_and_bumps_on_failure() {
    let store = test_store();
    store
        .enqueue_analytics_event(
            "dayone_cli_journal_create",
            r#"{"_en":"dayone_cli_journal_create","is_signed_in":false,"device_info_app_version":"0.1.0","device_info_os":"linux x86_64"}"#,
            crate::util::now_epoch_ms(),
            Some("AAECAwQFBgcICQoLDA0ODxAR"),
            Some("anon"),
        )
        .expect("enqueue");
    let sender = StubSender::new(true);

    drain_queue(&store, &sender, "AAECAwQFBgcICQoLDA0ODxAR").await;

    let remaining = store.take_analytics_events(10).expect("take");
    assert_eq!(remaining.len(), 1, "failed event should stay queued");
    assert_eq!(remaining[0].attempts, 1, "attempt count should be bumped");
}

#[test]
fn tracks_require_production_api_or_explicit_endpoint_override() {
    use crate::test_util::with_env;

    with_env(&[(tracks::ENDPOINT_ENV, None)], || {
        assert!(tracks_enabled_for_api(crate::constants::PRODUCTION_URL));
        assert!(tracks_enabled_for_api(&format!(
            "{}/",
            crate::constants::PRODUCTION_URL
        )));
        assert!(!tracks_enabled_for_api(crate::constants::STAGING_URL));
        assert!(!tracks_enabled_for_api("https://example.test"));
    });
    with_env(
        &[(
            tracks::ENDPOINT_ENV,
            Some("https://tracks.example.test/record"),
        )],
        || {
            assert!(tracks_enabled_for_api(crate::constants::STAGING_URL));
            assert!(tracks_enabled_for_api("https://example.test"));
        },
    );
}

#[test]
fn init_with_staging_api_disables_enqueue_and_flush() {
    use crate::test_util::with_env;

    with_env(
        &[
            (tracks::ENDPOINT_ENV, None),
            ("DO_NOT_TRACK", None),
            ("DAYONE_TELEMETRY", None),
        ],
        || {
            let config_dir = tempfile::tempdir().expect("config dir should exist");
            let store = test_store();
            store
                .enqueue_analytics_event(
                    "dayone_cli_entry_create",
                    r#"{"_en":"dayone_cli_entry_create","is_signed_in":false,"device_info_app_version":"0.1.0","device_info_os":"linux x86_64"}"#,
                    crate::util::now_epoch_ms(),
                    None,
                    None,
                )
                .expect("enqueue existing event");
            let mut config = AppConfig::default_config();

            init(
                config_dir.path(),
                &mut config,
                &store,
                crate::constants::STAGING_URL,
            );
            track(&store, Event::EntryCreate, &[("synced", json!(true))]);
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("runtime should build")
                .block_on(flush(&store));

            let remaining = store.take_analytics_events(10).expect("take");
            assert_eq!(remaining.len(), 1, "no new event should be enqueued");
            assert_eq!(remaining[0].attempts, 0, "flush should be a no-op");
        },
    );
}

#[test]
fn server_analytics_defaults_enabled_without_settings() {
    let store = test_store();
    assert!(server_analytics_enabled(&store));
}

#[test]
fn server_analytics_honours_track_usage_statistics_false() {
    let store = test_store();
    store
        .upsert_user_settings(None, r#"{"track_usage_statistics": false}"#)
        .expect("settings save");
    assert!(!server_analytics_enabled(&store));
}

#[test]
fn server_analytics_honours_nested_track_usage_statistics() {
    let store = test_store();
    store
        .upsert_user_settings(None, r#"{"web": {"track_usage_statistics": false}}"#)
        .expect("settings save");
    assert!(!server_analytics_enabled(&store));
}

#[test]
fn server_analytics_inverts_opt_out_flag() {
    let store = test_store();
    store
        .upsert_user_settings(None, r#"{"analytics_opt_out": true}"#)
        .expect("settings save");
    assert!(!server_analytics_enabled(&store));
}

#[test]
fn server_analytics_enabled_when_setting_true() {
    let store = test_store();
    store
        .upsert_user_settings(None, r#"{"track_usage_statistics": true}"#)
        .expect("settings save");
    assert!(server_analytics_enabled(&store));
}

#[tokio::test]
async fn stale_backlog_does_not_delay_current_consent_events() {
    let store = test_store();
    let now = crate::util::now_epoch_ms();
    let current_id = "AAECAwQFBgcICQoLDA0ODxAR";
    for _ in 0..MAX_FLUSH_PER_RUN {
        store
            .enqueue_analytics_event(
                "dayone_cli_user_sign_in",
                r#"{"_en":"dayone_cli_user_sign_in","is_signed_in":true}"#,
                now + 60_000,
                Some("YWJjZGVmZ2hpamtsbW5vcHFy"),
                Some("anon"),
            )
            .expect("enqueue stale backlog");
    }
    // The current identity must not exempt expired activity from retention.
    store
        .enqueue_analytics_event(
            "dayone_cli_user_sign_in",
            r#"{"_en":"dayone_cli_user_sign_in","is_signed_in":true}"#,
            now - 31 * 24 * 60 * 60 * 1000,
            Some(current_id),
            Some("anon"),
        )
        .expect("enqueue expired event");
    store
        .enqueue_analytics_event(
            "dayone_cli_entry_create",
            r#"{"_en":"dayone_cli_entry_create","is_signed_in":false}"#,
            now,
            Some(current_id),
            Some("anon"),
        )
        .expect("enqueue current event");
    let sender = StubSender::new(false);
    drain_queue(&store, &sender, current_id).await;
    let sent = sender.sent();
    let names: Vec<_> = sent
        .iter()
        .flat_map(|batch| batch["events"].as_array().unwrap())
        .map(|event| event["_en"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["dayone_cli_entry_create"]);
    assert!(store.take_analytics_events(100).expect("take").is_empty());
}

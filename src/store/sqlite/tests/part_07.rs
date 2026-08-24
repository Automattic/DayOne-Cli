use super::common::test_store_path;
use crate::store::sqlite::Store;

#[test]
fn open_for_profile_seeds_single_profile() {
    let dir = tempfile::TempDir::new().expect("should create temp dir");
    let store = Store::open_for_profile(dir.path(), "staging", "https://stg.dayone.me")
        .expect("open_for_profile should succeed");

    let profiles = store.list_profiles().expect("list_profiles should succeed");
    assert_eq!(profiles.len(), 1, "should have exactly one profile");
    assert_eq!(profiles[0].name, "staging");
    assert_eq!(profiles[0].base_url, "https://stg.dayone.me");
    assert!(profiles[0].is_active, "seeded profile should be active");
}

#[test]
fn open_for_profile_creates_db_in_expected_path() {
    let dir = tempfile::TempDir::new().expect("should create temp dir");
    let expected_db = dir
        .path()
        .join("profiles")
        .join("myprofile")
        .join("dayone.db");

    let _store = Store::open_for_profile(dir.path(), "myprofile", "https://example.com")
        .expect("open_for_profile should succeed");

    assert!(
        expected_db.exists(),
        "DB file should exist at {}",
        expected_db.display()
    );
}

#[test]
fn open_for_profile_rejects_invalid_name() {
    let dir = tempfile::TempDir::new().expect("should create temp dir");
    let result = Store::open_for_profile(dir.path(), "../escape", "https://example.com");
    assert!(
        result.is_err(),
        "should reject path traversal in profile name"
    );
}

#[test]
fn open_for_profile_reopening_is_idempotent() {
    let dir = tempfile::TempDir::new().expect("should create temp dir");
    let store = Store::open_for_profile(dir.path(), "test", "https://example.com")
        .expect("first open should succeed");
    drop(store);

    let store2 = Store::open_for_profile(dir.path(), "test", "https://example.com")
        .expect("second open should succeed");
    let profiles = store2
        .list_profiles()
        .expect("list_profiles should succeed");
    assert_eq!(
        profiles.len(),
        1,
        "should still have exactly one profile after reopen"
    );
    assert!(profiles[0].is_active);
}

#[test]
fn auth_profile_resolution_keeps_per_profile_identity() {
    let dir = tempfile::TempDir::new().expect("should create temp dir");
    let store = Store::open_for_profile(dir.path(), "production", "https://dayone.me")
        .expect("store should open");

    let profile = store
        .resolve_profile_for_auth("https://override.example.com")
        .expect("profile should resolve");

    assert_eq!(profile.name, "production");
    assert_eq!(profile.base_url, "https://dayone.me");
    assert_eq!(store.list_profiles().unwrap().len(), 1);
}

#[test]
fn open_at_still_seeds_both_default_profiles() {
    let path = test_store_path("legacy-both-profiles");
    let store = Store::open_at(&path).expect("open_at should succeed");
    let profiles = store.list_profiles().expect("list_profiles should succeed");
    assert_eq!(
        profiles.len(),
        2,
        "legacy open_at should seed both profiles"
    );

    let names: Vec<&str> = profiles.iter().map(|p| p.name.as_str()).collect();
    assert!(names.contains(&"staging"));
    assert!(names.contains(&"production"));
}

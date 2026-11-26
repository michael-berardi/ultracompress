//! Cross-adapter regression: the CLI, Pi and Hermes read the same canonical
//! cache fixture (snake_case keys, integer Unix seconds, one shared file name).

use ultracompress_core::update as contract;

const SHARED_CACHE: &str = include_str!("../../../tests/fixtures/update-cache.json");

#[test]
fn shared_cache_fixture_has_the_daily_interval_and_shared_file_name() {
    let cache = contract::parse_cache(SHARED_CACHE);
    assert_eq!(cache.checked_at, 1_700_000_000);
    assert_eq!(cache.shown_at, 1_700_000_000);
    assert_eq!(cache.release.as_ref().unwrap().version, "9.9.9");
    assert_eq!(contract::CACHE_FILE, "update.json");
    assert_eq!(contract::CHECK_INTERVAL_SECS, 86_400);
    assert!(!contract::check_due(
        &cache,
        1_700_000_000 + contract::CHECK_INTERVAL_SECS - 1
    ));
    assert!(contract::check_due(
        &cache,
        1_700_000_000 + contract::CHECK_INTERVAL_SECS
    ));
}

#[test]
fn cache_path_is_one_file_in_the_shared_cache_directory() {
    assert_eq!(
        contract::cache_path(),
        contract::cache_dir().join("update.json")
    );
}

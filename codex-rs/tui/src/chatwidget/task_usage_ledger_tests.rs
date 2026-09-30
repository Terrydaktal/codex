use super::PersistedTaskUsageLedger;
use super::TaskUsageLedger;
use super::current_unix_seconds;
use std::collections::BTreeSet;

const TEST_RESET_AT_UNIX_SECONDS: i64 = 1_700_000_000;

#[test]
fn endpoint_reset_from_lower_remaining_clears_usage() {
    let codex_home = tempfile::tempdir().expect("temporary Codex home");
    let ledger_path = codex_home.path().join("task_usage_weekly.json");
    let persisted = PersistedTaskUsageLedger {
        account_scope_id: None,
        reset_at_unix_seconds: TEST_RESET_AT_UNIX_SECONDS,
        used_percent: 42.0,
        weekly_credit_allowance: super::CURRENT_WEEKLY_CREDIT_ALLOWANCE,
        last_endpoint_remaining_percent: Some(65.0),
        applied_turn_ids: BTreeSet::from(["old-turn".to_string()]),
        applied_response_event_ids: BTreeSet::from(["old-response".to_string()]),
    };
    std::fs::write(
        &ledger_path,
        serde_json::to_vec(&persisted).expect("serialize ledger"),
    )
    .expect("write ledger");

    let mut ledger = TaskUsageLedger::load(
        codex_home.path(),
        codex_config::types::AuthCredentialsStoreMode::File,
        codex_login::AuthKeyringBackendKind::default(),
    );
    assert_eq!(ledger.remaining_percent(), 58.0);
    let before_reset = current_unix_seconds();
    ledger.observe_endpoint_remaining(Some(100.0));
    let after_reset = current_unix_seconds();
    assert_eq!(ledger.remaining_percent(), 100.0);
    assert!(ledger.reset_at_unix_seconds >= before_reset);
    assert!(ledger.reset_at_unix_seconds <= after_reset);
    ledger.record_response("old-response", Some(1.0));
    ledger.remaining_from_disk();
    assert_eq!(ledger.remaining_percent(), 99.0);

    let reloaded = TaskUsageLedger::load(
        codex_home.path(),
        codex_config::types::AuthCredentialsStoreMode::File,
        codex_login::AuthKeyringBackendKind::default(),
    );
    assert_eq!(reloaded.remaining_percent(), 99.0);
    assert_eq!(reloaded.reset_at_unix_seconds, ledger.reset_at_unix_seconds);
}

#[test]
fn first_full_endpoint_snapshot_creates_reset_anchor() {
    let codex_home = tempfile::tempdir().expect("temporary Codex home");
    let mut ledger = TaskUsageLedger::load(
        codex_home.path(),
        codex_config::types::AuthCredentialsStoreMode::File,
        codex_login::AuthKeyringBackendKind::default(),
    );
    assert_eq!(ledger.reset_at_unix_seconds, 0);

    let before_reset = current_unix_seconds();
    ledger.observe_endpoint_remaining(Some(100.0));
    let after_reset = current_unix_seconds();

    assert!(ledger.reset_at_unix_seconds >= before_reset);
    assert!(ledger.reset_at_unix_seconds <= after_reset);
    assert_eq!(ledger.remaining_percent(), 100.0);
}

#[test]
fn legacy_2400_credit_ledger_is_migrated_to_2700() {
    let codex_home = tempfile::tempdir().expect("temporary Codex home");
    let ledger_path = codex_home.path().join("task_usage_weekly.json");
    let persisted = PersistedTaskUsageLedger {
        account_scope_id: None,
        reset_at_unix_seconds: TEST_RESET_AT_UNIX_SECONDS,
        used_percent: 42.0,
        weekly_credit_allowance: super::LEGACY_WEEKLY_CREDIT_ALLOWANCE,
        last_endpoint_remaining_percent: Some(58.0),
        applied_turn_ids: BTreeSet::new(),
        applied_response_event_ids: BTreeSet::new(),
    };
    std::fs::write(
        &ledger_path,
        serde_json::to_vec(&persisted).expect("serialize legacy ledger"),
    )
    .expect("write legacy ledger");

    let ledger = TaskUsageLedger::load(
        codex_home.path(),
        codex_config::types::AuthCredentialsStoreMode::File,
        codex_login::AuthKeyringBackendKind::default(),
    );
    assert!((ledger.remaining_percent() - 62.66666666666667).abs() < 1e-12);
    let migrated: PersistedTaskUsageLedger =
        serde_json::from_slice(&std::fs::read(&ledger_path).expect("read migrated ledger"))
            .expect("parse migrated ledger");
    assert_eq!(migrated.weekly_credit_allowance, 2700.0);
}

#[test]
fn external_bootstrap_is_loaded_before_next_record() {
    let codex_home = tempfile::tempdir().expect("temporary Codex home");
    let ledger_path = codex_home.path().join("task_usage_weekly.json");
    let initial = PersistedTaskUsageLedger {
        account_scope_id: None,
        reset_at_unix_seconds: TEST_RESET_AT_UNIX_SECONDS,
        used_percent: 42.0,
        weekly_credit_allowance: super::CURRENT_WEEKLY_CREDIT_ALLOWANCE,
        last_endpoint_remaining_percent: Some(58.0),
        applied_turn_ids: BTreeSet::new(),
        applied_response_event_ids: BTreeSet::new(),
    };
    std::fs::write(
        &ledger_path,
        serde_json::to_vec(&initial).expect("serialize initial ledger"),
    )
    .expect("write initial ledger");

    let mut ledger = TaskUsageLedger::load(
        codex_home.path(),
        codex_config::types::AuthCredentialsStoreMode::File,
        codex_login::AuthKeyringBackendKind::default(),
    );
    let bootstrapped = PersistedTaskUsageLedger {
        account_scope_id: None,
        reset_at_unix_seconds: TEST_RESET_AT_UNIX_SECONDS,
        used_percent: 47.0,
        weekly_credit_allowance: super::CURRENT_WEEKLY_CREDIT_ALLOWANCE,
        last_endpoint_remaining_percent: Some(53.0),
        applied_turn_ids: BTreeSet::new(),
        applied_response_event_ids: BTreeSet::new(),
    };
    std::fs::write(
        &ledger_path,
        serde_json::to_vec(&bootstrapped).expect("serialize bootstrapped ledger"),
    )
    .expect("write bootstrapped ledger");

    ledger.remaining_from_disk();
    assert_eq!(ledger.remaining_percent(), 53.0);
    assert_eq!(ledger.endpoint_remaining_percent(), Some(53.0));
}

#[test]
fn separate_ledger_instances_merge_turn_updates_and_ignore_duplicates() {
    let codex_home = tempfile::tempdir().expect("temporary Codex home");
    let ledger_path = codex_home.path().join("task_usage_weekly.json");
    let persisted = PersistedTaskUsageLedger {
        account_scope_id: None,
        reset_at_unix_seconds: TEST_RESET_AT_UNIX_SECONDS,
        used_percent: 0.0,
        weekly_credit_allowance: super::CURRENT_WEEKLY_CREDIT_ALLOWANCE,
        last_endpoint_remaining_percent: Some(100.0),
        applied_turn_ids: BTreeSet::new(),
        applied_response_event_ids: BTreeSet::new(),
    };
    std::fs::write(
        &ledger_path,
        serde_json::to_vec(&persisted).expect("serialize ledger"),
    )
    .expect("write ledger");

    let mut first_session = TaskUsageLedger::load(
        codex_home.path(),
        codex_config::types::AuthCredentialsStoreMode::File,
        codex_login::AuthKeyringBackendKind::default(),
    );
    let mut second_session = TaskUsageLedger::load(
        codex_home.path(),
        codex_config::types::AuthCredentialsStoreMode::File,
        codex_login::AuthKeyringBackendKind::default(),
    );

    first_session.record_response("response-a", Some(1.25));
    assert_eq!(first_session.remaining_from_disk(), 98.75);
    second_session.record_response("response-b", Some(2.5));
    second_session.record_response("response-b", Some(2.5));
    second_session.remaining_from_disk();

    let reloaded = TaskUsageLedger::load(
        codex_home.path(),
        codex_config::types::AuthCredentialsStoreMode::File,
        codex_login::AuthKeyringBackendKind::default(),
    );
    assert_eq!(reloaded.remaining_percent(), 96.25);
}

#[test]
fn response_usage_is_persisted_immediately_and_deduplicated() {
    let codex_home = tempfile::tempdir().expect("temporary Codex home");
    let mut first_session = TaskUsageLedger::load(
        codex_home.path(),
        codex_config::types::AuthCredentialsStoreMode::File,
        codex_login::AuthKeyringBackendKind::default(),
    );
    let mut second_session = TaskUsageLedger::load(
        codex_home.path(),
        codex_config::types::AuthCredentialsStoreMode::File,
        codex_login::AuthKeyringBackendKind::default(),
    );

    first_session.record_response("response-a", Some(1.25));
    first_session.remaining_from_disk();
    assert_eq!(
        TaskUsageLedger::load(
            codex_home.path(),
            codex_config::types::AuthCredentialsStoreMode::File,
            codex_login::AuthKeyringBackendKind::default(),
        )
        .remaining_percent(),
        98.75
    );

    second_session.record_response("response-b", Some(2.5));
    second_session.record_response("response-a", Some(1.25));
    second_session.remaining_from_disk();

    let reloaded = TaskUsageLedger::load(
        codex_home.path(),
        codex_config::types::AuthCredentialsStoreMode::File,
        codex_login::AuthKeyringBackendKind::default(),
    );
    assert_eq!(reloaded.remaining_percent(), 96.25);
}

use super::PersistedTaskUsageLedger;
use super::TaskUsageLedger;
use super::current_unix_seconds;
use std::collections::BTreeSet;

const TEST_RESET_AT_UNIX_SECONDS: i64 = 1_700_000_000;

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

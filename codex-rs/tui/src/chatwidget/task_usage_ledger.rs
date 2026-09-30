use std::collections::BTreeSet;
use std::fs;
use std::fs::File;
use std::fs::OpenOptions;
use std::io::Write;
use std::io::{self};
use std::path::Path;
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

use chrono::DateTime;
use codex_protocol::protocol::EventMsg;
use codex_rollout::RolloutItem;
use fd_lock::RwLock;
use serde::Deserialize;
use serde::Serialize;
use tempfile::NamedTempFile;
use tracing::warn;

const CURRENT_WEEKLY_CREDIT_ALLOWANCE: f64 = 2700.0;
const LEGACY_WEEKLY_CREDIT_ALLOWANCE: f64 = 2400.0;

use super::task_usage_scope::TaskUsageScope;

enum ResponseWriterCommand {
    Record {
        response_event_id: String,
        response_used_percent: f64,
        reset_at_unix_seconds: i64,
    },
    ObserveEndpoint {
        current: Option<f64>,
        detected_at_unix_seconds: i64,
    },
    Shutdown,
}

struct ResponseWriter {
    sender: mpsc::Sender<ResponseWriterCommand>,
}

impl ResponseWriter {
    fn start(path: PathBuf, scope: TaskUsageScope) -> Self {
        let (sender, receiver) = mpsc::channel();
        if let Err(err) = std::thread::Builder::new()
            .name("codex-weekly-usage".to_string())
            .spawn(move || response_writer_loop(path, scope, receiver))
        {
            warn!(%err, "failed to start weekly task usage response writer");
        }
        Self { sender }
    }

    fn record(
        &self,
        response_event_id: String,
        response_used_percent: f64,
        reset_at_unix_seconds: i64,
    ) {
        if self
            .sender
            .send(ResponseWriterCommand::Record {
                response_event_id,
                response_used_percent,
                reset_at_unix_seconds,
            })
            .is_err()
        {
            warn!("weekly task usage response writer stopped before recording usage");
        }
    }

    fn observe_endpoint(&self, current: Option<f64>, detected_at_unix_seconds: i64) {
        if self
            .sender
            .send(ResponseWriterCommand::ObserveEndpoint {
                current,
                detected_at_unix_seconds,
            })
            .is_err()
        {
            warn!("weekly task usage response writer stopped before recording rate limits");
        }
    }
}

impl Drop for ResponseWriter {
    fn drop(&mut self) {
        let _ = self.sender.send(ResponseWriterCommand::Shutdown);
    }
}

#[derive(Debug, Deserialize, Serialize)]
struct PersistedTaskUsageLedger {
    #[serde(default)]
    account_scope_id: Option<String>,
    reset_at_unix_seconds: i64,
    used_percent: f64,
    #[serde(default = "legacy_weekly_credit_allowance")]
    weekly_credit_allowance: f64,
    #[serde(default)]
    last_endpoint_remaining_percent: Option<f64>,
    #[serde(default)]
    applied_turn_ids: BTreeSet<String>,
    #[serde(default)]
    applied_response_event_ids: BTreeSet<String>,
}

#[derive(Debug, Deserialize, Serialize)]
struct PendingResponseUsage {
    #[serde(default)]
    account_scope_id: Option<String>,
    reset_at_unix_seconds: i64,
    response_event_id: String,
    response_used_percent: f64,
}

pub(super) struct TaskUsageLedger {
    scope: TaskUsageScope,
    path: Option<PathBuf>,
    reset_at_unix_seconds: i64,
    used_percent: f64,
    last_endpoint_remaining_percent: Option<f64>,
    applied_turn_ids: BTreeSet<String>,
    applied_response_event_ids: BTreeSet<String>,
    response_writer: Option<ResponseWriter>,
}

impl TaskUsageLedger {
    pub(super) fn load(
        codex_home: &Path,
        auth_credentials_store_mode: codex_config::types::AuthCredentialsStoreMode,
        keyring_backend_kind: codex_login::AuthKeyringBackendKind,
    ) -> Self {
        let scope = TaskUsageScope::discover(
            codex_home,
            auth_credentials_store_mode,
            keyring_backend_kind,
        );
        let path = (!codex_home.as_os_str().is_empty()).then(|| scope.ledger_path(codex_home));
        let stored_ledger = path
            .as_deref()
            .and_then(read_ledger)
            .filter(|ledger| scope.accepts_persisted_id(ledger.account_scope_id.as_deref()));
        let current_ledger = stored_ledger
            .as_ref()
            .filter(|ledger| ledger.reset_at_unix_seconds > 0);
        let reset_at_unix_seconds = current_ledger
            .map(|ledger| ledger.reset_at_unix_seconds)
            .unwrap_or_default();
        let used_percent = current_ledger
            .map(|ledger| {
                normalized_used_percent(ledger.used_percent, ledger.weekly_credit_allowance)
            })
            .unwrap_or_else(|| {
                if reset_at_unix_seconds > 0 {
                    recorded_rollout_usage(codex_home, reset_at_unix_seconds)
                } else {
                    Default::default()
                }
                .clamp(0.0, 100.0)
            });
        let mut ledger = Self {
            scope: scope.clone(),
            path,
            reset_at_unix_seconds,
            used_percent,
            last_endpoint_remaining_percent: current_ledger
                .and_then(|ledger| ledger.last_endpoint_remaining_percent)
                .filter(|value| value.is_finite()),
            applied_turn_ids: current_ledger
                .map(|ledger| ledger.applied_turn_ids.clone())
                .unwrap_or_default(),
            applied_response_event_ids: current_ledger
                .map(|ledger| ledger.applied_response_event_ids.clone())
                .unwrap_or_default(),
            response_writer: None,
        };
        if let Some(path) = ledger.path.clone()
            && let Err(err) = ledger.initialize_from_disk(&path)
        {
            warn!(path = %path.display(), %err, "failed to initialize weekly task usage ledger");
        }
        let pending_response_events = ledger
            .path
            .clone()
            .map(|path| replay_pending_response_events(&mut ledger, &path))
            .unwrap_or_default();
        ledger.response_writer = ledger
            .path
            .clone()
            .map(|path| ResponseWriter::start(path, scope));
        if let Some(writer) = &ledger.response_writer {
            for event in pending_response_events {
                writer.record(
                    event.response_event_id,
                    event.response_used_percent,
                    event.reset_at_unix_seconds,
                );
            }
        }
        ledger
    }

    pub(super) fn remaining_percent(&self) -> f64 {
        (100.0 - self.used_percent).clamp(0.0, 100.0)
    }

    pub(super) fn endpoint_remaining_percent(&self) -> Option<f64> {
        self.last_endpoint_remaining_percent
    }

    pub(super) fn record_response(
        &mut self,
        response_event_id: &str,
        response_used_percent: Option<f64>,
    ) {
        let Some(response_used_percent) = response_used_percent
            .filter(|value| value.is_finite())
            .map(|value| value.max(0.0))
        else {
            return;
        };
        if response_event_id.is_empty() {
            return;
        }

        if !self.record_response_unlocked(response_event_id, response_used_percent) {
            return;
        }
        if let Some(path) = &self.path
            && let Err(err) = append_pending_response_event(
                path,
                &self.scope,
                response_event_id,
                response_used_percent,
                self.reset_at_unix_seconds,
            )
        {
            warn!(path = %path.display(), %err, "failed to journal response usage in weekly task ledger");
        }
        if let Some(writer) = &self.response_writer {
            writer.record(
                response_event_id.to_string(),
                response_used_percent,
                self.reset_at_unix_seconds,
            );
        }
    }

    pub(super) fn remaining_from_disk(&mut self) -> f64 {
        if let Some(path) = self.path.clone() {
            self.merge_from_disk(&path);
        }
        self.remaining_percent()
    }

    pub(super) fn observe_endpoint_remaining(&mut self, current: Option<f64>) {
        let detected_at_unix_seconds = current_unix_seconds();
        self.observe_endpoint_remaining_at(current, detected_at_unix_seconds);
        if let Some(writer) = &self.response_writer {
            writer.observe_endpoint(current, detected_at_unix_seconds);
        }
    }

    fn initialize_from_disk(&mut self, path: &Path) -> io::Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut lock = open_lock_file(path)?;
        let _guard = lock.write()?;
        if let Some(ledger) = read_ledger(path).filter(|ledger| {
            ledger.reset_at_unix_seconds > 0
                && self
                    .scope
                    .accepts_persisted_id(ledger.account_scope_id.as_deref())
        }) {
            let needs_migration = ledger.weekly_credit_allowance != CURRENT_WEEKLY_CREDIT_ALLOWANCE;
            self.apply_persisted_ledger(ledger);
            if needs_migration {
                self.persist_unlocked(path)?;
            }
        } else {
            self.persist_unlocked(path)?;
        }
        Ok(())
    }

    fn merge_from_disk(&mut self, path: &Path) {
        if let Some(ledger) = read_ledger(path).filter(|ledger| {
            ledger.reset_at_unix_seconds > 0
                && self
                    .scope
                    .accepts_persisted_id(ledger.account_scope_id.as_deref())
        }) {
            if ledger.reset_at_unix_seconds > self.reset_at_unix_seconds {
                self.apply_persisted_ledger(ledger);
            } else if ledger.reset_at_unix_seconds == self.reset_at_unix_seconds {
                self.used_percent = self.used_percent.max(normalized_used_percent(
                    ledger.used_percent,
                    ledger.weekly_credit_allowance,
                ));
                self.last_endpoint_remaining_percent = ledger
                    .last_endpoint_remaining_percent
                    .filter(|value| value.is_finite())
                    .or(self.last_endpoint_remaining_percent);
                self.applied_turn_ids.extend(ledger.applied_turn_ids);
                self.applied_response_event_ids
                    .extend(ledger.applied_response_event_ids);
            }
        }
    }

    fn apply_persisted_ledger(&mut self, ledger: PersistedTaskUsageLedger) {
        self.reset_at_unix_seconds = ledger.reset_at_unix_seconds;
        self.used_percent =
            normalized_used_percent(ledger.used_percent, ledger.weekly_credit_allowance);
        self.last_endpoint_remaining_percent = ledger
            .last_endpoint_remaining_percent
            .filter(|value| value.is_finite());
        self.applied_turn_ids = ledger.applied_turn_ids;
        self.applied_response_event_ids = ledger.applied_response_event_ids;
    }

    fn record_response_unlocked(
        &mut self,
        response_event_id: &str,
        response_used_percent: f64,
    ) -> bool {
        if !self
            .applied_response_event_ids
            .insert(response_event_id.to_string())
        {
            return false;
        }
        if self.reset_at_unix_seconds == 0 {
            self.reset_at_unix_seconds = current_unix_seconds();
        }
        self.used_percent = (self.used_percent + response_used_percent).min(100.0);
        true
    }

    fn observe_endpoint_remaining_at(&mut self, current: Option<f64>, detected_at: i64) {
        if let Some(current) = current.filter(|value| value.is_finite()) {
            let reset_detected = self.reset_at_unix_seconds == 0
                || endpoint_reset_detected(self.last_endpoint_remaining_percent, current);
            if reset_detected && current >= 100.0 {
                self.reset_at_unix_seconds = detected_at;
                self.used_percent = 0.0;
                self.applied_turn_ids.clear();
                self.applied_response_event_ids.clear();
            }
            self.last_endpoint_remaining_percent = Some(current.clamp(0.0, 100.0));
        }
    }

    fn persist_unlocked(&self, path: &Path) -> io::Result<()> {
        let persisted = PersistedTaskUsageLedger {
            account_scope_id: self.scope.persisted_id(),
            reset_at_unix_seconds: self.reset_at_unix_seconds,
            used_percent: self.used_percent,
            weekly_credit_allowance: CURRENT_WEEKLY_CREDIT_ALLOWANCE,
            last_endpoint_remaining_percent: self.last_endpoint_remaining_percent,
            applied_turn_ids: self.applied_turn_ids.clone(),
            applied_response_event_ids: self.applied_response_event_ids.clone(),
        };
        persist_ledger_unlocked(path, &persisted)
    }
}

fn response_writer_loop(
    path: PathBuf,
    scope: TaskUsageScope,
    receiver: mpsc::Receiver<ResponseWriterCommand>,
) {
    for command in receiver {
        match command {
            ResponseWriterCommand::Record {
                response_event_id,
                response_used_percent,
                reset_at_unix_seconds,
            } => {
                if let Err(err) = record_response_on_disk(
                    &path,
                    &scope,
                    &response_event_id,
                    response_used_percent,
                    reset_at_unix_seconds,
                ) {
                    warn!(path = %path.display(), %err, "failed to persist response usage in weekly task usage ledger");
                }
            }
            ResponseWriterCommand::ObserveEndpoint {
                current,
                detected_at_unix_seconds,
            } => {
                if let Err(err) =
                    observe_endpoint_on_disk(&path, &scope, current, detected_at_unix_seconds)
                {
                    warn!(path = %path.display(), %err, "failed to persist rate limits in weekly task usage ledger");
                }
            }
            ResponseWriterCommand::Shutdown => break,
        }
    }
}

fn record_response_on_disk(
    path: &Path,
    scope: &TaskUsageScope,
    response_event_id: &str,
    response_used_percent: f64,
    reset_at_unix_seconds: i64,
) -> io::Result<()> {
    let mut lock = open_lock_file(path)?;
    let _guard = lock.write()?;
    let mut persisted = read_ledger(path)
        .filter(|ledger| scope.accepts_persisted_id(ledger.account_scope_id.as_deref()))
        .unwrap_or_else(|| empty_persisted_ledger(scope));
    if reset_at_unix_seconds > persisted.reset_at_unix_seconds {
        persisted.reset_at_unix_seconds = reset_at_unix_seconds;
        persisted.used_percent = 0.0;
        persisted.applied_turn_ids.clear();
        persisted.applied_response_event_ids.clear();
    } else if reset_at_unix_seconds < persisted.reset_at_unix_seconds {
        return Ok(());
    }
    if !persisted
        .applied_response_event_ids
        .insert(response_event_id.to_string())
    {
        return Ok(());
    }
    if persisted.reset_at_unix_seconds == 0 {
        persisted.reset_at_unix_seconds = current_unix_seconds();
    }
    persisted.used_percent =
        (normalized_used_percent(persisted.used_percent, persisted.weekly_credit_allowance)
            + response_used_percent)
            .min(100.0);
    persisted.weekly_credit_allowance = CURRENT_WEEKLY_CREDIT_ALLOWANCE;
    persisted.account_scope_id = scope.persisted_id();
    persist_ledger_unlocked(path, &persisted)
}

fn append_pending_response_event(
    path: &Path,
    scope: &TaskUsageScope,
    response_event_id: &str,
    response_used_percent: f64,
    reset_at_unix_seconds: i64,
) -> io::Result<()> {
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path.with_extension("events"))?;
    serde_json::to_writer(
        &mut file,
        &PendingResponseUsage {
            account_scope_id: scope.persisted_id(),
            reset_at_unix_seconds,
            response_event_id: response_event_id.to_string(),
            response_used_percent,
        },
    )?;
    file.write_all(b"\n")?;
    file.flush()
}

fn replay_pending_response_events(
    ledger: &mut TaskUsageLedger,
    path: &Path,
) -> Vec<PendingResponseUsage> {
    let event_path = path.with_extension("events");
    let Ok(contents) = fs::read_to_string(event_path) else {
        return Vec::new();
    };
    let scope = ledger.scope.clone();
    contents
        .lines()
        .filter_map(|line| serde_json::from_str::<PendingResponseUsage>(line).ok())
        .filter(|event| {
            scope.accepts_persisted_id(event.account_scope_id.as_deref())
                && event.reset_at_unix_seconds > 0
                && event.response_used_percent.is_finite()
                && event.response_used_percent >= 0.0
                && !event.response_event_id.is_empty()
        })
        .filter_map(|event| {
            if event.reset_at_unix_seconds > ledger.reset_at_unix_seconds {
                ledger.reset_at_unix_seconds = event.reset_at_unix_seconds;
                ledger.used_percent = 0.0;
                ledger.applied_turn_ids.clear();
                ledger.applied_response_event_ids.clear();
            }
            (event.reset_at_unix_seconds == ledger.reset_at_unix_seconds)
                .then(|| {
                    ledger
                        .record_response_unlocked(
                            &event.response_event_id,
                            event.response_used_percent,
                        )
                        .then_some(event)
                })
                .flatten()
        })
        .collect()
}

fn observe_endpoint_on_disk(
    path: &Path,
    scope: &TaskUsageScope,
    current: Option<f64>,
    detected_at_unix_seconds: i64,
) -> io::Result<()> {
    let mut lock = open_lock_file(path)?;
    let _guard = lock.write()?;
    let mut persisted = read_ledger(path)
        .filter(|ledger| scope.accepts_persisted_id(ledger.account_scope_id.as_deref()))
        .unwrap_or_else(|| empty_persisted_ledger(scope));
    if let Some(current) = current.filter(|value| value.is_finite()) {
        let reset_detected = persisted.reset_at_unix_seconds == 0
            || endpoint_reset_detected(persisted.last_endpoint_remaining_percent, current);
        if reset_detected && current >= 100.0 {
            persisted.reset_at_unix_seconds = detected_at_unix_seconds;
            persisted.used_percent = 0.0;
            persisted.applied_turn_ids.clear();
            persisted.applied_response_event_ids.clear();
        }
        persisted.last_endpoint_remaining_percent = Some(current.clamp(0.0, 100.0));
    }
    persisted.weekly_credit_allowance = CURRENT_WEEKLY_CREDIT_ALLOWANCE;
    persisted.account_scope_id = scope.persisted_id();
    persist_ledger_unlocked(path, &persisted)
}

fn empty_persisted_ledger(scope: &TaskUsageScope) -> PersistedTaskUsageLedger {
    PersistedTaskUsageLedger {
        account_scope_id: scope.persisted_id(),
        reset_at_unix_seconds: 0,
        used_percent: 0.0,
        weekly_credit_allowance: CURRENT_WEEKLY_CREDIT_ALLOWANCE,
        last_endpoint_remaining_percent: None,
        applied_turn_ids: BTreeSet::new(),
        applied_response_event_ids: BTreeSet::new(),
    }
}

fn persist_ledger_unlocked(path: &Path, persisted: &PersistedTaskUsageLedger) -> io::Result<()> {
    let contents = serde_json::to_vec_pretty(&persisted).map_err(io::Error::other)?;
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let mut temporary = NamedTempFile::new_in(parent)?;
    temporary.write_all(&contents)?;
    temporary.flush()?;
    temporary.persist(path).map_err(|err| err.error)?;
    Ok(())
}

fn legacy_weekly_credit_allowance() -> f64 {
    LEGACY_WEEKLY_CREDIT_ALLOWANCE
}

fn normalized_used_percent(used_percent: f64, weekly_credit_allowance: f64) -> f64 {
    if weekly_credit_allowance.is_finite()
        && weekly_credit_allowance > 0.0
        && weekly_credit_allowance != CURRENT_WEEKLY_CREDIT_ALLOWANCE
    {
        used_percent * weekly_credit_allowance / CURRENT_WEEKLY_CREDIT_ALLOWANCE
    } else {
        used_percent
    }
    .clamp(0.0, 100.0)
}

fn open_lock_file(path: &Path) -> io::Result<RwLock<File>> {
    let lock_path = path.with_extension("lock");
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(lock_path)?;
    Ok(RwLock::new(file))
}

fn endpoint_reset_detected(previous: Option<f64>, current: f64) -> bool {
    previous.is_some_and(|previous| previous < 100.0) && current >= 100.0
}

fn read_ledger(path: &Path) -> Option<PersistedTaskUsageLedger> {
    match fs::read(path) {
        Ok(contents) => match serde_json::from_slice(&contents) {
            Ok(ledger) => Some(ledger),
            Err(err) => {
                warn!(path = %path.display(), %err, "failed to parse weekly task usage ledger");
                None
            }
        },
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
        Err(err) => {
            warn!(path = %path.display(), %err, "failed to read weekly task usage ledger");
            None
        }
    }
}

fn recorded_rollout_usage(codex_home: &Path, reset_at_unix_seconds: i64) -> f64 {
    [
        codex_rollout::SESSIONS_SUBDIR,
        codex_rollout::ARCHIVED_SESSIONS_SUBDIR,
    ]
    .into_iter()
    .map(|subdir| rollout_usage_in_directory(&codex_home.join(subdir), reset_at_unix_seconds))
    .sum()
}

fn rollout_usage_in_directory(directory: &Path, reset_at_unix_seconds: i64) -> f64 {
    let mut directories = vec![directory.to_path_buf()];
    let mut total = 0.0;
    while let Some(directory) = directories.pop() {
        let Ok(entries) = fs::read_dir(directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                directories.push(path);
            } else if path
                .extension()
                .is_some_and(|extension| extension == "jsonl")
            {
                total += rollout_usage_in_file(&path, reset_at_unix_seconds);
            }
        }
    }
    total
}

fn rollout_usage_in_file(path: &Path, reset_at_unix_seconds: i64) -> f64 {
    let Ok(contents) = fs::read_to_string(path) else {
        return 0.0;
    };
    contents
        .lines()
        .filter_map(|line| codex_rollout::parse_rollout_line(line).ok())
        .filter(|line| {
            DateTime::parse_from_rfc3339(&line.timestamp)
                .map(|timestamp| timestamp.timestamp() >= reset_at_unix_seconds)
                .unwrap_or(false)
        })
        .filter_map(|line| match line.item {
            RolloutItem::EventMsg(EventMsg::TaskUsageSummary(summary)) => {
                summary.weekly_limit_used_percent
            }
            _ => None,
        })
        .sum()
}

fn current_unix_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| i64::try_from(duration.as_secs()).unwrap_or(i64::MAX))
        .unwrap_or_default()
}

#[cfg(test)]
#[path = "task_usage_ledger_tests.rs"]
mod tests;

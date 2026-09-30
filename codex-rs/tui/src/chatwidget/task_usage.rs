use chrono::DateTime;
use chrono::Local;
use chrono::Utc;
use codex_app_server_protocol::RateLimitSnapshot;
use codex_otel::RuntimeMetricTotals;
use codex_otel::RuntimeMetricsSummary;
use codex_protocol::num_format::format_with_separators;
use codex_protocol::protocol::TaskUsageSummaryEvent;
use ratatui::style::Stylize;
use ratatui::text::Line;
use std::collections::HashSet;
use std::time::Instant;

use super::ChatWidget;
use crate::app_command::AppCommand;
use crate::app_event::AppEvent;
use crate::history_cell::HistoryCell;
use crate::token_usage::TokenUsage;
use crate::token_usage::TokenUsageInfo;

use super::task_workspace::TaskWorkspaceTracker;
use super::task_workspace::WorkspaceDiffStats;

const WEEKLY_LIMIT_WINDOW_MINUTES: i64 = 7 * 24 * 60;
const PLUS_WEEKLY_CREDIT_PERCENT_DENOMINATOR: f64 = 27_000_000.0;
const TASK_USAGE_RAW_DIVIDER_WIDTH: usize = 80;

#[derive(Debug)]
pub(super) struct TaskUsageBaseline {
    token_usage: Option<TokenUsage>,
    descendant_usage: TokenUsage,
    descendant_weekly_limit_used_percent: Option<f64>,
    descendant_response_event_ids: HashSet<String>,
    turn_id: String,
    model: String,
    diff_stats: WorkspaceDiffStats,
    workspace_tracker: TaskWorkspaceTracker,
    started_at: Instant,
    first_output_ms: Option<u64>,
    local_tool_intervals_ms: Vec<(u64, u64)>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct TaskUsageContribution {
    response_event_id: String,
    usage: TokenUsage,
    weekly_limit_used_percent: Option<f64>,
}

impl TaskUsageContribution {
    #[cfg(test)]
    pub(crate) fn new(thread_id: &str, turn_id: &str, model: &str, info: &TokenUsageInfo) -> Self {
        Self {
            response_event_id: response_usage_event_id(thread_id, turn_id, &info.total_token_usage),
            usage: info.last_token_usage.clone(),
            weekly_limit_used_percent: weekly_limit_used_percent(model, &info.last_token_usage),
        }
    }

    pub(crate) fn from_response(response_id: &str, model: &str, usage: TokenUsage) -> Self {
        Self {
            response_event_id: response_id.to_string(),
            weekly_limit_used_percent: weekly_limit_used_percent(model, &usage),
            usage,
        }
    }

    pub(crate) fn response_event_id(&self) -> &str {
        &self.response_event_id
    }
}

type TaskDiffStats = WorkspaceDiffStats;

#[derive(Debug, Default)]
struct TaskTiming {
    duration_ms: Option<i64>,
    finished_at: Option<i64>,
    runtime_metrics: RuntimeMetricsSummary,
    first_output_ms: Option<u64>,
    local_tool_duration_ms: u64,
}

impl ChatWidget {
    pub(super) fn capture_task_usage_baseline(&mut self) {
        let model = self.current_model().to_string();
        let turn_id = self.turn_lifecycle.last_turn_id.clone().unwrap_or_default();
        self.turn_lifecycle.task_usage_baseline = Some(TaskUsageBaseline {
            token_usage: self
                .token_info
                .as_ref()
                .map(|info| info.total_token_usage.clone())
                .filter(|usage| !usage.is_zero()),
            descendant_usage: TokenUsage::default(),
            descendant_weekly_limit_used_percent: Some(0.0),
            descendant_response_event_ids: HashSet::new(),
            turn_id: turn_id.clone(),
            model,
            diff_stats: TaskDiffStats::default(),
            workspace_tracker: TaskWorkspaceTracker::start(
                (!turn_id.is_empty()).then_some(turn_id.as_str()),
            ),
            started_at: Instant::now(),
            first_output_ms: None,
            local_tool_intervals_ms: Vec::new(),
        });
    }

    pub(super) fn record_task_first_output(&mut self) {
        if let Some(baseline) = self.turn_lifecycle.task_usage_baseline.as_mut()
            && baseline.first_output_ms.is_none()
        {
            baseline.first_output_ms =
                u64::try_from(baseline.started_at.elapsed().as_millis()).ok();
        }
    }

    pub(super) fn record_task_local_tool_duration(&mut self, duration_ms: Option<i64>) {
        if let Some(duration_ms) = duration_ms.and_then(|value| u64::try_from(value).ok())
            && let Some(baseline) = self.turn_lifecycle.task_usage_baseline.as_mut()
        {
            let end_ms =
                u64::try_from(baseline.started_at.elapsed().as_millis()).unwrap_or(u64::MAX);
            let start_ms = end_ms.saturating_sub(duration_ms);
            baseline.local_tool_intervals_ms.push((start_ms, end_ms));
        }
    }

    pub(super) fn record_task_response_usage(
        &mut self,
        _thread_id: &str,
        _turn_id: &str,
        info: &TokenUsageInfo,
    ) {
        let Some(baseline) = self.turn_lifecycle.task_usage_baseline.as_mut() else {
            return;
        };
        if baseline.token_usage.is_none() {
            baseline.token_usage = Some(token_usage_delta(
                &info.last_token_usage,
                &info.total_token_usage,
            ));
        }
    }

    pub(crate) fn record_task_response_contribution(
        &mut self,
        contribution: &TaskUsageContribution,
    ) {
        self.task_usage_ledger.record_response(
            contribution.response_event_id(),
            contribution.weekly_limit_used_percent,
        );
    }

    pub(crate) fn record_descendant_task_usage(
        &mut self,
        root_turn_id: &str,
        contributions: Vec<TaskUsageContribution>,
    ) {
        let Some(baseline) = self.turn_lifecycle.task_usage_baseline.as_mut() else {
            return;
        };
        if baseline.turn_id != root_turn_id {
            return;
        }

        for contribution in contributions {
            if !baseline
                .descendant_response_event_ids
                .insert(contribution.response_event_id)
            {
                continue;
            }
            token_usage_add_assign(&mut baseline.descendant_usage, &contribution.usage);
            baseline.descendant_weekly_limit_used_percent = baseline
                .descendant_weekly_limit_used_percent
                .zip(contribution.weekly_limit_used_percent)
                .map(|(total, contribution)| total + contribution);
        }
    }

    pub(super) fn record_task_file_changes(
        &mut self,
        changes: &[codex_app_server_protocol::FileUpdateChange],
    ) {
        if let Some(baseline) = self.turn_lifecycle.task_usage_baseline.as_mut() {
            baseline
                .workspace_tracker
                .record_file_changes(changes, &self.config.cwd);
        }
    }

    pub(super) fn observe_weekly_plan_remaining(&mut self, current: Option<f64>) {
        self.task_usage_ledger.observe_endpoint_remaining(current);
    }

    pub(super) fn take_task_usage_summary(
        &mut self,
        duration_ms: Option<i64>,
        runtime_metrics: RuntimeMetricsSummary,
    ) -> Option<TaskUsageSummaryEvent> {
        self.take_task_usage_summary_unfinalized(duration_ms, runtime_metrics)
            .map(|summary| self.finalize_task_usage_summary(summary))
    }

    pub(super) fn defer_task_usage_summary(
        &mut self,
        duration_ms: Option<i64>,
        runtime_metrics: RuntimeMetricsSummary,
    ) -> bool {
        let summary = self.take_task_usage_summary_unfinalized(duration_ms, runtime_metrics);
        if !self.has_chatgpt_account {
            if let Some(summary) = summary {
                let summary = self.finalize_task_usage_summary(summary);
                self.append_task_usage_summary(summary);
                return true;
            }
            return false;
        }

        let request_id = self.next_task_usage_refresh_request_id;
        self.next_task_usage_refresh_request_id = request_id.wrapping_add(1);
        let has_summary = summary.is_some();
        self.pending_task_usage_summaries
            .insert(request_id, summary);
        self.app_event_tx.send(AppEvent::RefreshRateLimits {
            origin: crate::app_event::RateLimitRefreshOrigin::TaskCompletion { request_id },
        });
        has_summary
    }

    pub(crate) fn finish_task_usage_rate_limit_refresh(
        &mut self,
        request_id: u64,
        snapshots: Vec<RateLimitSnapshot>,
    ) {
        for snapshot in snapshots {
            self.on_rate_limit_snapshot(Some(snapshot));
        }
        if let Some(Some(summary)) = self.pending_task_usage_summaries.remove(&request_id) {
            let summary = self.finalize_task_usage_summary(summary);
            self.append_task_usage_summary(summary);
        }
    }

    fn take_task_usage_summary_unfinalized(
        &mut self,
        duration_ms: Option<i64>,
        runtime_metrics: RuntimeMetricsSummary,
    ) -> Option<TaskUsageSummaryEvent> {
        let mut baseline = self.turn_lifecycle.task_usage_baseline.take()?;
        baseline
            .diff_stats
            .add_assign(baseline.workspace_tracker.finish());
        let diff_stats = baseline.diff_stats;
        let end_usage = self.total_token_usage();
        let mut usage = baseline.token_usage.as_ref().map_or_else(
            || {
                self.token_info
                    .as_ref()
                    .map(|info| info.last_token_usage.clone())
                    .unwrap_or_default()
            },
            |start_usage| token_usage_delta(start_usage, &end_usage),
        );
        let root_weekly_limit_used_percent = weekly_limit_used_percent(&baseline.model, &usage);
        token_usage_add_assign(&mut usage, &baseline.descendant_usage);
        let weekly_limit_used_percent = root_weekly_limit_used_percent
            .zip(baseline.descendant_weekly_limit_used_percent)
            .map(|(root, descendants)| root + descendants);
        #[cfg(not(test))]
        let local_tool_time_ms = union_duration_ms(&baseline.local_tool_intervals_ms);
        #[cfg(test)]
        let local_tool_time_ms = 0_u64;
        #[cfg(not(test))]
        let duration_ms =
            duration_ms.or_else(|| i64::try_from(baseline.started_at.elapsed().as_millis()).ok());
        #[cfg(test)]
        let duration_ms = Some(duration_ms.unwrap_or_default());

        if usage.is_zero()
            && weekly_limit_used_percent.is_none()
            && diff_stats.is_empty()
            && duration_ms.is_none()
            && runtime_metrics.is_empty()
            && local_tool_time_ms == 0
        {
            return None;
        }

        Some(TaskUsageSummaryEvent {
            turn_id: baseline.turn_id,
            model: baseline.model,
            total_tokens: usage.total_tokens,
            input_tokens: usage.input_tokens,
            cached_input_tokens: usage.cached_input_tokens,
            output_tokens: usage.output_tokens,
            reasoning_output_tokens: usage.reasoning_output_tokens,
            weekly_limit_used_percent,
            calculated_weekly_remaining_percent: None,
            plan_remaining_percent: None,
            files_changed: i64::try_from(diff_stats.files_changed).unwrap_or(i64::MAX),
            files_created: i64::try_from(diff_stats.files_created).unwrap_or(i64::MAX),
            files_deleted: i64::try_from(diff_stats.files_deleted).unwrap_or(i64::MAX),
            files_modified: i64::try_from(diff_stats.files_modified()).unwrap_or(i64::MAX),
            lines_added: i64::try_from(diff_stats.lines_added).unwrap_or(i64::MAX),
            lines_removed: i64::try_from(diff_stats.lines_removed).unwrap_or(i64::MAX),
            wall_time_ms: duration_ms,
            model_time_ms: (runtime_metrics.responses_api_inference_time_ms > 0).then(|| {
                i64::try_from(runtime_metrics.responses_api_inference_time_ms).unwrap_or(i64::MAX)
            }),
            local_tool_time_ms: (local_tool_time_ms > 0)
                .then_some(i64::try_from(local_tool_time_ms).unwrap_or(i64::MAX)),
            overhead_time_ms: (runtime_metrics.responses_api_overhead_ms > 0).then(|| {
                i64::try_from(runtime_metrics.responses_api_overhead_ms).unwrap_or(i64::MAX)
            }),
            #[cfg(not(test))]
            first_output_ms: (runtime_metrics.turn_ttft_ms > 0)
                .then(|| i64::try_from(runtime_metrics.turn_ttft_ms).unwrap_or(i64::MAX))
                .or_else(|| {
                    baseline
                        .first_output_ms
                        .and_then(|value| i64::try_from(value).ok())
                }),
            #[cfg(test)]
            first_output_ms: None,
            #[cfg(not(test))]
            finished_at: Some(Utc::now().timestamp()),
            #[cfg(test)]
            finished_at: Some(1_767_225_600),
        })
    }

    fn finalize_task_usage_summary(
        &mut self,
        mut summary: TaskUsageSummaryEvent,
    ) -> TaskUsageSummaryEvent {
        let calculated_weekly_remaining_percent = self.task_usage_ledger.remaining_from_disk();
        summary.calculated_weekly_remaining_percent = Some(calculated_weekly_remaining_percent);
        summary.plan_remaining_percent = self
            .task_usage_ledger
            .endpoint_remaining_percent()
            .or(Some(calculated_weekly_remaining_percent));
        summary
    }

    pub(super) fn append_task_usage_summary(&mut self, summary: TaskUsageSummaryEvent) {
        self.add_to_history(task_usage_summary_history_cell_from_summary(&summary));
        self.app_event_tx
            .send(AppEvent::CodexOp(AppCommand::RecordTaskUsage { summary }));
    }

    fn total_token_usage(&self) -> TokenUsage {
        self.token_info
            .as_ref()
            .map(|info| info.total_token_usage.clone())
            .unwrap_or_default()
    }

    fn weekly_limit_used_percent(&self) -> Option<f64> {
        self.rate_limit_snapshots_by_limit_id
            .iter()
            .find(|(limit_id, _)| limit_id.eq_ignore_ascii_case("codex"))
            .into_iter()
            .flat_map(|(_, snapshot)| [snapshot.primary.as_ref(), snapshot.secondary.as_ref()])
            .flatten()
            .find(|window| window.window_minutes == Some(WEEKLY_LIMIT_WINDOW_MINUTES))
            .map(|window| window.used_percent)
    }

    pub(super) fn weekly_plan_remaining_percent(&self) -> Option<f64> {
        self.weekly_limit_used_percent()
            .map(|used| (100.0 - used).clamp(0.0, 100.0))
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct ModelCreditRates {
    uncached_input: f64,
    cached_input: f64,
    output: f64,
}

fn weekly_limit_used_percent(model: &str, usage: &TokenUsage) -> Option<f64> {
    let rates = model_credit_rates(model)?;
    let cached_input = usage.cached_input() as f64;
    let uncached_input = usage.non_cached_input() as f64;
    let output = usage.output_tokens.max(0) as f64;
    Some(
        (rates.uncached_input * uncached_input
            + rates.cached_input * cached_input
            + rates.output * output)
            / PLUS_WEEKLY_CREDIT_PERCENT_DENOMINATOR,
    )
}

#[cfg(test)]
fn response_usage_event_id(thread_id: &str, turn_id: &str, total_usage: &TokenUsage) -> String {
    format!(
        "response:{thread_id}:{turn_id}:{}:{}:{}:{}:{}",
        total_usage.input_tokens,
        total_usage.cached_input_tokens,
        total_usage.output_tokens,
        total_usage.reasoning_output_tokens,
        total_usage.total_tokens,
    )
}

fn model_credit_rates(model: &str) -> Option<ModelCreditRates> {
    let model = model.to_ascii_lowercase();
    if model.contains("gpt-5.4-mini") {
        Some(ModelCreditRates {
            uncached_input: 18.75,
            cached_input: 1.875,
            output: 113.0,
        })
    } else if model.contains("gpt-6-astra") {
        Some(ModelCreditRates {
            uncached_input: 250.0,
            cached_input: 25.0,
            output: 1_250.0,
        })
    } else if model.contains("gpt-6.1-sol") {
        Some(ModelCreditRates {
            uncached_input: 50.0,
            cached_input: 2.5,
            output: 250.0,
        })
    } else if model.contains("gpt-6-sol") {
        Some(ModelCreditRates {
            uncached_input: 50.0,
            cached_input: 5.0,
            output: 250.0,
        })
    } else if model.contains("gpt-6-luna") {
        Some(ModelCreditRates {
            uncached_input: 2.5,
            cached_input: 0.25,
            output: 12.5,
        })
    } else if model.contains("gpt-5.6-sol") {
        Some(ModelCreditRates {
            uncached_input: 100.0,
            cached_input: 10.0,
            output: 500.0,
        })
    } else if model.contains("gpt-5.6-terra") || model.contains("gpt-5.4") {
        Some(ModelCreditRates {
            uncached_input: 62.5,
            cached_input: 6.25,
            output: 375.0,
        })
    } else if model.contains("gpt-5.6-luna") {
        Some(ModelCreditRates {
            uncached_input: 5.0,
            cached_input: 0.5,
            output: 30.0,
        })
    } else {
        None
    }
}

fn token_usage_delta(start: &TokenUsage, end: &TokenUsage) -> TokenUsage {
    TokenUsage {
        input_tokens: (end.input_tokens - start.input_tokens).max(0),
        cached_input_tokens: (end.cached_input_tokens - start.cached_input_tokens).max(0),
        output_tokens: (end.output_tokens - start.output_tokens).max(0),
        reasoning_output_tokens: (end.reasoning_output_tokens - start.reasoning_output_tokens)
            .max(0),
        total_tokens: (end.total_tokens - start.total_tokens).max(0),
    }
}

fn token_usage_add_assign(total: &mut TokenUsage, contribution: &TokenUsage) {
    total.input_tokens = total.input_tokens.saturating_add(contribution.input_tokens);
    total.cached_input_tokens = total
        .cached_input_tokens
        .saturating_add(contribution.cached_input_tokens);
    total.output_tokens = total
        .output_tokens
        .saturating_add(contribution.output_tokens);
    total.reasoning_output_tokens = total
        .reasoning_output_tokens
        .saturating_add(contribution.reasoning_output_tokens);
    total.total_tokens = total.total_tokens.saturating_add(contribution.total_tokens);
}

fn union_duration_ms(intervals: &[(u64, u64)]) -> u64 {
    let mut intervals = intervals
        .iter()
        .copied()
        .filter(|(start, end)| start < end)
        .collect::<Vec<_>>();
    intervals.sort_unstable();

    let Some((mut current_start, mut current_end)) = intervals.first().copied() else {
        return 0;
    };
    let mut total = 0u64;

    for (start, end) in intervals.into_iter().skip(1) {
        if start > current_end {
            total = total.saturating_add(current_end - current_start);
            current_start = start;
            current_end = end;
        } else {
            current_end = current_end.max(end);
        }
    }

    total.saturating_add(current_end - current_start)
}

fn task_usage_lines(
    model: &str,
    usage: &TokenUsage,
    weekly_limit_used_percent: Option<f64>,
    _calculated_weekly_remaining_percent: Option<f64>,
    plan_remaining_percent: Option<f64>,
    diff_stats: TaskDiffStats,
    timing: TaskTiming,
) -> Vec<Line<'static>> {
    let cached_input = usage.cached_input();
    let non_cached_input = usage.non_cached_input();
    let reasoning_output = usage.reasoning_output_tokens.max(0);
    let normal_output = (usage.output_tokens - reasoning_output).max(0);
    let token_percentage_units =
        token_usage_percentage_units(model, usage, weekly_limit_used_percent);
    let total_token_percent =
        token_percentage_units.map(|percentages| format_percent_units(percentages.iter().sum()));
    let plan_remaining = plan_remaining_percent.map(format_plan_remaining_percent);
    let total_time = timing
        .duration_ms
        .and_then(|duration_ms| u64::try_from(duration_ms).ok())
        .map(format_duration_ms)
        .unwrap_or_else(|| "n/a".to_string());
    let local_tool_duration_ms = timing
        .runtime_metrics
        .tool_calls
        .duration_ms
        .max(timing.local_tool_duration_ms);
    let first_token_time = (timing.runtime_metrics.turn_ttft_ms > 0)
        .then_some(timing.runtime_metrics.turn_ttft_ms)
        .or(timing.first_output_ms)
        .map(format_duration_ms);

    let mut time_line = vec!["• ".dim(), "time wall ".dim(), total_time.cyan().bold()];
    let mut breakdown = Vec::new();
    if local_tool_duration_ms > 0 {
        breakdown.extend([
            "local tools ".dim(),
            format_duration_ms(local_tool_duration_ms).green(),
        ]);
    }
    if !breakdown.is_empty() {
        time_line.push(" (".dim());
        time_line.extend(breakdown);
        time_line.push(")".dim());
    }
    if let Some(first_token_time) = first_token_time {
        time_line.extend([" · first output ".dim(), first_token_time.magenta()]);
    }
    if let Some(finished_at) = timing.finished_at.map(format_finished_at) {
        time_line.extend([" · finished ".dim(), finished_at.cyan()]);
    }

    let mut weekly_limit_line = vec!["• ".dim(), "weekly limit remaining:   ".dim()];
    if let Some(plan_remaining) = plan_remaining {
        weekly_limit_line.push(plan_remaining.cyan().bold());
    } else {
        weekly_limit_line.push("n/a".dim());
    }

    let token_percent = |index: usize| {
        token_percentage_units.map(|percentages| format_percent_units(percentages[index]))
    };

    vec![
        weekly_limit_line.into(),
        vec![
            "• ".dim(),
            "tokens: total:            ".dim(),
            format_with_separators(usage.total_tokens).cyan().bold(),
            total_token_percent
                .map(|percent| format!(" ({percent})"))
                .unwrap_or_default()
                .cyan()
                .bold(),
        ]
        .into(),
        vec![
            "          ".dim(),
            "cached-input:     ".dim(),
            format_with_separators(cached_input).magenta(),
            token_percent(0)
                .map(|percent| format!(" ({percent})"))
                .unwrap_or_default()
                .magenta(),
        ]
        .into(),
        vec![
            "          ".dim(),
            "non-cached-input: ".dim(),
            format_with_separators(non_cached_input).cyan(),
            token_percent(1)
                .map(|percent| format!(" ({percent})"))
                .unwrap_or_default()
                .cyan(),
        ]
        .into(),
        vec![
            "          ".dim(),
            "normal-output:    ".dim(),
            format_with_separators(normal_output).green(),
            token_percent(2)
                .map(|percent| format!(" ({percent})"))
                .unwrap_or_default()
                .green(),
        ]
        .into(),
        vec![
            "          ".dim(),
            "reasoning-output: ".dim(),
            format_with_separators(reasoning_output).magenta(),
            token_percent(3)
                .map(|percent| format!(" ({percent})"))
                .unwrap_or_default()
                .magenta(),
        ]
        .into(),
        vec![
            "• ".dim(),
            "files created ".dim(),
            format_with_separators(i64::try_from(diff_stats.files_created).unwrap_or(i64::MAX))
                .green(),
            " · deleted ".dim(),
            format_with_separators(i64::try_from(diff_stats.files_deleted).unwrap_or(i64::MAX))
                .red(),
            " · modified ".dim(),
            format_with_separators(i64::try_from(diff_stats.files_modified()).unwrap_or(i64::MAX))
                .cyan(),
            " · lines +".dim(),
            format_with_separators(i64::try_from(diff_stats.lines_added).unwrap_or(i64::MAX))
                .green(),
            " / -".dim(),
            format_with_separators(i64::try_from(diff_stats.lines_removed).unwrap_or(i64::MAX))
                .red(),
        ]
        .into(),
        time_line.into(),
    ]
}

#[derive(Debug)]
pub(crate) struct TaskUsageSummaryHistoryCell {
    lines: Vec<Line<'static>>,
}

impl TaskUsageSummaryHistoryCell {
    fn new(lines: Vec<Line<'static>>) -> Self {
        Self { lines }
    }
}

impl HistoryCell for TaskUsageSummaryHistoryCell {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        let divider = || Line::from_iter(["─".repeat(width as usize).dim()]);
        let mut lines = Vec::with_capacity(self.lines.len().saturating_add(2));
        lines.push(divider());
        lines.extend(self.lines.clone());
        lines.push(divider());
        lines
    }

    fn raw_lines(&self) -> Vec<Line<'static>> {
        let mut lines = Vec::with_capacity(self.lines.len().saturating_add(2));
        lines.push(Line::from("─".repeat(TASK_USAGE_RAW_DIVIDER_WIDTH)));
        lines.extend(crate::history_cell::plain_lines(self.lines.clone()));
        lines.push(Line::from("─".repeat(TASK_USAGE_RAW_DIVIDER_WIDTH)));
        lines
    }
}

pub(crate) fn task_usage_summary_history_cell_from_summary(
    summary: &TaskUsageSummaryEvent,
) -> TaskUsageSummaryHistoryCell {
    TaskUsageSummaryHistoryCell::new(task_usage_lines_from_summary(summary))
}

pub(super) fn task_usage_lines_from_summary(summary: &TaskUsageSummaryEvent) -> Vec<Line<'static>> {
    let runtime_metrics = RuntimeMetricsSummary {
        tool_calls: RuntimeMetricTotals {
            duration_ms: summary
                .local_tool_time_ms
                .and_then(|value| u64::try_from(value).ok())
                .unwrap_or_default(),
            ..Default::default()
        },
        responses_api_inference_time_ms: summary
            .model_time_ms
            .and_then(|value| u64::try_from(value).ok())
            .unwrap_or_default(),
        responses_api_overhead_ms: summary
            .overhead_time_ms
            .and_then(|value| u64::try_from(value).ok())
            .unwrap_or_default(),
        turn_ttft_ms: summary
            .first_output_ms
            .and_then(|value| u64::try_from(value).ok())
            .unwrap_or_default(),
        ..Default::default()
    };
    task_usage_lines(
        &summary.model,
        &TokenUsage {
            input_tokens: summary.input_tokens,
            cached_input_tokens: summary.cached_input_tokens,
            output_tokens: summary.output_tokens,
            reasoning_output_tokens: summary.reasoning_output_tokens,
            total_tokens: summary.total_tokens,
        },
        summary.weekly_limit_used_percent,
        summary.calculated_weekly_remaining_percent,
        summary.plan_remaining_percent,
        TaskDiffStats {
            files_changed: usize::try_from(summary.files_changed).unwrap_or(usize::MAX),
            files_created: usize::try_from(summary.files_created).unwrap_or(usize::MAX),
            files_deleted: usize::try_from(summary.files_deleted).unwrap_or(usize::MAX),
            lines_added: usize::try_from(summary.lines_added).unwrap_or(usize::MAX),
            lines_removed: usize::try_from(summary.lines_removed).unwrap_or(usize::MAX),
        },
        TaskTiming {
            duration_ms: summary.wall_time_ms,
            finished_at: summary.finished_at,
            runtime_metrics,
            first_output_ms: None,
            local_tool_duration_ms: 0,
        },
    )
}

fn format_percent(percent: f64) -> String {
    let formatted = format!("{percent:.4}");
    let formatted = formatted.trim_end_matches('0').trim_end_matches('.');
    format!("{formatted}%")
}

fn format_finished_at(timestamp: i64) -> String {
    DateTime::<Utc>::from_timestamp(timestamp, 0)
        .map(|timestamp| {
            timestamp
                .with_timezone(&Local)
                .format("%Y-%m-%d %H:%M:%S")
                .to_string()
        })
        .unwrap_or_else(|| "unknown".to_string())
}

fn format_percent_units(units: i64) -> String {
    format_percent(units as f64 / 10_000.0)
}

fn token_usage_percentage_units(
    model: &str,
    usage: &TokenUsage,
    weekly_limit_used_percent: Option<f64>,
) -> Option<[i64; 4]> {
    let cached_input = usage.cached_input().max(0) as f64;
    let non_cached_input = usage.non_cached_input().max(0) as f64;
    let normal_output = (usage.output_tokens - usage.reasoning_output_tokens).max(0) as f64;
    let reasoning_output = usage.reasoning_output_tokens.max(0) as f64;
    let token_counts = [
        cached_input,
        non_cached_input,
        normal_output,
        reasoning_output,
    ];
    let percentages = if let Some(rates) = model_credit_rates(model) {
        [
            rates.cached_input * cached_input / PLUS_WEEKLY_CREDIT_PERCENT_DENOMINATOR,
            rates.uncached_input * non_cached_input / PLUS_WEEKLY_CREDIT_PERCENT_DENOMINATOR,
            rates.output * normal_output / PLUS_WEEKLY_CREDIT_PERCENT_DENOMINATOR,
            rates.output * reasoning_output / PLUS_WEEKLY_CREDIT_PERCENT_DENOMINATOR,
        ]
    } else {
        let total_tokens = token_counts.iter().sum::<f64>();
        let total_percent = weekly_limit_used_percent?;
        if total_tokens <= 0.0 {
            return None;
        }
        token_counts.map(|count| total_percent * count / total_tokens)
    };
    let total_percent = weekly_limit_used_percent.unwrap_or_else(|| percentages.iter().sum());
    let target_units = (total_percent * 10_000.0).round() as i64;
    let mut units = percentages.map(|percent| (percent * 10_000.0).round() as i64);
    let rounded_total = units.iter().sum::<i64>();
    let adjustment = target_units - rounded_total;
    if adjustment != 0 {
        let adjustment_index = units
            .iter()
            .enumerate()
            .max_by_key(|(_, units)| **units)
            .map(|(index, _)| index)
            .unwrap_or(0);
        units[adjustment_index] += adjustment;
    }
    Some(units)
}

fn format_plan_remaining_percent(percent: f64) -> String {
    format!("{percent:.0}%")
}

fn format_duration_ms(duration_ms: u64) -> String {
    const TENTHS_PER_MINUTE: u64 = 600;
    const TENTHS_PER_HOUR: u64 = 36_000;

    if duration_ms < 1_000 {
        return format!("{duration_ms}ms");
    }

    let rounded_tenths = duration_ms.saturating_add(50) / 100;
    let seconds = (rounded_tenths % TENTHS_PER_MINUTE) as f64 / 10.0;
    if rounded_tenths >= TENTHS_PER_HOUR {
        let hours = rounded_tenths / TENTHS_PER_HOUR;
        let minutes = (rounded_tenths % TENTHS_PER_HOUR) / TENTHS_PER_MINUTE;
        format!("{hours}h {minutes}m {seconds:.1}s")
    } else if rounded_tenths >= TENTHS_PER_MINUTE {
        let minutes = rounded_tenths / TENTHS_PER_MINUTE;
        format!("{minutes}m {seconds:.1}s")
    } else {
        format!("{seconds:.1}s")
    }
}

#[cfg(test)]
#[path = "task_usage_tests.rs"]
mod tests;

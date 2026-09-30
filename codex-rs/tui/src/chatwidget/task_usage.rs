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

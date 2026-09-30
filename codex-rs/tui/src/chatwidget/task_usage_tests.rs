use super::*;

#[test]
fn task_usage_lines_show_colored_task_summary() {
    let usage = TokenUsage {
        input_tokens: 12_726_447,
        cached_input_tokens: 12_393_984,
        output_tokens: 22_477,
        reasoning_output_tokens: 10_418,
        total_tokens: 12_748_924,
    };

    let runtime_metrics = RuntimeMetricsSummary {
        tool_calls: codex_otel::RuntimeMetricTotals {
            count: 3,
            duration_ms: 101_700,
        },
        responses_api_overhead_ms: 2_100,
        responses_api_inference_time_ms: 21_400,
        turn_ttft_ms: 4_000,
        ..RuntimeMetricsSummary::default()
    };

    insta::assert_debug_snapshot!(task_usage_lines(
        "gpt-5.6-luna",
        &usage,
        Some(1.896359333333333),
        Some(49.645),
        Some(50.0),
        TaskDiffStats {
            files_changed: 2,
            files_created: 0,
            files_deleted: 0,
            lines_added: 115,
            lines_removed: 0,
        },
        TaskTiming {
            duration_ms: Some(740_800),
            runtime_metrics,
            ..TaskTiming::default()
        },
    ));
}

#[test]
fn task_usage_lines_show_task_finished_timestamp() {
    let finished_at = 1_750_000_000;
    let lines = task_usage_lines(
        "gpt-5.6-luna",
        &TokenUsage::default(),
        None,
        None,
        None,
        TaskDiffStats::default(),
        TaskTiming {
            finished_at: Some(finished_at),
            ..TaskTiming::default()
        },
    );

    let rendered = lines
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(rendered.contains(&format!(" · finished {}", format_finished_at(finished_at))));
}

#[test]
fn task_usage_lines_omit_unavailable_telemetry_without_na_values() {
    let lines = task_usage_lines(
        "gpt-5.6-luna",
        &TokenUsage::default(),
        Some(0.0),
        None,
        Some(77.0),
        TaskDiffStats::default(),
        TaskTiming {
            duration_ms: Some(99_000),
            first_output_ms: Some(8_300),
            local_tool_duration_ms: 14_700,
            ..TaskTiming::default()
        },
    );

    let rendered = lines
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(!rendered.contains("n/a"));
    assert!(rendered.contains("files created 0 · deleted 0 · modified 0 · lines +0 / -0"));
    assert!(rendered.contains("time wall 1m 39.0s (local tools 14.7s) · first output 8.3s"));
}

#[test]
fn task_usage_durations_show_minutes_and_hours_when_needed() {
    assert_eq!(format_duration_ms(59_949), "59.9s");
    assert_eq!(format_duration_ms(59_999), "1m 0.0s");
    assert_eq!(format_duration_ms(60_000), "1m 0.0s");
    assert_eq!(format_duration_ms(3_661_250), "1h 1m 1.3s");
}

#[test]
fn task_usage_summary_history_cell_has_one_divider_on_each_side() {
    let cell = TaskUsageSummaryHistoryCell::new(vec![Line::from("summary")]);
    let rendered = cell
        .display_lines(20)
        .into_iter()
        .map(|line| line.to_string())
        .collect::<Vec<_>>();

    assert_eq!(
        rendered,
        vec!["────────────────────", "summary", "────────────────────"]
    );
}

#[test]
fn token_percentage_breakdown_sums_to_displayed_total() {
    let usage = TokenUsage {
        input_tokens: 12_726_447,
        cached_input_tokens: 12_393_984,
        output_tokens: 22_477,
        reasoning_output_tokens: 10_418,
        total_tokens: 12_748_924,
    };

    let percentages = token_usage_percentage_units("gpt-5.6-luna", &usage, Some(1.896359333333333))
        .expect("known model should have a token percentage breakdown");

    assert_eq!(percentages.iter().sum::<i64>(), 18_964);
}

#[test]
fn token_usage_delta_clamps_each_counter_at_zero() {
    let start = TokenUsage {
        input_tokens: 100,
        cached_input_tokens: 50,
        output_tokens: 30,
        reasoning_output_tokens: 20,
        total_tokens: 130,
    };
    let end = TokenUsage {
        input_tokens: 90,
        cached_input_tokens: 75,
        output_tokens: 45,
        reasoning_output_tokens: 25,
        total_tokens: 135,
    };

    assert_eq!(
        token_usage_delta(&start, &end),
        TokenUsage {
            input_tokens: 0,
            cached_input_tokens: 25,
            output_tokens: 15,
            reasoning_output_tokens: 5,
            total_tokens: 5,
        }
    );
}

#[test]
fn local_tool_time_uses_the_union_of_overlapping_intervals() {
    assert_eq!(union_duration_ms(&[(0, 100), (50, 150), (200, 250)]), 200);
}

#[test]
fn weekly_limit_used_percent_uses_model_credit_rates() {
    let usage = TokenUsage {
        input_tokens: 337_141,
        cached_input_tokens: 329_216,
        output_tokens: 578,
        reasoning_output_tokens: 162,
        total_tokens: 337_719,
    };

    assert_eq!(
        weekly_limit_used_percent("gpt-5.6-luna", &usage),
        Some(0.008_206_407_407_407_407)
    );
    assert_eq!(
        weekly_limit_used_percent("gpt-5.6-sol", &usage),
        Some(0.161_987_407_407_407_4)
    );
    assert_eq!(
        weekly_limit_used_percent("gpt-6-astra", &usage),
        Some(0.404_968_518_518_518_5)
    );
    assert_eq!(
        weekly_limit_used_percent("gpt-6-sol", &usage),
        Some(0.080_993_703_703_703_7)
    );
    assert_eq!(
        weekly_limit_used_percent("gpt-6.1-sol", &usage),
        Some(0.050_510_740_740_740_74)
    );
    assert_eq!(
        weekly_limit_used_percent("gpt-6-luna", &usage),
        Some(0.004_049_685_185_185_185)
    );
}

#[test]
fn model_credit_rates_cover_supported_models() {
    assert_eq!(
        model_credit_rates("gpt-6-astra"),
        Some(ModelCreditRates {
            uncached_input: 250.0,
            cached_input: 25.0,
            output: 1_250.0,
        })
    );
    assert_eq!(
        model_credit_rates("gpt-6.1-sol"),
        Some(ModelCreditRates {
            uncached_input: 50.0,
            cached_input: 2.5,
            output: 250.0,
        })
    );
    assert_eq!(
        model_credit_rates("gpt-6-sol"),
        Some(ModelCreditRates {
            uncached_input: 50.0,
            cached_input: 5.0,
            output: 250.0,
        })
    );
    assert_eq!(
        model_credit_rates("gpt-6-luna"),
        Some(ModelCreditRates {
            uncached_input: 2.5,
            cached_input: 0.25,
            output: 12.5,
        })
    );
    assert_eq!(
        model_credit_rates("gpt-5.6-sol"),
        Some(ModelCreditRates {
            uncached_input: 100.0,
            cached_input: 10.0,
            output: 500.0,
        })
    );
    assert_eq!(
        model_credit_rates("gpt-5.6-luna"),
        Some(ModelCreditRates {
            uncached_input: 5.0,
            cached_input: 0.5,
            output: 30.0,
        })
    );
    assert_eq!(
        model_credit_rates("gpt-5.6-terra"),
        model_credit_rates("gpt-5.4")
    );
    assert_eq!(
        model_credit_rates("gpt-5.4-mini"),
        Some(ModelCreditRates {
            uncached_input: 18.75,
            cached_input: 1.875,
            output: 113.0,
        })
    );
    assert_eq!(model_credit_rates("unknown-model"), None);
}

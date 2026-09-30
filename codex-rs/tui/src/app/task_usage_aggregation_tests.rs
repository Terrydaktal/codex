use super::*;
use crate::app::test_support::make_test_app;
use crate::app::thread_events::ThreadEventChannel;
use crate::multi_agents::SubAgentActivityDisplay;
use crate::token_usage::TokenUsage;
use crate::token_usage::TokenUsageInfo;

fn contribution(thread_id: ThreadId, turn_id: &str, total_tokens: i64) -> TaskUsageContribution {
    TaskUsageContribution::new(
        &thread_id.to_string(),
        turn_id,
        "gpt-5.6-sol",
        &TokenUsageInfo {
            total_token_usage: TokenUsage {
                input_tokens: total_tokens,
                total_tokens,
                ..Default::default()
            },
            last_token_usage: TokenUsage {
                input_tokens: total_tokens,
                total_tokens,
                ..Default::default()
            },
            model_context_window: None,
        },
    )
}

#[test]
fn aggregate_deduplicates_response_snapshots_by_stable_event_id() {
    let root_thread_id = ThreadId::new();
    let child_thread_id = ThreadId::new();
    let mut state = TaskUsageAggregationState::default();
    let first = contribution(child_thread_id, "turn-child", 100);
    let second = contribution(child_thread_id, "turn-child", 200);

    state.record(root_thread_id, "turn-root".to_string(), first.clone());
    state.record(root_thread_id, "turn-root".to_string(), first);
    state.record(root_thread_id, "turn-root".to_string(), second);

    assert_eq!(state.take(root_thread_id, "turn-root").len(), 2);
    assert!(state.take(root_thread_id, "turn-root").is_empty());
}

#[tokio::test]
async fn descendant_usage_is_attributed_to_the_active_primary_turn() {
    let mut app = make_test_app().await;
    let root_thread_id = ThreadId::new();
    let child_thread_id = ThreadId::new();
    let root_turn_id = "turn-root";
    let root_channel = ThreadEventChannel::new(/*capacity*/ 8);
    root_channel
        .store
        .lock()
        .await
        .push_notification(ServerNotification::TurnStarted(
            codex_app_server_protocol::TurnStartedNotification {
                thread_id: root_thread_id.to_string(),
                turn: codex_app_server_protocol::Turn {
                    id: root_turn_id.to_string(),
                    items_view: codex_app_server_protocol::TurnItemsView::Full,
                    items: Vec::new(),
                    status: codex_app_server_protocol::TurnStatus::InProgress,
                    error: None,
                    started_at: None,
                    completed_at: None,
                    duration_ms: None,
                },
            },
        ));
    app.primary_thread_id = Some(root_thread_id);
    app.thread_event_channels
        .insert(root_thread_id, root_channel);
    app.agent_navigation
        .record_sub_agent_activity(SubAgentActivityDisplay {
            thread_id: child_thread_id,
            agent_path: "/root/child".to_string(),
            is_running_hint: true,
        });

    let info = TokenUsageInfo {
        total_token_usage: TokenUsage {
            input_tokens: 100,
            total_tokens: 100,
            ..Default::default()
        },
        last_token_usage: TokenUsage {
            input_tokens: 100,
            total_tokens: 100,
            ..Default::default()
        },
        model_context_window: None,
    };
    let notification = RawResponseCompletedNotification {
        thread_id: child_thread_id.to_string(),
        turn_id: "turn-child".to_string(),
        response_id: "response-child".to_string(),
        usage: Some(codex_app_server_protocol::TokenUsageBreakdown {
            total_tokens: info.last_token_usage.total_tokens,
            input_tokens: info.last_token_usage.input_tokens,
            cached_input_tokens: 0,
            cache_write_input_tokens: 0,
            output_tokens: 0,
            reasoning_output_tokens: 0,
        }),
        usage_metadata: None,
    };

    app.record_task_usage_notification(child_thread_id, &notification)
        .await;

    assert_eq!(
        app.task_usage_aggregation
            .take(root_thread_id, root_turn_id)
            .len(),
        1
    );
}

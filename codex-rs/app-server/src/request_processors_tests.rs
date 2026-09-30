use super::*;
use codex_app_server_protocol::ThreadItem;
use codex_app_server_protocol::UserInput as ApiUserInput;
use codex_protocol::ThreadId;
use codex_protocol::items::AgentMessageContent;
use codex_protocol::items::AgentMessageItem;
use codex_protocol::items::UserMessageItem;
use codex_protocol::protocol::ItemCompletedEvent;
use codex_protocol::protocol::TurnCompleteEvent;
use codex_protocol::protocol::TurnStartedEvent;
use codex_protocol::user_input::UserInput;
use pretty_assertions::assert_eq;

#[test]
fn legacy_turn_replay_restores_materialized_paginated_messages() {
    let thread_id = ThreadId::new();
    let turn_id = "turn-1";
    let user_item = TurnItem::UserMessage(UserMessageItem {
        id: "user-1".to_string(),
        client_id: None,
        content: vec![UserInput::Text {
            text: "restore this prompt".to_string(),
            text_elements: Vec::new(),
        }],
    });
    let agent_item = TurnItem::AgentMessage(AgentMessageItem {
        id: "agent-1".to_string(),
        content: vec![AgentMessageContent::Text {
            text: "restored answer".to_string(),
        }],
        phase: None,
        memory_citation: None,
        delivery: None,
        questions: None,
    });
    let items = vec![
        RolloutItem::EventMsg(EventMsg::TurnStarted(TurnStartedEvent {
            turn_id: turn_id.to_string(),
            root_turn_id: None,
            trace_id: None,
            started_at: None,
            model_context_window: None,
            collaboration_mode_kind: Default::default(),
        })),
        RolloutItem::EventMsg(EventMsg::ItemCompleted(ItemCompletedEvent {
            thread_id,
            turn_id: turn_id.to_string(),
            item: user_item,
            started_at_ms: None,
            completed_at_ms: 1,
        })),
        RolloutItem::EventMsg(EventMsg::ItemCompleted(ItemCompletedEvent {
            thread_id,
            turn_id: turn_id.to_string(),
            item: agent_item,
            started_at_ms: None,
            completed_at_ms: 2,
        })),
        RolloutItem::EventMsg(EventMsg::TurnComplete(TurnCompleteEvent {
            turn_id: turn_id.to_string(),
            last_agent_message: Some("restored answer".to_string()),
            error: None,
            started_at: None,
            completed_at: None,
            duration_ms: None,
            time_to_first_token_ms: None,
        })),
    ];

    let turns = build_legacy_api_turns_from_rollout_items(&items);

    assert_eq!(
        turns
            .into_iter()
            .flat_map(|turn| turn.items)
            .collect::<Vec<_>>(),
        vec![
            ThreadItem::UserMessage {
                id: "item-1".to_string(),
                client_id: None,
                content: vec![ApiUserInput::Text {
                    text: "restore this prompt".to_string(),
                    text_elements: Vec::new(),
                }],
            },
            ThreadItem::AgentMessage {
                id: "item-2".to_string(),
                text: "restored answer".to_string(),
                phase: None,
                memory_citation: None,
                delivery: None,
                questions: None,
            },
        ]
    );
}

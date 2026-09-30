//! Aggregates response usage from sub-agent threads into the root turn that owns them.

use super::*;
use crate::chatwidget::TaskUsageContribution;
use crate::chatwidget::token_usage_from_app_server;
use codex_app_server_protocol::RawResponseCompletedNotification;
use std::collections::BTreeMap;
use std::collections::HashSet;

#[derive(Debug, Default)]
pub(super) struct TaskUsageAggregationState {
    contributions_by_root_turn:
        HashMap<(ThreadId, String), BTreeMap<String, TaskUsageContribution>>,
    descendant_threads: HashSet<ThreadId>,
}

impl TaskUsageAggregationState {
    pub(super) fn mark_descendant(&mut self, thread_id: ThreadId) {
        self.descendant_threads.insert(thread_id);
    }

    pub(super) fn is_descendant(&self, thread_id: ThreadId) -> bool {
        self.descendant_threads.contains(&thread_id)
    }

    pub(super) fn clear(&mut self) {
        self.contributions_by_root_turn.clear();
        self.descendant_threads.clear();
    }

    fn record(
        &mut self,
        root_thread_id: ThreadId,
        root_turn_id: String,
        contribution: TaskUsageContribution,
    ) {
        self.contributions_by_root_turn
            .entry((root_thread_id, root_turn_id))
            .or_default()
            .insert(contribution.response_event_id().to_string(), contribution);
    }

    fn take(&mut self, root_thread_id: ThreadId, root_turn_id: &str) -> Vec<TaskUsageContribution> {
        self.contributions_by_root_turn
            .remove(&(root_thread_id, root_turn_id.to_string()))
            .map(|contributions| contributions.into_values().collect())
            .unwrap_or_default()
    }
}

impl App {
    pub(super) async fn record_task_usage_notification(
        &mut self,
        thread_id: ThreadId,
        notification: &RawResponseCompletedNotification,
    ) {
        let Some(usage) = notification.usage.clone() else {
            return;
        };
        let model = self.task_usage_model_for_thread(thread_id).await;
        let contribution = TaskUsageContribution::from_response(
            &notification.response_id,
            &model,
            token_usage_from_app_server(usage),
        );

        // Persist every response immediately, even if its thread is not currently visible. The
        // normal ChatWidget path records the same stable id and is therefore safely idempotent.
        self.chat_widget
            .record_task_response_contribution(&contribution);

        let Some((root_thread_id, root_turn_id)) =
            self.active_root_turn_for_descendant_usage(thread_id).await
        else {
            return;
        };
        self.task_usage_aggregation
            .record(root_thread_id, root_turn_id, contribution);
    }

    pub(super) fn apply_descendant_task_usage_for_completion(
        &mut self,
        notification: &ServerNotification,
    ) {
        let ServerNotification::TurnCompleted(notification) = notification else {
            return;
        };
        let Ok(root_thread_id) = ThreadId::from_string(&notification.thread_id) else {
            return;
        };
        let contributions = self
            .task_usage_aggregation
            .take(root_thread_id, &notification.turn.id);
        if contributions.is_empty() {
            return;
        }
        self.chat_widget
            .record_descendant_task_usage(&notification.turn.id, contributions);
    }

    async fn active_root_turn_for_descendant_usage(
        &self,
        thread_id: ThreadId,
    ) -> Option<(ThreadId, String)> {
        let root_thread_id = self.primary_thread_id?;
        if thread_id == root_thread_id || !self.is_descendant_agent_thread(thread_id) {
            return None;
        }
        let channel = self.thread_event_channels.get(&root_thread_id)?;
        let store = channel.store.lock().await;
        Some((
            root_thread_id,
            store.active_turn_id().map(ToOwned::to_owned)?,
        ))
    }

    fn is_descendant_agent_thread(&self, thread_id: ThreadId) -> bool {
        self.task_usage_aggregation.is_descendant(thread_id)
            || self.agent_navigation.is_parent_owned(thread_id)
            || self
                .agent_navigation
                .get(&thread_id)
                .and_then(|entry| entry.agent_path.as_deref())
                .is_some_and(|path| path.starts_with("/root/") && path.len() > "/root/".len())
    }

    async fn task_usage_model_for_thread(&self, thread_id: ThreadId) -> String {
        if self.active_thread_id == Some(thread_id) {
            return self.chat_widget.current_model().to_string();
        }
        if let Some(channel) = self.thread_event_channels.get(&thread_id) {
            let store = channel.store.lock().await;
            if let Some(model) = store
                .session
                .as_ref()
                .map(|session| session.model.trim())
                .filter(|model| !model.is_empty())
            {
                return model.to_string();
            }
        }
        if let Some(model) = self
            .primary_session_configured
            .as_ref()
            .filter(|session| session.thread_id == thread_id)
            .map(|session| session.model.trim())
            .filter(|model| !model.is_empty())
        {
            return model.to_string();
        }
        self.chat_widget.current_model().to_string()
    }
}

#[cfg(test)]
#[path = "task_usage_aggregation_tests.rs"]
mod tests;

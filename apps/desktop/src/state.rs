use approval_protocol::*;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Page {
    Inbox,
    History,
    Health,
}
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Detail {
    Overview,
    Evidence,
    Policy,
    Investigation,
}
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Pane {
    Queues,
    Requests,
    Details,
}
#[derive(Clone)]
pub struct Confirmation {
    pub review: Review,
    pub action: ReviewAction,
}
#[derive(Default)]
pub struct Snapshot {
    pub queues: Vec<QueueInfo>,
    pub pending: Vec<Review>,
    pub history: Vec<Review>,
    pub health: Option<Health>,
    pub sources: Vec<SourceInfo>,
    pub selected: Option<Review>,
    pub preview: Option<PolicyPreview>,
    pub conversation: Option<Conversation>,
    pub detail_error: String,
}
pub struct State {
    pub snapshot: Snapshot,
    pub page: Page,
    pub detail: Detail,
    pub pane: Pane,
    pub queue: Option<String>,
    pub selected: Option<String>,
    pub connected: bool,
    pub busy: bool,
    pub refreshing: bool,
    pub notice: String,
    pub confirmation: Option<Confirmation>,
    pub permission_duration: String,
    pub permission_any_destination: bool,
}
impl Default for State {
    fn default() -> Self {
        Self {
            snapshot: Snapshot::default(),
            page: Page::Inbox,
            detail: Detail::Overview,
            pane: Pane::Requests,
            queue: None,
            selected: None,
            connected: false,
            busy: false,
            refreshing: false,
            notice: "Connecting to the local approval service…".into(),
            confirmation: None,
            permission_duration: "once".into(),
            permission_any_destination: false,
        }
    }
}
impl State {
    pub fn rows(&self, filter: &str) -> Vec<&Review> {
        let reviews = if self.page == Page::History {
            &self.snapshot.history
        } else {
            &self.snapshot.pending
        };
        let filter = filter.to_lowercase();
        reviews
            .iter()
            .filter(|review| {
                self.queue
                    .as_ref()
                    .is_none_or(|queue| queue == &review.request.queue)
                    && (filter.is_empty()
                        || format!(
                            "{} {} {} {}",
                            review.request.summary,
                            review.request.queue,
                            review.request.subject,
                            review.id
                        )
                        .to_lowercase()
                        .contains(&filter))
            })
            .collect()
    }
    pub fn application(review: &Review) -> &str {
        review.request.subject["process"]
            .as_str()
            .filter(|path| !path.is_empty())
            .unwrap_or(&review.producer)
    }
    pub fn review(&self) -> Option<&Review> {
        let id = self.selected.as_deref()?;
        self.snapshot
            .selected
            .as_ref()
            .filter(|review| review.id == id)
            .or_else(|| {
                self.snapshot
                    .pending
                    .iter()
                    .chain(&self.snapshot.history)
                    .find(|review| review.id == id)
            })
    }
    pub fn apply(&mut self, snapshot: Snapshot, target: Option<&str>) {
        let selected = self.snapshot.selected.take();
        let preview = self.snapshot.preview.take();
        let conversation = self.snapshot.conversation.take();
        self.snapshot = snapshot;
        if self.selected.as_deref() != target {
            self.snapshot.selected =
                selected.filter(|review| Some(review.id.as_str()) == self.selected.as_deref());
            self.snapshot.preview =
                preview.filter(|value| Some(value.request_id.as_str()) == self.selected.as_deref());
            self.snapshot.conversation = conversation
                .filter(|value| Some(value.request_id.as_str()) == self.selected.as_deref());
        }
        self.connected = true;
        if self.selected.is_none() {
            self.selected = self.rows("").first().map(|review| review.id.clone());
        }
        if self.notice.starts_with("Connecting") || self.notice.starts_with("Disconnected") {
            self.notice =
                "Live updates · decisions are recorded in the shared approval service".into();
        }
    }
    pub fn can_confirm(&self, confirmation: &Confirmation, now: i64) -> bool {
        self.connected
            && !self.busy
            && self.review().is_some_and(|review| {
                review.id == confirmation.review.id
                    && review.revision == confirmation.review.revision
                    && review.subject_digest == confirmation.review.subject_digest
                    && review.evidence_digest == confirmation.review.evidence_digest
                    && review.state == ReviewState::Pending
                    && review.expires_at_ms > now
                    && review
                        .actions
                        .iter()
                        .any(|action| action == &confirmation.action)
            })
    }
    pub fn select(&mut self, id: String) {
        if self.selected.as_ref() != Some(&id) {
            self.selected = Some(id);
            self.confirmation = None;
            self.permission_duration = "once".into();
            self.permission_any_destination = false;
            self.snapshot.selected = None;
            self.snapshot.preview = None;
            self.snapshot.conversation = None;
            self.snapshot.detail_error.clear();
        }
        self.pane = Pane::Details;
    }
}
pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}
pub fn text(value: &str) -> String {
    display_text(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn confirmation_is_bound_to_selection_revision_scope_expiry_and_connection() {
        let review = crate::backend::demo_review();
        let confirmation = Confirmation {
            action: review.actions[0].clone(),
            review: review.clone(),
        };
        let mut state = State {
            connected: true,
            selected: Some(review.id.clone()),
            snapshot: Snapshot {
                pending: vec![review.clone()],
                ..Default::default()
            },
            ..Default::default()
        };
        assert!(state.can_confirm(&confirmation, review.expires_at_ms - 1));
        assert!(!state.can_confirm(&confirmation, review.expires_at_ms));
        state.connected = false;
        assert!(!state.can_confirm(&confirmation, 0));
        state.connected = true;
        state.busy = true;
        assert!(!state.can_confirm(&confirmation, 0));
        state.busy = false;
        state.snapshot.pending[0].revision += 1;
        assert!(!state.can_confirm(&confirmation, 0));
        state.snapshot.pending[0] = review.clone();
        state.snapshot.pending[0].actions[0].scope = serde_json::json!({"duration":"always"});
        assert!(!state.can_confirm(&confirmation, 0));
        state.select("another-request".into());
        assert!(!state.can_confirm(&confirmation, 0));
    }
    #[test]
    fn background_refresh_cannot_replace_new_selection_with_old_details() {
        let review = crate::backend::demo_review();
        let mut state = State {
            selected: Some("new-selection".into()),
            ..Default::default()
        };
        state.apply(
            Snapshot {
                selected: Some(review.clone()),
                pending: vec![review.clone()],
                ..Default::default()
            },
            Some(&review.id),
        );
        assert!(state.review().is_none());
        assert_eq!(state.selected.as_deref(), Some("new-selection"));
        assert!(state.rows("EXAMPLE.INVALID").len() == 1);
        state.queue = Some("other.queue".into());
        assert!(state.rows("").is_empty());
    }
}

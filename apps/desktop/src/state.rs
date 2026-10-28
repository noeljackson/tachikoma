use approval_protocol::*;
use std::{
    collections::BTreeSet,
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Page {
    Decisions,
    Settings,
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
    pub show_pending: bool,
    pub show_history: bool,
    pub detail: Detail,
    pub pane: Pane,
    pub queue_filter: Option<BTreeSet<String>>,
    pub selected: Option<String>,
    pub connected: bool,
    pub busy: bool,
    pub refreshing: bool,
    pub notice: String,
    pub confirmation: Option<Confirmation>,
}
impl Default for State {
    fn default() -> Self {
        Self {
            snapshot: Snapshot::default(),
            page: Page::Decisions,
            show_pending: true,
            show_history: true,
            detail: Detail::Overview,
            pane: Pane::Requests,
            queue_filter: None,
            selected: None,
            connected: false,
            busy: false,
            refreshing: false,
            notice: "Connecting to the local approval service…".into(),
            confirmation: None,
        }
    }
}
impl State {
    pub fn rows(&self, filter: &str) -> Vec<&Review> {
        // Resolve lifecycle races between the two API reads before applying filters.
        let mut latest = std::collections::BTreeMap::<&str, &Review>::new();
        for review in self.snapshot.pending.iter().chain(&self.snapshot.history) {
            if latest
                .get(review.id.as_str())
                .is_none_or(|old| review.revision >= old.revision)
            {
                latest.insert(&review.id, review);
            }
        }
        let filter = filter.to_lowercase();
        let mut rows: Vec<_> = latest
            .into_values()
            .filter(|review| {
                (if review.state == ReviewState::Pending {
                    self.show_pending
                } else {
                    self.show_history
                }) && self.includes_queue(&review.request.queue)
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
            .collect();
        rows.sort_by(|a, b| {
            b.created_at_ms
                .cmp(&a.created_at_ms)
                .then_with(|| a.id.cmp(&b.id))
        });
        rows
    }
    pub fn includes_queue(&self, id: &str) -> bool {
        self.queue_filter
            .as_ref()
            .is_none_or(|queues| queues.contains(id))
    }
    pub fn toggle_queue(&mut self, id: &str) {
        let queues = self.queue_filter.get_or_insert_with(|| {
            self.snapshot
                .queues
                .iter()
                .map(|queue| queue.manifest.id.clone())
                .collect()
        });
        if !queues.remove(id) {
            queues.insert(id.to_owned());
        }
    }
    pub fn visible_count(&self, pending: u64, history: u64) -> u64 {
        (if self.show_pending { pending } else { 0 }).saturating_add(if self.show_history {
            history
        } else {
            0
        })
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
            .iter()
            .chain(&self.snapshot.pending)
            .chain(&self.snapshot.history)
            .filter(|review| review.id == id)
            .max_by_key(|review| (review.revision, review.state != ReviewState::Pending))
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
                    && review.manifest_revision == confirmation.review.manifest_revision
                    && review.request.nonce == confirmation.review.request.nonce
                    && review.expires_at_ms == confirmation.review.expires_at_ms
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
    fn queue_multiselect_composes_with_status_and_distinguishes_all_from_none() {
        let network = crate::backend::demo_review();
        let mut operations = network.clone();
        operations.id = "operations".into();
        operations.request.queue = "operations".into();
        operations.state = ReviewState::Allowed;
        let manifest: QueueManifest = serde_json::from_str(include_str!(
            "../../../contrib/adapters/snitchgate.queue.json"
        ))
        .unwrap();
        let mut other = manifest.clone();
        other.id = "operations".into();
        let mut state = State {
            snapshot: Snapshot {
                queues: vec![
                    QueueInfo {
                        manifest,
                        revision: 1,
                        pending: 1,
                        settled: 0,
                    },
                    QueueInfo {
                        manifest: other,
                        revision: 1,
                        pending: 0,
                        settled: 1,
                    },
                ],
                pending: vec![network.clone()],
                history: vec![operations],
                ..Default::default()
            },
            ..Default::default()
        };
        assert_eq!(state.rows("").len(), 2);
        state.toggle_queue(&network.request.queue);
        assert_eq!(state.rows("").len(), 1);
        assert_eq!(state.rows("")[0].request.queue, "operations");
        assert!(!state.includes_queue("new-queue"));
        state.toggle_queue("operations");
        assert!(state.rows("").is_empty());
        state.toggle_queue(&network.request.queue);
        assert_eq!(state.rows("EXAMPLE.INVALID").len(), 1);
        state.show_pending = false;
        assert!(state.rows("").is_empty());
        state.show_pending = true;
        state.queue_filter = None;
        assert!(state.includes_queue("new-queue"));
        assert_eq!(state.rows("").len(), 2);
    }
    #[test]
    fn unified_feed_filters_search_and_queue_and_coalesces_lifecycle_updates() {
        let pending = crate::backend::demo_review();
        let mut settled = pending.clone();
        settled.revision += 1;
        settled.state = ReviewState::Allowed;
        let mut older = pending.clone();
        older.id = "older-history".into();
        older.state = ReviewState::Denied;
        older.created_at_ms -= 100;
        let mut state = State {
            snapshot: Snapshot {
                pending: vec![pending.clone()],
                history: vec![older],
                ..Default::default()
            },
            ..Default::default()
        };
        assert_eq!(state.rows("").len(), 2);
        assert_eq!(state.rows("")[0].id, pending.id);
        state.show_history = false;
        assert_eq!(state.rows("").len(), 1);
        state.show_pending = false;
        assert!(state.rows("").is_empty());
        state.show_history = true;
        assert_eq!(state.rows("")[0].id, "older-history");
        state.show_pending = true;
        state.snapshot.history.push(settled);
        state.selected = Some(pending.id.clone());
        state.connected = true;
        assert_eq!(state.review().unwrap().state, ReviewState::Allowed);
        assert!(!state.can_confirm(
            &Confirmation {
                review: pending.clone(),
                action: pending.actions[0].clone()
            },
            0
        ));
        // The history read observed settlement after the pending read. Never
        // display or approve an outdated pending copy, even with history hidden.
        assert_eq!(state.rows("").len(), 2);
        assert_eq!(state.rows("")[0].state, ReviewState::Allowed);
        state.show_history = false;
        assert!(state.rows("").is_empty());
        state.show_history = true;
        assert_eq!(state.rows("EXAMPLE.INVALID").len(), 2);
        assert!(state.rows("no-match").is_empty());
        state.queue_filter = Some(["different.queue".into()].into());
        assert!(state.rows("").is_empty());
    }
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
        state.snapshot.pending[0].manifest_revision += 1;
        assert!(!state.can_confirm(&confirmation, 0));
        state.snapshot.pending[0] = review.clone();
        state.snapshot.pending[0].request.nonce = "different-request".into();
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
        state.queue_filter = Some(["other.queue".into()].into());
        assert!(state.rows("").is_empty());
    }
}

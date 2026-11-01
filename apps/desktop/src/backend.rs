use crate::feed::{self, Shared};
use crate::state::{Detail, Page, Snapshot, now_ms};
use anyhow::{Context, Result, ensure};
use approval_client::Client;
use approval_protocol::*;
use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{Arc, Mutex, atomic::Ordering},
    time::{Duration, Instant},
};
#[derive(Default)]
struct Details {
    epoch: i64,
    at: Option<Instant>,
    preview: Option<PolicyPreview>,
    conversation: Option<Conversation>,
    review: Option<Review>,
}

#[derive(Clone)]
pub struct Backend {
    pub socket: PathBuf,
    pub credential: PathBuf,
    pub demo: bool,
    pub(crate) shared: Arc<Shared>,
    details: Arc<Mutex<BTreeMap<(String, u8), Details>>>,
}
impl Backend {
    pub fn new(socket: PathBuf, credential: PathBuf, demo: bool) -> Self {
        Self {
            socket,
            credential,
            demo,
            shared: Arc::new(Shared::default()),
            details: Arc::new(Mutex::new(BTreeMap::new())),
        }
    }
    pub fn start(&self) {
        if !self.demo && !self.shared.started.swap(true, Ordering::AcqRel) {
            tokio::spawn(feed::run(self.clone()));
        }
    }
    pub fn query(&self, search: String, queues: Option<Vec<String>>) {
        let mut bytes = 0;
        let search = search
            .chars()
            .take_while(|ch| {
                bytes += ch.len_utf8();
                bytes <= 256
            })
            .collect();
        let next = feed::Query { search, queues };
        self.shared.query.send_if_modified(|old| {
            if *old == next {
                false
            } else {
                *old = next;
                true
            }
        });
    }
    pub fn more(&self) {
        self.shared.more();
    }
    pub async fn live(&self) -> Result<Arc<feed::Data>> {
        self.start();
        let mut changes = self.shared.updates.subscribe();
        tokio::time::timeout(Duration::from_secs(6), async {
            loop {
                let data = changes.borrow_and_update().clone();
                if data.connected {
                    return Ok(data);
                }
                if !data.queues.is_empty() {
                    anyhow::bail!("approval feed disconnected");
                }
                changes.changed().await?;
            }
        })
        .await
        .context("approval feed unavailable")?
    }

    pub(crate) fn client(&self) -> Result<Client> {
        let mut saved = self.shared.client.lock().expect("client cache");
        if let Some(client) = saved.as_ref() {
            return Ok(client.clone());
        }
        let client = Client::from_credential_file(
            "http://tachikoma.local",
            Some(&self.socket),
            &self.credential,
        )?;
        *saved = Some(client.clone());
        Ok(client)
    }
    #[cfg(test)]
    pub async fn snapshot(&self, selected: Option<&str>) -> Result<Snapshot> {
        self.snapshot_context(selected, Page::Decisions, Detail::Overview)
            .await
    }
    pub async fn snapshot_context(
        &self,
        selected: Option<&str>,
        page: Page,
        detail: Detail,
    ) -> Result<Snapshot> {
        if self.demo {
            return Ok(demo_snapshot());
        }
        let data = self.live().await?;
        let mut snapshot = Snapshot {
            queues: data.queues.clone(),
            pending: data.pending.clone(),
            history: data.history.clone(),
            health: data.health.clone(),
            sources: data.sources.clone(),
            history_more: data.next_before.is_some() && data.history.len() < feed::HISTORY_LIMIT,
            ..Default::default()
        };
        if let Some(id) = selected {
            let epoch = data.epochs.get(id).copied().unwrap_or(0);
            let kind = if page == Page::Settings {
                0
            } else {
                match detail {
                    Detail::Policy => 1,
                    Detail::Investigation => 2,
                    _ => 0,
                }
            };
            let key = (id.to_owned(), kind);
            let cached = self
                .details
                .lock()
                .expect("detail cache")
                .get(&key)
                .filter(|d| {
                    d.epoch == epoch && d.at.is_some_and(|t| t.elapsed() < Duration::from_secs(5))
                })
                .map(|d| (d.review.clone(), d.preview.clone(), d.conversation.clone()));
            let record = data
                .pending
                .iter()
                .chain(&data.history)
                .find(|r| r.id == id)
                .cloned();
            let (review, preview, conversation) = if let Some(cached) = cached {
                cached
            } else {
                let client = self.client()?;
                let review = if record.is_some() {
                    record.clone()
                } else {
                    tokio::time::timeout(Duration::from_secs(2), client.get(id))
                        .await
                        .ok()
                        .and_then(Result::ok)
                };
                let preview = if kind == 1 {
                    tokio::time::timeout(Duration::from_secs(2), client.policy_preview(id))
                        .await
                        .ok()
                        .and_then(Result::ok)
                } else {
                    None
                };
                let conversation = if kind == 2 {
                    tokio::time::timeout(Duration::from_secs(2), client.conversation(id))
                        .await
                        .ok()
                        .and_then(Result::ok)
                } else {
                    None
                };
                let mut cache = self.details.lock().expect("detail cache");
                cache.insert(
                    key,
                    Details {
                        epoch,
                        at: Some(Instant::now()),
                        review: review.clone(),
                        preview: preview.clone(),
                        conversation: conversation.clone(),
                    },
                );
                while cache.len() > 64 {
                    cache.pop_first();
                }
                (review, preview, conversation)
            };
            snapshot.selected = record.or(review);
            snapshot.preview = preview;
            snapshot.conversation = conversation;
            if snapshot.selected.is_none() {
                snapshot.detail_error =
                    "Selected request unavailable. Refresh before deciding.".into();
            }
        }
        Ok(snapshot)
    }
    pub async fn decide(&self, review: &Review, action: &ReviewAction) -> Result<()> {
        ensure!(!self.demo, "demo is read-only");
        self.client()?
            .decide(
                &review.id,
                &DecideRequest {
                    revision: review.revision,
                    action_id: action.id.clone(),
                    scope: Some(action.scope.clone()),
                },
            )
            .await?;
        Ok(())
    }
    pub async fn message(&self, id: &str, message: &SendMessage) -> Result<Conversation> {
        ensure!(!self.demo, "demo is read-only");
        self.client()?.send_message(id, message).await
    }
    pub async fn cancel(&self, id: &str) -> Result<Conversation> {
        ensure!(!self.demo, "demo is read-only");
        self.client()?.cancel_conversation(id).await
    }
}
pub fn demo_review() -> Review {
    let mut review: Review = serde_json::from_value(serde_json::json!({
        "id":"demo-network-request", "producer":"workstation-network", "service_kind":"snitchgate", "revision":1, "manifest_revision":1,
        "request":{"queue":"network.egress", "producer_request_id":"demo", "nonce":"demo", "idempotency_key":"demo", "schema_id":"network.connect.v1",
            "subject":{"process":"/usr/bin/node", "host":"registry.example.invalid", "ip":"192.0.2.10", "port":443,"pid":1432,"uid":1000,"protocol":"tcp"},
            "evidence":{"process_context":{"working_directory":"/home/noel/src/example","ancestors":[{"path":"/usr/bin/npm"}],"lifecycle_event":"install"}, "url_hints":["https://registry.example.invalid/example-package"],"coverage":"Illustrative metadata; encrypted HTTP contents unavailable"},
            "summary":"node → registry.example.invalid:443", "action_ids":["allow-once","deny-once","allow-five-minutes"], "expires_at_ms":now_ms()+3600000},
        "subject_digest":"demo-subject", "evidence_digest":"demo-evidence", "created_at_ms":now_ms(), "expires_at_ms":now_ms()+3600000,
        "mode":"manual", "state":"pending", "actions":[
            {"id":"allow-once", "label":"Allow once", "effect":"permit", "scope":{"duration":"once"}},
            {"id":"deny-once", "label":"Deny once", "effect":"block", "scope":{"duration":"once"}},
            {"id":"allow-five-minutes", "label":"Allow for 5 minutes", "effect":"permit", "scope":{"duration":"five-minutes"}}],
        "assessment":{"state":"complete","category":"needs_review","confidence":0.84,"rationale":"The destination resembles a package registry, but process identity and a familiar hostname cannot establish that the package is safe. Inspect lifecycle context and available sensor evidence.","jev_model":"Jev","llm_model":"qwen3.5:9b","evidence_digest":"demo-evidence","completed_at_ms":now_ms(),"judgments":{"activity":{"choice":"package_download","confidence":0.91}}},
        "decision":null,"receipt":null
    })).expect("checked built-in desktop fixture");
    let queue: QueueManifest = serde_json::from_str(include_str!(
        "../../../contrib/adapters/snitchgate.queue.json"
    ))
    .expect("checked example manifest");
    review.actions = queue.actions;
    review.request.action_ids = review
        .actions
        .iter()
        .map(|action| action.id.clone())
        .collect();
    review
}
fn demo_snapshot() -> Snapshot {
    static REVIEW: std::sync::OnceLock<Review> = std::sync::OnceLock::new();
    let review = REVIEW.get_or_init(demo_review).clone();
    let manifest: QueueManifest = serde_json::from_str(include_str!(
        "../../../contrib/adapters/snitchgate.queue.json"
    ))
    .expect("checked example queue");
    let mut other_manifest = manifest.clone();
    other_manifest.id = "demo.operations".into();
    other_manifest.title = "Operations".into();
    other_manifest.service_kind = "example".into();
    let mut other = review.clone();
    other.id = "demo-operations-request".into();
    other.request.queue = other_manifest.id.clone();
    other.request.schema_id = "example.operation.v1".into();
    other.request.subject = serde_json::json!({"service":"build","target":"staging"});
    other.request.summary = "Deploy build to staging".into();
    other.state = ReviewState::Allowed;
    other.created_at_ms -= 60_000;
    other.assessment = None;
    Snapshot {
        queues: vec![
            QueueInfo {
                manifest,
                revision: 1,
                pending: 1,
                settled: 0,
            },
            QueueInfo {
                manifest: other_manifest,
                revision: 1,
                pending: 0,
                settled: 1,
            },
        ],
        pending: vec![review.clone()],
        history: vec![other],
        selected: Some(review),
        ..Default::default()
    }
}

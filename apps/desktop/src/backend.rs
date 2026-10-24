use crate::state::{Snapshot, now_ms};
use anyhow::{Context, Result, ensure};
use approval_client::Client;
use approval_protocol::*;
use std::{path::PathBuf, time::Duration};

#[derive(Clone)]
pub struct Backend {
    pub socket: PathBuf,
    pub credential: PathBuf,
    pub demo: bool,
}
impl Backend {
    fn client(&self) -> Result<Client> {
        Client::from_credential_file(
            "http://tachikoma.local",
            Some(&self.socket),
            &self.credential,
        )
    }
    pub async fn snapshot(&self, selected: Option<&str>) -> Result<Snapshot> {
        if self.demo {
            return Ok(demo_snapshot());
        }
        let client = self.client()?;
        tokio::time::timeout(Duration::from_secs(6), async {
            let (queues, pending, history, health) = tokio::try_join!(
                client.queues(),
                client.list(),
                client.list_state("history"),
                client.health()
            )?;
            let mut snapshot = Snapshot {
                queues,
                pending,
                history,
                health: Some(health),
                ..Default::default()
            };
            match client.sources().await {
                Ok(sources) => snapshot.sources = sources,
                Err(_) => snapshot.detail_error = "Evidence source status unavailable.".into(),
            }
            if let Some(id) = selected {
                snapshot.selected = client.get(id).await.ok();
                let (preview, conversation) =
                    tokio::join!(client.policy_preview(id), client.conversation(id));
                snapshot.preview = preview.ok();
                snapshot.conversation = conversation.ok();
                if snapshot.selected.is_none() {
                    snapshot.detail_error =
                        "Selected request unavailable. Refresh before deciding.".into();
                }
            }
            Ok(snapshot)
        })
        .await
        .context("approval API timed out")?
    }
    pub async fn decide(&self, review: &Review, action: &ReviewAction) -> Result<()> {
        ensure!(!self.demo, "demo is read-only");
        self.client()?
            .decide(
                &review.id,
                &DecideRequest {
                    revision: review.revision,
                    action_id: action.id.clone(),
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
    let review = demo_review();
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

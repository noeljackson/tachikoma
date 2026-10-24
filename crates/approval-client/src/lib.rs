//! Optional public HTTP JSON client. It knows no domain implementations or databases.
use anyhow::{Context, Result, ensure};
use approval_protocol::*;
use reqwest::{Method, StatusCode};
use serde::{Serialize, de::DeserializeOwned};
use std::{path::Path, time::Duration};

#[derive(Clone)]
pub struct Client {
    http: reqwest::Client,
    endpoint: String,
    credential: String,
}

impl Client {
    pub fn new(endpoint: &str, socket: Option<&Path>, credential: String) -> Result<Self> {
        let url = reqwest::Url::parse(endpoint).context("invalid approval endpoint")?;
        ensure!(
            url.username().is_empty()
                && url.password().is_none()
                && url.query().is_none()
                && url.fragment().is_none(),
            "approval endpoint must not contain credentials, query or fragment"
        );
        ensure!(
            url.scheme() == "https" || (url.scheme() == "http" && socket.is_some()),
            "approval API requires HTTPS or an explicit private Unix socket"
        );
        ensure!(
            credential.len() == 64 && credential.bytes().all(|b| b.is_ascii_hexdigit()),
            "invalid approval credential"
        );
        let mut builder = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(3))
            .timeout(Duration::from_secs(35))
            .no_proxy();
        if let Some(socket) = socket {
            builder = builder.unix_socket(socket);
        }
        Ok(Self {
            http: builder.build()?,
            endpoint: endpoint.trim_end_matches('/').to_owned(),
            credential,
        })
    }
    pub fn from_credential_file(
        endpoint: &str,
        socket: Option<&Path>,
        path: &Path,
    ) -> Result<Self> {
        use std::os::unix::fs::MetadataExt;
        let metadata =
            std::fs::symlink_metadata(path).context("read approval credential metadata")?;
        let owner = std::fs::metadata("/proc/self")?.uid();
        let private = metadata.uid() == owner && metadata.mode() & 0o077 == 0;
        let systemd = std::env::var_os("CREDENTIALS_DIRECTORY")
            .map(std::path::PathBuf::from)
            .filter(|directory| path.parent() == Some(directory.as_path()))
            .and_then(|directory| std::fs::symlink_metadata(directory).ok())
            .is_some_and(|directory| {
                metadata.uid() == 0
                    && metadata.gid() == 0
                    && metadata.mode() & 0o037 == 0
                    && directory.is_dir()
                    && !directory.file_type().is_symlink()
                    && directory.uid() == 0
                    && directory.gid() == 0
                    && directory.mode() & 0o027 == 0
            });
        ensure!(
            metadata.is_file() && metadata.len() <= 1024 && (private || systemd),
            "approval credential must be a protected regular file"
        );
        Self::new(
            endpoint,
            socket,
            std::fs::read_to_string(path)
                .context("read approval credential")?
                .trim()
                .to_owned(),
        )
    }
    async fn call<T: DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        body: Option<&impl Serialize>,
    ) -> Result<T> {
        let mut call = self
            .http
            .request(method, format!("{}{API_PREFIX}{path}", self.endpoint))
            .bearer_auth(&self.credential);
        if let Some(body) = body {
            call = call.json(body);
        }
        let mut response = call.send().await.context("approval API unavailable")?;
        let status = response.status();
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.context("read approval response")? {
            ensure!(
                bytes.len() + chunk.len() <= 33_554_432,
                "approval response exceeds limit"
            );
            bytes.extend_from_slice(&chunk);
        }
        if !status.is_success() {
            // Error bodies are deliberately not included: they may contain untrusted service data.
            anyhow::bail!("approval API rejected operation (HTTP {})", status.as_u16());
        }
        serde_json::from_slice(&bytes).context("invalid approval API response")
    }
    pub async fn identity(&self) -> Result<PrincipalSpec> {
        self.call(Method::GET, "/identity", None::<&()>).await
    }
    pub async fn queues(&self) -> Result<Vec<QueueInfo>> {
        self.call(Method::GET, "/queues", None::<&()>).await
    }
    pub async fn sources(&self) -> Result<Vec<SourceInfo>> {
        self.call(Method::GET, "/evidence/sources", None::<&()>)
            .await
    }
    pub async fn put_source(&self, spec: &SourceSpec) -> Result<SourceInfo> {
        self.call(Method::POST, "/evidence/sources", Some(spec))
            .await
    }
    pub async fn source_heartbeat(
        &self,
        id: &str,
        heartbeat: &SourceHeartbeat,
    ) -> Result<SourceInfo> {
        Self::valid_id(id)?;
        self.call(
            Method::POST,
            &format!("/evidence/sources/{id}/heartbeat"),
            Some(heartbeat),
        )
        .await
    }
    pub async fn publish_observation(
        &self,
        id: &str,
        observation: &Observation,
    ) -> Result<ObservationReceipt> {
        Self::valid_id(id)?;
        self.call(
            Method::POST,
            &format!("/evidence/sources/{id}/observations"),
            Some(observation),
        )
        .await
    }
    pub async fn evidence_policy(&self, queue: &str) -> Result<Option<EvidencePolicyInfo>> {
        Self::valid_id(queue)?;
        self.call(
            Method::GET,
            &format!("/evidence/policies/{queue}"),
            None::<&()>,
        )
        .await
    }
    pub async fn put_evidence_policy(&self, policy: &EvidencePolicy) -> Result<EvidencePolicyInfo> {
        self.call(Method::POST, "/evidence/policies", Some(policy))
            .await
    }
    pub async fn policy_preview(&self, id: &str) -> Result<PolicyPreview> {
        Self::valid_id(id)?;
        self.call(
            Method::GET,
            &format!("/requests/{id}/evidence-preview"),
            None::<&()>,
        )
        .await
    }
    pub async fn put_queue(&self, queue: &QueueManifest) -> Result<QueueInfo> {
        self.call(Method::POST, "/queues", Some(queue)).await
    }
    pub async fn enroll(&self, spec: &PrincipalSpec) -> Result<EnrolledPrincipal> {
        self.call(Method::POST, "/principals", Some(spec)).await
    }
    pub async fn update_principal(&self, spec: &PrincipalSpec) -> Result<EnrolledPrincipal> {
        self.call(Method::POST, "/principals/update", Some(spec))
            .await
    }
    pub async fn revoke(&self, id: &str) -> Result<()> {
        Self::valid_id(id)?;
        self.call(Method::POST, &format!("/principals/{id}/revoke"), Some(&()))
            .await
    }
    pub async fn rotate(&self, id: &str) -> Result<EnrolledPrincipal> {
        Self::valid_id(id)?;
        self.call(Method::POST, &format!("/principals/{id}/rotate"), Some(&()))
            .await
    }
    pub async fn submit(&self, request: &SubmitRequest) -> Result<Review> {
        self.call(Method::POST, "/requests", Some(request)).await
    }
    pub async fn list(&self) -> Result<Vec<Review>> {
        self.list_state("pending").await
    }
    pub async fn list_state(&self, state: &str) -> Result<Vec<Review>> {
        ensure!(
            ["pending", "history", "all"].contains(&state),
            "invalid request state filter"
        );
        let mut before = i64::MAX;
        let mut reviews = Vec::new();
        for _ in 0..128 {
            let page: Reviews = self
                .call(
                    Method::GET,
                    &format!("/requests?before={before}&state={state}"),
                    None::<&()>,
                )
                .await?;
            reviews.extend(page.reviews);
            match page.next_before {
                Some(next) if next < before => before = next,
                _ => break,
            }
        }
        Ok(reviews)
    }
    pub async fn get(&self, id: &str) -> Result<Review> {
        Self::valid_id(id)?;
        self.call(Method::GET, &format!("/requests/{id}"), None::<&()>)
            .await
    }
    pub async fn decide(&self, id: &str, request: &DecideRequest) -> Result<Review> {
        Self::valid_id(id)?;
        self.call(
            Method::POST,
            &format!("/requests/{id}/decision"),
            Some(request),
        )
        .await
    }
    pub async fn cancel(&self, id: &str) -> Result<Review> {
        Self::valid_id(id)?;
        self.call(Method::POST, &format!("/requests/{id}/cancel"), Some(&()))
            .await
    }
    pub async fn receipt(&self, id: &str, receipt: &EnforcementReceipt) -> Result<Review> {
        Self::valid_id(id)?;
        self.call(
            Method::POST,
            &format!("/requests/{id}/receipt"),
            Some(receipt),
        )
        .await
    }
    pub async fn events(&self, after: i64, wait_ms: u32) -> Result<Events> {
        self.call(
            Method::GET,
            &format!("/events?after={after}&wait_ms={}", wait_ms.min(30_000)),
            None::<&()>,
        )
        .await
    }
    pub async fn claim(&self) -> Result<Option<Job>> {
        self.call(
            Method::POST,
            "/jobs/claim",
            Some(&ClaimJob {
                kind: "assessment_or_investigation".into(),
            }),
        )
        .await
    }
    pub async fn complete(&self, id: &str, job: &CompleteJob) -> Result<Review> {
        Self::valid_id(id)?;
        self.call(Method::POST, &format!("/jobs/{id}/complete"), Some(job))
            .await
    }
    pub async fn fail(&self, id: &str, job: &FailJob) -> Result<()> {
        Self::valid_id(id)?;
        self.call(Method::POST, &format!("/jobs/{id}/fail"), Some(job))
            .await
    }
    pub async fn conversation(&self, id: &str) -> Result<Conversation> {
        Self::valid_id(id)?;
        self.call(
            Method::GET,
            &format!("/requests/{id}/conversation"),
            None::<&()>,
        )
        .await
    }
    pub async fn send_message(&self, id: &str, input: &SendMessage) -> Result<Conversation> {
        Self::valid_id(id)?;
        self.call(
            Method::POST,
            &format!("/requests/{id}/conversation"),
            Some(input),
        )
        .await
    }
    pub async fn cancel_conversation(&self, id: &str) -> Result<Conversation> {
        Self::valid_id(id)?;
        self.call(
            Method::POST,
            &format!("/requests/{id}/conversation/cancel"),
            Some(&()),
        )
        .await
    }
    pub async fn heartbeat(&self, detail: &str) -> Result<()> {
        self.call(
            Method::POST,
            "/heartbeat",
            Some(&Heartbeat {
                detail: detail.into(),
            }),
        )
        .await
    }
    pub async fn health(&self) -> Result<Health> {
        self.call(Method::GET, "/health", None::<&()>).await
    }
    pub async fn register_route(&self, route: &RouteRegistration) -> Result<ReviewRoute> {
        self.call(Method::POST, "/transport/route", Some(route))
            .await
    }
    pub async fn route(&self) -> Result<Option<ReviewRoute>> {
        self.call(Method::GET, "/transport/route", None::<&()>)
            .await
    }
    pub async fn reserve(&self, request: &ReserveDelivery) -> Result<Delivery> {
        self.call(Method::POST, "/transport/deliveries", Some(request))
            .await
    }
    pub async fn deliveries(&self) -> Result<Vec<Delivery>> {
        self.call(Method::GET, "/transport/deliveries", None::<&()>)
            .await
    }
    pub async fn delivered(&self, id: &str, message: &str) -> Result<Delivery> {
        Self::valid_id(id)?;
        self.call(
            Method::POST,
            &format!("/transport/deliveries/{id}/delivered"),
            Some(&Delivered {
                message: message.into(),
            }),
        )
        .await
    }
    pub async fn reply(&self, id: &str, reply: &DeliveryReply) -> Result<Review> {
        Self::valid_id(id)?;
        self.call(
            Method::POST,
            &format!("/transport/deliveries/{id}/reply"),
            Some(reply),
        )
        .await
    }
    fn valid_id(id: &str) -> Result<()> {
        ensure!(
            !id.is_empty()
                && id != "."
                && id != ".."
                && id.len() <= 128
                && id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.'),
            "invalid resource id"
        );
        Ok(())
    }
}

pub fn is_conflict(status: StatusCode) -> bool {
    status == StatusCode::CONFLICT
}

/// Verify generic bindings. The producer must additionally validate the action's
/// domain-specific meaning and its own monotonic pending-operation deadline.
pub fn verified_decision<'a>(
    review: &'a Review,
    expected: &SubmitRequest,
    producer: &str,
    now_ms: i64,
) -> Result<&'a Decision> {
    use sha2::{Digest, Sha256};
    let hash = |value: &[u8]| format!("{:x}", Sha256::digest(value));
    ensure!(
        review.request == *expected && review.producer == producer,
        "approval request identity changed"
    );
    ensure!(
        review.subject_digest
            == hash(&canonical_bytes(&(
                expected.schema_id.as_str(),
                &expected.subject
            ))?)
            && review.evidence_digest == hash(&canonical_bytes(&expected.evidence)?),
        "approval evidence binding mismatch"
    );
    let decision = review
        .decision
        .as_ref()
        .context("request has no decision")?;
    ensure!(
        decision.authoritative
            && decision.request_id == review.id
            && decision.producer == producer
            && decision.queue == expected.queue
            && decision.nonce == expected.nonce,
        "decision identity mismatch"
    );
    ensure!(
        decision.request_revision == review.revision
            && decision.manifest_revision == review.manifest_revision
            && decision.subject_digest == review.subject_digest
            && decision.evidence_digest == review.evidence_digest,
        "decision revision mismatch"
    );
    ensure!(
        decision.issued_at_ms <= now_ms
            && now_ms < decision.expires_at_ms
            && decision.expires_at_ms <= review.expires_at_ms
            && review.expires_at_ms <= expected.expires_at_ms,
        "decision expired or outside the request deadline"
    );
    ensure!(
        expected.action_ids.contains(&decision.action.id)
            && review.actions.contains(&decision.action),
        "decision action or scope changed"
    );
    ensure!(
        matches!(
            (&review.state, &decision.action.effect),
            (ReviewState::Allowed, Effect::Permit) | (ReviewState::Denied, Effect::Block)
        ),
        "decision is not an enforceable terminal outcome"
    );
    Ok(decision)
}

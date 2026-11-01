use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use connectrpc::{
    ConnectError, ErrorCode, RequestContext, Response, ServiceRequest, ServiceResult, ServiceStream,
};
use futures_util::{StreamExt, stream};
use pi_session::EventEnvelope;

use crate::proto::tachikoma::v1::{
    AdapterStatus, AnalysisRun, AnalysisService, AnalysisSummary, AnalyzeProposalRequest,
    AnalyzeProposalResponse, ApproveProposalRequest, AutomationPolicy, AutomationPolicyResponse,
    AutomationPolicyService, CancelTurnRequest, CreateProposalRequest, CreateProposalResponse,
    GetInvestigationRequest, GetStatusRequest, GetStatusResponse, Investigation,
    InvestigationEvent, InvestigationResponse, InvestigationService, ListAnalysesRequest,
    ListAnalysesResponse, ListModelsRequest, ListModelsResponse, ListPoliciesRequest,
    ListPoliciesResponse, ListProposalsRequest, ListProposalsResponse, ModelInfo,
    OpenInvestigationRequest, Proposal, ProposalResponse, ProposalService, ProposalState,
    QueueRemediationDraftRequest, RejectProposalRequest, SendMessageRequest, SetActiveModelRequest,
    StatusService, UpsertPolicyRequest, WatchInvestigationRequest,
};
use crate::store::{
    AnalysisRunRecord, AnalysisSummaryRecord, AutomationPolicyRecord, ProposalRecord, Store,
};

/// Minimal live-status service; proposal and policy services are added as their
/// durable store operations become available.
pub struct StatusApi {
    pub store: Arc<Mutex<Store>>,
    pub opensnitch_enabled: Arc<AtomicBool>,
    pub ai_enabled: Arc<AtomicBool>,
}

pub struct ProposalApi {
    pub store: Arc<Mutex<Store>>,
    pub analysis: Arc<crate::analysis::AnalysisScheduler>,
    pub investigations: Arc<crate::investigation::InvestigationManager>,
}

fn now() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .to_string()
}

pub fn new_proposal_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("p-{nanos:x}")
}

fn to_proto(record: ProposalRecord, latest_analysis: Option<AnalysisSummaryRecord>) -> Proposal {
    let state = match record.state.as_str() {
        "awaiting_review" => ProposalState::AwaitingReview,
        "approved" => ProposalState::Approved,
        "rejected" => ProposalState::Rejected,
        "queued" => ProposalState::Queued,
        "applied" => ProposalState::Applied,
        "failed" => ProposalState::Failed,
        _ => ProposalState::Proposed,
    };
    Proposal {
        id: record.id,
        adapter: record.adapter,
        action: record.action,
        state: state.into(),
        risk: record.risk,
        evidence_json: record.evidence_json,
        preview: record.preview,
        rollback: record.rollback,
        idempotency_key: record.idempotency_key,
        expires_at_unix: record.expires_at_unix,
        created_at: record.created_at,
        updated_at: record.updated_at,
        latest_analysis: latest_analysis.map(analysis_summary_to_proto).into(),
        ..Default::default()
    }
}

fn analysis_summary_to_proto(record: AnalysisSummaryRecord) -> AnalysisSummary {
    AnalysisSummary {
        id: record.id,
        state: record.state,
        profile_id: record.profile_id,
        provider: record.provider,
        model: record.model,
        finding: record.finding,
        priority: record.priority,
        confidence: record.confidence,
        rationale: record.rationale,
        next_step: record.next_step,
        error: record.error,
        created_at: record.created_at,
        completed_at: record.completed_at.unwrap_or_default(),
        ..Default::default()
    }
}

fn analysis_to_proto(record: AnalysisRunRecord) -> AnalysisRun {
    AnalysisRun {
        id: record.id,
        proposal_id: record.proposal_id,
        profile_id: record.profile_id,
        state: record.state,
        provider: record.provider,
        model: record.model,
        config_hash: record.config_hash,
        prompt_hash: record.prompt_hash,
        prompt_snapshot: record.prompt_snapshot,
        tool_trace_json: record.tool_trace_json,
        response_json: record.response_json,
        finding: record.finding,
        priority: record.priority,
        confidence: record.confidence,
        rationale: record.rationale,
        next_step: record.next_step,
        error: record.error,
        created_at: record.created_at,
        started_at: record.started_at.unwrap_or_default(),
        completed_at: record.completed_at.unwrap_or_default(),
        ..Default::default()
    }
}

fn investigation_to_proto(snapshot: crate::investigation::InvestigationSnapshot) -> Investigation {
    Investigation {
        id: snapshot.record.id,
        proposal_id: snapshot.record.proposal_id,
        active_provider: snapshot.record.active_provider,
        active_model: snapshot.record.active_model,
        status: snapshot.record.status,
        latest_remediation_json: snapshot.record.latest_remediation_json,
        events: snapshot.events.into_iter().map(event_to_proto).collect(),
        created_at: snapshot.record.created_at,
        updated_at: snapshot.record.updated_at,
        ..Default::default()
    }
}

fn event_to_proto(envelope: EventEnvelope) -> InvestigationEvent {
    let value = serde_json::to_value(&envelope.event).unwrap_or_default();
    InvestigationEvent {
        sequence: envelope.sequence,
        timestamp_ms: envelope.timestamp_ms,
        kind: value
            .get("event")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("unknown")
            .to_string(),
        json: serde_json::to_string(&envelope.event).unwrap_or_else(|_| "{}".into()),
        ..Default::default()
    }
}

fn policy_to_proto(record: AutomationPolicyRecord) -> AutomationPolicy {
    AutomationPolicy {
        id: record.id,
        adapter: record.adapter,
        action: record.action,
        scope: record.scope,
        mode: record.mode,
        risk_ceiling: record.risk_ceiling,
        ..Default::default()
    }
}

fn internal(error: impl std::fmt::Display) -> ConnectError {
    ConnectError::new(ErrorCode::Internal, error.to_string())
}

fn validate_policy(policy: &AutomationPolicy) -> Result<(), ConnectError> {
    if policy.id.is_empty()
        || policy.adapter.is_empty()
        || policy.action.is_empty()
        || policy.scope.is_empty()
        || !matches!(policy.mode.as_str(), "review" | "automatic")
        || !matches!(policy.risk_ceiling.as_str(), "low" | "medium" | "high")
    {
        return Err(ConnectError::new(
            ErrorCode::InvalidArgument,
            "policy id, adapter, action, JSON scope, mode (review|automatic), and risk ceiling (low|medium|high) are required",
        ));
    }
    let scope = serde_json::from_str::<serde_json::Value>(&policy.scope).map_err(|_| {
        ConnectError::new(
            ErrorCode::InvalidArgument,
            "policy scope must be a JSON object",
        )
    })?;
    if !scope.is_object() || scope.as_object().is_some_and(|value| value.is_empty()) {
        return Err(ConnectError::new(
            ErrorCode::InvalidArgument,
            "policy scope must contain at least one explicit constraint",
        ));
    }
    if policy.mode == "automatic" && policy.risk_ceiling != "low" {
        return Err(ConnectError::new(
            ErrorCode::InvalidArgument,
            "automatic policies are limited to a low risk ceiling",
        ));
    }
    Ok(())
}

impl ProposalService for ProposalApi {
    #[allow(clippy::manual_async_fn)]
    fn list_proposals<'a>(
        &'a self,
        _ctx: RequestContext,
        request: ServiceRequest<'_, ListProposalsRequest>,
    ) -> impl std::future::Future<
        Output = ServiceResult<impl connectrpc::Encodable<ListProposalsResponse> + Send + use<'a>>,
    > + Send {
        let request = request.to_owned_message();
        async move {
            let store = self
                .store
                .lock()
                .map_err(|_| internal("proposal store lock poisoned"))?;
            let state = request.state.as_known().ok_or_else(|| {
                ConnectError::new(ErrorCode::InvalidArgument, "unknown proposal state")
            })?;
            let state = match state {
                ProposalState::Unspecified => None,
                ProposalState::Observed => Some("observed"),
                ProposalState::Proposed => Some("proposed"),
                ProposalState::Validated => Some("validated"),
                ProposalState::AwaitingReview => Some("awaiting_review"),
                ProposalState::Approved => Some("approved"),
                ProposalState::Rejected => Some("rejected"),
                ProposalState::Expired => Some("expired"),
                ProposalState::Queued => Some("queued"),
                ProposalState::Executing => Some("executing"),
                ProposalState::Applied => Some("applied"),
                ProposalState::Failed => Some("failed"),
            };
            let records = store
                .list_for_adapter(
                    state,
                    (!request.adapter.is_empty()).then_some(request.adapter.as_str()),
                )
                .map_err(internal)?;
            let proposals = records
                .into_iter()
                .map(|record| {
                    let latest = store.latest_analysis(&record.id).ok().flatten();
                    to_proto(record, latest)
                })
                .collect();
            Response::ok(ListProposalsResponse {
                proposals,
                ..Default::default()
            })
        }
    }

    #[allow(clippy::manual_async_fn)]
    fn create_proposal<'a>(
        &'a self,
        _ctx: RequestContext,
        request: ServiceRequest<'_, CreateProposalRequest>,
    ) -> impl std::future::Future<
        Output = ServiceResult<impl connectrpc::Encodable<CreateProposalResponse> + Send + use<'a>>,
    > + Send {
        let request = request.to_owned_message();
        async move {
            if request.adapter.is_empty()
                || request.action.is_empty()
                || request.idempotency_key.is_empty()
            {
                return Err(ConnectError::new(
                    ErrorCode::InvalidArgument,
                    "adapter, action, and idempotency_key are required",
                ));
            }
            let timestamp = now();
            let mut record = ProposalRecord {
                id: new_proposal_id(),
                adapter: request.adapter,
                action: request.action,
                state: "awaiting_review".into(),
                risk: request.risk,
                evidence_json: request.evidence_json,
                preview: request.preview,
                rollback: request.rollback,
                idempotency_key: request.idempotency_key,
                expires_at_unix: request.expires_at_unix,
                created_at: timestamp.clone(),
                updated_at: timestamp,
            };
            {
                let store = self
                    .store
                    .lock()
                    .map_err(|_| internal("proposal store lock poisoned"))?;
                if let Some(policy_id) = store
                    .apply_automatic_policy(&mut record)
                    .map_err(internal)?
                {
                    tracing::info!(proposal_id = %record.id, %policy_id, "queued proposal through scoped automatic policy");
                }
                store.create(&record).map_err(internal)?;
            }
            if let Err(error) = self.analysis.schedule_automatic(&record.id).await {
                tracing::debug!(proposal_id = %record.id, %error, "no automatic AI analysis scheduled");
            }
            let latest = self
                .store
                .lock()
                .map_err(|_| internal("proposal store lock poisoned"))?
                .latest_analysis(&record.id)
                .map_err(internal)?;
            Response::ok(CreateProposalResponse {
                proposal: Some(to_proto(record, latest)).into(),
                ..Default::default()
            })
        }
    }

    #[allow(clippy::manual_async_fn)]
    fn approve_proposal<'a>(
        &'a self,
        _ctx: RequestContext,
        request: ServiceRequest<'_, ApproveProposalRequest>,
    ) -> impl std::future::Future<
        Output = ServiceResult<impl connectrpc::Encodable<ProposalResponse> + Send + use<'a>>,
    > + Send {
        let id = request.id.to_owned();
        async move {
            let (record, latest) = {
                let store = self
                    .store
                    .lock()
                    .map_err(|_| internal("proposal store lock poisoned"))?;
                let record = store
                    .transition(&id, "approved", &now())
                    .map_err(internal)?;
                let latest = store.latest_analysis(&id).ok().flatten();
                (record, latest)
            };
            Response::ok(ProposalResponse {
                proposal: Some(to_proto(record, latest)).into(),
                ..Default::default()
            })
        }
    }

    #[allow(clippy::manual_async_fn)]
    fn reject_proposal<'a>(
        &'a self,
        _ctx: RequestContext,
        request: ServiceRequest<'_, RejectProposalRequest>,
    ) -> impl std::future::Future<
        Output = ServiceResult<impl connectrpc::Encodable<ProposalResponse> + Send + use<'a>>,
    > + Send {
        let id = request.id.to_owned();
        async move {
            let (record, latest) = {
                let store = self
                    .store
                    .lock()
                    .map_err(|_| internal("proposal store lock poisoned"))?;
                let record = store
                    .transition(&id, "rejected", &now())
                    .map_err(internal)?;
                let latest = store.latest_analysis(&id).ok().flatten();
                (record, latest)
            };
            Response::ok(ProposalResponse {
                proposal: Some(to_proto(record, latest)).into(),
                ..Default::default()
            })
        }
    }
}

impl AnalysisService for ProposalApi {
    #[allow(clippy::manual_async_fn)]
    fn analyze_proposal<'a>(
        &'a self,
        _ctx: RequestContext,
        request: ServiceRequest<'_, AnalyzeProposalRequest>,
    ) -> impl std::future::Future<
        Output = ServiceResult<
            impl connectrpc::Encodable<AnalyzeProposalResponse> + Send + use<'a>,
        >,
    > + Send {
        let request = request.to_owned_message();
        async move {
            if request.proposal_id.is_empty() {
                return Err(ConnectError::new(
                    ErrorCode::InvalidArgument,
                    "proposal_id is required",
                ));
            }
            let run = self
                .analysis
                .schedule_manual(
                    &request.proposal_id,
                    (!request.profile_id.is_empty()).then_some(request.profile_id.as_str()),
                    request.force,
                )
                .await
                .map_err(internal)?;
            let run = self
                .store
                .lock()
                .map_err(|_| internal("proposal store lock poisoned"))?
                .get_analysis(&run.id)
                .map_err(internal)?;
            Response::ok(AnalyzeProposalResponse {
                run: Some(analysis_to_proto(run)).into(),
                ..Default::default()
            })
        }
    }

    #[allow(clippy::manual_async_fn)]
    fn list_analyses<'a>(
        &'a self,
        _ctx: RequestContext,
        request: ServiceRequest<'_, ListAnalysesRequest>,
    ) -> impl std::future::Future<
        Output = ServiceResult<impl connectrpc::Encodable<ListAnalysesResponse> + Send + use<'a>>,
    > + Send {
        let proposal_id = request.proposal_id.to_owned();
        async move {
            let runs = self
                .store
                .lock()
                .map_err(|_| internal("proposal store lock poisoned"))?
                .list_analyses((!proposal_id.is_empty()).then_some(proposal_id.as_str()))
                .map_err(internal)?
                .into_iter()
                .map(analysis_to_proto)
                .collect();
            Response::ok(ListAnalysesResponse {
                runs,
                ..Default::default()
            })
        }
    }
}

impl AutomationPolicyService for ProposalApi {
    #[allow(clippy::manual_async_fn)]
    fn list_policies<'a>(
        &'a self,
        _ctx: RequestContext,
        _request: ServiceRequest<'_, ListPoliciesRequest>,
    ) -> impl std::future::Future<
        Output = ServiceResult<impl connectrpc::Encodable<ListPoliciesResponse> + Send + use<'a>>,
    > + Send {
        async move {
            let policies = self
                .store
                .lock()
                .map_err(|_| internal("proposal store lock poisoned"))?
                .list_policies()
                .map_err(internal)?;
            Response::ok(ListPoliciesResponse {
                policies: policies.into_iter().map(policy_to_proto).collect(),
                ..Default::default()
            })
        }
    }

    #[allow(clippy::manual_async_fn)]
    fn upsert_policy<'a>(
        &'a self,
        _ctx: RequestContext,
        request: ServiceRequest<'_, UpsertPolicyRequest>,
    ) -> impl std::future::Future<
        Output = ServiceResult<
            impl connectrpc::Encodable<AutomationPolicyResponse> + Send + use<'a>,
        >,
    > + Send {
        let request = request.to_owned_message();
        let policy = request
            .policy
            .as_option()
            .cloned()
            .ok_or_else(|| ConnectError::new(ErrorCode::InvalidArgument, "policy is required"));
        async move {
            let policy = policy?;
            validate_policy(&policy)?;
            let record = AutomationPolicyRecord {
                id: policy.id,
                adapter: policy.adapter,
                action: policy.action,
                scope: policy.scope,
                mode: policy.mode,
                risk_ceiling: policy.risk_ceiling,
            };
            self.store
                .lock()
                .map_err(|_| internal("proposal store lock poisoned"))?
                .upsert_policy(&record)
                .map_err(internal)?;
            Response::ok(AutomationPolicyResponse {
                policy: Some(policy_to_proto(record)).into(),
                ..Default::default()
            })
        }
    }
}

impl InvestigationService for ProposalApi {
    #[allow(clippy::manual_async_fn)]
    fn list_models<'a>(
        &'a self,
        _ctx: RequestContext,
        _request: ServiceRequest<'_, ListModelsRequest>,
    ) -> impl std::future::Future<
        Output = ServiceResult<impl connectrpc::Encodable<ListModelsResponse> + Send + use<'a>>,
    > + Send {
        async move {
            let models = self
                .investigations
                .list_models()
                .into_iter()
                .map(|availability| ModelInfo {
                    provider: availability.model.provider,
                    id: availability.model.id,
                    name: availability.model.name,
                    authenticated: availability.authenticated,
                    credential_source: availability.credential_source.unwrap_or_default(),
                    context_window: availability.model.context_window.unwrap_or_default(),
                    usage_scope: match availability.model.usage_scope {
                        pi_providers::UsageScope::General => "general".into(),
                        pi_providers::UsageScope::CodingSubscription => {
                            "coding_subscription".into()
                        }
                    },
                    ..Default::default()
                })
                .collect();
            Response::ok(ListModelsResponse {
                models,
                default_model: self.investigations.default_model(),
                ..Default::default()
            })
        }
    }

    #[allow(clippy::manual_async_fn)]
    fn open_investigation<'a>(
        &'a self,
        _ctx: RequestContext,
        request: ServiceRequest<'_, OpenInvestigationRequest>,
    ) -> impl std::future::Future<
        Output = ServiceResult<impl connectrpc::Encodable<InvestigationResponse> + Send + use<'a>>,
    > + Send {
        let request = request.to_owned_message();
        async move {
            if request.proposal_id.is_empty() {
                return Err(ConnectError::new(
                    ErrorCode::InvalidArgument,
                    "proposal_id is required",
                ));
            }
            let snapshot = self
                .investigations
                .open(
                    &request.proposal_id,
                    (!request.model.is_empty()).then_some(request.model.as_str()),
                )
                .map_err(precondition)?;
            Response::ok(InvestigationResponse {
                investigation: Some(investigation_to_proto(snapshot)).into(),
                ..Default::default()
            })
        }
    }

    #[allow(clippy::manual_async_fn)]
    fn get_investigation<'a>(
        &'a self,
        _ctx: RequestContext,
        request: ServiceRequest<'_, GetInvestigationRequest>,
    ) -> impl std::future::Future<
        Output = ServiceResult<impl connectrpc::Encodable<InvestigationResponse> + Send + use<'a>>,
    > + Send {
        let id = request.investigation_id.to_owned();
        async move {
            let snapshot = self.investigations.get(&id).map_err(precondition)?;
            Response::ok(InvestigationResponse {
                investigation: Some(investigation_to_proto(snapshot)).into(),
                ..Default::default()
            })
        }
    }

    #[allow(clippy::manual_async_fn)]
    fn send_message<'a>(
        &'a self,
        _ctx: RequestContext,
        request: ServiceRequest<'_, SendMessageRequest>,
    ) -> impl std::future::Future<
        Output = ServiceResult<impl connectrpc::Encodable<InvestigationResponse> + Send + use<'a>>,
    > + Send {
        let request = request.to_owned_message();
        async move {
            let snapshot = self
                .investigations
                .send_message(&request.investigation_id, request.message)
                .map_err(precondition)?;
            Response::ok(InvestigationResponse {
                investigation: Some(investigation_to_proto(snapshot)).into(),
                ..Default::default()
            })
        }
    }

    #[allow(clippy::manual_async_fn, refining_impl_trait)]
    fn watch_investigation(
        &self,
        _ctx: RequestContext,
        request: ServiceRequest<'_, WatchInvestigationRequest>,
    ) -> impl std::future::Future<Output = ServiceResult<ServiceStream<InvestigationEvent>>> + Send
    {
        let request = request.to_owned_message();
        let manager = Arc::clone(&self.investigations);
        async move {
            let receiver = manager
                .subscribe(&request.investigation_id)
                .map_err(precondition)?;
            let snapshot = manager
                .get(&request.investigation_id)
                .map_err(precondition)?;
            let cutoff = snapshot
                .events
                .iter()
                .map(|event| event.sequence)
                .max()
                .unwrap_or(0);
            let replay = snapshot
                .events
                .into_iter()
                .filter(move |event| event.sequence > request.after_sequence)
                .map(|event| Ok(event_to_proto(event)))
                .collect::<Vec<_>>();
            let live = stream::unfold((receiver, cutoff), |(mut receiver, cutoff)| async move {
                loop {
                    match receiver.recv().await {
                        Ok(event) if event.sequence > cutoff => {
                            return Some((Ok(event_to_proto(event)), (receiver, cutoff)));
                        }
                        Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                            continue;
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => return None,
                    }
                }
            });
            Response::stream_ok(stream::iter(replay).chain(live))
        }
    }

    #[allow(clippy::manual_async_fn)]
    fn set_active_model<'a>(
        &'a self,
        _ctx: RequestContext,
        request: ServiceRequest<'_, SetActiveModelRequest>,
    ) -> impl std::future::Future<
        Output = ServiceResult<impl connectrpc::Encodable<InvestigationResponse> + Send + use<'a>>,
    > + Send {
        let request = request.to_owned_message();
        async move {
            if request.model.is_empty() {
                return Err(ConnectError::new(
                    ErrorCode::InvalidArgument,
                    "model is required",
                ));
            }
            let snapshot = self
                .investigations
                .set_model(&request.investigation_id, &request.model)
                .map_err(precondition)?;
            Response::ok(InvestigationResponse {
                investigation: Some(investigation_to_proto(snapshot)).into(),
                ..Default::default()
            })
        }
    }

    #[allow(clippy::manual_async_fn)]
    fn cancel_turn<'a>(
        &'a self,
        _ctx: RequestContext,
        request: ServiceRequest<'_, CancelTurnRequest>,
    ) -> impl std::future::Future<
        Output = ServiceResult<impl connectrpc::Encodable<InvestigationResponse> + Send + use<'a>>,
    > + Send {
        let id = request.investigation_id.to_owned();
        async move {
            let snapshot = self.investigations.cancel(&id).map_err(precondition)?;
            Response::ok(InvestigationResponse {
                investigation: Some(investigation_to_proto(snapshot)).into(),
                ..Default::default()
            })
        }
    }

    #[allow(clippy::manual_async_fn)]
    fn queue_remediation_draft<'a>(
        &'a self,
        _ctx: RequestContext,
        request: ServiceRequest<'_, QueueRemediationDraftRequest>,
    ) -> impl std::future::Future<
        Output = ServiceResult<impl connectrpc::Encodable<ProposalResponse> + Send + use<'a>>,
    > + Send {
        let request = request.to_owned_message();
        async move {
            let record = self
                .investigations
                .queue_remediation(
                    &request.investigation_id,
                    (!request.draft_json.is_empty()).then_some(request.draft_json.as_str()),
                )
                .map_err(precondition)?;
            Response::ok(ProposalResponse {
                proposal: Some(to_proto(record, None)).into(),
                ..Default::default()
            })
        }
    }
}

fn precondition(error: impl std::fmt::Display) -> ConnectError {
    ConnectError::new(ErrorCode::FailedPrecondition, error.to_string())
}

impl StatusService for StatusApi {
    #[allow(clippy::manual_async_fn)]
    fn get_status<'a>(
        &'a self,
        _ctx: RequestContext,
        _request: ServiceRequest<'_, GetStatusRequest>,
    ) -> impl std::future::Future<
        Output = ServiceResult<impl connectrpc::Encodable<GetStatusResponse> + Send + use<'a>>,
    > + Send {
        async move {
            let proposal_count = self
                .store
                .lock()
                .map_err(|_| internal("proposal store lock poisoned"))?
                .count()
                .map_err(internal)?;
            Response::ok(GetStatusResponse {
                version: env!("CARGO_PKG_VERSION").to_owned(),
                adapters: vec![
                    AdapterStatus {
                        name: "opensnitch".to_owned(),
                        enabled: self.opensnitch_enabled.load(Ordering::Relaxed),
                        detail: format!("proposal store ready ({proposal_count} proposals)"),
                        ..Default::default()
                    },
                    AdapterStatus {
                        name: "kubernetes".to_owned(),
                        enabled: true,
                        detail: "command observation is available; cluster execution is disabled"
                            .to_owned(),
                        ..Default::default()
                    },
                    AdapterStatus {
                        name: "ai".to_owned(),
                        enabled: self.ai_enabled.load(Ordering::Relaxed),
                        detail:
                            "prompt-driven analysis is advisory; proposal execution is disabled"
                                .to_owned(),
                        ..Default::default()
                    },
                ],
                ..Default::default()
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::validate_policy;
    use crate::proto::tachikoma::v1::AutomationPolicy;

    fn policy() -> AutomationPolicy {
        AutomationPolicy {
            id: "only-local-kubectl-read".into(),
            adapter: "kubernetes".into(),
            action: "review_kubectl_observation".into(),
            scope: r#"{"context":"development","namespace":"default"}"#.into(),
            mode: "review".into(),
            risk_ceiling: "low".into(),
            ..Default::default()
        }
    }

    #[test]
    fn policy_scope_must_be_narrow_and_automatic_is_low_risk_only() {
        assert!(validate_policy(&policy()).is_ok());
        let mut invalid_scope = policy();
        invalid_scope.scope = "{}".into();
        assert!(validate_policy(&invalid_scope).is_err());
        let mut broad_automatic = policy();
        broad_automatic.mode = "automatic".into();
        broad_automatic.risk_ceiling = "medium".into();
        assert!(validate_policy(&broad_automatic).is_err());
    }
}

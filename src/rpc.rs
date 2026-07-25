use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use connectrpc::{
    ConnectError, ErrorCode, RequestContext, Response, ServiceRequest, ServiceResult,
};

use crate::proto::tachikoma::v1::{
    AdapterStatus, ApproveProposalRequest, AutomationPolicy, AutomationPolicyResponse,
    AutomationPolicyService, CreateProposalRequest, CreateProposalResponse, GetStatusRequest,
    GetStatusResponse, ListPoliciesRequest, ListPoliciesResponse, ListProposalsRequest,
    ListProposalsResponse, Proposal, ProposalResponse, ProposalService, ProposalState,
    RejectProposalRequest, StatusService, UpsertPolicyRequest,
};
use crate::store::{AutomationPolicyRecord, ProposalRecord, Store};

/// Minimal live-status service; proposal and policy services are added as their
/// durable store operations become available.
pub struct StatusApi {
    pub store: Arc<Mutex<Store>>,
    pub opensnitch_enabled: bool,
}

pub struct ProposalApi {
    pub store: Arc<Mutex<Store>>,
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

fn to_proto(record: ProposalRecord) -> Proposal {
    let state = match record.state.as_str() {
        "awaiting_review" => ProposalState::AwaitingReview,
        "approved" => ProposalState::Approved,
        "rejected" => ProposalState::Rejected,
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
        _request: ServiceRequest<'_, ListProposalsRequest>,
    ) -> impl std::future::Future<
        Output = ServiceResult<impl connectrpc::Encodable<ListProposalsResponse> + Send + use<'a>>,
    > + Send {
        async move {
            let records = self
                .store
                .lock()
                .map_err(|_| internal("proposal store lock poisoned"))?
                .list(None)
                .map_err(internal)?;
            Response::ok(ListProposalsResponse {
                proposals: records.into_iter().map(to_proto).collect(),
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
            let record = ProposalRecord {
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
            self.store
                .lock()
                .map_err(|_| internal("proposal store lock poisoned"))?
                .create(&record)
                .map_err(internal)?;
            Response::ok(CreateProposalResponse {
                proposal: Some(to_proto(record)).into(),
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
            let record = self
                .store
                .lock()
                .map_err(|_| internal("proposal store lock poisoned"))?
                .transition(&id, "approved", &now())
                .map_err(internal)?;
            Response::ok(ProposalResponse {
                proposal: Some(to_proto(record)).into(),
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
            let record = self
                .store
                .lock()
                .map_err(|_| internal("proposal store lock poisoned"))?
                .transition(&id, "rejected", &now())
                .map_err(internal)?;
            Response::ok(ProposalResponse {
                proposal: Some(to_proto(record)).into(),
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
                        enabled: self.opensnitch_enabled,
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

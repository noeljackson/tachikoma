//! Transport-neutral adapter contract.
//!
//! Adapters observe local signals and produce proposals; they never execute a
//! suggested change. The daemon owns persistence and review transitions.

use crate::store::ProposalRecord;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProposalDraft {
    pub adapter: String,
    pub action: String,
    pub risk: String,
    pub evidence_json: String,
    pub preview: String,
    pub rollback: String,
    pub idempotency_key: String,
}

impl ProposalDraft {
    pub fn into_record(
        self,
        id: String,
        created_at: String,
        expires_at_unix: i64,
    ) -> ProposalRecord {
        ProposalRecord {
            id,
            adapter: self.adapter,
            action: self.action,
            state: "awaiting_review".into(),
            risk: self.risk,
            evidence_json: self.evidence_json,
            preview: self.preview,
            rollback: self.rollback,
            idempotency_key: self.idempotency_key,
            expires_at_unix,
            created_at: created_at.clone(),
            updated_at: created_at,
        }
    }
}

/// Every integration is observation-only at this layer. An executor, if one is
/// ever added, must consume an approved proposal through a separate capability
/// boundary rather than being embedded in an adapter.
pub trait ObservationAdapter {
    type Observation;

    fn name(&self) -> &'static str;
    fn draft(&self, observation: &Self::Observation) -> Option<ProposalDraft>;
}

use super::*;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceSpec {
    pub id: String,
    pub title: String,
    pub adapter: String,
    pub enabled: bool,
    pub queues: Vec<String>,
    pub capabilities: BTreeMap<String, Shape>,
    pub max_age_ms: i64,
    /// Descriptive coverage, never an assertion that missing events prove safety.
    pub coverage: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceInfo {
    pub spec: SourceSpec,
    pub revision: i64,
    pub last_seen_ms: i64,
    pub ready: bool,
    pub detail: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceHeartbeat {
    pub ready: bool,
    pub detail: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Observation {
    pub id: String,
    pub queue: String,
    pub capability: String,
    pub observed_at_ms: i64,
    pub data: Value,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObservationReceipt {
    pub id: String,
    pub source_revision: i64,
    pub digest: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapabilityRequirement {
    pub source: String,
    pub capability: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum Condition {
    All {
        conditions: Vec<Condition>,
    },
    Any {
        conditions: Vec<Condition>,
    },
    Equals {
        path: String,
        value: Value,
    },
    AtLeast {
        path: String,
        value: f64,
    },
    Observed {
        source: String,
        capability: String,
        within_ms: i64,
        /// Observation-data JSON pointers mapped to request-context pointers.
        joins: BTreeMap<String, String>,
        equals: BTreeMap<String, Value>,
    },
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum PolicyOutcome {
    Ask,
    Action { action_id: String },
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum UnavailableBehavior {
    Ask,
    Continue,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvidenceRule {
    pub id: String,
    pub description: String,
    #[serde(default)]
    pub requires: Vec<CapabilityRequirement>,
    pub when: Condition,
    pub then: PolicyOutcome,
    pub on_unavailable: UnavailableBehavior,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvidencePolicy {
    pub queue: String,
    pub rules: Vec<EvidenceRule>,
    pub default: PolicyOutcome,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvidencePolicyInfo {
    pub revision: i64,
    pub policy: EvidencePolicy,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MatchState {
    Matched,
    NotMatched,
    Unknown,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObservationRef {
    pub source: String,
    pub source_revision: i64,
    pub id: String,
    pub digest: String,
    pub observed_at_ms: i64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuleTrace {
    pub rule: String,
    pub state: MatchState,
    pub reason: String,
    pub observations: Vec<ObservationRef>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PolicyPreview {
    pub request_id: String,
    pub request_revision: i64,
    pub manifest_revision: i64,
    pub policy_revision: i64,
    pub evaluated_at_ms: i64,
    pub authoritative: bool,
    pub outcome: PolicyOutcome,
    pub reason: String,
    pub snapshot_digest: String,
    pub traces: Vec<RuleTrace>,
}

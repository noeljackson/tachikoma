//! Public, domain-independent wire contracts. No service implementation belongs here.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

pub const API_PREFIX: &str = "/api/approval/v1";
pub const MAX_BODY_BYTES: usize = 65_536;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Admin,
    Producer,
    Reviewer,
    Worker,
    Transport,
    /// Queue and process health only; cannot read requests, claim jobs or decide.
    Monitor,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrincipalSpec {
    pub id: String,
    pub role: Role,
    pub queues: Vec<String>,
    #[serde(default)]
    pub service_kind: String,
    #[serde(default)]
    pub realm: String,
    #[serde(default)]
    pub actors: Vec<String>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct EnrolledPrincipal {
    pub principal: PrincipalSpec,
    pub credential: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Effect {
    Permit,
    Block,
    Observe,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ReviewAction {
    pub id: String,
    pub label: String,
    pub effect: Effect,
    pub scope: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Shape {
    String {
        max_len: usize,
        #[serde(default)]
        choices: Vec<String>,
    },
    Integer {
        min: i64,
        max: i64,
    },
    Boolean,
    Object {
        fields: BTreeMap<String, Shape>,
        required: Vec<String>,
    },
    Array {
        items: Box<Shape>,
        max_items: usize,
    },
}

impl Shape {
    /// Deliberately bounded structural schema; no remote references or executable validators.
    pub fn accepts(&self, value: &Value) -> bool {
        self.accepts_at(value, 0)
    }
    fn accepts_at(&self, value: &Value, depth: usize) -> bool {
        if depth > 8 {
            return false;
        }
        match self {
            Self::String { max_len, choices } => value.as_str().is_some_and(|s| {
                s.len() <= *max_len && (choices.is_empty() || choices.iter().any(|c| c == s))
            }),
            Self::Integer { min, max } => value.as_i64().is_some_and(|n| n >= *min && n <= *max),
            Self::Boolean => value.is_boolean(),
            Self::Object { fields, required } => value.as_object().is_some_and(|v| {
                v.len() <= 64
                    && required.iter().all(|k| v.contains_key(k))
                    && v.iter().all(|(k, v)| {
                        fields
                            .get(k)
                            .is_some_and(|shape| shape.accepts_at(v, depth + 1))
                    })
            }),
            Self::Array { items, max_items } => value.as_array().is_some_and(|v| {
                v.len() <= *max_items && v.iter().all(|v| items.accepts_at(v, depth + 1))
            }),
        }
    }
    pub fn valid(&self) -> bool {
        self.valid_at(0)
    }
    fn valid_at(&self, depth: usize) -> bool {
        if depth > 8 {
            return false;
        }
        match self {
            Self::String { max_len, choices } => {
                *max_len > 0
                    && *max_len <= 4096
                    && choices.len() <= 64
                    && choices.iter().all(|s| s.len() <= *max_len)
            }
            Self::Integer { min, max } => {
                min <= max && *min >= -9_007_199_254_740_991 && *max <= 9_007_199_254_740_991
            }
            Self::Boolean => true,
            Self::Object { fields, required } => {
                fields.len() <= 64
                    && required.iter().all(|key| fields.contains_key(key))
                    && fields.values().all(|shape| shape.valid_at(depth + 1))
            }
            Self::Array { items, max_items } => *max_items <= 128 && items.valid_at(depth + 1),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    Shadow,
    #[default]
    Manual,
    Policy,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyRule {
    pub id: String,
    pub equals: BTreeMap<String, Value>,
    pub action: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct AssessmentProfile {
    pub instructions: String,
    pub labels: BTreeMap<String, String>,
    pub subject_fields: Vec<String>,
    #[serde(default)]
    pub evidence_fields: Vec<String>,
    #[serde(default)]
    pub jev: bool,
    #[serde(default)]
    pub llm: bool,
    #[serde(default)]
    pub models: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum SchedulingClass {
    Interactive,
    #[default]
    Background,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QueueManifest {
    pub id: String,
    pub title: String,
    pub service_kind: String,
    pub schema_id: String,
    pub subject_shape: Shape,
    pub actions: Vec<ReviewAction>,
    pub max_pending: u32,
    pub deadline_cap_ms: i64,
    #[serde(default)]
    pub scheduling_class: SchedulingClass,
    #[serde(default)]
    pub mode: Mode,
    #[serde(default)]
    pub rules: Vec<PolicyRule>,
    #[serde(default)]
    pub assessment: Option<AssessmentProfile>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueueInfo {
    pub revision: i64,
    pub manifest: QueueManifest,
    pub pending: u64,
    pub settled: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SubmitRequest {
    pub queue: String,
    pub producer_request_id: String,
    pub nonce: String,
    pub idempotency_key: String,
    pub schema_id: String,
    pub subject: Value,
    pub evidence: Value,
    pub action_ids: Vec<String>,
    pub expires_at_ms: i64,
    pub summary: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReviewState {
    Pending,
    Allowed,
    Denied,
    Observed,
    Expired,
    Cancelled,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Decision {
    pub id: String,
    pub request_id: String,
    pub producer: String,
    pub queue: String,
    pub request_revision: i64,
    pub manifest_revision: i64,
    pub nonce: String,
    pub subject_digest: String,
    pub evidence_digest: String,
    pub action: ReviewAction,
    pub actor: String,
    pub issued_at_ms: i64,
    pub expires_at_ms: i64,
    pub authoritative: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Assessment {
    pub state: String,
    pub category: String,
    pub confidence: Option<f64>,
    pub rationale: String,
    pub jev_model: String,
    pub llm_model: String,
    pub evidence_digest: String,
    pub completed_at_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EnforcementState {
    Enforced,
    NotEnforced,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnforcementReceipt {
    pub decision_id: String,
    pub state: EnforcementState,
    pub reason: String,
    pub observed_at_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Review {
    pub id: String,
    pub producer: String,
    pub service_kind: String,
    pub revision: i64,
    pub manifest_revision: i64,
    pub request: SubmitRequest,
    pub subject_digest: String,
    pub evidence_digest: String,
    pub created_at_ms: i64,
    pub expires_at_ms: i64,
    pub mode: Mode,
    pub state: ReviewState,
    pub actions: Vec<ReviewAction>,
    pub decision: Option<Decision>,
    pub assessment: Option<Assessment>,
    pub receipt: Option<EnforcementReceipt>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DecideRequest {
    pub revision: i64,
    pub action_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub sequence: i64,
    pub request_id: String,
    pub kind: String,
    pub at_ms: i64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Events {
    pub events: Vec<Event>,
    pub cursor: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClaimJob {
    pub kind: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Job {
    pub id: String,
    pub lease: String,
    pub attempt: u32,
    pub expires_at_ms: i64,
    pub review: Review,
    pub profile: AssessmentProfile,
    #[serde(default)]
    pub conversation: Option<ConversationTask>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompleteJob {
    pub lease: String,
    pub assessment: Assessment,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FailJob {
    pub lease: String,
    pub retryable: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Heartbeat {
    pub detail: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceHealth {
    pub id: String,
    pub service_kind: String,
    pub last_seen_ms: i64,
    pub detail: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Health {
    pub queues: Vec<QueueInfo>,
    pub services: Vec<ServiceHealth>,
    pub ready_jobs: u64,
    pub leased_jobs: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RouteRegistration {
    pub channel: String,
    pub actor: String,
    pub event_micros: i64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewRoute {
    pub channel: String,
    pub realm: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReserveDelivery {
    pub request_id: String,
    pub revision: i64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Delivery {
    pub id: String,
    pub realm: String,
    pub channel: String,
    pub message: Option<String>,
    pub nonce: String,
    pub review: Review,
    pub status: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Delivered {
    pub message: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeliveryReply {
    pub actor: String,
    pub channel: String,
    pub message: String,
    pub nonce: String,
    pub action_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiError {
    pub code: String,
    pub message: String,
}

/// Strip terminal control characters from data rendered by approval clients.
pub fn display_text(value: &str) -> String {
    value
        .chars()
        .filter(|c| {
            (!c.is_control() || matches!(c, '\n' | '\t'))
                && !matches!(c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
        })
        .collect()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Reviews {
    pub reviews: Vec<Review>,
    pub next_before: Option<i64>,
}

/// Canonical protocol encoding: sorted object keys, exact UTF-8 strings and JSON numbers.
/// Inputs must already have passed duplicate-key and schema validation.
pub fn canonical_bytes<T: Serialize>(input: &T) -> serde_json::Result<Vec<u8>> {
    fn append(value: &Value, out: &mut Vec<u8>) -> serde_json::Result<()> {
        match value {
            Value::Number(number)
                if !number.as_i64().is_some_and(|n| {
                    (-9_007_199_254_740_991..=9_007_199_254_740_991).contains(&n)
                }) =>
            {
                return Err(<serde_json::Error as serde::ser::Error>::custom(
                    "immutable protocol facts require interoperable integer numbers; encode decimal scores as strings",
                ));
            }
            Value::Object(fields) => {
                out.push(b'{');
                let mut keys = fields.keys().collect::<Vec<_>>();
                keys.sort();
                for (n, key) in keys.iter().enumerate() {
                    if n > 0 {
                        out.push(b',')
                    }
                    out.extend(serde_json::to_vec(key)?);
                    out.push(b':');
                    append(&fields[*key], out)?;
                }
                out.push(b'}');
            }
            Value::Array(values) => {
                out.push(b'[');
                for (n, value) in values.iter().enumerate() {
                    if n > 0 {
                        out.push(b',')
                    }
                    append(value, out)?;
                }
                out.push(b']');
            }
            _ => out.extend(serde_json::to_vec(value)?),
        }
        Ok(())
    }
    let mut out = vec![];
    append(&serde_json::to_value(input)?, &mut out)?;
    Ok(out)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationMessage {
    pub id: String,
    pub role: String,
    pub content: String,
    pub model: String,
    pub at_ms: i64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Conversation {
    pub request_id: String,
    pub queue: String,
    pub messages: Vec<ConversationMessage>,
    pub status: String,
    pub active_model: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SendMessage {
    pub message: String,
    #[serde(default)]
    pub model: Option<String>,
    pub idempotency_key: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationTask {
    pub messages: Vec<ConversationMessage>,
    pub model: Option<String>,
}

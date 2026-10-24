//! Network-specific implementation of the public approval producer contract.
use crate::{
    config::TachikomaConfig,
    model::{self, Action, Assessment, Category, Connection, Duration, Entry, MatchScope, Verdict},
};
use anyhow::{Context, Result, ensure};
use approval_client::Client;
use approval_protocol::{self as wire, *};
use serde_json::json;

pub fn client(config: &TachikomaConfig) -> Result<Client> {
    Client::new(
        "http://tachikoma.local",
        Some(&config.socket),
        crate::credentials::read(&config.credential_env)?
            .trim()
            .into(),
    )
}
const ACTIONS: [(&str, &str, Action, Duration, MatchScope); 14] = [
    (
        "allow-once",
        "Allow once",
        Action::Allow,
        Duration::Once,
        MatchScope::Connection,
    ),
    (
        "deny-once",
        "Deny once",
        Action::Deny,
        Duration::Once,
        MatchScope::Connection,
    ),
    (
        "allow-five-minutes",
        "Allow this IP/port for 5 minutes",
        Action::Allow,
        Duration::FiveMinutes,
        MatchScope::Connection,
    ),
    (
        "deny-five-minutes",
        "Deny this IP/port for 5 minutes",
        Action::Deny,
        Duration::FiveMinutes,
        MatchScope::Connection,
    ),
    (
        "deny-until-restart",
        "Deny this IP/port until restart",
        Action::Deny,
        Duration::UntilRestart,
        MatchScope::Connection,
    ),
    (
        "deny-executable-five-minutes",
        "Deny executable everywhere for 5 minutes",
        Action::Deny,
        Duration::FiveMinutes,
        MatchScope::Executable,
    ),
    (
        "deny-executable-until-restart",
        "Deny executable everywhere until restart",
        Action::Deny,
        Duration::UntilRestart,
        MatchScope::Executable,
    ),
    (
        "allow-until-restart",
        "Allow this IP/port until restart",
        Action::Allow,
        Duration::UntilRestart,
        MatchScope::Connection,
    ),
    (
        "allow-forever",
        "Allow this IP/port forever",
        Action::Allow,
        Duration::Forever,
        MatchScope::Connection,
    ),
    (
        "deny-forever",
        "Deny this IP/port forever",
        Action::Deny,
        Duration::Forever,
        MatchScope::Connection,
    ),
    (
        "allow-executable-five-minutes",
        "Allow executable anywhere for 5 minutes",
        Action::Allow,
        Duration::FiveMinutes,
        MatchScope::Executable,
    ),
    (
        "allow-executable-until-restart",
        "Allow executable anywhere until restart",
        Action::Allow,
        Duration::UntilRestart,
        MatchScope::Executable,
    ),
    (
        "allow-executable-forever",
        "Allow executable anywhere forever",
        Action::Allow,
        Duration::Forever,
        MatchScope::Executable,
    ),
    (
        "deny-executable-forever",
        "Deny executable everywhere forever",
        Action::Deny,
        Duration::Forever,
        MatchScope::Executable,
    ),
];
fn action_scope(duration: Duration, match_scope: MatchScope) -> serde_json::Value {
    let duration = match duration {
        Duration::Once => "once",
        Duration::FiveMinutes => "five-minutes",
        Duration::UntilRestart => "until-restart",
        Duration::Forever => "forever",
    };
    if match_scope == MatchScope::Executable {
        json!({"duration":duration,"match":"executable"})
    } else {
        json!({"duration":duration})
    }
}
pub fn actions() -> Vec<ReviewAction> {
    ACTIONS
        .iter()
        .map(|(id, label, action, duration, match_scope)| ReviewAction {
            id: (*id).into(),
            label: (*label).into(),
            effect: if *action == Action::Allow {
                Effect::Permit
            } else {
                Effect::Block
            },
            scope: action_scope(*duration, *match_scope),
        })
        .collect()
}
fn subject(c: &Connection) -> serde_json::Value {
    json!({"process":c.process,"pid":c.pid,"uid":c.uid,"protocol":c.protocol,"ip":c.ip.to_string(),"host":c.host,"port":c.port})
}
pub fn request(config: &TachikomaConfig, entry: &Entry) -> SubmitRequest {
    let c = &entry.connection;
    let action_ids = ACTIONS
        .iter()
        .filter(|(_, _, _, duration, _)| *duration == Duration::Once || c.process.starts_with('/'))
        .map(|(id, _, _, _, _)| (*id).to_owned())
        .collect();
    SubmitRequest {
        queue: config.queue.clone(),
        producer_request_id: entry.id.clone(),
        nonce: entry.id.clone(),
        idempotency_key: entry.id.clone(),
        schema_id: "network.connect.v1".into(),
        subject: subject(c),
        evidence: json!({"resource_details":c.resource_details(),"url_hints":c.url_hints,"process_context":c.context,"context_limits":"OpenSnitch-reported metadata; ancestry, cwd, argv markers and lifecycle variables are unverified claims. Checksums are reported, not locally verified. Missing fields do not prove absence. No file-access, HTTP payload, package provenance or test-listener attribution is available.","attribution":"Captured process path and hostname are unverified claims. Approval covers the network connection, not an HTTP file."}),
        action_ids,
        expires_at_ms: i64::try_from(entry.deadline)
            .unwrap_or(i64::MAX / 1000)
            .saturating_mul(1000),
        summary: format!(
            "{} → {}:{}",
            c.process,
            if c.host.is_empty() {
                c.ip.to_string()
            } else {
                c.host.clone()
            },
            c.port
        )
        .chars()
        .take(256)
        .collect(),
    }
}
pub fn verdict(
    config: &TachikomaConfig,
    submitted: &SubmitRequest,
    review: &Review,
    connection: &Connection,
) -> Result<Verdict> {
    let now = i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_millis(),
    )
    .context("clock overflow")?;
    ensure!(
        submitted.subject == subject(connection),
        "captured connection binding changed"
    );
    let decision = approval_client::verified_decision(review, submitted, &config.producer_id, now)?;
    let (_, _, action, duration, match_scope) = ACTIONS
        .iter()
        .find(|(id, _, _, _, _)| *id == decision.action.id)
        .copied()
        .ok_or_else(|| anyhow::anyhow!("unsupported network action"))?;
    let scope = action_scope(duration, match_scope);
    if duration != Duration::Once {
        ensure!(
            connection.process.starts_with('/'),
            "extended permissions require an absolute executable path"
        );
    }
    if match_scope == MatchScope::Executable
        || matches!(duration, Duration::UntilRestart | Duration::Forever)
    {
        ensure!(
            review.mode == Mode::Manual && !decision.actor.starts_with("policy:"),
            "extended permissions require manual human review"
        );
    }
    ensure!(decision.action.scope == scope, "network scope mismatch");
    ensure!(
        (action == Action::Allow && decision.action.effect == Effect::Permit)
            || (action == Action::Deny && decision.action.effect == Effect::Block),
        "network effect mismatch"
    );
    let rule_id = uuid::Uuid::parse_str(&decision.id)
        .context("invalid central decision identifier")?
        .to_string();
    Ok(Verdict {
        action,
        duration,
        match_scope,
        rule_id: Some(rule_id),
        source: "tachikoma".into(),
        reason: format!(
            "Central approval {} by {}",
            decision.id,
            model::display_text(&decision.actor)
        ),
    })
}
pub fn assessment(value: &wire::Assessment) -> Assessment {
    let category =
        serde_json::from_value::<Category>(json!(value.category)).unwrap_or(Category::Unknown);
    Assessment {
        category,
        confidence: value.confidence.unwrap_or(0.0),
        model: model::display_text(&value.jev_model),
        llm_model: (!value.llm_model.is_empty()).then(|| model::display_text(&value.llm_model)),
        explanation: model::display_text(&value.rationale),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    pub fn config() -> TachikomaConfig {
        TachikomaConfig {
            socket: "/tmp/test.sock".into(),
            producer_id: "test".into(),
            queue: "network.egress".into(),
            credential_env: "TEST_KEY".into(),
            shadow: false,
        }
    }
    pub fn entry() -> Entry {
        Entry {
            id: "request-unique-nonce-123456".into(),
            connection: crate::broker::demo_connection(
                "/usr/bin/example",
                "example.invalid",
                "127.0.0.1",
                443,
            ),
            received: model::now(),
            deadline: model::now() + 60,
            assessment: None,
            classifier_status: "queued".into(),
            verdict: None,
            finished: None,
        }
    }
    #[test]
    fn url_hints_are_human_evidence_only_and_manifest_scopes_match() {
        let value = request(&config(), &entry());
        assert!(value.subject.get("url_hints").is_none());
        assert!(value.evidence.get("url_hints").is_some());
        assert_eq!(value.producer_request_id, entry().id);
        assert_eq!(value.action_ids.len(), ACTIONS.len());
        let manifest: QueueManifest = serde_json::from_str(include_str!(
            "../../../contrib/adapters/snitchgate.queue.json"
        ))
        .unwrap();
        assert_eq!(
            serde_json::to_value(manifest.actions).unwrap(),
            serde_json::to_value(actions()).unwrap()
        );
        let mut entry = entry();
        entry.connection.process.clear();
        assert_eq!(
            request(&config(), &entry).action_ids,
            vec!["allow-once", "deny-once"]
        );
    }
    fn approved(action_id: &str) -> (Entry, SubmitRequest, Review) {
        use sha2::{Digest, Sha256};
        let entry = entry();
        let request = request(&config(), &entry);
        let digest = |value: Vec<u8>| format!("{:x}", Sha256::digest(value));
        let subject_digest =
            digest(canonical_bytes(&(request.schema_id.as_str(), &request.subject)).unwrap());
        let evidence_digest = digest(canonical_bytes(&request.evidence).unwrap());
        let id = uuid::Uuid::new_v4().to_string();
        let action = actions()
            .into_iter()
            .find(|action| action.id == action_id)
            .unwrap();
        let decision = Decision {
            id: uuid::Uuid::new_v4().to_string(),
            request_id: id.clone(),
            producer: config().producer_id,
            queue: request.queue.clone(),
            request_revision: 1,
            manifest_revision: 1,
            nonce: request.nonce.clone(),
            subject_digest: subject_digest.clone(),
            evidence_digest: evidence_digest.clone(),
            action: action.clone(),
            actor: "local-desktop".into(),
            issued_at_ms: model::now() as i64 * 1000,
            expires_at_ms: request.expires_at_ms,
            authoritative: true,
        };
        let review = Review {
            id,
            producer: config().producer_id,
            service_kind: "snitchgate".into(),
            revision: 1,
            manifest_revision: 1,
            request: request.clone(),
            subject_digest,
            evidence_digest,
            created_at_ms: model::now() as i64 * 1000,
            expires_at_ms: request.expires_at_ms,
            mode: Mode::Manual,
            state: if action.effect == Effect::Permit {
                ReviewState::Allowed
            } else {
                ReviewState::Denied
            },
            actions: actions(),
            decision: Some(decision),
            assessment: None,
            receipt: None,
        };
        (entry, request, review)
    }
    #[test]
    fn extended_decisions_validate_every_scope_and_bind_exact_executable_and_user() {
        for action in actions() {
            let (entry, request, review) = approved(&action.id);
            let verdict = verdict(&config(), &request, &review, &entry.connection).unwrap();
            let rule = crate::policy::rule(
                verdict.rule_id.as_deref().unwrap(),
                &entry.connection,
                &verdict,
            );
            assert_eq!(
                rule.action,
                if action.effect == Effect::Permit {
                    "allow"
                } else {
                    "deny"
                }
            );
            let operands = rule.operator.unwrap().list;
            assert!(operands.iter().any(|operator| operator.operand == "user.id"
                && operator.data == entry.connection.uid.to_string()));
            assert!(
                operands
                    .iter()
                    .any(|operator| operator.operand == "process.path"
                        && operator.data == entry.connection.process)
            );
            assert_eq!(
                operands.len(),
                if action.scope["match"] == "executable" {
                    2
                } else {
                    5
                }
            );
            if action.scope["duration"] == "forever" {
                assert_eq!(rule.duration, "always");
            }
            if action.scope["duration"] == "until-restart" {
                assert_eq!(rule.duration, "until restart");
            }
        }
        let (entry, request, mut review) = approved("allow-executable-forever");
        let mut changed = entry.connection.clone();
        changed.process = "/usr/bin/other".into();
        assert!(verdict(&config(), &request, &review, &changed).is_err());
        changed = entry.connection.clone();
        changed.uid += 1;
        assert!(verdict(&config(), &request, &review, &changed).is_err());
        review.mode = Mode::Policy;
        assert!(verdict(&config(), &request, &review, &entry.connection).is_err());
        review.mode = Mode::Manual;
        review.decision.as_mut().unwrap().actor = "policy:1:auto".into();
        assert!(verdict(&config(), &request, &review, &entry.connection).is_err());
    }
    #[test]
    fn changed_expired_or_opaque_permissions_never_produce_reusable_rules() {
        let (entry, request, mut review) = approved("allow-executable-forever");
        review
            .actions
            .iter_mut()
            .find(|action| action.id == "allow-executable-forever")
            .unwrap()
            .scope["extra"] = json!("broaden");
        review.decision.as_mut().unwrap().action.scope["extra"] = json!("broaden");
        assert!(verdict(&config(), &request, &review, &entry.connection).is_err());
        let (entry, request, mut review) = approved("allow-forever");
        review.decision.as_mut().unwrap().expires_at_ms = 0;
        assert!(verdict(&config(), &request, &review, &entry.connection).is_err());
    }
    #[test]
    fn persistent_scope_and_rule_identity_survive_local_audit_storage() {
        let (mut entry, request, review) = approved("allow-executable-forever");
        let verdict = verdict(&config(), &request, &review, &entry.connection).unwrap();
        let rule_id = verdict.rule_id.clone();
        entry.verdict = Some(verdict);
        let mut store = crate::store::Store::memory(10).unwrap();
        store.record(&entry).unwrap();
        let verdict = store.recent(1).unwrap().remove(0).verdict.unwrap();
        assert_eq!(verdict.duration, Duration::Forever);
        assert_eq!(verdict.match_scope, MatchScope::Executable);
        assert_eq!(verdict.rule_id, rule_id);
        let old: Verdict = serde_json::from_value(
            json!({"action":"allow","duration":"once","source":"human","reason":"old"}),
        )
        .unwrap();
        assert_eq!(old.match_scope, MatchScope::Connection);
        assert!(old.rule_id.is_none());
    }
}

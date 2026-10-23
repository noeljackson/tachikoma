//! Network-specific implementation of the public approval producer contract.
use crate::{
    config::TachikomaConfig,
    model::{self, Action, Assessment, Category, Connection, Duration, Entry, Verdict},
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
pub fn request(config: &TachikomaConfig, entry: &Entry) -> SubmitRequest {
    let c = &entry.connection;
    let mut action_ids = vec!["allow-once".into(), "deny-once".into()];
    if !c.process.is_empty() {
        action_ids.push("allow-five-minutes".into());
    }
    SubmitRequest {
        queue: config.queue.clone(),
        producer_request_id: entry.id.clone(),
        nonce: entry.id.clone(),
        idempotency_key: entry.id.clone(),
        schema_id: "network.connect.v1".into(),
        subject: json!({"process":c.process,"pid":c.pid,"uid":c.uid,"protocol":c.protocol,"ip":c.ip.to_string(),"host":c.host,"port":c.port}),
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
    let decision = approval_client::verified_decision(review, submitted, &config.producer_id, now)?;
    let (action, duration, scope) = match decision.action.id.as_str() {
        "allow-once" => (Action::Allow, Duration::Once, json!({"duration":"once"})),
        "deny-once" => (Action::Deny, Duration::Once, json!({"duration":"once"})),
        "allow-five-minutes" if !connection.process.is_empty() => (
            Action::Allow,
            Duration::FiveMinutes,
            json!({"duration":"five-minutes"}),
        ),
        _ => anyhow::bail!("unsupported network action"),
    };
    ensure!(decision.action.scope == scope, "network scope mismatch");
    ensure!(
        (action == Action::Allow && decision.action.effect == Effect::Permit)
            || (action == Action::Deny && decision.action.effect == Effect::Block),
        "network effect mismatch"
    );
    Ok(Verdict {
        action,
        duration,
        source: "tachikoma".into(),
        reason: format!(
            "Scoped central decision by {}",
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
    #[test]
    fn url_hints_are_human_evidence_only() {
        let config = TachikomaConfig {
            socket: "/tmp/test.sock".into(),
            producer_id: "test".into(),
            queue: "network.egress".into(),
            credential_env: "TEST_KEY".into(),
            shadow: false,
        };
        let entry = Entry {
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
        };
        let value = request(&config, &entry);
        assert!(value.subject.get("url_hints").is_none());
        assert!(value.evidence.get("url_hints").is_some());
        assert_eq!(value.producer_request_id, entry.id);
        assert_eq!(value.action_ids.len(), 3);
    }
}

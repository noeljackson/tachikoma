//! Read-only ingestion from the OpenSnitch UI history database.

use std::path::Path;

use anyhow::{Context, Result};
use rusqlite::Connection;
use serde_json::json;

use crate::store::ProposalRecord;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectionSignal {
    pub time: String,
    pub action: String,
    pub process: String,
    pub destination_host: String,
    pub destination_port: i64,
    pub rule: String,
}

/// Read recent OpenSnitch UI history without touching daemon rules or state.
pub fn recent_connections(database: &Path, limit: usize) -> Result<Vec<ConnectionSignal>> {
    let connection =
        Connection::open_with_flags(database, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .with_context(|| format!("open OpenSnitch history {}", database.display()))?;
    let mut statement = connection.prepare(
        "SELECT time, action, process, COALESCE(dst_host, ''), dst_port, COALESCE(rule, '')
         FROM connections ORDER BY time DESC LIMIT ?1",
    )?;
    let signals = statement
        .query_map([limit as i64], |row| {
            Ok(ConnectionSignal {
                time: row.get(0)?,
                action: row.get(1)?,
                process: row.get(2)?,
                destination_host: row.get(3)?,
                destination_port: row.get(4)?,
                rule: row.get(5)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(signals)
}

/// Convert an OpenSnitch denial into a review-only proposal. This adapter never
/// writes OpenSnitch rules and intentionally ignores allowed traffic: a denial
/// is a useful prompt for human review, not authority to create an allow rule.
pub fn denial_proposal(
    signal: &ConnectionSignal,
    id: String,
    created_at: String,
    expires_at_unix: i64,
) -> Option<ProposalRecord> {
    if !signal.action.eq_ignore_ascii_case("deny") {
        return None;
    }

    let target = if signal.destination_port > 0 {
        format!("{}:{}", signal.destination_host, signal.destination_port)
    } else {
        signal.destination_host.clone()
    };
    let evidence_json = json!({
        "time": signal.time,
        "action": signal.action,
        "process": signal.process,
        "destination_host": signal.destination_host,
        "destination_port": signal.destination_port,
        "matched_rule": signal.rule,
    })
    .to_string();
    Some(ProposalRecord {
        id,
        adapter: "opensnitch".into(),
        action: "review_denied_connection".into(),
        state: "awaiting_review".into(),
        risk: "medium".into(),
        evidence_json,
        preview: format!(
            "Review denied connection from {} to {target}. No OpenSnitch rule will be changed automatically.",
            signal.process
        ),
        rollback:
            "No action has been executed; rejecting or expiring this proposal changes nothing."
                .into(),
        idempotency_key: format!(
            "opensnitch-deny:{}:{}:{}:{}",
            signal.time, signal.process, signal.destination_host, signal.destination_port
        ),
        expires_at_unix,
        created_at: created_at.clone(),
        updated_at: created_at,
    })
}

#[cfg(test)]
mod tests {
    use super::{ConnectionSignal, denial_proposal};

    fn signal(action: &str) -> ConnectionSignal {
        ConnectionSignal {
            time: "2026-07-25T10:00:00Z".into(),
            action: action.into(),
            process: "/usr/bin/example".into(),
            destination_host: "api.example.test".into(),
            destination_port: 443,
            rule: "default".into(),
        }
    }

    #[test]
    fn only_denials_become_review_only_proposals() {
        assert!(denial_proposal(&signal("allow"), "p-1".into(), "1".into(), 2).is_none());
        let proposal =
            denial_proposal(&signal("deny"), "p-1".into(), "1".into(), 2).expect("denial proposal");
        assert_eq!(proposal.state, "awaiting_review");
        assert_eq!(proposal.action, "review_denied_connection");
        assert!(proposal.preview.contains("No OpenSnitch rule"));
    }
}

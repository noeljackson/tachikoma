use std::path::Path;

use anyhow::{Context, Result};
use rusqlite::Connection;
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProposalRecord {
    pub id: String,
    pub adapter: String,
    pub action: String,
    pub state: String,
    pub risk: String,
    pub evidence_json: String,
    pub preview: String,
    pub rollback: String,
    pub idempotency_key: String,
    pub expires_at_unix: i64,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutomationPolicyRecord {
    pub id: String,
    pub adapter: String,
    pub action: String,
    pub scope: String,
    pub mode: String,
    pub risk_ceiling: String,
}

/// Durable SQLite store. The proposal schema is intentionally transport-neutral.
pub struct Store {
    connection: Connection,
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create state directory {}", parent.display()))?;
        }
        let connection =
            Connection::open(path).with_context(|| format!("open database {}", path.display()))?;
        connection.execute_batch(
            "
            PRAGMA foreign_keys = ON;
            PRAGMA journal_mode = WAL;
            CREATE TABLE IF NOT EXISTS proposals (
              id TEXT PRIMARY KEY,
              adapter TEXT NOT NULL,
              action TEXT NOT NULL,
              state TEXT NOT NULL,
              risk TEXT NOT NULL,
              evidence_json TEXT NOT NULL,
              preview TEXT NOT NULL,
              rollback TEXT NOT NULL,
              idempotency_key TEXT NOT NULL UNIQUE,
              expires_at_unix INTEGER NOT NULL,
              created_at TEXT NOT NULL,
              updated_at TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS proposals_state_updated
              ON proposals(state, updated_at DESC);
            CREATE TABLE IF NOT EXISTS automation_policies (
              id TEXT PRIMARY KEY,
              adapter TEXT NOT NULL,
              action TEXT NOT NULL,
              scope TEXT NOT NULL,
              mode TEXT NOT NULL,
              risk_ceiling TEXT NOT NULL
            );
            ",
        )?;
        Ok(Self { connection })
    }

    pub fn count(&self) -> Result<i64> {
        self.connection
            .query_row("SELECT COUNT(*) FROM proposals", [], |row| row.get(0))
            .context("count proposals")
    }

    pub fn create(&self, proposal: &ProposalRecord) -> Result<()> {
        self.connection.execute(
            "INSERT INTO proposals (id, adapter, action, state, risk, evidence_json, preview, rollback, idempotency_key, expires_at_unix, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            rusqlite::params![proposal.id, proposal.adapter, proposal.action, proposal.state, proposal.risk, proposal.evidence_json, proposal.preview, proposal.rollback, proposal.idempotency_key, proposal.expires_at_unix, proposal.created_at, proposal.updated_at],
        ).context("create proposal")?;
        Ok(())
    }

    /// Persist a scanner-generated proposal only once for its idempotency key.
    /// A repeated read of an immutable signal must never create another review.
    pub fn create_if_absent(&self, proposal: &ProposalRecord) -> Result<bool> {
        let changed = self.connection.execute(
            "INSERT OR IGNORE INTO proposals (id, adapter, action, state, risk, evidence_json, preview, rollback, idempotency_key, expires_at_unix, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            rusqlite::params![proposal.id, proposal.adapter, proposal.action, proposal.state, proposal.risk, proposal.evidence_json, proposal.preview, proposal.rollback, proposal.idempotency_key, proposal.expires_at_unix, proposal.created_at, proposal.updated_at],
        )?;
        Ok(changed == 1)
    }

    /// Apply a policy only when it is an exact, low-risk automatic match.
    /// `queued` is not execution: no executor exists in this crate, so this
    /// state remains visible and inert until an adapter-specific capability is
    /// deliberately added in the future.
    pub fn apply_automatic_policy(&self, proposal: &mut ProposalRecord) -> Result<Option<String>> {
        if proposal.state != "awaiting_review" || proposal.risk != "low" {
            return Ok(None);
        }
        let evidence = match serde_json::from_str::<Value>(&proposal.evidence_json) {
            Ok(Value::Object(value)) => Value::Object(value),
            _ => return Ok(None),
        };
        for policy in self.list_policies()? {
            if policy.mode != "automatic"
                || policy.risk_ceiling != "low"
                || policy.adapter != proposal.adapter
                || policy.action != proposal.action
            {
                continue;
            }
            let scope = match serde_json::from_str::<Value>(&policy.scope) {
                Ok(Value::Object(value)) if !value.is_empty() => Value::Object(value),
                _ => continue,
            };
            if scope_matches(&scope, &evidence) {
                proposal.state = "queued".into();
                return Ok(Some(policy.id));
            }
        }
        Ok(None)
    }

    pub fn list(&self, state: Option<&str>) -> Result<Vec<ProposalRecord>> {
        let mut statement = self.connection.prepare(
            "SELECT id, adapter, action, state, risk, evidence_json, preview, rollback, idempotency_key, expires_at_unix, created_at, updated_at
             FROM proposals
             WHERE (?1 IS NULL OR state = ?1)
             ORDER BY updated_at DESC",
        )?;
        let rows = statement.query_map([state], |row| {
            Ok(ProposalRecord {
                id: row.get(0)?,
                adapter: row.get(1)?,
                action: row.get(2)?,
                state: row.get(3)?,
                risk: row.get(4)?,
                evidence_json: row.get(5)?,
                preview: row.get(6)?,
                rollback: row.get(7)?,
                idempotency_key: row.get(8)?,
                expires_at_unix: row.get(9)?,
                created_at: row.get(10)?,
                updated_at: row.get(11)?,
            })
        })?;
        let records = rows.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(records)
    }

    pub fn transition(&self, id: &str, state: &str, updated_at: &str) -> Result<ProposalRecord> {
        if !matches!(state, "approved" | "rejected") {
            anyhow::bail!("invalid review state: {state}");
        }
        let changed = self.connection.execute(
            "UPDATE proposals SET state = ?1, updated_at = ?2 WHERE id = ?3 AND state = 'awaiting_review'",
            rusqlite::params![state, updated_at, id],
        )?;
        if changed != 1 {
            anyhow::bail!("proposal is missing or no longer awaiting review");
        }
        self.connection.query_row(
            "SELECT id, adapter, action, state, risk, evidence_json, preview, rollback, idempotency_key, expires_at_unix, created_at, updated_at FROM proposals WHERE id = ?1",
            [id],
            |row| Ok(ProposalRecord { id: row.get(0)?, adapter: row.get(1)?, action: row.get(2)?, state: row.get(3)?, risk: row.get(4)?, evidence_json: row.get(5)?, preview: row.get(6)?, rollback: row.get(7)?, idempotency_key: row.get(8)?, expires_at_unix: row.get(9)?, created_at: row.get(10)?, updated_at: row.get(11)? }),
        ).context("load transitioned proposal")
    }

    pub fn list_policies(&self) -> Result<Vec<AutomationPolicyRecord>> {
        let mut statement = self.connection.prepare(
            "SELECT id, adapter, action, scope, mode, risk_ceiling
             FROM automation_policies ORDER BY adapter, action, id",
        )?;
        let rows = statement.query_map([], |row| {
            Ok(AutomationPolicyRecord {
                id: row.get(0)?,
                adapter: row.get(1)?,
                action: row.get(2)?,
                scope: row.get(3)?,
                mode: row.get(4)?,
                risk_ceiling: row.get(5)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Insert or replace an explicit, narrowly-scoped automation policy.
    /// Policy evaluation is deliberately separate from proposal creation.
    pub fn upsert_policy(&self, policy: &AutomationPolicyRecord) -> Result<()> {
        self.connection.execute(
            "INSERT INTO automation_policies (id, adapter, action, scope, mode, risk_ceiling)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(id) DO UPDATE SET
                adapter = excluded.adapter,
                action = excluded.action,
                scope = excluded.scope,
                mode = excluded.mode,
                risk_ceiling = excluded.risk_ceiling",
            rusqlite::params![
                policy.id,
                policy.adapter,
                policy.action,
                policy.scope,
                policy.mode,
                policy.risk_ceiling,
            ],
        )?;
        Ok(())
    }
}

fn scope_matches(scope: &Value, evidence: &Value) -> bool {
    match (scope, evidence) {
        (Value::Object(scope), Value::Object(evidence)) => scope.iter().all(|(key, value)| {
            evidence
                .get(key)
                .is_some_and(|candidate| scope_matches(value, candidate))
        }),
        _ => scope == evidence,
    }
}

#[cfg(test)]
mod tests {
    use super::{ProposalRecord, Store};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn test_store() -> (Store, std::path::PathBuf) {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!("tachikoma-store-{suffix}.sqlite3"));
        let store = Store::open(&path).expect("open store");
        (store, path)
    }

    fn proposal() -> ProposalRecord {
        ProposalRecord {
            id: "proposal-1".into(),
            adapter: "opensnitch".into(),
            action: "review_connection".into(),
            state: "awaiting_review".into(),
            risk: "medium".into(),
            evidence_json: "{}".into(),
            preview: "No policy changes.".into(),
            rollback: "No action executed.".into(),
            idempotency_key: "test-1".into(),
            expires_at_unix: 0,
            created_at: "1".into(),
            updated_at: "1".into(),
        }
    }

    #[test]
    fn review_transition_is_single_use_and_state_is_validated() {
        let (store, path) = test_store();
        store.create(&proposal()).expect("create proposal");
        let reviewed = store
            .transition("proposal-1", "approved", "2")
            .expect("approve proposal");
        assert_eq!(reviewed.state, "approved");
        assert!(store.transition("proposal-1", "rejected", "3").is_err());
        assert!(store.transition("proposal-1", "applied", "3").is_err());
        drop(store);
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(path.with_extension("sqlite3-wal"));
        let _ = std::fs::remove_file(path.with_extension("sqlite3-shm"));
    }

    #[test]
    fn state_filter_limits_queue_view() {
        let (store, path) = test_store();
        store.create(&proposal()).expect("create proposal");
        assert_eq!(store.list(None).expect("list all").len(), 1);
        assert_eq!(
            store
                .list(Some("awaiting_review"))
                .expect("list queue")
                .len(),
            1
        );
        assert!(
            store
                .list(Some("approved"))
                .expect("list approved")
                .is_empty()
        );
        drop(store);
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(path.with_extension("sqlite3-wal"));
        let _ = std::fs::remove_file(path.with_extension("sqlite3-shm"));
    }

    #[test]
    fn scanner_insert_is_idempotent() {
        let (store, path) = test_store();
        let record = proposal();
        assert!(
            store
                .create_if_absent(&record)
                .expect("first scanner insert")
        );
        assert!(
            !store
                .create_if_absent(&record)
                .expect("duplicate scanner insert")
        );
        assert_eq!(store.count().expect("count"), 1);
        drop(store);
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(path.with_extension("sqlite3-wal"));
        let _ = std::fs::remove_file(path.with_extension("sqlite3-shm"));
    }

    #[test]
    fn automatic_policy_is_exact_low_risk_and_only_queues() {
        let (store, path) = test_store();
        store
            .upsert_policy(&super::AutomationPolicyRecord {
                id: "kubectl-dev-read".into(),
                adapter: "kubernetes".into(),
                action: "review_kubectl_observation".into(),
                scope: r#"{"context":"development"}"#.into(),
                mode: "automatic".into(),
                risk_ceiling: "low".into(),
            })
            .expect("store policy");
        let mut matching = proposal();
        matching.adapter = "kubernetes".into();
        matching.action = "review_kubectl_observation".into();
        matching.risk = "low".into();
        matching.evidence_json = r#"{"context":"development","arguments":["get","pods"]}"#.into();
        assert_eq!(
            store
                .apply_automatic_policy(&mut matching)
                .expect("evaluate policy"),
            Some("kubectl-dev-read".into())
        );
        assert_eq!(matching.state, "queued");

        let mut wrong_context = matching.clone();
        wrong_context.state = "awaiting_review".into();
        wrong_context.evidence_json = r#"{"context":"production"}"#.into();
        assert!(
            store
                .apply_automatic_policy(&mut wrong_context)
                .expect("evaluate policy")
                .is_none()
        );
        assert_eq!(wrong_context.state, "awaiting_review");
        drop(store);
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(path.with_extension("sqlite3-wal"));
        let _ = std::fs::remove_file(path.with_extension("sqlite3-shm"));
    }
}

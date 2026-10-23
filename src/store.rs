use std::path::Path;

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension};
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

#[derive(Debug, Clone, PartialEq)]
pub struct AnalysisRunRecord {
    pub id: String,
    pub proposal_id: String,
    pub profile_id: String,
    pub state: String,
    pub provider: String,
    pub model: String,
    pub config_hash: String,
    pub prompt_hash: String,
    pub prompt_snapshot: String,
    pub tool_trace_json: String,
    pub response_json: String,
    pub finding: String,
    pub priority: String,
    pub confidence: f64,
    pub rationale: String,
    pub next_step: String,
    pub error: String,
    pub created_at: String,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AnalysisSummaryRecord {
    pub id: String,
    pub state: String,
    pub profile_id: String,
    pub provider: String,
    pub model: String,
    pub finding: String,
    pub priority: String,
    pub confidence: f64,
    pub rationale: String,
    pub next_step: String,
    pub error: String,
    pub created_at: String,
    pub completed_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvestigationRecord {
    pub id: String,
    pub proposal_id: String,
    pub jsonl_path: String,
    pub active_provider: String,
    pub active_model: String,
    pub status: String,
    pub is_primary: bool,
    pub latest_remediation_json: String,
    pub created_at: String,
    pub updated_at: String,
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
            CREATE TABLE IF NOT EXISTS analysis_runs (
              id TEXT PRIMARY KEY,
              proposal_id TEXT NOT NULL,
              profile_id TEXT NOT NULL,
              state TEXT NOT NULL,
              provider TEXT NOT NULL,
              model TEXT NOT NULL,
              config_hash TEXT NOT NULL,
              prompt_hash TEXT NOT NULL,
              prompt_snapshot TEXT NOT NULL,
              tool_trace_json TEXT NOT NULL,
              response_json TEXT NOT NULL,
              finding TEXT NOT NULL,
              priority TEXT NOT NULL,
              confidence REAL NOT NULL,
              rationale TEXT NOT NULL,
              next_step TEXT NOT NULL,
              error TEXT NOT NULL,
              created_at TEXT NOT NULL,
              started_at TEXT,
              completed_at TEXT
            );
            CREATE INDEX IF NOT EXISTS analysis_runs_proposal_created
              ON analysis_runs(proposal_id, created_at DESC);
            CREATE TABLE IF NOT EXISTS investigation_sessions (
              id TEXT PRIMARY KEY,
              proposal_id TEXT NOT NULL,
              jsonl_path TEXT NOT NULL UNIQUE,
              active_provider TEXT NOT NULL,
              active_model TEXT NOT NULL,
              status TEXT NOT NULL,
              is_primary INTEGER NOT NULL DEFAULT 1,
              latest_remediation_json TEXT NOT NULL DEFAULT '',
              created_at TEXT NOT NULL,
              updated_at TEXT NOT NULL
            );
            CREATE UNIQUE INDEX IF NOT EXISTS investigation_primary_proposal
              ON investigation_sessions(proposal_id) WHERE is_primary = 1;
            ",
        )?;
        Ok(Self { connection })
    }

    pub fn count(&self) -> Result<i64> {
        self.connection
            .query_row("SELECT COUNT(*) FROM proposals", [], |row| row.get(0))
            .context("count proposals")
    }

    pub fn get_proposal(&self, id: &str) -> Result<ProposalRecord> {
        self.connection
            .query_row(
                "SELECT id, adapter, action, state, risk, evidence_json, preview, rollback, idempotency_key, expires_at_unix, created_at, updated_at FROM proposals WHERE id = ?1",
                [id],
                |row| {
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
                },
            )
            .context("load proposal")
    }

    pub fn get_proposal_by_idempotency_key(&self, key: &str) -> Result<ProposalRecord> {
        self.connection
            .query_row(
                "SELECT id, adapter, action, state, risk, evidence_json, preview, rollback, idempotency_key, expires_at_unix, created_at, updated_at FROM proposals WHERE idempotency_key = ?1",
                [key],
                |row| {
                    Ok(ProposalRecord {
                        id: row.get(0)?, adapter: row.get(1)?, action: row.get(2)?,
                        state: row.get(3)?, risk: row.get(4)?, evidence_json: row.get(5)?,
                        preview: row.get(6)?, rollback: row.get(7)?, idempotency_key: row.get(8)?,
                        expires_at_unix: row.get(9)?, created_at: row.get(10)?, updated_at: row.get(11)?,
                    })
                },
            )
            .context("load proposal by idempotency key")
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
        self.list_for_adapter(state, None)
    }

    pub fn list_for_adapter(
        &self,
        state: Option<&str>,
        adapter: Option<&str>,
    ) -> Result<Vec<ProposalRecord>> {
        let mut statement = self.connection.prepare(
            "SELECT id, adapter, action, state, risk, evidence_json, preview, rollback, idempotency_key, expires_at_unix, created_at, updated_at
             FROM proposals
             WHERE (?1 IS NULL OR state = ?1)
               AND (?2 IS NULL OR adapter = ?2)
             ORDER BY updated_at DESC",
        )?;
        let rows = statement.query_map(rusqlite::params![state, adapter], |row| {
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

    pub fn create_analysis(&self, run: &AnalysisRunRecord) -> Result<()> {
        self.connection.execute(
            "INSERT INTO analysis_runs (id, proposal_id, profile_id, state, provider, model, config_hash, prompt_hash, prompt_snapshot, tool_trace_json, response_json, finding, priority, confidence, rationale, next_step, error, created_at, started_at, completed_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20)",
            rusqlite::params![
                run.id, run.proposal_id, run.profile_id, run.state, run.provider, run.model,
                run.config_hash, run.prompt_hash, run.prompt_snapshot, run.tool_trace_json,
                run.response_json, run.finding, run.priority, run.confidence, run.rationale,
                run.next_step, run.error, run.created_at, run.started_at, run.completed_at,
            ],
        )
        .context("create analysis run")?;
        Ok(())
    }

    pub fn update_analysis(&self, run: &AnalysisRunRecord) -> Result<()> {
        self.connection.execute(
            "UPDATE analysis_runs SET state = ?1, provider = ?2, model = ?3, config_hash = ?4, prompt_hash = ?5, prompt_snapshot = ?6, tool_trace_json = ?7, response_json = ?8, finding = ?9, priority = ?10, confidence = ?11, rationale = ?12, next_step = ?13, error = ?14, started_at = ?15, completed_at = ?16 WHERE id = ?17",
            rusqlite::params![
                run.state, run.provider, run.model, run.config_hash, run.prompt_hash,
                run.prompt_snapshot, run.tool_trace_json, run.response_json, run.finding,
                run.priority, run.confidence, run.rationale, run.next_step, run.error,
                run.started_at, run.completed_at, run.id,
            ],
        )
        .context("update analysis run")?;
        Ok(())
    }

    pub fn get_analysis(&self, id: &str) -> Result<AnalysisRunRecord> {
        self.connection
            .query_row(
                "SELECT id, proposal_id, profile_id, state, provider, model, config_hash, prompt_hash, prompt_snapshot, tool_trace_json, response_json, finding, priority, confidence, rationale, next_step, error, created_at, started_at, completed_at FROM analysis_runs WHERE id = ?1",
                [id],
                analysis_from_row,
            )
            .context("load analysis run")
    }

    pub fn list_analyses(&self, proposal_id: Option<&str>) -> Result<Vec<AnalysisRunRecord>> {
        let mut statement = self.connection.prepare(
            "SELECT id, proposal_id, profile_id, state, provider, model, config_hash, prompt_hash, prompt_snapshot, tool_trace_json, response_json, finding, priority, confidence, rationale, next_step, error, created_at, started_at, completed_at FROM analysis_runs WHERE (?1 IS NULL OR proposal_id = ?1) ORDER BY created_at DESC",
        )?;
        let rows = statement.query_map([proposal_id], analysis_from_row)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn latest_analysis(&self, proposal_id: &str) -> Result<Option<AnalysisSummaryRecord>> {
        self.connection
            .query_row(
                "SELECT id, state, profile_id, provider, model, finding, priority, confidence, rationale, next_step, error, created_at, completed_at FROM analysis_runs WHERE proposal_id = ?1 ORDER BY created_at DESC LIMIT 1",
                [proposal_id],
                |row| {
                    Ok(AnalysisSummaryRecord {
                        id: row.get(0)?, state: row.get(1)?, profile_id: row.get(2)?,
                        provider: row.get(3)?, model: row.get(4)?, finding: row.get(5)?,
                        priority: row.get(6)?, confidence: row.get(7)?, rationale: row.get(8)?,
                        next_step: row.get(9)?, error: row.get(10)?, created_at: row.get(11)?,
                        completed_at: row.get(12)?,
                    })
                },
            )
            .optional()
            .context("load latest analysis")
    }

    pub fn recover_running_analyses(&self, completed_at: &str) -> Result<()> {
        self.connection.execute(
            "UPDATE analysis_runs SET state = 'failed', error = 'daemon restarted while analysis was running', completed_at = ?1 WHERE state IN ('queued', 'running')",
            [completed_at],
        )?;
        Ok(())
    }

    pub fn create_investigation(&self, record: &InvestigationRecord) -> Result<()> {
        self.connection.execute(
            "INSERT INTO investigation_sessions (id, proposal_id, jsonl_path, active_provider, active_model, status, is_primary, latest_remediation_json, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            rusqlite::params![
                record.id,
                record.proposal_id,
                record.jsonl_path,
                record.active_provider,
                record.active_model,
                record.status,
                record.is_primary,
                record.latest_remediation_json,
                record.created_at,
                record.updated_at,
            ],
        )?;
        Ok(())
    }

    pub fn get_investigation(&self, id: &str) -> Result<InvestigationRecord> {
        self.connection
            .query_row(
                "SELECT id, proposal_id, jsonl_path, active_provider, active_model, status, is_primary, latest_remediation_json, created_at, updated_at FROM investigation_sessions WHERE id = ?1",
                [id],
                investigation_from_row,
            )
            .context("load investigation")
    }

    pub fn list_investigations(&self) -> Result<Vec<InvestigationRecord>> {
        let mut statement = self.connection.prepare(
            "SELECT id, proposal_id, jsonl_path, active_provider, active_model, status, is_primary, latest_remediation_json, created_at, updated_at FROM investigation_sessions ORDER BY updated_at DESC",
        )?;
        let rows = statement.query_map([], investigation_from_row)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn primary_investigation(&self, proposal_id: &str) -> Result<Option<InvestigationRecord>> {
        self.connection
            .query_row(
                "SELECT id, proposal_id, jsonl_path, active_provider, active_model, status, is_primary, latest_remediation_json, created_at, updated_at FROM investigation_sessions WHERE proposal_id = ?1 AND is_primary = 1",
                [proposal_id],
                investigation_from_row,
            )
            .optional()
            .context("load primary investigation")
    }

    pub fn set_investigation_model(
        &self,
        id: &str,
        provider: &str,
        model: &str,
        updated_at: &str,
    ) -> Result<()> {
        let changed = self.connection.execute(
            "UPDATE investigation_sessions SET active_provider = ?1, active_model = ?2, updated_at = ?3 WHERE id = ?4 AND status != 'running'",
            rusqlite::params![provider, model, updated_at, id],
        )?;
        if changed != 1 {
            anyhow::bail!("investigation is missing or currently running");
        }
        Ok(())
    }

    pub fn set_investigation_status(&self, id: &str, status: &str, updated_at: &str) -> Result<()> {
        self.connection.execute(
            "UPDATE investigation_sessions SET status = ?1, updated_at = ?2 WHERE id = ?3",
            rusqlite::params![status, updated_at, id],
        )?;
        Ok(())
    }

    pub fn save_remediation_draft(
        &self,
        id: &str,
        draft_json: &str,
        updated_at: &str,
    ) -> Result<()> {
        self.connection.execute(
            "UPDATE investigation_sessions SET latest_remediation_json = ?1, updated_at = ?2 WHERE id = ?3",
            rusqlite::params![draft_json, updated_at, id],
        )?;
        Ok(())
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

fn analysis_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<AnalysisRunRecord> {
    Ok(AnalysisRunRecord {
        id: row.get(0)?,
        proposal_id: row.get(1)?,
        profile_id: row.get(2)?,
        state: row.get(3)?,
        provider: row.get(4)?,
        model: row.get(5)?,
        config_hash: row.get(6)?,
        prompt_hash: row.get(7)?,
        prompt_snapshot: row.get(8)?,
        tool_trace_json: row.get(9)?,
        response_json: row.get(10)?,
        finding: row.get(11)?,
        priority: row.get(12)?,
        confidence: row.get(13)?,
        rationale: row.get(14)?,
        next_step: row.get(15)?,
        error: row.get(16)?,
        created_at: row.get(17)?,
        started_at: row.get(18)?,
        completed_at: row.get(19)?,
    })
}

fn investigation_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<InvestigationRecord> {
    Ok(InvestigationRecord {
        id: row.get(0)?,
        proposal_id: row.get(1)?,
        jsonl_path: row.get(2)?,
        active_provider: row.get(3)?,
        active_model: row.get(4)?,
        status: row.get(5)?,
        is_primary: row.get(6)?,
        latest_remediation_json: row.get(7)?,
        created_at: row.get(8)?,
        updated_at: row.get(9)?,
    })
}

#[cfg(test)]
mod tests {
    use super::{AnalysisRunRecord, ProposalRecord, Store};
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

    fn analysis_run(id: &str, state: &str) -> AnalysisRunRecord {
        AnalysisRunRecord {
            id: id.into(),
            proposal_id: "proposal-1".into(),
            profile_id: "review-1".into(),
            state: state.into(),
            provider: "codex".into(),
            model: "default".into(),
            config_hash: "config-hash".into(),
            prompt_hash: "prompt-hash".into(),
            prompt_snapshot: "review this".into(),
            tool_trace_json: "[]".into(),
            response_json: "{}".into(),
            finding: "unknown".into(),
            priority: "normal".into(),
            confidence: 0.5,
            rationale: "needs a human".into(),
            next_step: "human_review".into(),
            error: String::new(),
            created_at: id.into(),
            started_at: None,
            completed_at: None,
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
    fn adapter_filter_is_exact_and_combines_with_state() {
        let (store, path) = test_store();
        store
            .create(&proposal())
            .expect("create opensnitch proposal");
        let mut other = proposal();
        other.id = "registry-proposal".into();
        other.idempotency_key = "registry-request".into();
        other.adapter = "registrygate".into();
        store.create(&other).expect("create registry proposal");
        assert_eq!(store.list(None).unwrap().len(), 2);
        let filtered = store.list_for_adapter(None, Some("registrygate")).unwrap();
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].id, other.id);
        assert_eq!(
            store
                .list_for_adapter(Some("awaiting_review"), Some("registrygate"))
                .unwrap()
                .len(),
            1
        );
        assert!(
            store
                .list_for_adapter(Some("approved"), Some("registrygate"))
                .unwrap()
                .is_empty()
        );
        assert!(
            store
                .list_for_adapter(None, Some("registry"))
                .unwrap()
                .is_empty()
        );
        assert!(
            store
                .list_for_adapter(None, Some("' OR 1=1 --"))
                .unwrap()
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

    #[test]
    fn analysis_runs_are_durable_and_recoverable() {
        let (store, path) = test_store();
        store.create(&proposal()).expect("create proposal");
        let mut run = analysis_run("1", "queued");
        store.create_analysis(&run).expect("create analysis");
        assert_eq!(store.get_analysis("1").expect("load analysis"), run);
        assert_eq!(
            store
                .latest_analysis("proposal-1")
                .expect("latest")
                .expect("summary")
                .state,
            "queued"
        );

        run.state = "running".into();
        run.started_at = Some("2".into());
        store.update_analysis(&run).expect("update analysis");
        store
            .recover_running_analyses("3")
            .expect("recover analyses");
        let recovered = store.get_analysis("1").expect("load recovered analysis");
        assert_eq!(recovered.state, "failed");
        assert_eq!(recovered.completed_at.as_deref(), Some("3"));
        assert!(recovered.error.contains("daemon restarted"));
        drop(store);
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(path.with_extension("sqlite3-wal"));
        let _ = std::fs::remove_file(path.with_extension("sqlite3-shm"));
    }
}

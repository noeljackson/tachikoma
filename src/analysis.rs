//! Durable scheduling and execution of prompt-driven proposal analyses.

use anyhow::{Context, Result};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::{Semaphore, mpsc, watch};

use crate::{
    ai::{self, AnalysisOutcome, Redactor},
    config::{AiConfig, AiReviewConfig, ResolvedConfig},
    store::{AnalysisRunRecord, ProposalRecord, Store},
};

const QUEUE_CAPACITY: usize = 256;

#[derive(Clone)]
pub struct AnalysisScheduler {
    queue: mpsc::Sender<AnalysisJob>,
    store: Arc<Mutex<Store>>,
    config: watch::Receiver<ResolvedConfig>,
}

struct AnalysisJob {
    run_id: String,
    proposal: ProposalRecord,
    review: AiReviewConfig,
    ai: AiConfig,
}

impl AnalysisScheduler {
    pub fn new(store: Arc<Mutex<Store>>, config: watch::Receiver<ResolvedConfig>) -> Self {
        let (queue, mut receiver) = mpsc::channel::<AnalysisJob>(QUEUE_CAPACITY);
        let worker_store = Arc::clone(&store);
        let permits = Arc::new(Semaphore::new(8));
        tokio::spawn({
            let permits = Arc::clone(&permits);
            async move {
                while let Some(job) = receiver.recv().await {
                    let concurrency = job.ai.max_concurrency.clamp(1, 8);
                    let permit_count = 8_usize.div_ceil(concurrency).max(1) as u32;
                    let permit = match permits.clone().acquire_many_owned(permit_count).await {
                        Ok(permit) => permit,
                        Err(_) => break,
                    };
                    let store = Arc::clone(&worker_store);
                    tokio::spawn(async move {
                        run_job(store, job).await;
                        drop(permit);
                    });
                }
            }
        });
        Self {
            queue,
            store,
            config,
        }
    }

    pub async fn schedule_automatic(&self, proposal_id: &str) -> Result<Vec<AnalysisRunRecord>> {
        self.schedule(proposal_id, None, false, true).await
    }

    pub async fn schedule_manual(
        &self,
        proposal_id: &str,
        profile_id: Option<&str>,
        force: bool,
    ) -> Result<AnalysisRunRecord> {
        let runs = self.schedule(proposal_id, profile_id, force, false).await?;
        runs.into_iter()
            .next()
            .context("no matching AI review profile")
    }

    async fn schedule(
        &self,
        proposal_id: &str,
        profile_id: Option<&str>,
        force: bool,
        automatic_only: bool,
    ) -> Result<Vec<AnalysisRunRecord>> {
        let config = self.config.borrow().clone();
        if !config.ai.enabled {
            anyhow::bail!("AI analysis is disabled");
        }
        let proposal = self
            .store
            .lock()
            .map_err(|_| anyhow::anyhow!("proposal store lock poisoned"))?
            .get_proposal(proposal_id)?;
        let mut reviews = config
            .ai
            .reviews
            .iter()
            .filter(|review| {
                review.adapter == proposal.adapter
                    && review.action == proposal.action
                    && (!automatic_only || review.automatic)
                    && profile_id.is_none_or(|profile| review.id == profile)
            })
            .cloned()
            .collect::<Vec<_>>();
        if !automatic_only && profile_id.is_none() && reviews.len() > 1 {
            anyhow::bail!("multiple AI review profiles match; specify --profile");
        }
        if reviews.is_empty() {
            anyhow::bail!("no AI review profile matches this proposal");
        }
        let mut runs = Vec::with_capacity(reviews.len());
        for review in reviews.drain(..) {
            let config_hash = ai::hash_text(&serde_json::to_string(&(&config.ai, &review))?);
            let existing = self
                .store
                .lock()
                .map_err(|_| anyhow::anyhow!("proposal store lock poisoned"))?
                .list_analyses(Some(proposal_id))?
                .into_iter()
                .find(|run| {
                    run.profile_id == review.id
                        && ((run.state == "queued" || run.state == "running")
                            || (!force && run.config_hash == config_hash))
                });
            if let Some(existing) = existing {
                runs.push(existing);
                continue;
            }
            let run = AnalysisRunRecord {
                id: new_analysis_id(),
                proposal_id: proposal_id.to_owned(),
                profile_id: review.id.clone(),
                state: "queued".into(),
                provider: review.provider.clone(),
                model: config
                    .ai
                    .providers
                    .get(&review.provider)
                    .map(|provider| provider.model.clone())
                    .unwrap_or_default(),
                config_hash,
                prompt_hash: ai::hash_text(&review.prompt),
                prompt_snapshot: review.prompt.clone(),
                tool_trace_json: "[]".into(),
                response_json: "".into(),
                finding: "".into(),
                priority: "".into(),
                confidence: 0.0,
                rationale: "".into(),
                next_step: "".into(),
                error: "".into(),
                created_at: now_nanos(),
                started_at: None,
                completed_at: None,
            };
            self.store
                .lock()
                .map_err(|_| anyhow::anyhow!("proposal store lock poisoned"))?
                .create_analysis(&run)?;
            let job = AnalysisJob {
                run_id: run.id.clone(),
                proposal: proposal.clone(),
                review,
                ai: config.ai.clone(),
            };
            if let Err(error) = self.queue.send(job).await {
                let mut failed = run.clone();
                failed.state = "failed".into();
                failed.error = format!("analysis queue unavailable: {error}");
                failed.completed_at = Some(now());
                self.store
                    .lock()
                    .map_err(|_| anyhow::anyhow!("proposal store lock poisoned"))?
                    .update_analysis(&failed)?;
                return Err(anyhow::anyhow!("queue AI analysis"));
            }
            runs.push(run);
        }
        Ok(runs)
    }
}

async fn run_job(store: Arc<Mutex<Store>>, job: AnalysisJob) {
    let mut run = match store
        .lock()
        .ok()
        .and_then(|store| store.get_analysis(&job.run_id).ok())
    {
        Some(run) => run,
        None => return,
    };
    run.state = "running".into();
    run.started_at = Some(now());
    if let Ok(store) = store.lock() {
        let _ = store.update_analysis(&run);
    }
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(job.ai.request_timeout_seconds),
        ai::run_investigation(&job.ai, &job.review, &job.proposal),
    )
    .await;
    match result {
        Ok(Ok(outcome)) => complete_success(&store, &mut run, outcome),
        Ok(Err(error)) => {
            complete_failure(&store, &mut run, redact_error(&job.ai, &error.to_string()))
        }
        Err(_) => complete_failure(&store, &mut run, "AI analysis request timed out".into()),
    }
}

fn complete_success(
    store: &Arc<Mutex<Store>>,
    run: &mut AnalysisRunRecord,
    outcome: AnalysisOutcome,
) {
    run.state = "completed".into();
    run.provider = outcome.provider;
    run.model = outcome.model;
    run.prompt_hash = outcome.prompt_hash;
    run.prompt_snapshot = outcome.prompt_snapshot;
    run.tool_trace_json = outcome.tool_trace_json;
    run.response_json = outcome.response_json;
    run.finding = outcome.result.finding;
    run.priority = outcome.result.priority;
    run.confidence = outcome.result.confidence;
    run.rationale = outcome.result.rationale;
    run.next_step = outcome.result.next_step;
    run.completed_at = Some(now());
    if let Ok(store) = store.lock() {
        let _ = store.update_analysis(run);
    }
}

fn complete_failure(store: &Arc<Mutex<Store>>, run: &mut AnalysisRunRecord, error: String) {
    run.state = "failed".into();
    run.error = error.chars().take(4096).collect();
    run.completed_at = Some(now());
    if let Ok(store) = store.lock() {
        let _ = store.update_analysis(run);
    }
}

fn redact_error(config: &AiConfig, error: &str) -> String {
    Redactor::new(&config.redactions)
        .map(|redactor| redactor.redact(error))
        .unwrap_or_else(|_| error.to_owned())
}

fn new_analysis_id() -> String {
    let nanos = analysis_nanos();
    format!("a-{nanos:x}")
}

fn now_nanos() -> String {
    analysis_nanos().to_string()
}

fn analysis_nanos() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
}

fn now() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .to_string()
}

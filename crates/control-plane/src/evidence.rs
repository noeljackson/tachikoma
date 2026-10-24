//! Optional evidence publishers and deterministic shadow policies. Never settles a request.
use super::*;
use serde_json::{Value, json as value};
use std::collections::{BTreeMap, BTreeSet};

fn pointer(path: &str) -> bool {
    path.len() <= 256 && path.starts_with('/') && !path.chars().any(char::is_control)
}
fn context_pointer(path: &str) -> bool {
    pointer(path)
        && ["/subject/", "/evidence/", "/assessment/"]
            .iter()
            .any(|p| path.starts_with(p))
}
fn valid_condition(condition: &Condition, depth: usize, count: &mut usize) -> bool {
    *count += 1;
    if depth > 6 || *count > 256 {
        return false;
    }
    match condition {
        Condition::All { conditions } | Condition::Any { conditions } => {
            !conditions.is_empty()
                && conditions.len() <= 16
                && conditions
                    .iter()
                    .all(|c| valid_condition(c, depth + 1, count))
        }
        Condition::Equals { path, value } => {
            context_pointer(path)
                && !value.is_null()
                && serde_json::to_vec(value).is_ok_and(|v| v.len() <= 1024)
        }
        Condition::AtLeast { path, value } => context_pointer(path) && value.is_finite(),
        Condition::Observed {
            source,
            capability,
            within_ms,
            joins,
            equals,
        } => {
            identifier(source)
                && identifier(capability)
                && (1..=3_600_000).contains(within_ms)
                && !joins.is_empty()
                && joins.len() <= 6
                && joins
                    .iter()
                    .all(|(event, context)| pointer(event) && context_pointer(context))
                && equals.len() <= 8
                && equals.iter().all(|(p, v)| {
                    pointer(p)
                        && !v.is_null()
                        && serde_json::to_vec(v).is_ok_and(|v| v.len() <= 1024)
                })
        }
    }
}
fn trace(state: MatchState, reason: impl Into<String>) -> RuleTrace {
    RuleTrace {
        rule: String::new(),
        state,
        reason: reason.into(),
        observations: vec![],
    }
}

// Advisory policy snapshots contain decimal thresholds/model scores. They are
// separate from immutable wire facts, whose interoperable integer-only hash
// contract must remain unchanged.
fn snapshot_fingerprint<T: Serialize>(input: &T) -> Result<String> {
    fn sorted(value: Value) -> Value {
        match value {
            Value::Object(object) => {
                let fields: BTreeMap<_, _> = object.into_iter().collect();
                Value::Object(fields.into_iter().map(|(k, v)| (k, sorted(v))).collect())
            }
            Value::Array(array) => Value::Array(array.into_iter().map(sorted).collect()),
            other => other,
        }
    }
    Ok(digest(&serde_json::to_vec(&sorted(serde_json::to_value(
        input,
    )?))?))
}

impl Store {
    fn source_info(&self, id: &str) -> Result<SourceInfo> {
        let row:Option<(String,i64,i64,bool,String,String)>=self.db.query_row("SELECT spec,revision,last_seen,ready,detail,writer FROM evidence_sources WHERE id=?1",[id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?))).optional()?;
        let (body, revision, last_seen_ms, ready, detail, writer) =
            row.ok_or_else(Fault::missing)?;
        let spec: SourceSpec = decode(&body)?;
        let valid_writer:bool=self.db.query_row("SELECT EXISTS(SELECT 1 FROM principals WHERE id=?1 AND id NOT IN(SELECT id FROM revoked_principals) AND json_extract(spec,'$.role')='source' AND json_extract(spec,'$.realm')=?2)",params![writer,id],|r|r.get(0))?;
        let fresh = last_seen_ms > 0
            && last_seen_ms <= now_ms()
            && now_ms() - last_seen_ms <= spec.max_age_ms;
        let detail = if !spec.enabled {
            "Disabled by configuration".into()
        } else if !valid_writer {
            "No active publisher identity".into()
        } else if !fresh {
            "Publisher heartbeat missing or stale".into()
        } else {
            detail
        };
        Ok(SourceInfo {
            ready: spec.enabled && ready && fresh && valid_writer,
            spec,
            revision,
            last_seen_ms,
            detail,
        })
    }
    pub fn sources(&self, actor: &Principal) -> Result<Vec<SourceInfo>> {
        let ids = self
            .db
            .prepare("SELECT id FROM evidence_sources ORDER BY id")?
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let mut result = Vec::new();
        for id in ids {
            let info = self.source_info(&id)?;
            if (actor.spec.role != Role::Source || actor.spec.realm == id)
                && info.spec.queues.iter().any(|q| actor.queue(q))
            {
                result.push(info);
            }
        }
        Ok(result)
    }
    pub fn put_source(&mut self, actor: &Principal, spec: SourceSpec) -> Result<SourceInfo> {
        actor.require(Role::Admin)?;
        if !identifier(&spec.id)
            || !identifier(&spec.adapter)
            || spec.title.is_empty()
            || spec.title.len() > 128
            || spec.coverage.len() > 512
            || spec.queues.is_empty()
            || spec.queues.len() > 64
            || spec.capabilities.is_empty()
            || spec.capabilities.len() > 16
            || !(1000..=300_000).contains(&spec.max_age_ms)
            || spec
                .capabilities
                .iter()
                .any(|(id, shape)| !identifier(id) || !shape.valid())
            || json(&spec)?.len() > 32768
        {
            return Err(Fault::bad());
        }
        for queue in &spec.queues {
            self.manifest(queue)?;
        }
        let previous: Option<String> = self
            .db
            .query_row(
                "SELECT spec FROM evidence_sources WHERE id=?1",
                [&spec.id],
                |r| r.get(0),
            )
            .optional()?;
        let body = json(&spec)?;
        if previous.as_deref() == Some(&body) {
            return self.source_info(&spec.id);
        }
        let count: i64 = self
            .db
            .query_row("SELECT count(*) FROM evidence_sources", [], |r| r.get(0))?;
        if previous.is_none() && count >= 128 {
            return Err(Fault::conflict());
        }
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute("INSERT INTO evidence_sources VALUES(?1,1,?2,0,0,'','') ON CONFLICT(id) DO UPDATE SET revision=revision+1,spec=excluded.spec,last_seen=0,ready=0,detail='',writer=''",params![spec.id,body])?;
        tx.execute("INSERT INTO evidence_source_revisions SELECT id,revision,spec,?1 FROM evidence_sources WHERE id=?2",params![now_ms(),spec.id])?;
        tx.execute("INSERT INTO administrative_events(actor,kind,subject,at) VALUES(?1,'source_configured',?2,?3)",params![actor.spec.id,spec.id,now_ms()])?;
        tx.commit()?;
        self.source_info(&spec.id)
    }
    fn publisher(&self, actor: &Principal, id: &str) -> Result<SourceInfo> {
        if actor.spec.role != Role::Source || actor.spec.realm != id {
            return Err(Fault::forbidden());
        }
        let info = self.source_info(id)?;
        if !info.spec.enabled || !info.spec.queues.iter().any(|q| actor.queue(q)) {
            return Err(Fault::forbidden());
        }
        Ok(info)
    }
    pub fn source_heartbeat(
        &mut self,
        actor: &Principal,
        id: &str,
        heartbeat: SourceHeartbeat,
    ) -> Result<SourceInfo> {
        self.publisher(actor, id)?;
        if heartbeat.detail.len() > 256 || heartbeat.detail.chars().any(char::is_control) {
            return Err(Fault::bad());
        }
        self.db.execute(
            "UPDATE evidence_sources SET last_seen=?1,ready=?2,detail=?3,writer=?4 WHERE id=?5",
            params![
                now_ms(),
                heartbeat.ready,
                heartbeat.detail,
                actor.spec.id,
                id
            ],
        )?;
        self.source_info(id)
    }
    pub fn publish_observation(
        &mut self,
        actor: &Principal,
        id: &str,
        observation: Observation,
    ) -> Result<ObservationReceipt> {
        let source = self.publisher(actor, id)?;
        let now = now_ms();
        if !identifier(&observation.id)
            || !actor.queue(&observation.queue)
            || !source.spec.queues.contains(&observation.queue)
            || observation.observed_at_ms < now - 3_600_000
            || observation.observed_at_ms > now + 2000
            || json(&observation.data)?.len() > 8192
            || source
                .spec
                .capabilities
                .get(&observation.capability)
                .is_none_or(|shape| !shape.accepts(&observation.data))
        {
            return Err(Fault::bad());
        }
        let hash = fingerprint(&observation)?;
        let old:Option<String>=self.db.query_row("SELECT digest FROM evidence_observations WHERE source=?1 AND revision=?2 AND id=?3",params![id,source.revision,observation.id],|r|r.get(0)).optional()?;
        if let Some(old) = old {
            if old != hash {
                return Err(Fault::conflict());
            }
        } else {
            let tx = self
                .db
                .transaction_with_behavior(TransactionBehavior::Immediate)?;
            tx.execute("INSERT INTO evidence_observations(source,revision,id,queue,capability,observed,received,digest,body,writer) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",params![id,source.revision,observation.id,observation.queue,observation.capability,observation.observed_at_ms,now,hash,json(&observation)?,actor.spec.id])?;
            tx.execute("DELETE FROM evidence_observations WHERE received<?1 OR seq NOT IN(SELECT seq FROM evidence_observations ORDER BY seq DESC LIMIT 10000)",[now-3_600_000])?;
            tx.commit()?;
        }
        Ok(ObservationReceipt {
            id: observation.id,
            source_revision: source.revision,
            digest: hash,
        })
    }
    pub fn evidence_policy(
        &self,
        actor: &Principal,
        queue: &str,
    ) -> Result<Option<EvidencePolicyInfo>> {
        if actor.spec.role == Role::Source || !actor.queue(queue) {
            return Err(Fault::forbidden());
        }
        self.db
            .query_row(
                "SELECT revision,body FROM evidence_policies WHERE queue=?1",
                [queue],
                |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)),
            )
            .optional()?
            .map(|(revision, body)| {
                Ok(EvidencePolicyInfo {
                    revision,
                    policy: decode(&body)?,
                })
            })
            .transpose()
    }
    pub fn put_evidence_policy(
        &mut self,
        actor: &Principal,
        policy: EvidencePolicy,
    ) -> Result<EvidencePolicyInfo> {
        actor.require(Role::Admin)?;
        let (_, manifest) = self.manifest(&policy.queue)?;
        let outcome_valid = |outcome: &PolicyOutcome| match outcome {
            PolicyOutcome::Ask => true,
            PolicyOutcome::Action { action_id } => {
                manifest.actions.iter().any(|a| a.id == *action_id)
            }
        };
        let mut ids = BTreeSet::new();
        let mut node_count = 0;
        if policy.rules.len() > 32
            || !outcome_valid(&policy.default)
            || json(&policy)?.len() > 32768
            || policy.rules.iter().any(|r| {
                !identifier(&r.id)
                    || !ids.insert(&r.id)
                    || r.description.len() > 512
                    || r.requires.len() > 16
                    || r.requires
                        .iter()
                        .any(|c| !identifier(&c.source) || !identifier(&c.capability))
                    || !outcome_valid(&r.then)
                    || !valid_condition(&r.when, 0, &mut node_count)
            })
        {
            return Err(Fault::bad());
        }
        if let Some(old) = self.evidence_policy(actor, &policy.queue)?
            && snapshot_fingerprint(&old.policy)? == snapshot_fingerprint(&policy)?
        {
            return Ok(old);
        }
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute("INSERT INTO evidence_policies VALUES(?1,1,?2) ON CONFLICT(queue) DO UPDATE SET revision=revision+1,body=excluded.body",params![policy.queue,json(&policy)?])?;
        let revision: i64 = tx.query_row(
            "SELECT revision FROM evidence_policies WHERE queue=?1",
            [&policy.queue],
            |r| r.get(0),
        )?;
        tx.execute(
            "INSERT INTO evidence_policy_revisions VALUES(?1,?2,?3)",
            params![policy.queue, revision, json(&policy)?],
        )?;
        tx.execute("INSERT INTO administrative_events(actor,kind,subject,at) VALUES(?1,'evidence_policy_configured',?2,?3)",params![actor.spec.id,policy.queue,now_ms()])?;
        tx.commit()?;
        Ok(EvidencePolicyInfo { revision, policy })
    }
    fn capability(
        &self,
        source: &str,
        capability: &str,
        queue: &str,
    ) -> std::result::Result<SourceInfo, String> {
        let info = self
            .source_info(source)
            .map_err(|_| format!("{source}: source not configured"))?;
        if !info.ready {
            return Err(format!("{source}: {}", info.detail));
        }
        let writer_allowed:bool=self.db.query_row("SELECT EXISTS(SELECT 1 FROM evidence_sources e JOIN principals p ON p.id=e.writer, json_each(p.spec,'$.queues') q WHERE e.id=?1 AND q.value=?2)",params![source,queue],|r|r.get(0)).map_err(|_|"Source identity unavailable".to_owned())?;
        if !writer_allowed {
            return Err(format!("{source}: publisher queue scope changed"));
        }
        if !info.spec.queues.iter().any(|q| q == queue)
            || !info.spec.capabilities.contains_key(capability)
        {
            return Err(format!(
                "{source}: capability {capability} unavailable for this queue"
            ));
        }
        Ok(info)
    }
    fn evaluate_condition(
        &self,
        c: &Condition,
        review: &Review,
        context: &Value,
    ) -> Result<RuleTrace> {
        Ok(match c {
            Condition::Equals { path, value } => {
                match context.pointer(path).filter(|v| !v.is_null()) {
                    None => trace(MatchState::Unknown, format!("Missing fact {path}")),
                    Some(actual) => trace(
                        if actual == value {
                            MatchState::Matched
                        } else {
                            MatchState::NotMatched
                        },
                        format!("Compared {path}"),
                    ),
                }
            }
            Condition::AtLeast { path, value } => {
                match context.pointer(path).and_then(Value::as_f64) {
                    None => trace(MatchState::Unknown, format!("Missing numeric fact {path}")),
                    Some(actual) => trace(
                        if actual >= *value {
                            MatchState::Matched
                        } else {
                            MatchState::NotMatched
                        },
                        format!("Compared numeric fact {path}"),
                    ),
                }
            }
            Condition::All { conditions } | Condition::Any { conditions } => {
                let all = matches!(c, Condition::All { .. });
                let mut result = trace(
                    if all {
                        MatchState::Matched
                    } else {
                        MatchState::NotMatched
                    },
                    String::new(),
                );
                for condition in conditions {
                    let current = self.evaluate_condition(condition, review, context)?;
                    if (all && current.state == MatchState::NotMatched)
                        || (!all && current.state == MatchState::Matched)
                    {
                        return Ok(current);
                    }
                    if current.state == MatchState::Unknown {
                        result.state = MatchState::Unknown;
                    }
                    if !result.reason.is_empty() {
                        result.reason.push_str("; ");
                    }
                    result.reason.push_str(&current.reason);
                    result.observations.extend(current.observations);
                }
                result
            }
            Condition::Observed {
                source,
                capability,
                within_ms,
                joins,
                equals,
            } => {
                let info = match self.capability(source, capability, &review.request.queue) {
                    Ok(info) => info,
                    Err(reason) => return Ok(trace(MatchState::Unknown, reason)),
                };
                let values: Option<BTreeMap<_, _>> = joins
                    .iter()
                    .map(|(event, request)| {
                        context
                            .pointer(request)
                            .filter(|v| {
                                v.as_i64().is_some()
                                    || v.as_str().is_some_and(|s| !s.trim().is_empty())
                            })
                            .map(|v| (event, v))
                    })
                    .collect();
                let Some(values) = values else {
                    return Ok(trace(
                        MatchState::Unknown,
                        "Correlation identity missing or non-scalar; no empty, PID-only or null fallback",
                    ));
                };
                let rows=self.db.prepare("SELECT body,digest FROM evidence_observations WHERE source=?1 AND revision=?2 AND queue=?3 AND capability=?4 AND observed>=?5 AND observed<=?6 AND writer NOT IN(SELECT id FROM revoked_principals) ORDER BY observed DESC LIMIT 129")?.query_map(params![source,info.revision,review.request.queue,capability,review.created_at_ms-within_ms,review.created_at_ms],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?)))?.collect::<std::result::Result<Vec<_>,_>>()?;
                let mut result = trace(
                    MatchState::Unknown,
                    "No matching prior observation in the bounded window; absence does not prove safety",
                );
                for (body, digest) in rows {
                    let event: Observation = decode(&body)?;
                    if values.iter().all(|(p, v)| {
                        event
                            .data
                            .pointer(p)
                            .is_some_and(|actual| !actual.is_null() && actual == *v)
                    }) && equals.iter().all(|(p, v)| event.data.pointer(p) == Some(v))
                    {
                        result = trace(
                            MatchState::Matched,
                            format!("{source}/{capability}: exact configured correlation matched"),
                        );
                        result.observations.push(ObservationRef {
                            source: source.clone(),
                            source_revision: info.revision,
                            id: event.id,
                            digest,
                            observed_at_ms: event.observed_at_ms,
                        });
                        break;
                    }
                }
                result
            }
        })
    }
    pub fn policy_preview(&mut self, actor: &Principal, id: &str) -> Result<PolicyPreview> {
        let review = self.get(actor, id)?;
        let policy = self.evidence_policy(actor, &review.request.queue)?;
        let context = value!({"subject":review.request.subject,"evidence":review.request.evidence,"assessment":review.assessment});
        let mut preview = PolicyPreview {
            request_id: id.into(),
            request_revision: review.revision,
            manifest_revision: review.manifest_revision,
            policy_revision: policy.as_ref().map(|p| p.revision).unwrap_or(0),
            evaluated_at_ms: now_ms(),
            authoritative: false,
            outcome: PolicyOutcome::Ask,
            reason: "No evidence policy configured; manual review".into(),
            snapshot_digest: String::new(),
            traces: vec![],
        };
        if let Some(policy) = policy {
            let mut ask = false;
            let mut candidates = BTreeSet::new();
            for rule in policy.policy.rules {
                let mut result = self.evaluate_condition(&rule.when, &review, &context)?;
                result.rule = rule.id;
                if result.state != MatchState::NotMatched {
                    for r in &rule.requires {
                        if let Err(reason) =
                            self.capability(&r.source, &r.capability, &review.request.queue)
                        {
                            result.state = MatchState::Unknown;
                            result.reason.push_str(&format!("; {reason}"));
                        }
                    }
                }
                if result.state == MatchState::Matched {
                    match rule.then {
                        PolicyOutcome::Ask => ask = true,
                        PolicyOutcome::Action { action_id } => {
                            candidates.insert(action_id);
                        }
                    }
                } else if result.state == MatchState::Unknown
                    && rule.on_unavailable == UnavailableBehavior::Ask
                {
                    ask = true;
                }
                preview.traces.push(result);
            }
            preview.outcome = if ask || candidates.len() > 1 {
                PolicyOutcome::Ask
            } else if let Some(action_id) = candidates.into_iter().next() {
                PolicyOutcome::Action { action_id }
            } else {
                policy.policy.default
            };
            preview.reason = if ask {
                "A matching rule or unavailable required evidence requires review"
            } else if matches!(preview.outcome, PolicyOutcome::Ask) {
                "Policy fallback or conflicting actions requires review"
            } else {
                "Shadow recommendation only; no decision was recorded"
            }
            .into();
            if let PolicyOutcome::Action { action_id } = &preview.outcome
                && !review.actions.iter().any(|a| a.id == *action_id)
            {
                preview.outcome = PolicyOutcome::Ask;
                preview.reason = "Recommended action is not available on this request".into();
            }
        }
        if review.state != ReviewState::Pending || review.expires_at_ms <= now_ms() {
            preview.outcome = PolicyOutcome::Ask;
            preview.reason = "Request settled or expired; preview cannot authorize it".into();
        }
        let sources = self.sources(actor)?;
        preview.snapshot_digest =
            snapshot_fingerprint(&value!({"request":review,"sources":sources,"preview":preview}))?;
        Ok(preview)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn setup() -> (Store, Principal, Principal, Principal, Review) {
        let mut store = Store::init(Connection::open_in_memory().unwrap()).unwrap();
        let admin = Principal {
            spec: PrincipalSpec {
                id: "admin".into(),
                role: Role::Admin,
                queues: vec![],
                service_kind: String::new(),
                realm: String::new(),
                actors: vec![],
            },
        };
        let manifest:QueueManifest=serde_json::from_value(value!({"id":"fixture.review","title":"Fixture","service_kind":"fixture","schema_id":"fixture.v1","subject_shape":{"type":"object","fields":{"entity":{"type":"string","max_len":128}},"required":["entity"]},"actions":[{"id":"allow-once","label":"Allow once","effect":"permit","scope":{"uses":1}}],"max_pending":16,"deadline_cap_ms":60000})).unwrap();
        store.put_queue(&admin, manifest).unwrap();
        let mut enroll = |id: &str, role, realm: &str| {
            let enrolled = store
                .enroll(
                    &admin,
                    PrincipalSpec {
                        id: id.into(),
                        role,
                        queues: vec!["fixture.review".into()],
                        service_kind: "fixture".into(),
                        realm: realm.into(),
                        actors: vec![],
                    },
                )
                .unwrap();
            store.authenticate(&enrolled.credential).unwrap()
        };
        let producer = enroll("producer", Role::Producer, "");
        let source = enroll("sensor", Role::Source, "host");
        let source_spec:SourceSpec=serde_json::from_value(value!({"id":"host","title":"Host","adapter":"custom","enabled":true,"queues":["fixture.review"],"capabilities":{"credential.open":{"type":"object","fields":{"entity":{"type":"string","max_len":128}},"required":["entity"]}},"max_age_ms":30000,"coverage":"Selected opens only"})).unwrap();
        store.put_source(&admin, source_spec).unwrap();
        store
            .source_heartbeat(
                &source,
                "host",
                SourceHeartbeat {
                    ready: true,
                    detail: "Ready".into(),
                },
            )
            .unwrap();
        store.publish_observation(&source, "host", event()).unwrap();
        let request = store
            .submit(
                &producer,
                SubmitRequest {
                    queue: "fixture.review".into(),
                    producer_request_id: "request".into(),
                    nonce: "nonce-long-enough-123456789".into(),
                    idempotency_key: "request".into(),
                    schema_id: "fixture.v1".into(),
                    subject: value!({"entity":"stable-identity-123"}),
                    evidence: value!({}),
                    action_ids: vec!["allow-once".into()],
                    expires_at_ms: now_ms() + 30000,
                    summary: "Fixture".into(),
                },
            )
            .unwrap();
        (store, admin, producer, source, request)
    }
    fn event() -> Observation {
        Observation {
            id: "observation".into(),
            queue: "fixture.review".into(),
            capability: "credential.open".into(),
            observed_at_ms: now_ms() - 100,
            data: value!({"entity":"stable-identity-123"}),
        }
    }
    fn policy(condition: Condition) -> EvidencePolicy {
        EvidencePolicy {
            queue: "fixture.review".into(),
            default: PolicyOutcome::Ask,
            rules: vec![EvidenceRule {
                id: "example".into(),
                description: "Fixture policy".into(),
                requires: vec![],
                when: condition,
                then: PolicyOutcome::Action {
                    action_id: "allow-once".into(),
                },
                on_unavailable: UnavailableBehavior::Ask,
            }],
        }
    }
    fn observed() -> Condition {
        Condition::Observed {
            source: "host".into(),
            capability: "credential.open".into(),
            within_ms: 60_000,
            joins: [("/entity".into(), "/subject/entity".into())].into(),
            equals: BTreeMap::new(),
        }
    }
    #[test]
    fn cross_source_preview_is_shadow_and_preserves_request_and_receipt_bindings() {
        let (mut store, admin, producer, _, review) = setup();
        store
            .put_evidence_policy(
                &admin,
                policy(Condition::All {
                    conditions: vec![
                        Condition::Equals {
                            path: "/subject/entity".into(),
                            value: value!("stable-identity-123"),
                        },
                        observed(),
                    ],
                }),
            )
            .unwrap();
        let preview = store.policy_preview(&producer, &review.id).unwrap();
        assert!(!preview.authoritative);
        assert_eq!(
            preview.outcome,
            PolicyOutcome::Action {
                action_id: "allow-once".into()
            }
        );
        assert_eq!(preview.traces[0].observations.len(), 1);
        let after = store.get(&producer, &review.id).unwrap();
        assert!(after.decision.is_none() && after.receipt.is_none());
        assert_eq!(after.state, ReviewState::Pending);
        assert_eq!(after.evidence_digest, review.evidence_digest);
    }
    #[test]
    fn unavailable_sources_and_missing_join_values_remain_unknown() {
        let (mut store, admin, producer, source, review) = setup();
        store
            .put_evidence_policy(&admin, policy(observed()))
            .unwrap();
        for identity in [value!(""), value!({}), Value::Null] {
            let context = value!({"subject":{"entity":identity}});
            assert_eq!(
                store
                    .evaluate_condition(&observed(), &review, &context)
                    .unwrap()
                    .state,
                MatchState::Unknown
            );
        }
        store
            .source_heartbeat(
                &source,
                "host",
                SourceHeartbeat {
                    ready: false,
                    detail: "Capture gap".into(),
                },
            )
            .unwrap();
        let preview = store.policy_preview(&producer, &review.id).unwrap();
        assert_eq!(preview.outcome, PolicyOutcome::Ask);
        assert_eq!(preview.traces[0].state, MatchState::Unknown);
        store
            .source_heartbeat(
                &source,
                "host",
                SourceHeartbeat {
                    ready: true,
                    detail: "Ready".into(),
                },
            )
            .unwrap();
        let mut condition = observed();
        if let Condition::Observed { joins, .. } = &mut condition {
            *joins = [("/entity".into(), "/evidence/missing".into())].into();
        }
        store
            .put_evidence_policy(&admin, policy(condition))
            .unwrap();
        assert_eq!(
            store.policy_preview(&producer, &review.id).unwrap().traces[0].state,
            MatchState::Unknown
        );
    }
    #[test]
    fn source_tokens_cannot_read_requests_forge_other_sources_or_change_policies() {
        let (mut store, _, _, source, review) = setup();
        assert!(store.get(&source, &review.id).is_err());
        assert!(store.list(&source, i64::MAX, "pending").is_err());
        assert!(
            store
                .claim(
                    &source,
                    ClaimJob {
                        kind: "assessment".into()
                    }
                )
                .is_err()
        );
        assert!(
            store
                .put_evidence_policy(&source, policy(observed()))
                .is_err()
        );
        assert!(
            store
                .publish_observation(&source, "other", event())
                .is_err()
        );
        let mut invalid = event();
        invalid.id = "invalid".into();
        invalid.data["secret"] = value!("not-in-schema");
        assert!(store.publish_observation(&source, "host", invalid).is_err());
    }
    #[test]
    fn observation_replay_is_idempotent_and_configuration_changes_fence_old_evidence() {
        let (mut store, admin, producer, source, review) = setup();
        let body: String = store
            .db
            .query_row("SELECT body FROM evidence_observations", [], |r| r.get(0))
            .unwrap();
        let original: Observation = decode(&body).unwrap();
        assert!(
            store
                .publish_observation(&source, "host", original.clone())
                .is_ok()
        );
        let mut changed = original;
        changed.data["entity"] = value!("different");
        assert!(store.publish_observation(&source, "host", changed).is_err());
        store
            .put_evidence_policy(&admin, policy(observed()))
            .unwrap();
        let mut spec = store.source_info("host").unwrap().spec;
        spec.coverage = "Changed coverage".into();
        store.put_source(&admin, spec).unwrap();
        store
            .source_heartbeat(
                &source,
                "host",
                SourceHeartbeat {
                    ready: true,
                    detail: "Ready".into(),
                },
            )
            .unwrap();
        assert_eq!(
            store.policy_preview(&producer, &review.id).unwrap().outcome,
            PolicyOutcome::Ask
        );
    }
    #[test]
    fn optional_missing_source_can_continue_but_required_unknown_overrides_allow() {
        let (mut store, admin, producer, _, review) = setup();
        let mut p = policy(Condition::Equals {
            path: "/subject/entity".into(),
            value: value!("stable-identity-123"),
        });
        let mut detector = p.rules[0].clone();
        detector.id = "optional".into();
        detector.requires = vec![CapabilityRequirement {
            source: "not-installed".into(),
            capability: "anything".into(),
        }];
        detector.then = PolicyOutcome::Ask;
        detector.on_unavailable = UnavailableBehavior::Continue;
        p.rules.push(detector);
        store.put_evidence_policy(&admin, p.clone()).unwrap();
        assert!(matches!(
            store.policy_preview(&producer, &review.id).unwrap().outcome,
            PolicyOutcome::Action { .. }
        ));
        p.rules[1].on_unavailable = UnavailableBehavior::Ask;
        store.put_evidence_policy(&admin, p).unwrap();
        assert_eq!(
            store.policy_preview(&producer, &review.id).unwrap().outcome,
            PolicyOutcome::Ask
        );
    }
    #[test]
    fn stale_or_revoked_publisher_cannot_supply_fresh_evidence() {
        let (mut store, admin, producer, source, review) = setup();
        store
            .put_evidence_policy(&admin, policy(observed()))
            .unwrap();
        store
            .db
            .execute("UPDATE evidence_sources SET last_seen=1", [])
            .unwrap();
        assert_eq!(
            store.policy_preview(&producer, &review.id).unwrap().outcome,
            PolicyOutcome::Ask
        );
        store
            .source_heartbeat(
                &source,
                "host",
                SourceHeartbeat {
                    ready: true,
                    detail: "Ready".into(),
                },
            )
            .unwrap();
        store.revoke(&admin, &source.spec.id).unwrap();
        assert_eq!(
            store.policy_preview(&producer, &review.id).unwrap().outcome,
            PolicyOutcome::Ask
        );
        let rotated = store.rotate(&admin, &source.spec.id).unwrap();
        let publisher = store.authenticate(&rotated.credential).unwrap();
        store
            .source_heartbeat(
                &publisher,
                "host",
                SourceHeartbeat {
                    ready: true,
                    detail: "New key ready".into(),
                },
            )
            .unwrap();
        assert_eq!(
            store.policy_preview(&producer, &review.id).unwrap().outcome,
            PolicyOutcome::Ask
        );
    }
}

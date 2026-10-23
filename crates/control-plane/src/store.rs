use approval_protocol::*;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Serialize, de::DeserializeOwned};
use sha2::{Digest, Sha256};
use std::{
    fs,
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    path::Path,
};
use uuid::Uuid;

pub type Result<T> = std::result::Result<T, Fault>;
#[derive(Debug)]
pub struct Fault {
    pub status: u16,
    pub code: &'static str,
}
impl Fault {
    pub fn bad() -> Self {
        Self {
            status: 400,
            code: "invalid_argument",
        }
    }
    pub fn forbidden() -> Self {
        Self {
            status: 403,
            code: "permission_denied",
        }
    }
    pub fn conflict() -> Self {
        Self {
            status: 409,
            code: "conflict_or_expired",
        }
    }
    fn missing() -> Self {
        Self {
            status: 404,
            code: "not_found",
        }
    }
    fn internal() -> Self {
        Self {
            status: 500,
            code: "storage_unavailable",
        }
    }
}
impl From<rusqlite::Error> for Fault {
    fn from(_: rusqlite::Error) -> Self {
        Self::internal()
    }
}
impl From<serde_json::Error> for Fault {
    fn from(_: serde_json::Error) -> Self {
        Self::internal()
    }
}

pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}
fn id() -> String {
    Uuid::new_v4().to_string()
}
fn json<T: Serialize>(value: &T) -> Result<String> {
    Ok(serde_json::to_string(value)?)
}
fn decode<T: DeserializeOwned>(value: &str) -> Result<T> {
    Ok(serde_json::from_str(value)?)
}
fn identifier(value: &str) -> bool {
    !value.is_empty()
        && value != "."
        && value != ".."
        && value.len() <= 128
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
}
fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
pub fn fingerprint<T: Serialize>(value: &T) -> Result<String> {
    Ok(digest(&canonical_bytes(value)?))
}
fn credential() -> Result<String> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|_| Fault::internal())?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

#[derive(Clone)]
pub struct Principal {
    pub spec: PrincipalSpec,
}
impl Principal {
    fn admin(&self) -> bool {
        self.spec.role == Role::Admin
    }
    fn queue(&self, queue: &str) -> bool {
        self.admin() || self.spec.queues.iter().any(|q| q == queue)
    }
    fn read(&self, review: &Review) -> bool {
        self.spec.role != Role::Monitor
            && self.queue(&review.request.queue)
            && (self.spec.role != Role::Producer || self.spec.id == review.producer)
    }
    fn require(&self, role: Role) -> Result<()> {
        if self.admin() || self.spec.role == role {
            Ok(())
        } else {
            Err(Fault::forbidden())
        }
    }
}

pub struct Store {
    db: Connection,
}
impl Store {
    pub fn open(path: &Path, admin_file: &Path) -> anyhow::Result<Self> {
        let parent = path
            .parent()
            .ok_or_else(|| anyhow::anyhow!("approval database needs a parent"))?;
        fs::create_dir_all(parent)?;
        let owner = fs::metadata("/proc/self")?.uid();
        let directory = fs::symlink_metadata(parent)?;
        anyhow::ensure!(
            directory.is_dir() && !directory.file_type().is_symlink() && directory.uid() == owner,
            "approval directory must be owned by this service"
        );
        fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
        if let Ok(metadata) = fs::symlink_metadata(path) {
            anyhow::ensure!(
                metadata.is_file() && metadata.uid() == owner && metadata.mode() & 0o077 == 0,
                "approval database must be private"
            );
        }
        fs::OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(path)?;
        let mut store = Self::init(Connection::open(path)?)?;
        let admins: i64 = store.db.query_row(
            "SELECT count(*) FROM principals WHERE id='local-admin'",
            [],
            |r| r.get(0),
        )?;
        if admins == 0 {
            let token = if admin_file.exists() {
                let metadata = fs::symlink_metadata(admin_file)?;
                anyhow::ensure!(
                    metadata.is_file()
                        && metadata.uid() == owner
                        && metadata.mode() & 0o077 == 0
                        && metadata.len() == 64,
                    "invalid private administrator credential"
                );
                let token = fs::read_to_string(admin_file)?;
                anyhow::ensure!(
                    token.bytes().all(|b| b.is_ascii_hexdigit()),
                    "invalid administrator credential format"
                );
                token
            } else {
                let token =
                    credential().map_err(|_| anyhow::anyhow!("generate local credential"))?;
                let mut file = fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(0o600)
                    .open(admin_file)?;
                use std::io::Write;
                file.write_all(token.as_bytes())?;
                file.sync_all()?;
                token
            };
            let spec = PrincipalSpec {
                id: "local-admin".into(),
                role: Role::Admin,
                queues: vec![],
                service_kind: String::new(),
                realm: String::new(),
                actors: vec![],
            };
            store.db.execute(
                "INSERT INTO principals(id,hash,spec,last_seen,detail) VALUES(?1,?2,?3,0,'')",
                params![
                    spec.id,
                    digest(token.as_bytes()),
                    serde_json::to_string(&spec)?
                ],
            )?;
        }
        // No request or grant is revived at restart; expiry is checked on every operation.
        store
            .expire(now_ms())
            .map_err(|_| anyhow::anyhow!("recover approval request deadlines"))?;
        Ok(store)
    }
    fn init(db: Connection) -> anyhow::Result<Self> {
        db.busy_timeout(std::time::Duration::from_secs(3))?;
        db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON; PRAGMA max_page_count=262144;
            CREATE TABLE IF NOT EXISTS schema_version(version INTEGER PRIMARY KEY);
            INSERT OR IGNORE INTO schema_version VALUES(1);
            CREATE TABLE IF NOT EXISTS principals(id TEXT PRIMARY KEY,hash TEXT UNIQUE NOT NULL,spec TEXT NOT NULL,last_seen INTEGER NOT NULL,detail TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS revoked_principals(id TEXT PRIMARY KEY,at INTEGER NOT NULL);
            CREATE TABLE IF NOT EXISTS administrative_events(seq INTEGER PRIMARY KEY AUTOINCREMENT,actor TEXT NOT NULL,kind TEXT NOT NULL,subject TEXT NOT NULL,at INTEGER NOT NULL);
            CREATE TABLE IF NOT EXISTS queue_revisions(queue TEXT NOT NULL,revision INTEGER NOT NULL,manifest TEXT NOT NULL,actor TEXT NOT NULL,at INTEGER NOT NULL,PRIMARY KEY(queue,revision));
            CREATE TABLE IF NOT EXISTS queues(id TEXT PRIMARY KEY,revision INTEGER NOT NULL,manifest TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS requests(seq INTEGER PRIMARY KEY AUTOINCREMENT,id TEXT UNIQUE NOT NULL,producer TEXT NOT NULL,queue TEXT NOT NULL,idempotency TEXT NOT NULL,fingerprint TEXT NOT NULL,state TEXT NOT NULL,expires INTEGER NOT NULL,body TEXT NOT NULL,UNIQUE(producer,queue,idempotency));
            CREATE INDEX IF NOT EXISTS requests_pending ON requests(state,expires,queue);
            CREATE TABLE IF NOT EXISTS decisions(id TEXT PRIMARY KEY,request_id TEXT UNIQUE NOT NULL,body TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS events(seq INTEGER PRIMARY KEY AUTOINCREMENT,request_id TEXT NOT NULL,queue TEXT NOT NULL,producer TEXT NOT NULL,kind TEXT NOT NULL,at INTEGER NOT NULL);
            CREATE TABLE IF NOT EXISTS jobs(id TEXT PRIMARY KEY,request_id TEXT UNIQUE NOT NULL,queue TEXT NOT NULL,kind TEXT NOT NULL,state TEXT NOT NULL,lease TEXT NOT NULL,owner TEXT NOT NULL,until INTEGER NOT NULL,attempt INTEGER NOT NULL,ready_at INTEGER NOT NULL,profile TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS conversations(request_id TEXT PRIMARY KEY,body TEXT NOT NULL,status TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS conversation_keys(request_id TEXT NOT NULL,key TEXT NOT NULL,fingerprint TEXT NOT NULL,PRIMARY KEY(request_id,key));
            CREATE TABLE IF NOT EXISTS turn_jobs(id TEXT PRIMARY KEY,request_id TEXT NOT NULL,queue TEXT NOT NULL,state TEXT NOT NULL,owner TEXT NOT NULL,lease TEXT NOT NULL,until INTEGER NOT NULL,attempt INTEGER NOT NULL,ready_at INTEGER NOT NULL,deadline INTEGER NOT NULL,profile TEXT NOT NULL,payload TEXT NOT NULL,manifest_revision INTEGER NOT NULL);
            CREATE TABLE IF NOT EXISTS routes(realm TEXT PRIMARY KEY,channel TEXT NOT NULL,at INTEGER NOT NULL);
            CREATE TABLE IF NOT EXISTS deliveries(id TEXT PRIMARY KEY,realm TEXT NOT NULL,request_id TEXT NOT NULL,revision INTEGER NOT NULL,channel TEXT NOT NULL,message TEXT,nonce TEXT NOT NULL,status TEXT NOT NULL,body TEXT NOT NULL,created INTEGER NOT NULL,UNIQUE(realm,request_id,revision));")?;
        let version: i64 =
            db.query_row("SELECT max(version) FROM schema_version", [], |r| r.get(0))?;
        anyhow::ensure!(version == 1, "unsupported approval schema version");
        Ok(Self { db })
    }
    pub fn authenticate(&self, token: &str) -> Result<Principal> {
        if token.len() != 64 || !token.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(Fault {
                status: 401,
                code: "unauthenticated",
            });
        }
        let spec: Option<String> = self
            .db
            .query_row(
                "SELECT spec FROM principals WHERE hash=?1 AND id NOT IN (SELECT id FROM revoked_principals)",
                [digest(token.as_bytes())],
                |r| r.get(0),
            )
            .optional()?;
        Ok(Principal {
            spec: decode(&spec.ok_or(Fault {
                status: 401,
                code: "unauthenticated",
            })?)?,
        })
    }
    fn validate_principal(&self, spec: &PrincipalSpec) -> Result<()> {
        if !identifier(&spec.id)
            || spec.id == "local-admin"
            || spec.queues.len() > 64
            || spec.actors.len() > 64
            || spec.actors.iter().any(|a| !identifier(a))
            || spec.realm.len() > 128
            || spec.realm.chars().any(char::is_control)
            || (spec.role != Role::Admin && spec.queues.is_empty())
            || (spec.role == Role::Producer && !identifier(&spec.service_kind))
            || (spec.role == Role::Transport && (spec.realm.is_empty() || spec.actors.is_empty()))
        {
            return Err(Fault::bad());
        }
        for queue in &spec.queues {
            self.manifest(queue)?;
        }
        Ok(())
    }
    pub fn update_principal(
        &mut self,
        actor: &Principal,
        spec: PrincipalSpec,
    ) -> Result<EnrolledPrincipal> {
        actor.require(Role::Admin)?;
        self.validate_principal(&spec)?;
        // Rotation fences old worker leases and pending producer consent before changing ACLs.
        let enrolled = self.rotate(actor, &spec.id)?;
        self.db.execute(
            "UPDATE principals SET spec=?1 WHERE id=?2",
            params![json(&spec)?, spec.id],
        )?;
        self.db.execute("INSERT INTO administrative_events(actor,kind,subject,at) VALUES(?1,'principal_updated',?2,?3)",params![actor.spec.id,spec.id,now_ms()])?;
        Ok(EnrolledPrincipal {
            principal: spec,
            credential: enrolled.credential,
        })
    }
    pub fn enroll(&mut self, actor: &Principal, spec: PrincipalSpec) -> Result<EnrolledPrincipal> {
        actor.require(Role::Admin)?;
        self.validate_principal(&spec)?;
        if self
            .db
            .query_row("SELECT 1 FROM principals WHERE id=?1", [&spec.id], |_| {
                Ok(())
            })
            .optional()?
            .is_some()
        {
            return Err(Fault::conflict());
        }
        let token = credential()?;
        self.db.execute(
            "INSERT INTO principals VALUES(?1,?2,?3,0,'')",
            params![spec.id, digest(token.as_bytes()), json(&spec)?],
        )?;
        Ok(EnrolledPrincipal {
            principal: spec,
            credential: token,
        })
    }
    pub fn revoke(&mut self, actor: &Principal, identity: &str) -> Result<()> {
        actor.require(Role::Admin)?;
        if identity == "local-admin" || !identifier(identity) {
            return Err(Fault::bad());
        }
        let exists: bool = self.db.query_row(
            "SELECT EXISTS(SELECT 1 FROM principals WHERE id=?1)",
            [identity],
            |r| r.get(0),
        )?;
        if !exists {
            return Err(Fault::missing());
        }
        let mut pending = self
            .all_pending()?
            .into_iter()
            .filter(|r| r.producer == identity)
            .collect::<Vec<_>>();
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute(
            "INSERT OR IGNORE INTO revoked_principals VALUES(?1,?2)",
            params![identity, now_ms()],
        )?;
        tx.execute("INSERT INTO administrative_events(actor,kind,subject,at) VALUES(?1,'principal_revoked',?2,?3)", params![actor.spec.id, identity, now_ms()])?;
        for review in &mut pending {
            review.state = ReviewState::Cancelled;
            tx.execute(
                "UPDATE requests SET state='cancelled',body=?1 WHERE id=?2 AND state='pending'",
                params![json(review)?, review.id],
            )?;
            tx.execute("UPDATE jobs SET state='cancelled',lease='' WHERE request_id=?1 AND state!='completed'", [&review.id])?;
            Self::event(&tx, review, "producer_revoked", now_ms())?;
        }
        tx.execute("UPDATE jobs SET state='ready',lease='',owner='',until=0,ready_at=?1 WHERE state='leased' AND owner=?2", params![now_ms(), identity])?;
        tx.execute("UPDATE turn_jobs SET state='ready',lease='',owner='',until=0,ready_at=?1 WHERE state='leased' AND owner=?2", params![now_ms(), identity])?;
        tx.execute("UPDATE turn_jobs SET state='cancelled',lease='' WHERE state IN('ready','leased') AND request_id IN (SELECT id FROM requests WHERE producer=?1)", [identity])?;
        tx.execute("UPDATE conversations SET status='cancelled',body=json_set(body,'$.status','cancelled') WHERE status='running' AND request_id IN (SELECT id FROM requests WHERE producer=?1)", [identity])?;
        tx.commit()?;
        Ok(())
    }
    pub fn rotate(&mut self, actor: &Principal, identity: &str) -> Result<EnrolledPrincipal> {
        actor.require(Role::Admin)?;
        self.revoke(actor, identity)?;
        let spec: String =
            self.db
                .query_row("SELECT spec FROM principals WHERE id=?1", [identity], |r| {
                    r.get(0)
                })?;
        let principal: PrincipalSpec = decode(&spec)?;
        let credential = credential()?;
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute(
            "UPDATE principals SET hash=?1 WHERE id=?2",
            params![digest(credential.as_bytes()), identity],
        )?;
        tx.execute("DELETE FROM revoked_principals WHERE id=?1", [identity])?;
        tx.execute("INSERT INTO administrative_events(actor,kind,subject,at) VALUES(?1,'principal_rotated',?2,?3)", params![actor.spec.id, identity, now_ms()])?;
        tx.commit()?;
        Ok(EnrolledPrincipal {
            principal,
            credential,
        })
    }
    fn manifest(&self, queue: &str) -> Result<(i64, QueueManifest)> {
        let row: Option<(i64, String)> = self
            .db
            .query_row(
                "SELECT revision,manifest FROM queues WHERE id=?1",
                [queue],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let (revision, value) = row.ok_or_else(Fault::missing)?;
        Ok((revision, decode(&value)?))
    }
    pub fn put_queue(&mut self, actor: &Principal, manifest: QueueManifest) -> Result<QueueInfo> {
        actor.require(Role::Admin)?;
        if !identifier(&manifest.id)
            || !identifier(&manifest.service_kind)
            || !identifier(&manifest.schema_id)
            || manifest.title.len() > 128
            || !manifest.subject_shape.valid()
            || manifest.actions.is_empty()
            || manifest.actions.len() > 16
            || manifest.max_pending == 0
            || manifest.max_pending > 1024
            || manifest.deadline_cap_ms < 1000
            || manifest.deadline_cap_ms > 86_400_000
        {
            return Err(Fault::bad());
        }
        let mut ids = std::collections::BTreeSet::new();
        for action in &manifest.actions {
            if !identifier(&action.id)
                || !ids.insert(&action.id)
                || action.label.is_empty()
                || action.label.len() > 100
                || !action.scope.is_object()
                || json(&action.scope)?.len() > 4096
                || canonical_bytes(&action.scope).is_err()
            {
                return Err(Fault::bad());
            }
        }
        for rule in &manifest.rules {
            if !identifier(&rule.id)
                || rule.equals.is_empty()
                || !manifest.actions.iter().any(|a| a.id == rule.action)
            {
                return Err(Fault::bad());
            }
        }
        if let Some(profile) = &manifest.assessment
            && (profile.instructions.len() > 4096
                || profile.labels.is_empty()
                || profile.labels.len() > 32
                || profile.subject_fields.len() > 32
                || profile.evidence_fields.len() > 32
                || profile.models.len() > 16
                || profile
                    .models
                    .iter()
                    .any(|m| m.is_empty() || m.len() > 128 || m.chars().any(char::is_control)))
        {
            return Err(Fault::bad());
        }
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute("INSERT INTO queues VALUES(?1,1,?2) ON CONFLICT(id) DO UPDATE SET revision=revision+1,manifest=excluded.manifest", params![manifest.id,json(&manifest)?])?;
        let revision: i64 = tx.query_row(
            "SELECT revision FROM queues WHERE id=?1",
            [&manifest.id],
            |row| row.get(0),
        )?;
        tx.execute(
            "INSERT INTO queue_revisions VALUES(?1,?2,?3,?4,?5)",
            params![
                manifest.id,
                revision,
                json(&manifest)?,
                actor.spec.id,
                now_ms()
            ],
        )?;
        tx.execute("INSERT INTO administrative_events(actor,kind,subject,at) VALUES(?1,'queue_updated',?2,?3)", params![actor.spec.id, manifest.id, now_ms()])?;
        tx.execute("UPDATE turn_jobs SET state='cancelled',lease='' WHERE queue=?1 AND state IN('ready','leased')", [&manifest.id])?;
        tx.execute("UPDATE conversations SET status='cancelled',body=json_set(body,'$.status','cancelled') WHERE status='running' AND request_id IN (SELECT id FROM requests WHERE queue=?1)", [&manifest.id])?;
        tx.commit()?;
        // Changed manifests invalidate old pending consent and outstanding jobs.
        let pending = self.all_pending()?;
        for mut request in pending
            .into_iter()
            .filter(|r| r.request.queue == manifest.id)
        {
            request.state = ReviewState::Cancelled;
            self.save_terminal(&request, "queue_changed", now_ms())?;
        }
        self.queue_info(&manifest.id)
    }
    fn queue_info(&self, id: &str) -> Result<QueueInfo> {
        let (revision, manifest) = self.manifest(id)?;
        let pending: i64 = self.db.query_row(
            "SELECT count(*) FROM requests WHERE queue=?1 AND state='pending'",
            [id],
            |r| r.get(0),
        )?;
        let settled: i64 = self.db.query_row(
            "SELECT count(*) FROM requests WHERE queue=?1 AND state!='pending'",
            [id],
            |r| r.get(0),
        )?;
        Ok(QueueInfo {
            revision,
            manifest,
            pending: pending as u64,
            settled: settled as u64,
        })
    }
    pub fn queues(&mut self, actor: &Principal) -> Result<Vec<QueueInfo>> {
        self.expire(now_ms())?;
        let ids = self
            .db
            .prepare("SELECT id FROM queues ORDER BY id")?
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        ids.into_iter()
            .filter(|id| actor.queue(id))
            .map(|id| self.queue_info(&id))
            .collect()
    }
    fn load(&self, id: &str) -> Result<Review> {
        let body: Option<String> = self
            .db
            .query_row("SELECT body FROM requests WHERE id=?1", [id], |r| r.get(0))
            .optional()?;
        decode(&body.ok_or_else(Fault::missing)?)
    }
    fn all_pending(&self) -> Result<Vec<Review>> {
        self.db
            .prepare("SELECT body FROM requests WHERE state='pending'")?
            .query_map([], |r| r.get::<_, String>(0))?
            .map(|row| decode(&row?))
            .collect()
    }
    fn event(db: &Connection, review: &Review, kind: &str, now: i64) -> Result<()> {
        db.execute(
            "INSERT INTO events(request_id,queue,producer,kind,at) VALUES(?1,?2,?3,?4,?5)",
            params![review.id, review.request.queue, review.producer, kind, now],
        )?;
        Ok(())
    }
    fn state(review: &Review) -> &'static str {
        match review.state {
            ReviewState::Pending => "pending",
            ReviewState::Allowed => "allowed",
            ReviewState::Denied => "denied",
            ReviewState::Observed => "observed",
            ReviewState::Cancelled => "cancelled",
            ReviewState::Expired => "expired",
        }
    }
    fn save_terminal(&mut self, review: &Review, kind: &str, now: i64) -> Result<()> {
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute(
            "UPDATE requests SET state=?1,body=?2 WHERE id=?3 AND state='pending'",
            params![Self::state(review), json(review)?, review.id],
        )?;
        tx.execute(
            "UPDATE jobs SET state='cancelled',lease='' WHERE request_id=?1 AND state!='completed'",
            [&review.id],
        )?;
        Self::event(&tx, review, kind, now)?;
        tx.commit()?;
        Ok(())
    }
    pub fn expire(&mut self, now: i64) -> Result<()> {
        let expired = self
            .db
            .prepare("SELECT body FROM requests WHERE state='pending' AND expires<=?1")?
            .query_map([now], |row| row.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        for body in expired {
            let mut review: Review = decode(&body)?;
            review.state = ReviewState::Expired;
            self.save_terminal(&review, "expired", now)?;
        }
        Ok(())
    }
    pub fn submit(&mut self, actor: &Principal, request: SubmitRequest) -> Result<Review> {
        actor.require(Role::Producer)?;
        if !actor.queue(&request.queue) {
            return Err(Fault::forbidden());
        }
        let now = now_ms();
        self.expire(now)?;
        if !identifier(&request.producer_request_id)
            || !identifier(&request.nonce)
            || request.nonce.len() < 16
            || request.idempotency_key.is_empty()
            || request.idempotency_key.len() > 128
            || request.summary.len() > 1024
            || request.action_ids.is_empty()
            || request.action_ids.len() > 16
            || json(&request.subject)?.len() > 24_576
            || json(&request.evidence)?.len() > 32_768
            || !request.evidence.is_object()
        {
            return Err(Fault::bad());
        }
        let submitted_fingerprint = fingerprint(&request).map_err(|_| Fault::bad())?;
        let previous: Option<(String,String)> = self.db.query_row("SELECT id,fingerprint FROM requests WHERE producer=?1 AND queue=?2 AND idempotency=?3", params![actor.spec.id,request.queue,request.idempotency_key], |r| Ok((r.get(0)?,r.get(1)?))).optional()?;
        if let Some((id, old)) = previous {
            return if old == submitted_fingerprint {
                self.get(actor, &id)
            } else {
                Err(Fault::conflict())
            };
        }
        let (revision, manifest) = self.manifest(&request.queue)?;
        if (!actor.admin() && actor.spec.service_kind != manifest.service_kind)
            || manifest.schema_id != request.schema_id
            || !manifest.subject_shape.accepts(&request.subject)
        {
            return Err(Fault::bad());
        }
        let expires = request
            .expires_at_ms
            .min(now.saturating_add(manifest.deadline_cap_ms));
        if expires <= now {
            return Err(Fault::conflict());
        }
        let count: i64 = self.db.query_row(
            "SELECT count(*) FROM requests WHERE state='pending' AND queue=?1",
            [&request.queue],
            |r| r.get(0),
        )?;
        let total: i64 = self.db.query_row(
            "SELECT count(*) FROM requests WHERE state='pending'",
            [],
            |r| r.get(0),
        )?;
        if count >= i64::from(manifest.max_pending)
            || total >= 2048
            || (manifest.scheduling_class == SchedulingClass::Background && total >= 1536)
        {
            return Err(Fault {
                status: 429,
                code: "queue_full",
            });
        }
        let actions = request
            .action_ids
            .iter()
            .map(|id| {
                manifest
                    .actions
                    .iter()
                    .find(|a| a.id == *id)
                    .cloned()
                    .ok_or_else(Fault::bad)
            })
            .collect::<Result<Vec<_>>>()?;
        let review = Review {
            id: id(),
            producer: actor.spec.id.clone(),
            service_kind: manifest.service_kind,
            revision: 1,
            manifest_revision: revision,
            subject_digest: fingerprint(&(request.schema_id.as_str(), &request.subject))?,
            evidence_digest: fingerprint(&request.evidence)?,
            created_at_ms: now,
            expires_at_ms: expires,
            mode: manifest.mode,
            state: ReviewState::Pending,
            actions,
            request,
            decision: None,
            assessment: None,
            receipt: None,
        };
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute("INSERT INTO requests(id,producer,queue,idempotency,fingerprint,state,expires,body) VALUES(?1,?2,?3,?4,?5,'pending',?6,?7)", params![review.id,review.producer,review.request.queue,review.request.idempotency_key,submitted_fingerprint,expires,json(&review)?])?;
        Self::event(&tx, &review, "submitted", now)?;
        if let Some(profile) = manifest.assessment {
            tx.execute(
                "INSERT INTO jobs VALUES(?1,?2,?3,'assessment','ready','','',0,0,?4,?5)",
                params![id(), review.id, review.request.queue, now, json(&profile)?],
            )?;
        }
        tx.commit()?;
        if review.mode == Mode::Policy
            && let Some(rule) = manifest.rules.iter().find(|rule| {
                rule.equals
                    .iter()
                    .all(|(key, value)| review.request.subject.get(key) == Some(value))
            })
            && review.actions.iter().any(|a| a.id == rule.action)
        {
            return self.commit_decision(
                review,
                &rule.action,
                &format!("policy:{}:{}", revision, rule.id),
                now,
            );
        }
        Ok(review)
    }
    pub fn get(&mut self, actor: &Principal, id: &str) -> Result<Review> {
        self.expire(now_ms())?;
        let review = self.load(id)?;
        if !actor.read(&review) {
            return Err(Fault::forbidden());
        }
        Ok(review)
    }
    pub fn list(&mut self, actor: &Principal, before: i64, state: &str) -> Result<Reviews> {
        self.expire(now_ms())?;
        if !["pending", "history", "all"].contains(&state) {
            return Err(Fault::bad());
        }
        let rows = self.db.prepare("SELECT seq,body FROM requests WHERE seq<?1 AND (?2='all' OR (?2='pending' AND state='pending') OR (?2='history' AND state!='pending')) AND (?3 OR (queue IN (SELECT value FROM json_each(?4)) AND (?5='' OR producer=?5))) ORDER BY seq DESC LIMIT 16")?.query_map(params![before,state,actor.admin(),json(&actor.spec.queues)?,if actor.spec.role==Role::Producer {actor.spec.id.as_str()}else{""}], |r| Ok((r.get::<_,i64>(0)?, r.get::<_,String>(1)?)))?.collect::<std::result::Result<Vec<_>,_>>()?;
        let next_before = (rows.len() == 16).then(|| rows.last().unwrap().0);
        let mut reviews = vec![];
        for (_, body) in rows {
            let review: Review = decode(&body)?;
            if actor.read(&review) {
                reviews.push(review);
            }
        }
        Ok(Reviews {
            reviews,
            next_before,
        })
    }
    fn commit_decision(
        &mut self,
        mut review: Review,
        action_id: &str,
        actor: &str,
        now: i64,
    ) -> Result<Review> {
        if review.state != ReviewState::Pending || review.expires_at_ms <= now {
            return Err(Fault::conflict());
        }
        let (revision, _) = self.manifest(&review.request.queue)?;
        if revision != review.manifest_revision {
            return Err(Fault::conflict());
        }
        let action = review
            .actions
            .iter()
            .find(|a| a.id == action_id)
            .cloned()
            .ok_or_else(Fault::bad)?;
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let now = now_ms();
        if review.expires_at_ms <= now {
            return Err(Fault::conflict());
        }
        let decision = Decision {
            id: id(),
            request_id: review.id.clone(),
            producer: review.producer.clone(),
            queue: review.request.queue.clone(),
            request_revision: review.revision,
            manifest_revision: review.manifest_revision,
            nonce: review.request.nonce.clone(),
            subject_digest: review.subject_digest.clone(),
            evidence_digest: review.evidence_digest.clone(),
            action: action.clone(),
            actor: actor.into(),
            issued_at_ms: now,
            expires_at_ms: review.expires_at_ms,
            authoritative: review.mode != Mode::Shadow && action.effect != Effect::Observe,
        };
        review.state = if review.mode == Mode::Shadow {
            ReviewState::Observed
        } else {
            match action.effect {
                Effect::Permit => ReviewState::Allowed,
                Effect::Block => ReviewState::Denied,
                Effect::Observe => ReviewState::Observed,
            }
        };
        review.decision = Some(decision.clone());
        let changed = tx.execute(
            "UPDATE requests SET state=?1,body=?2 WHERE id=?3 AND state='pending' AND expires>?4",
            params![Self::state(&review), json(&review)?, review.id, now],
        )?;
        if changed != 1 {
            return Err(Fault::conflict());
        }
        tx.execute(
            "INSERT INTO decisions VALUES(?1,?2,?3)",
            params![decision.id, review.id, json(&decision)?],
        )?;
        tx.execute(
            "UPDATE jobs SET state='cancelled',lease='' WHERE request_id=?1 AND state!='completed'",
            [&review.id],
        )?;
        Self::event(&tx, &review, "decided", now)?;
        tx.commit()?;
        Ok(review)
    }
    pub fn decide(
        &mut self,
        actor: &Principal,
        id: &str,
        request: DecideRequest,
    ) -> Result<Review> {
        actor.require(Role::Reviewer)?;
        let review = self.get(actor, id)?;
        if request.revision != review.revision {
            return Err(Fault::conflict());
        }
        self.commit_decision(review, &request.action_id, &actor.spec.id, now_ms())
    }
    pub fn cancel(&mut self, actor: &Principal, id: &str) -> Result<Review> {
        actor.require(Role::Producer)?;
        let mut review = self.get(actor, id)?;
        if !actor.admin() && actor.spec.id != review.producer {
            return Err(Fault::forbidden());
        }
        if review.state == ReviewState::Cancelled {
            return Ok(review);
        }
        if review.state != ReviewState::Pending {
            return Err(Fault::conflict());
        }
        review.state = ReviewState::Cancelled;
        self.save_terminal(&review, "cancelled", now_ms())?;
        Ok(review)
    }
    pub fn receipt(
        &mut self,
        actor: &Principal,
        id: &str,
        receipt: EnforcementReceipt,
    ) -> Result<Review> {
        actor.require(Role::Producer)?;
        let mut review = self.get(actor, id)?;
        if !actor.admin() && actor.spec.id != review.producer {
            return Err(Fault::forbidden());
        }
        if receipt.reason.len() > 1024
            || review
                .decision
                .as_ref()
                .is_none_or(|d| d.id != receipt.decision_id || !d.authoritative)
        {
            return Err(Fault::bad());
        }
        if let Some(existing) = &review.receipt {
            return if json(existing)? == json(&receipt)? {
                Ok(review)
            } else {
                Err(Fault::conflict())
            };
        }
        review.receipt = Some(receipt);
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute(
            "UPDATE requests SET body=?1 WHERE id=?2",
            params![json(&review)?, review.id],
        )?;
        Self::event(&tx, &review, "enforcement_reported", now_ms())?;
        tx.commit()?;
        Ok(review)
    }
    pub fn events(&mut self, actor: &Principal, after: i64) -> Result<Events> {
        if actor.spec.role == Role::Monitor {
            return Err(Fault::forbidden());
        }
        self.expire(now_ms())?;
        let rows = self.db.prepare("SELECT seq,request_id,queue,producer,kind,at FROM events WHERE seq>?1 ORDER BY seq LIMIT 500")?.query_map([after], |r| Ok((r.get::<_,i64>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?,r.get::<_,String>(4)?,r.get::<_,i64>(5)?)))?.collect::<std::result::Result<Vec<_>,_>>()?;
        let mut result = Events {
            events: vec![],
            cursor: after,
        };
        for (sequence, request_id, queue, producer, kind, at_ms) in rows {
            result.cursor = sequence;
            if actor.spec.role == Role::Producer && kind.starts_with("investigation_") {
                continue;
            }
            if actor.queue(&queue)
                && (actor.spec.role != Role::Producer || actor.spec.id == producer)
            {
                result.events.push(Event {
                    sequence,
                    request_id,
                    kind,
                    at_ms,
                });
            }
        }
        Ok(result)
    }
    pub fn heartbeat(&self, actor: &Principal, detail: &str) -> Result<()> {
        if detail.len() > 1024 {
            return Err(Fault::bad());
        }
        self.db.execute(
            "UPDATE principals SET last_seen=?1,detail=?2 WHERE id=?3",
            params![now_ms(), detail, actor.spec.id],
        )?;
        Ok(())
    }
    pub fn health(&mut self, actor: &Principal) -> Result<Health> {
        let queues = self.queues(actor)?;
        let rows = self
            .db
            .prepare("SELECT spec,last_seen,detail FROM principals WHERE id NOT IN (SELECT id FROM revoked_principals)")?
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let mut services = vec![];
        for (spec, last_seen_ms, detail) in rows {
            let spec: PrincipalSpec = decode(&spec)?;
            if matches!(spec.role, Role::Producer | Role::Worker | Role::Transport)
                && (actor.admin()
                    || spec.id == actor.spec.id
                    || (actor.spec.role != Role::Producer
                        && spec.queues.iter().any(|q| actor.queue(q))))
            {
                services.push(ServiceHealth {
                    id: spec.id,
                    service_kind: spec.service_kind,
                    last_seen_ms,
                    detail,
                });
            }
        }
        let mut ready_jobs = 0;
        let mut leased_jobs = 0;
        for queue in &queues {
            ready_jobs += self.db.query_row(
                "SELECT (SELECT count(*) FROM jobs WHERE queue=?1 AND state='ready') + (SELECT count(*) FROM turn_jobs WHERE queue=?1 AND state='ready')",
                [&queue.manifest.id],
                |r| r.get::<_, i64>(0),
            )? as u64;
            leased_jobs += self.db.query_row(
                "SELECT (SELECT count(*) FROM jobs WHERE queue=?1 AND state='leased') + (SELECT count(*) FROM turn_jobs WHERE queue=?1 AND state='leased')",
                [&queue.manifest.id],
                |r| r.get::<_, i64>(0),
            )? as u64;
        }
        Ok(Health {
            queues,
            services,
            ready_jobs,
            leased_jobs,
        })
    }
    pub fn claim(&mut self, actor: &Principal, request: ClaimJob) -> Result<Option<Job>> {
        actor.require(Role::Worker)?;
        if !["assessment", "assessment_or_investigation"].contains(&request.kind.as_str()) {
            return Err(Fault::bad());
        }
        let now = now_ms();
        self.expire(now)?;
        self.db.execute("UPDATE jobs SET state='ready',lease='',owner='' WHERE state='leased' AND until<=?1 AND attempt<3",[now])?;
        self.db.execute("UPDATE jobs SET state='failed',lease='' WHERE state='leased' AND until<=?1 AND attempt>=3",[now])?;
        let candidates=self.db.prepare("SELECT jobs.id,jobs.request_id,jobs.queue,jobs.attempt,jobs.profile FROM jobs JOIN requests ON requests.id=jobs.request_id WHERE jobs.state='ready' AND jobs.ready_at<=?1 AND requests.state='pending' ORDER BY requests.expires,requests.seq LIMIT 2048")?.query_map([now],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,u32>(3)?,r.get::<_,String>(4)?)))?.collect::<std::result::Result<Vec<_>,_>>()?;
        let Some((job_id, request_id, _, attempt, profile)) = candidates
            .into_iter()
            .find(|(_, _, q, _, _)| actor.queue(q))
        else {
            return if request.kind == "assessment_or_investigation" {
                self.claim_turn(actor)
            } else {
                Ok(None)
            };
        };
        let review = self.load(&request_id)?;
        let expires = review.expires_at_ms.min(now + 30_000);
        let lease = id();
        let changed=self.db.execute("UPDATE jobs SET state='leased',lease=?1,owner=?2,until=?3,attempt=attempt+1 WHERE id=?4 AND state='ready'",params![lease,actor.spec.id,expires,job_id])?;
        if changed != 1 {
            return Ok(None);
        };
        Ok(Some(Job {
            id: job_id,
            lease,
            attempt: attempt + 1,
            expires_at_ms: expires,
            review,
            profile: decode(&profile)?,
            conversation: None,
        }))
    }
    fn leased(&mut self, actor: &Principal, id: &str, lease: &str) -> Result<Review> {
        actor.require(Role::Worker)?;
        self.expire(now_ms())?;
        let request:Option<String>=self.db.query_row("SELECT request_id FROM jobs WHERE id=?1 AND lease=?2 AND owner=?3 AND state='leased' AND until>?4",params![id,lease,actor.spec.id,now_ms()],|r|r.get(0)).optional()?;
        let review = self.load(&request.ok_or_else(Fault::conflict)?)?;
        if !actor.queue(&review.request.queue) || review.state != ReviewState::Pending {
            return Err(Fault::conflict());
        }
        Ok(review)
    }
    pub fn complete(
        &mut self,
        actor: &Principal,
        id: &str,
        request: CompleteJob,
    ) -> Result<Review> {
        if id.starts_with("turn-") {
            return self.complete_turn(actor, id, request);
        }
        let mut review = self.leased(actor, id, &request.lease)?;
        let result = &request.assessment;
        if result.evidence_digest != review.evidence_digest
            || result.rationale.len() > 4096
            || result.category.len() > 128
            || result.jev_model.len() > 256
            || result.llm_model.len() > 256
            || result
                .confidence
                .is_some_and(|c| !c.is_finite() || !(0.0..=1.0).contains(&c))
            || !["complete", "partial", "unavailable"].contains(&result.state.as_str())
        {
            return Err(Fault::bad());
        }
        let profile: String =
            self.db
                .query_row("SELECT profile FROM jobs WHERE id=?1", [id], |r| r.get(0))?;
        let profile: AssessmentProfile = decode(&profile)?;
        if !result.category.is_empty() && !profile.labels.contains_key(&result.category) {
            return Err(Fault::bad());
        }
        review.assessment = Some(request.assessment);
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let commit_time = now_ms();
        let changed = tx.execute(
            "UPDATE jobs SET state='completed',lease='' WHERE id=?1 AND lease=?2 AND owner=?3 AND state='leased' AND until>?4",
            params![id, request.lease, actor.spec.id, commit_time],
        )?;
        if changed != 1 {
            return Err(Fault::conflict());
        }
        let changed = tx.execute(
            "UPDATE requests SET body=?1 WHERE id=?2 AND state='pending' AND expires>?3",
            params![json(&review)?, review.id, commit_time],
        )?;
        if changed != 1 {
            return Err(Fault::conflict());
        }
        Self::event(&tx, &review, "assessed", now_ms())?;
        tx.commit()?;
        Ok(review)
    }
    pub fn fail(&mut self, actor: &Principal, id: &str, request: FailJob) -> Result<()> {
        if id.starts_with("turn-") {
            actor.require(Role::Worker)?;
            let row: Option<(String,u32,i64,String)> = self.db.query_row("SELECT request_id,attempt,deadline,queue FROM turn_jobs WHERE id=?1 AND owner=?2 AND lease=?3 AND state='leased' AND until>?4",params![id,actor.spec.id,request.lease,now_ms()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional()?;
            let (request_id, attempt, deadline, queue) = row.ok_or_else(Fault::conflict)?;
            if !actor.queue(&queue) {
                return Err(Fault::forbidden());
            }
            let retry = request.retryable && attempt < 3 && deadline > now_ms() + 5000;
            let tx = self
                .db
                .transaction_with_behavior(TransactionBehavior::Immediate)?;
            let changed = tx.execute("UPDATE turn_jobs SET state=?1,lease='',ready_at=?2 WHERE id=?3 AND lease=?4 AND until>?5",params![if retry {"ready"}else{"failed"},now_ms()+2000,id,request.lease,now_ms()])?;
            if changed != 1 {
                return Err(Fault::conflict());
            }
            if !retry {
                tx.execute("UPDATE conversations SET status='failed',body=json_set(body,'$.status','failed') WHERE request_id=?1", [&request_id])?;
            }
            tx.commit()?;
            return Ok(());
        }
        let review = self.leased(actor, id, &request.lease)?;
        let attempt: u32 =
            self.db
                .query_row("SELECT attempt FROM jobs WHERE id=?1", [id], |r| r.get(0))?;
        let retry = request.retryable && attempt < 3 && review.expires_at_ms > now_ms() + 2000;
        self.db.execute(
            "UPDATE jobs SET state=?1,lease='',ready_at=?2 WHERE id=?3",
            params![
                if retry { "ready" } else { "failed" },
                now_ms() + i64::from(1u32 << attempt) * 1000,
                id
            ],
        )?;
        Ok(())
    }
    pub fn conversation(&mut self, actor: &Principal, request_id: &str) -> Result<Conversation> {
        actor.require(Role::Reviewer)?;
        let review = self.get(actor, request_id)?;
        let body: Option<String> = self
            .db
            .query_row(
                "SELECT body FROM conversations WHERE request_id=?1",
                [request_id],
                |r| r.get(0),
            )
            .optional()?;
        match body {
            Some(body) => decode(&body),
            None => Ok(Conversation {
                request_id: request_id.into(),
                queue: review.request.queue,
                messages: vec![],
                status: "idle".into(),
                active_model: String::new(),
            }),
        }
    }
    pub fn send_message(
        &mut self,
        actor: &Principal,
        request_id: &str,
        input: SendMessage,
    ) -> Result<Conversation> {
        actor.require(Role::Reviewer)?;
        let review = self.get(actor, request_id)?;
        if input.message.trim().is_empty()
            || input.message.len() > 2048
            || !identifier(&input.idempotency_key)
        {
            return Err(Fault::bad());
        }
        let fingerprint = fingerprint(&input)?;
        let previous: Option<String> = self
            .db
            .query_row(
                "SELECT fingerprint FROM conversation_keys WHERE request_id=?1 AND key=?2",
                params![request_id, input.idempotency_key],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(previous) = previous {
            return if previous == fingerprint {
                self.conversation(actor, request_id)
            } else {
                Err(Fault::conflict())
            };
        }
        let (revision, manifest) = self.manifest(&review.request.queue)?;
        let mut profile = manifest.assessment.ok_or_else(Fault::bad)?;
        if !profile.llm
            || input
                .model
                .as_ref()
                .is_some_and(|m| !profile.models.contains(m))
        {
            return Err(Fault::bad());
        }
        let active: i64 = self.db.query_row(
            "SELECT count(*) FROM turn_jobs WHERE state IN('ready','leased')",
            [],
            |r| r.get(0),
        )?;
        if active >= 512 {
            return Err(Fault {
                status: 429,
                code: "investigation_queue_full",
            });
        }
        profile.jev = false;
        let mut conversation = self.conversation(actor, request_id)?;
        if conversation.status == "running" || conversation.messages.len() >= 64 {
            return Err(Fault::conflict());
        }
        conversation.status = "running".into();
        if let Some(model) = &input.model {
            conversation.active_model = model.clone();
        }
        conversation.messages.push(ConversationMessage {
            id: id(),
            role: "user".into(),
            content: input.message,
            model: input.model.clone().unwrap_or_default(),
            at_ms: now_ms(),
        });
        let task = ConversationTask {
            messages: conversation.messages.clone(),
            model: input.model,
        };
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute(
            "INSERT INTO conversation_keys VALUES(?1,?2,?3)",
            params![request_id, input.idempotency_key, fingerprint],
        )?;
        tx.execute("INSERT INTO conversations VALUES(?1,?2,'running') ON CONFLICT(request_id) DO UPDATE SET body=excluded.body,status='running'",params![request_id,json(&conversation)?])?;
        tx.execute(
            "INSERT INTO turn_jobs VALUES(?1,?2,?3,'ready','','',0,0,?4,?5,?6,?7,?8)",
            params![
                format!("turn-{}", id()),
                request_id,
                review.request.queue,
                now_ms(),
                now_ms() + 90_000,
                json(&profile)?,
                json(&task)?,
                revision
            ],
        )?;
        Self::event(&tx, &review, "investigation_started", now_ms())?;
        tx.commit()?;
        Ok(conversation)
    }
    pub fn cancel_conversation(
        &mut self,
        actor: &Principal,
        request_id: &str,
    ) -> Result<Conversation> {
        let mut conversation = self.conversation(actor, request_id)?;
        if conversation.status != "running" {
            return Ok(conversation);
        }
        conversation.status = "cancelled".into();
        let review = self.get(actor, request_id)?;
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute("UPDATE turn_jobs SET state='cancelled',lease='' WHERE request_id=?1 AND state IN('ready','leased')",[request_id])?;
        tx.execute(
            "UPDATE conversations SET status='cancelled',body=?1 WHERE request_id=?2",
            params![json(&conversation)?, request_id],
        )?;
        Self::event(&tx, &review, "investigation_cancelled", now_ms())?;
        tx.commit()?;
        Ok(conversation)
    }
    fn claim_turn(&mut self, actor: &Principal) -> Result<Option<Job>> {
        let now = now_ms();
        self.db.execute("UPDATE turn_jobs SET state='ready',lease='',owner='' WHERE state='leased' AND until<=?1 AND attempt<3",[now])?;
        let failed=self.db.prepare("SELECT request_id FROM turn_jobs WHERE (state='leased' AND until<=?1 AND attempt>=3) OR (state IN('ready','leased') AND deadline<=?1)")?.query_map([now],|r|r.get::<_,String>(0))?.collect::<std::result::Result<Vec<_>,_>>()?;
        for request_id in failed {
            self.db.execute("UPDATE turn_jobs SET state='failed',lease='' WHERE request_id=?1 AND state IN('ready','leased')",[&request_id])?;
            self.db.execute("UPDATE conversations SET status='failed',body=json_set(body,'$.status','failed') WHERE request_id=?1",[&request_id])?;
        }
        let candidates=self.db.prepare("SELECT t.id,t.request_id,t.queue,t.attempt,t.deadline,t.profile,t.payload FROM turn_jobs t JOIN queues q ON q.id=t.queue AND q.revision=t.manifest_revision WHERE t.state='ready' AND t.ready_at<=?1 AND t.deadline>?1 ORDER BY t.ready_at LIMIT 2048")?.query_map([now],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,u32>(3)?,r.get::<_,i64>(4)?,r.get::<_,String>(5)?,r.get::<_,String>(6)?)))?.collect::<std::result::Result<Vec<_>,_>>()?;
        let Some((job_id, request_id, _, attempt, deadline, profile, task)) = candidates
            .into_iter()
            .find(|(_, _, q, _, _, _, _)| actor.queue(q))
        else {
            return Ok(None);
        };
        let lease = id();
        let until = deadline.min(now + 30_000);
        let changed=self.db.execute("UPDATE turn_jobs SET state='leased',lease=?1,owner=?2,until=?3,attempt=attempt+1 WHERE id=?4 AND state='ready'",params![lease,actor.spec.id,until,job_id])?;
        if changed != 1 {
            return Ok(None);
        };
        Ok(Some(Job {
            id: job_id,
            lease,
            attempt: attempt + 1,
            expires_at_ms: until,
            review: self.load(&request_id)?,
            profile: decode(&profile)?,
            conversation: Some(decode(&task)?),
        }))
    }
    fn complete_turn(
        &mut self,
        actor: &Principal,
        job_id: &str,
        input: CompleteJob,
    ) -> Result<Review> {
        actor.require(Role::Worker)?;
        let request_id:Option<String>=self.db.query_row("SELECT request_id FROM turn_jobs WHERE id=?1 AND owner=?2 AND lease=?3 AND state='leased' AND until>?4",params![job_id,actor.spec.id,input.lease,now_ms()],|r|r.get(0)).optional()?;
        let request_id = request_id.ok_or_else(Fault::conflict)?;
        let review = self.load(&request_id)?;
        if !actor.queue(&review.request.queue)
            || input.assessment.evidence_digest != review.evidence_digest
            || input.assessment.rationale.len() > 4096
            || input.assessment.llm_model.len() > 256
        {
            return Err(Fault::bad());
        }
        let body: String = self.db.query_row(
            "SELECT body FROM conversations WHERE request_id=?1",
            [&request_id],
            |r| r.get(0),
        )?;
        let mut conversation: Conversation = decode(&body)?;
        if conversation.status != "running" {
            return Err(Fault::conflict());
        }
        conversation.status = if input.assessment.state == "complete" {
            "idle"
        } else {
            "failed"
        }
        .into();
        conversation.active_model = input.assessment.llm_model.clone();
        conversation.messages.push(ConversationMessage {
            id: job_id.into(),
            role: "assistant".into(),
            content: input.assessment.rationale,
            model: input.assessment.llm_model,
            at_ms: now_ms(),
        });
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let changed=tx.execute("UPDATE turn_jobs SET state='completed',lease='' WHERE id=?1 AND owner=?2 AND lease=?3 AND state='leased' AND until>?4 AND deadline>?4",params![job_id,actor.spec.id,input.lease,now_ms()])?;
        if changed != 1 {
            return Err(Fault::conflict());
        }
        tx.execute(
            "UPDATE conversations SET body=?1,status=?2 WHERE request_id=?3",
            params![json(&conversation)?, conversation.status, request_id],
        )?;
        Self::event(&tx, &review, "investigation_completed", now_ms())?;
        tx.commit()?;
        Ok(review)
    }
    fn transport(actor: &Principal) -> Result<()> {
        if actor.spec.role != Role::Transport || actor.spec.realm.is_empty() {
            return Err(Fault::forbidden());
        }
        Ok(())
    }
    pub fn register_route(
        &self,
        actor: &Principal,
        route: RouteRegistration,
    ) -> Result<ReviewRoute> {
        Self::transport(actor)?;
        if !actor.spec.actors.contains(&route.actor)
            || !identifier(&route.channel)
            || route.event_micros <= 0
        {
            return Err(Fault::forbidden());
        }
        self.db.execute("INSERT INTO routes VALUES(?1,?2,?3) ON CONFLICT(realm) DO UPDATE SET channel=excluded.channel,at=excluded.at WHERE excluded.at>routes.at",params![actor.spec.realm,route.channel,route.event_micros])?;
        self.route(actor)?.ok_or_else(Fault::missing)
    }
    pub fn route(&self, actor: &Principal) -> Result<Option<ReviewRoute>> {
        Self::transport(actor)?;
        let channel: Option<String> = self
            .db
            .query_row(
                "SELECT channel FROM routes WHERE realm=?1",
                [&actor.spec.realm],
                |r| r.get(0),
            )
            .optional()?;
        Ok(channel.map(|channel| ReviewRoute {
            channel,
            realm: actor.spec.realm.clone(),
        }))
    }
    fn delivery(&mut self, actor: &Principal, id: &str) -> Result<Delivery> {
        Self::transport(actor)?;
        let row:Option<(String,Option<String>,String,String,String)>=self.db.query_row("SELECT channel,message,nonce,status,request_id FROM deliveries WHERE id=?1 AND realm=?2",params![id,actor.spec.realm],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).optional()?;
        let (channel, message, nonce, status, request_id) = row.ok_or_else(Fault::missing)?;
        let review = self.get(actor, &request_id)?;
        Ok(Delivery {
            id: id.into(),
            realm: actor.spec.realm.clone(),
            channel,
            message,
            nonce,
            review,
            status,
        })
    }
    pub fn reserve_delivery(
        &mut self,
        actor: &Principal,
        request: ReserveDelivery,
    ) -> Result<Delivery> {
        Self::transport(actor)?;
        let review = self.get(actor, &request.request_id)?;
        if review.state != ReviewState::Pending
            || request.revision != review.revision
            || review.mode == Mode::Shadow
        {
            return Err(Fault::conflict());
        }
        let existing: Option<String> = self
            .db
            .query_row(
                "SELECT id FROM deliveries WHERE realm=?1 AND request_id=?2 AND revision=?3",
                params![actor.spec.realm, review.id, review.revision],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(id) = existing {
            return self.delivery(actor, &id);
        }
        let route = self.route(actor)?.ok_or_else(Fault::missing)?;
        let recent: i64 = self.db.query_row(
            "SELECT count(*) FROM deliveries WHERE realm=?1 AND created>?2",
            params![actor.spec.realm, now_ms() - 60_000],
            |r| r.get(0),
        )?;
        if recent >= 10 {
            return Err(Fault {
                status: 429,
                code: "transport_rate_limit",
            });
        }
        let delivery_id = id();
        self.db.execute(
            "INSERT INTO deliveries VALUES(?1,?2,?3,?4,?5,NULL,?6,'reserved',?7,?8)",
            params![
                delivery_id,
                actor.spec.realm,
                review.id,
                review.revision,
                route.channel,
                credential()?,
                json(&review)?,
                now_ms()
            ],
        )?;
        let mut delivery = self.delivery(actor, &delivery_id)?;
        delivery.status = "newly_reserved".into();
        Ok(delivery)
    }
    pub fn deliveries(&mut self, actor: &Principal) -> Result<Vec<Delivery>> {
        Self::transport(actor)?;
        self.expire(now_ms())?;
        let ids = self
            .db
            .prepare("SELECT deliveries.id FROM deliveries JOIN requests ON requests.id=deliveries.request_id WHERE deliveries.realm=?1 AND requests.queue IN (SELECT value FROM json_each(?2)) ORDER BY deliveries.created DESC LIMIT 200")?
            .query_map(params![actor.spec.realm,json(&actor.spec.queues)?], |r| r.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        ids.into_iter()
            .map(|id| self.delivery(actor, &id))
            .collect()
    }
    pub fn delivered(
        &mut self,
        actor: &Principal,
        id: &str,
        request: Delivered,
    ) -> Result<Delivery> {
        let delivery = self.delivery(actor, id)?;
        if request.message.is_empty() || request.message.len() > 128 {
            return Err(Fault::bad());
        }
        if let Some(message) = delivery.message {
            return if message == request.message {
                self.delivery(actor, id)
            } else {
                Err(Fault::conflict())
            };
        }
        self.db.execute("UPDATE deliveries SET message=?1,status='delivered' WHERE id=?2 AND realm=?3 AND status='reserved'",params![request.message,id,actor.spec.realm])?;
        self.delivery(actor, id)
    }
    pub fn delivery_reply(
        &mut self,
        actor: &Principal,
        id: &str,
        reply: DeliveryReply,
    ) -> Result<Review> {
        let delivery = self.delivery(actor, id)?;
        if !actor.spec.actors.contains(&reply.actor)
            || delivery.status != "delivered"
            || delivery.channel != reply.channel
            || delivery.message.as_deref() != Some(reply.message.as_str())
            || delivery.nonce != reply.nonce
            || delivery.review.revision != self.load(&delivery.review.id)?.revision
        {
            return Err(Fault::forbidden());
        }
        let review = self.commit_decision(
            delivery.review,
            &reply.action_id,
            &format!("{}:{}", actor.spec.realm, reply.actor),
            now_ms(),
        )?;
        self.db.execute(
            "UPDATE deliveries SET status='settled' WHERE request_id=?1",
            [&review.id],
        )?;
        Ok(review)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn admin() -> Principal {
        Principal {
            spec: PrincipalSpec {
                id: "admin".into(),
                role: Role::Admin,
                queues: vec![],
                service_kind: String::new(),
                realm: String::new(),
                actors: vec![],
            },
        }
    }
    fn manifest() -> QueueManifest {
        QueueManifest {
            id: "example.review".into(),
            title: "Example".into(),
            service_kind: "example".into(),
            schema_id: "example.subject.v1".into(),
            subject_shape: Shape::Object {
                fields: [(
                    "target".into(),
                    Shape::String {
                        max_len: 128,
                        choices: vec![],
                    },
                )]
                .into(),
                required: vec!["target".into()],
            },
            actions: vec![
                ReviewAction {
                    id: "allow-once".into(),
                    label: "Allow once".into(),
                    effect: Effect::Permit,
                    scope: json!({"uses":1}),
                },
                ReviewAction {
                    id: "deny-once".into(),
                    label: "Deny once".into(),
                    effect: Effect::Block,
                    scope: json!({"uses":1}),
                },
            ],
            max_pending: 16,
            deadline_cap_ms: 90_000,
            scheduling_class: SchedulingClass::Background,
            mode: Mode::Manual,
            rules: vec![],
            assessment: None,
        }
    }
    fn make_producer(store: &mut Store, name: &str) -> Principal {
        let enrolled = store
            .enroll(
                &admin(),
                PrincipalSpec {
                    id: name.into(),
                    role: Role::Producer,
                    queues: vec!["example.review".into()],
                    service_kind: "example".into(),
                    realm: String::new(),
                    actors: vec![],
                },
            )
            .unwrap();
        store.authenticate(&enrolled.credential).unwrap()
    }
    fn fixture() -> (Store, Principal) {
        let mut store = Store::init(Connection::open_in_memory().unwrap()).unwrap();
        store.put_queue(&admin(), manifest()).unwrap();
        let producer = make_producer(&mut store, "producer");
        (store, producer)
    }
    fn submission() -> SubmitRequest {
        SubmitRequest {
            queue: "example.review".into(),
            producer_request_id: "local-1".into(),
            nonce: "unique-request-nonce-1234".into(),
            idempotency_key: "key-1".into(),
            schema_id: "example.subject.v1".into(),
            subject: json!({"target":"example.invalid"}),
            evidence: json!({"source":"fixture"}),
            action_ids: vec!["allow-once".into(), "deny-once".into()],
            expires_at_ms: now_ms() + 60_000,
            summary: "Example request".into(),
        }
    }
    fn decide() -> DecideRequest {
        DecideRequest {
            revision: 1,
            action_id: "allow-once".into(),
        }
    }
    fn expire_now(store: &mut Store, request: &Review) {
        let mut request = request.clone();
        request.expires_at_ms = now_ms() - 1;
        store
            .db
            .execute(
                "UPDATE requests SET body=?1,expires=?2 WHERE id=?3",
                params![json(&request).unwrap(), request.expires_at_ms, request.id],
            )
            .unwrap();
    }

    #[test]
    fn idempotency_is_scoped_and_payload_changes_conflict() {
        let (mut store, producer) = fixture();
        let input = submission();
        let request = store.submit(&producer, input.clone()).unwrap();
        assert_eq!(
            store.submit(&producer, input.clone()).unwrap().id,
            request.id
        );
        let mut changed = input;
        changed.subject = json!({"target":"other.invalid"});
        assert_eq!(store.submit(&producer, changed).unwrap_err().status, 409);
        let other = make_producer(&mut store, "other");
        assert_eq!(store.get(&other, &request.id).unwrap_err().status, 403);
    }

    #[test]
    fn producer_cannot_review_or_enroll_and_shape_cannot_expand() {
        let (mut store, producer) = fixture();
        let request = store.submit(&producer, submission()).unwrap();
        assert_eq!(
            store
                .decide(&producer, &request.id, decide())
                .unwrap_err()
                .status,
            403
        );
        let mut input = submission();
        input.idempotency_key = "invalid-shape".into();
        input.subject = json!({"target":"example.invalid","arbitrary_command":"execute"});
        assert_eq!(store.submit(&producer, input).unwrap_err().status, 400);
        assert!(store.enroll(&producer, producer.spec.clone()).is_err());
    }

    #[test]
    fn expiry_cancellation_revision_and_duplicate_decisions_fail_closed() {
        let (mut store, producer) = fixture();
        let request = store.submit(&producer, submission()).unwrap();
        let mut stale = decide();
        stale.revision = 2;
        assert_eq!(
            store
                .decide(&admin(), &request.id, stale)
                .unwrap_err()
                .status,
            409
        );
        expire_now(&mut store, &request);
        assert_eq!(
            store
                .decide(&admin(), &request.id, decide())
                .unwrap_err()
                .status,
            409
        );
        assert_eq!(
            store.get(&producer, &request.id).unwrap().state,
            ReviewState::Expired
        );
        let mut input = submission();
        input.idempotency_key = "new".into();
        let request = store.submit(&producer, input).unwrap();
        store.cancel(&producer, &request.id).unwrap();
        assert!(store.decide(&admin(), &request.id, decide()).is_err());
        let mut input = submission();
        input.idempotency_key = "last".into();
        let request = store.submit(&producer, input).unwrap();
        let settled = store.decide(&admin(), &request.id, decide()).unwrap();
        assert_eq!(settled.state, ReviewState::Allowed);
        assert!(
            store
                .decide(
                    &admin(),
                    &request.id,
                    DecideRequest {
                        revision: 1,
                        action_id: "deny-once".into()
                    }
                )
                .is_err()
        );
        assert!(store.cancel(&producer, &request.id).is_err());
    }

    #[test]
    fn decision_is_digest_scope_and_nonce_bound_and_receipt_is_idempotent() {
        let (mut store, producer) = fixture();
        let request = store.submit(&producer, submission()).unwrap();
        let settled = store.decide(&admin(), &request.id, decide()).unwrap();
        let decision = settled.decision.unwrap();
        assert_eq!(decision.nonce, request.request.nonce);
        assert_eq!(decision.subject_digest, request.subject_digest);
        assert_eq!(decision.evidence_digest, request.evidence_digest);
        assert_eq!(decision.action.scope, json!({"uses":1}));
        let receipt = EnforcementReceipt {
            decision_id: decision.id,
            state: EnforcementState::Enforced,
            reason: "fixture enforcer reports consumption".into(),
            observed_at_ms: now_ms(),
        };
        store
            .receipt(&producer, &request.id, receipt.clone())
            .unwrap();
        store
            .receipt(&producer, &request.id, receipt.clone())
            .unwrap();
        let mut changed = receipt;
        changed.state = EnforcementState::Unknown;
        assert_eq!(
            store
                .receipt(&producer, &request.id, changed)
                .unwrap_err()
                .status,
            409
        );
    }

    #[test]
    fn audit_failure_cannot_commit_permission() {
        let (mut store, producer) = fixture();
        let request = store.submit(&producer, submission()).unwrap();
        store.db.execute_batch("CREATE TRIGGER fixture_audit_failure BEFORE INSERT ON decisions BEGIN SELECT RAISE(ABORT,'fixture'); END;").unwrap();
        assert_eq!(
            store
                .decide(&admin(), &request.id, decide())
                .unwrap_err()
                .status,
            500
        );
        assert_eq!(
            store.get(&producer, &request.id).unwrap().state,
            ReviewState::Pending
        );
        assert!(
            store
                .get(&producer, &request.id)
                .unwrap()
                .decision
                .is_none()
        );
    }

    #[test]
    fn restart_preserves_pending_and_lease_fencing_rejects_stale_workers() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("approval.sqlite3");
        let token = root.path().join("admin.token");
        let mut store = Store::open(&path, &token).unwrap();
        let admin = store
            .authenticate(&fs::read_to_string(&token).unwrap())
            .unwrap();
        let mut queue = manifest();
        queue.assessment = Some(AssessmentProfile {
            instructions: "Classify the fixture".into(),
            labels: [("unknown".into(), "Insufficient evidence".into())].into(),
            subject_fields: vec!["target".into()],
            ..Default::default()
        });
        store.put_queue(&admin, queue).unwrap();
        let producer = make_producer(&mut store, "producer");
        let request = store.submit(&producer, submission()).unwrap();
        let enrolled = store
            .enroll(
                &admin,
                PrincipalSpec {
                    id: "worker".into(),
                    role: Role::Worker,
                    queues: vec!["example.review".into()],
                    service_kind: String::new(),
                    realm: String::new(),
                    actors: vec![],
                },
            )
            .unwrap();
        let worker = store.authenticate(&enrolled.credential).unwrap();
        let first = store
            .claim(
                &worker,
                ClaimJob {
                    kind: "assessment".into(),
                },
            )
            .unwrap()
            .unwrap();
        drop(store);
        let mut store = Store::open(&path, &token).unwrap();
        assert_eq!(
            store.get(&producer, &request.id).unwrap().state,
            ReviewState::Pending
        );
        store.db.execute("UPDATE jobs SET until=0", []).unwrap();
        let second = store
            .claim(
                &worker,
                ClaimJob {
                    kind: "assessment".into(),
                },
            )
            .unwrap()
            .unwrap();
        assert_ne!(first.lease, second.lease);
        let assessment = Assessment {
            state: "complete".into(),
            category: "unknown".into(),
            confidence: Some(0.5),
            rationale: "fixture".into(),
            jev_model: "fixture".into(),
            llm_model: String::new(),
            evidence_digest: request.evidence_digest,
            completed_at_ms: now_ms(),
        };
        assert!(
            store
                .complete(
                    &worker,
                    &first.id,
                    CompleteJob {
                        lease: first.lease,
                        assessment: assessment.clone()
                    }
                )
                .is_err()
        );
        let result = store
            .complete(
                &worker,
                &second.id,
                CompleteJob {
                    lease: second.lease,
                    assessment,
                },
            )
            .unwrap();
        assert_eq!(result.state, ReviewState::Pending);
        assert!(result.decision.is_none());
    }

    #[test]
    fn transport_reply_is_bound_authorized_and_one_shot() {
        let (mut store, producer) = fixture();
        let request = store.submit(&producer, submission()).unwrap();
        let enrolled = store
            .enroll(
                &admin(),
                PrincipalSpec {
                    id: "slack".into(),
                    role: Role::Transport,
                    queues: vec!["example.review".into()],
                    service_kind: String::new(),
                    realm: "slack:T1".into(),
                    actors: vec!["U1".into()],
                },
            )
            .unwrap();
        let worker = store.authenticate(&enrolled.credential).unwrap();
        assert!(
            store
                .register_route(
                    &worker,
                    RouteRegistration {
                        channel: "C1".into(),
                        actor: "OTHER".into(),
                        event_micros: 1
                    }
                )
                .is_err()
        );
        store
            .register_route(
                &worker,
                RouteRegistration {
                    channel: "C1".into(),
                    actor: "U1".into(),
                    event_micros: 1,
                },
            )
            .unwrap();
        let delivery = store
            .reserve_delivery(
                &worker,
                ReserveDelivery {
                    request_id: request.id.clone(),
                    revision: 1,
                },
            )
            .unwrap();
        assert_eq!(
            store
                .reserve_delivery(
                    &worker,
                    ReserveDelivery {
                        request_id: request.id.clone(),
                        revision: 1
                    }
                )
                .unwrap()
                .id,
            delivery.id
        );
        store
            .delivered(
                &worker,
                &delivery.id,
                Delivered {
                    message: "1.000001".into(),
                },
            )
            .unwrap();
        let mut reply = DeliveryReply {
            actor: "U1".into(),
            channel: "C1".into(),
            message: "1.000001".into(),
            nonce: "wrong".into(),
            action_id: "allow-once".into(),
        };
        assert!(
            store
                .delivery_reply(&worker, &delivery.id, reply.clone())
                .is_err()
        );
        reply.nonce = delivery.nonce;
        let settled = store
            .delivery_reply(&worker, &delivery.id, reply.clone())
            .unwrap();
        assert_eq!(settled.state, ReviewState::Allowed);
        assert!(store.delivery_reply(&worker, &delivery.id, reply).is_err());
    }

    #[test]
    fn shadow_decisions_never_grant_and_queue_changes_cancel_old_consent() {
        let (mut store, producer) = fixture();
        let mut queue = manifest();
        queue.mode = Mode::Shadow;
        store.put_queue(&admin(), queue).unwrap();
        let request = store.submit(&producer, submission()).unwrap();
        let result = store.decide(&admin(), &request.id, decide()).unwrap();
        assert_eq!(result.state, ReviewState::Observed);
        assert!(!result.decision.unwrap().authoritative);
        let mut input = submission();
        input.idempotency_key = "second".into();
        let request = store.submit(&producer, input).unwrap();
        store.put_queue(&admin(), manifest()).unwrap();
        assert_eq!(
            store.get(&producer, &request.id).unwrap().state,
            ReviewState::Cancelled
        );
        assert!(store.decide(&admin(), &request.id, decide()).is_err());
    }
    fn with_chat() -> (Store, Principal, Principal) {
        let (mut store, producer) = fixture();
        let mut queue = manifest();
        queue.assessment = Some(AssessmentProfile {
            instructions: "Explain uncertainty".into(),
            labels: [("unknown".into(), "Unknown".into())].into(),
            llm: true,
            models: vec!["fixture".into()],
            ..Default::default()
        });
        store.put_queue(&admin(), queue).unwrap();
        let enrolled = store
            .enroll(
                &admin(),
                PrincipalSpec {
                    id: "worker".into(),
                    role: Role::Worker,
                    queues: vec!["example.review".into()],
                    service_kind: String::new(),
                    realm: String::new(),
                    actors: vec![],
                },
            )
            .unwrap();
        let worker = store.authenticate(&enrolled.credential).unwrap();
        (store, producer, worker)
    }
    fn message(key: &str) -> SendMessage {
        SendMessage {
            message: "What evidence is missing?".into(),
            model: Some("fixture".into()),
            idempotency_key: key.into(),
        }
    }
    fn answer(job: &Job) -> CompleteJob {
        CompleteJob {
            lease: job.lease.clone(),
            assessment: Assessment {
                state: "complete".into(),
                category: String::new(),
                confidence: None,
                rationale: "No additional evidence is available.".into(),
                jev_model: String::new(),
                llm_model: "fixture".into(),
                evidence_digest: job.review.evidence_digest.clone(),
                completed_at_ms: now_ms(),
            },
        }
    }
    #[test]
    fn historical_investigation_is_idempotent_advisory_and_cancellation_fences_workers() {
        let (mut store, producer, worker) = with_chat();
        let review = store.submit(&producer, submission()).unwrap();
        expire_now(&mut store, &review);
        let chat = store
            .send_message(&admin(), &review.id, message("first"))
            .unwrap();
        assert_eq!(chat.messages.len(), 1);
        assert_eq!(
            store
                .send_message(&admin(), &review.id, message("first"))
                .unwrap()
                .messages
                .len(),
            1
        );
        assert!(
            store
                .send_message(&producer, &review.id, message("forbidden"))
                .is_err()
        );
        let mut wrong = message("wrong");
        wrong.model = Some("unregistered".into());
        assert_eq!(
            store
                .send_message(&admin(), &review.id, wrong)
                .unwrap_err()
                .status,
            400
        );
        let job = store
            .claim(
                &worker,
                ClaimJob {
                    kind: "assessment_or_investigation".into(),
                },
            )
            .unwrap()
            .unwrap();
        assert!(job.conversation.is_some());
        let result = store.complete(&worker, &job.id, answer(&job)).unwrap();
        assert_eq!(result.state, ReviewState::Expired);
        assert!(result.decision.is_none());
        assert_eq!(
            store
                .conversation(&admin(), &review.id)
                .unwrap()
                .messages
                .len(),
            2
        );
        store
            .send_message(&admin(), &review.id, message("second"))
            .unwrap();
        let job = store
            .claim(
                &worker,
                ClaimJob {
                    kind: "assessment_or_investigation".into(),
                },
            )
            .unwrap()
            .unwrap();
        store.cancel_conversation(&admin(), &review.id).unwrap();
        assert_eq!(
            store
                .complete(&worker, &job.id, answer(&job))
                .unwrap_err()
                .status,
            409
        );
        store
            .send_message(&admin(), &review.id, message("third"))
            .unwrap();
        let job = store
            .claim(
                &worker,
                ClaimJob {
                    kind: "assessment_or_investigation".into(),
                },
            )
            .unwrap()
            .unwrap();
        store.put_queue(&admin(), manifest()).unwrap();
        assert_eq!(
            store
                .complete(&worker, &job.id, answer(&job))
                .unwrap_err()
                .status,
            409
        );
    }
    #[test]
    fn credential_rotation_revokes_old_key_and_pending_consent() {
        let (mut store, _) = fixture();
        let enrolled = store
            .enroll(
                &admin(),
                PrincipalSpec {
                    id: "rotate-me".into(),
                    role: Role::Producer,
                    queues: vec!["example.review".into()],
                    service_kind: "example".into(),
                    realm: String::new(),
                    actors: vec![],
                },
            )
            .unwrap();
        let producer = store.authenticate(&enrolled.credential).unwrap();
        let review = store.submit(&producer, submission()).unwrap();
        let rotated = store.rotate(&admin(), "rotate-me").unwrap();
        assert!(store.authenticate(&enrolled.credential).is_err());
        assert!(store.authenticate(&rotated.credential).is_ok());
        assert_eq!(
            store.get(&admin(), &review.id).unwrap().state,
            ReviewState::Cancelled
        );
        store.revoke(&admin(), "rotate-me").unwrap();
        assert!(store.authenticate(&rotated.credential).is_err());
        assert!(store.revoke(&admin(), "local-admin").is_err());
    }
    #[test]
    fn background_pressure_preserves_capacity_for_interactive_requests() {
        let (mut store, producer) = fixture();
        // Populate pressure rows directly so the test covers admission without a costly fixture workload.
        let review = store.submit(&producer, submission()).unwrap();
        let body = json(&review).unwrap();
        for index in 0..1535 {
            store.db.execute("INSERT INTO requests(id,producer,queue,idempotency,fingerprint,state,expires,body) VALUES(?1,'fixture','other',?1,'fixture','pending',?2,?3)",params![format!("pressure-{index}"),now_ms()+60_000,body]).unwrap();
        }
        let mut input = submission();
        input.idempotency_key = "pressure-background".into();
        assert_eq!(
            store.submit(&producer, input.clone()).unwrap_err().status,
            429
        );
        let mut queue = manifest();
        queue.scheduling_class = SchedulingClass::Interactive;
        // Update fixture manifest without invalidating the pressure population.
        store
            .db
            .execute(
                "UPDATE queues SET manifest=?1 WHERE id=?2",
                params![json(&queue).unwrap(), queue.id],
            )
            .unwrap();
        input.idempotency_key = "pressure-interactive".into();
        assert!(store.submit(&producer, input).is_ok());
    }
    #[test]
    fn monitor_sees_readiness_without_request_or_decision_authority() {
        let (mut store, producer) = fixture();
        let review = store.submit(&producer, submission()).unwrap();
        let enrolled = store
            .enroll(
                &admin(),
                PrincipalSpec {
                    id: "monitor".into(),
                    role: Role::Monitor,
                    queues: vec!["example.review".into()],
                    service_kind: String::new(),
                    realm: String::new(),
                    actors: vec![],
                },
            )
            .unwrap();
        let monitor = store.authenticate(&enrolled.credential).unwrap();
        assert_eq!(store.health(&monitor).unwrap().queues.len(), 1);
        assert!(store.get(&monitor, &review.id).is_err());
        assert!(
            store
                .list(&monitor, i64::MAX, "all")
                .unwrap()
                .reviews
                .is_empty()
        );
        assert!(store.events(&monitor, 0).is_err());
        assert!(
            store
                .claim(
                    &monitor,
                    ClaimJob {
                        kind: "assessment".into()
                    }
                )
                .is_err()
        );
        assert!(store.decide(&monitor, &review.id, decide()).is_err());
        assert!(store.submit(&monitor, submission()).is_err());
    }
}

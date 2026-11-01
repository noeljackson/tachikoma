//! Bounded, non-authoritative daemon activity. Never records argv or environment.
use crate::{model::now, protocol};
use anyhow::{Result, ensure};
use rusqlite::{Connection, OpenFlags, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};
use tokio::sync::mpsc;
const BATCH: usize = 512;
const ROWS: i64 = 50_000;
const STORAGE_BYTES: i64 = 128 * 1024 * 1024;
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Status {
    pub received: u64,
    pub persisted: u64,
    pub duplicates: u64,
    pub dropped: u64,
    pub errors: u64,
}
#[derive(Default)]
struct Counters {
    received: AtomicU64,
    persisted: AtomicU64,
    duplicates: AtomicU64,
    dropped: AtomicU64,
    errors: AtomicU64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub observed_ns: i64,
    pub received_at: i64,
    pub uid: u32,
    pub pid: u32,
    pub executable: String,
    pub protocol: String,
    pub ip: String,
    pub hostname: String,
    pub port: u16,
    pub reported_action: String,
    pub rule_ref: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Row {
    pub sequence: i64,
    pub event: Event,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Page {
    pub events: Vec<Row>,
    pub next_before: Option<i64>,
    pub status: Status,
    pub coverage: String,
}
#[derive(Clone)]
pub struct Recorder {
    sender: mpsc::Sender<Vec<Event>>,
    counters: Arc<Counters>,
    path: PathBuf,
    session: String,
}
impl Recorder {
    pub fn start(path: PathBuf) -> Self {
        let (sender, mut receiver) = mpsc::channel::<Vec<Event>>(8);
        let counters = Arc::new(Counters::default());
        let saved = counters.clone();
        let file = path.clone();
        let session = uuid::Uuid::new_v4().to_string();
        let identity = session.clone();
        tokio::task::spawn_blocking(move || {
            let mut db = open(&file).ok();
            let mut warned = db.is_none();
            if db.is_none() {
                eprintln!("Connection activity storage unavailable; approval service continues.");
                saved.errors.fetch_add(1, Ordering::Relaxed);
            }
            while let Some(events) = receiver.blocking_recv() {
                if db.is_none() {
                    db = open(&file).ok();
                }
                let result = match db.as_mut() {
                    Some(db) => write(db, &identity, &events),
                    None => Err(anyhow::anyhow!("activity storage unavailable")),
                };
                match result {
                    Ok(inserted) => {
                        warned = false;
                        saved.persisted.fetch_add(inserted, Ordering::Relaxed);
                        saved
                            .duplicates
                            .fetch_add(events.len() as u64 - inserted, Ordering::Relaxed);
                    }
                    Err(_) => {
                        if !warned {
                            eprintln!(
                                "Connection activity write failed; inspect recording counters. Approval service continues."
                            );
                            warned = true;
                        }
                        saved.errors.fetch_add(1, Ordering::Relaxed);
                        saved
                            .dropped
                            .fetch_add(events.len() as u64, Ordering::Relaxed);
                        db = None;
                    }
                }
            }
            if let Some(db) = db {
                let _ = db.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)");
            }
        });
        Self {
            sender,
            counters,
            path,
            session,
        }
    }
    pub fn status(&self) -> Status {
        let c = &self.counters;
        Status {
            received: c.received.load(Ordering::Relaxed),
            persisted: c.persisted.load(Ordering::Relaxed),
            duplicates: c.duplicates.load(Ordering::Relaxed),
            dropped: c.dropped.load(Ordering::Relaxed),
            errors: c.errors.load(Ordering::Relaxed),
        }
    }
    pub fn record(&self, stats: protocol::Statistics) {
        let count = stats.events.len();
        self.counters
            .received
            .fetch_add(count as u64, Ordering::Relaxed);
        let events: Vec<_> = stats
            .events
            .into_iter()
            .take(BATCH)
            .filter_map(sanitize)
            .collect();
        self.counters
            .dropped
            .fetch_add((count - events.len()) as u64, Ordering::Relaxed);
        if events.is_empty() {
            return;
        }
        if let Err(error) = self.sender.try_send(events) {
            self.counters
                .dropped
                .fetch_add(error.into_inner().len() as u64, Ordering::Relaxed);
        }
    }
    pub fn page(&self, before: Option<i64>, limit: usize) -> Result<Page> {
        ensure!(
            (1..=100).contains(&limit) && before.is_none_or(|n| n > 0),
            "invalid activity page"
        );
        let db = Connection::open_with_flags(
            &self.path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )?;
        let events = db
            .prepare("SELECT seq,body FROM connections WHERE seq<?1 ORDER BY seq DESC LIMIT ?2")?
            .query_map(params![before.unwrap_or(i64::MAX), limit as i64], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
            })?
            .map(|row| {
                let (sequence, body) = row?;
                Ok(Row {
                    sequence,
                    event: serde_json::from_str(&body)?,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        let next_before = (events.len() == limit).then(|| events.last().unwrap().sequence);
        Ok(Page {
            events,
            next_before,
            status: self.status(),
            coverage: format!(
                "Daemon-reported activity only; not approvals or complete packet capture. Deduplicated within broker session {}. Counters reset on broker restart; older GUI history is untouched.",
                self.session
            ),
        })
    }
}
fn sanitize(event: protocol::Event) -> Option<Event> {
    if event.unixnano <= 0 {
        return None;
    }
    let connection = event.connection?;
    let rule = event.rule?;
    if connection.process_path.len() > 4096 || connection.dst_host.len() > 253 {
        return None;
    }
    let ip = connection
        .dst_ip
        .parse::<std::net::IpAddr>()
        .ok()?
        .to_string();
    let port = u16::try_from(connection.dst_port).ok()?;
    let protocol = connection.protocol.to_ascii_lowercase();
    if !matches!(protocol.as_str(), "tcp" | "udp") {
        return None;
    }
    let action = if matches!(rule.action.as_str(), "allow" | "deny" | "reject") {
        rule.action
    } else {
        "unknown".into()
    };
    Some(Event {
        observed_ns: event.unixnano,
        received_at: now() as i64,
        uid: connection.user_id,
        pid: connection.process_id,
        executable: crate::model::display_text(&connection.process_path),
        protocol,
        ip,
        hostname: crate::model::display_text(&connection.dst_host),
        port,
        reported_action: action,
        rule_ref: format!("{:x}", Sha256::digest(rule.name.as_bytes())),
    })
}
fn open(path: &Path) -> Result<Connection> {
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
    {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error.into()),
    }
    let metadata = std::fs::symlink_metadata(path)?;
    ensure!(
        metadata.is_file()
            && !metadata.file_type().is_symlink()
            && metadata.uid() == std::fs::metadata("/proc/self")?.uid()
            && metadata.mode() & 0o077 == 0,
        "activity database must be private and owned by this user"
    );
    let db = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )?;
    let page_size: i64 = db.query_row("PRAGMA page_size", [], |row| row.get(0))?;
    db.pragma_update(None, "max_page_count", STORAGE_BYTES / page_size)?;
    db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA busy_timeout=250; PRAGMA wal_autocheckpoint=256;
        CREATE TABLE IF NOT EXISTS connections(seq INTEGER PRIMARY KEY AUTOINCREMENT,session TEXT NOT NULL,key TEXT NOT NULL,received INTEGER NOT NULL,body TEXT NOT NULL,UNIQUE(session,key));
        CREATE INDEX IF NOT EXISTS connections_received ON connections(received);")?;
    Ok(db)
}
fn write(db: &mut Connection, session: &str, events: &[Event]) -> Result<u64> {
    let pages: i64 = db.query_row("PRAGMA page_count", [], |row| row.get(0))?;
    let free: i64 = db.query_row("PRAGMA freelist_count", [], |row| row.get(0))?;
    let page_size: i64 = db.query_row("PRAGMA page_size", [], |row| row.get(0))?;
    let transaction = db.transaction()?;
    // Reuse old pages before reaching the byte ceiling, even with long paths.
    if (pages - free) * page_size > STORAGE_BYTES * 3 / 4 {
        transaction.execute("DELETE FROM connections WHERE seq IN (SELECT seq FROM connections ORDER BY seq LIMIT 10000)", [])?;
    }
    // Retention is by receipt time, independently of the daemon's clock.
    transaction.execute("DELETE FROM connections WHERE received<?1 OR seq IN (SELECT seq FROM connections ORDER BY seq DESC LIMIT -1 OFFSET ?2)",params![now().saturating_sub(7*86400) as i64,ROWS-events.len() as i64])?;
    let mut inserted = 0;
    {
        let mut statement = transaction.prepare(
            "INSERT OR IGNORE INTO connections(session,key,received,body) VALUES(?1,?2,?3,?4)",
        )?;
        for event in events {
            let mut identity = event.clone();
            identity.received_at = 0;
            let key = format!("{:x}", Sha256::digest(serde_json::to_vec(&identity)?));
            inserted += statement.execute(params![
                session,
                key,
                event.received_at,
                serde_json::to_string(event)?
            ])? as u64;
        }
    }
    transaction.commit()?;
    Ok(inserted)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn event() -> protocol::Event {
        protocol::Event {
            unixnano: now() as i64 * 1_000_000_000,
            connection: Some(protocol::Connection {
                dst_ip: "192.0.2.1".into(),
                dst_host: "example.invalid".into(),
                dst_port: 443,
                protocol: "tcp".into(),
                process_path: "/usr/bin/example".into(),
                user_id: 1000,
                process_id: 42,
                process_cwd: "PRIVATE_CWD".into(),
                process_args: vec!["PRIVATE_ARG".into()],
                process_env: std::collections::HashMap::from([(
                    "KEY".into(),
                    "PRIVATE_ENV".into(),
                )]),
                ..Default::default()
            }),
            rule: Some(protocol::Rule {
                name: "PRIVATE_RULE".into(),
                action: "allow".into(),
                ..Default::default()
            }),
            ..Default::default()
        }
    }
    #[tokio::test]
    async fn daemon_events_are_private_deduplicated_and_never_approval_records() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("activity.sqlite3");
        let recorder = Recorder::start(path.clone());
        recorder.record(protocol::Statistics {
            events: {
                let event = event();
                vec![event.clone(), event]
            },
            ..Default::default()
        });
        for _ in 0..100 {
            if recorder.status().persisted == 1 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        let page = recorder.page(None, 10).unwrap();
        assert_eq!(page.events.len(), 1);
        assert_eq!(page.status.duplicates, 1);
        let text = serde_json::to_string(&page).unwrap();
        for secret in ["PRIVATE_ARG", "PRIVATE_ENV", "PRIVATE_CWD", "PRIVATE_RULE"] {
            assert!(!text.contains(secret));
        }
        assert_eq!(page.events[0].event.reported_action, "allow");
        assert_eq!(std::fs::metadata(&path).unwrap().mode() & 0o777, 0o600);
        let db = Connection::open(&path).unwrap();
        let approvals: i64 = db
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE name='decisions'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(approvals, 0);
        assert!(recorder.page(None, 101).is_err());
        recorder.record(protocol::Statistics {
            events: vec![protocol::Event::default(); BATCH + 2],
            ..Default::default()
        });
        assert!(recorder.status().dropped >= BATCH as u64 + 2);
    }
    #[test]
    fn private_storage_and_retention_are_enforced() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("events.sqlite3");
        let mut db = open(&path).unwrap();
        let mut old = sanitize(event()).unwrap();
        old.received_at = 0;
        write(&mut db, "fixture", &[old]).unwrap();
        write(&mut db, "fixture", &[sanitize(event()).unwrap()]).unwrap();
        let count: i64 = db
            .query_row("SELECT count(*) FROM connections", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1);
        let link = root.path().join("link.sqlite3");
        std::os::unix::fs::symlink(&path, &link).unwrap();
        assert!(open(&link).is_err());
    }
}

//! Opt-in schema-v3 history compatibility. No approval or daemon-management authority.
use crate::{config::OpenSnitchDatabase, model::now, protocol};
use anyhow::{Result, ensure};
use chrono::{Local, TimeZone};
use rusqlite::{Connection, OpenFlags, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    fs::{File, OpenOptions},
    os::{
        fd::AsRawFd,
        unix::fs::{MetadataExt, OpenOptionsExt},
    },
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::sync::mpsc;

const EVENTS: usize = 1024;
const SUMMARY: usize = 1024;
const TABLES: [&str; 5] = ["hosts", "procs", "addrs", "ports", "users"];
const SUMMARY_SQL: [&str; 5] = [
    "INSERT INTO hosts(what,hits) VALUES(?1,?2) ON CONFLICT(what) DO UPDATE SET hits=excluded.hits",
    "INSERT INTO procs(what,hits) VALUES(?1,?2) ON CONFLICT(what) DO UPDATE SET hits=excluded.hits",
    "INSERT INTO addrs(what,hits) VALUES(?1,?2) ON CONFLICT(what) DO UPDATE SET hits=excluded.hits",
    "INSERT INTO ports(what,hits) VALUES(?1,?2) ON CONFLICT(what) DO UPDATE SET hits=excluded.hits",
    "INSERT INTO users(what,hits) VALUES(?1,?2) ON CONFLICT(what) DO UPDATE SET hits=excluded.hits",
];
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Status {
    pub ready: bool,
    pub received: u64,
    pub persisted: u64,
    pub duplicates: u64,
    pub dropped: u64,
    pub errors: u64,
    pub summary_dropped: u64,
    pub last_write_at: u64,
}
#[derive(Default)]
struct Counters {
    ready: AtomicBool,
    received: AtomicU64,
    persisted: AtomicU64,
    duplicates: AtomicU64,
    dropped: AtomicU64,
    errors: AtomicU64,
    summary_dropped: AtomicU64,
    last_write_at: AtomicU64,
}
#[derive(Clone, Default)]
struct Identity {
    hostname: String,
    version: String,
}
struct Row {
    timestamp: i64,
    action: String,
    protocol: String,
    src_ip: String,
    src_port: u16,
    dst_ip: String,
    host: String,
    dst_port: u16,
    uid: u32,
    pid: u32,
    process: String,
    rule: String,
}
struct Batch {
    identity: Identity,
    daemon_version: String,
    uptime: u64,
    rules: u64,
    connections: u64,
    dropped: u64,
    rows: Vec<Row>,
    summaries: [Vec<(String, i64)>; 5],
}
#[derive(Clone)]
pub struct Sink {
    sender: mpsc::Sender<Batch>,
    counters: Arc<Counters>,
    identity: Arc<Mutex<Identity>>,
}
impl Sink {
    pub fn start(config: OpenSnitchDatabase) -> Self {
        let (sender, mut receiver) = mpsc::channel::<Batch>(4);
        let counters = Arc::new(Counters::default());
        let saved = counters.clone();
        tokio::task::spawn_blocking(move || {
            let mut writer = Writer::open(&config).ok();
            let mut warned = writer.is_none();
            if warned {
                saved.errors.fetch_add(1, Ordering::Relaxed);
                eprintln!(
                    "OpenSnitch history writer unavailable; approval service continues. Check database schema, ownership and writer lease."
                );
            }
            saved.ready.store(!warned, Ordering::Relaxed);
            let mut retry = Instant::now();
            while let Some(batch) = receiver.blocking_recv() {
                if writer.is_none() && retry.elapsed() >= Duration::from_secs(1) {
                    writer = Writer::open(&config).ok();
                    retry = Instant::now();
                }
                let result = writer.as_mut().map(|w| w.write(&batch));
                match result {
                    Some(Ok(inserted)) => {
                        saved.persisted.fetch_add(inserted, Ordering::Relaxed);
                        saved
                            .duplicates
                            .fetch_add(batch.rows.len() as u64 - inserted, Ordering::Relaxed);
                        saved.last_write_at.store(now(), Ordering::Relaxed);
                        saved.ready.store(true, Ordering::Relaxed);
                        warned = false;
                    }
                    failure => {
                        if !warned {
                            eprintln!(
                                "OpenSnitch history writer paused; inspect recording counters. Approval service continues."
                            );
                            warned = true;
                        }
                        if failure.is_some() {
                            saved.errors.fetch_add(1, Ordering::Relaxed);
                        }
                        saved
                            .dropped
                            .fetch_add(batch.rows.len() as u64, Ordering::Relaxed);
                        saved.ready.store(false, Ordering::Relaxed);
                        saved.summary_dropped.fetch_add(
                            batch.summaries.iter().map(Vec::len).sum::<usize>() as u64,
                            Ordering::Relaxed,
                        );
                        writer = None;
                    }
                }
            }
        });
        Self {
            sender,
            counters,
            identity: Arc::new(Mutex::new(Identity::default())),
        }
    }
    pub fn identity(&self, hostname: &str, version: &str) {
        *self.identity.lock().expect("history identity") = Identity {
            hostname: text(hostname, 253),
            version: text(version, 128),
        };
    }
    /// Only bounded whitelisted metadata crosses this queue. No SQLite work on the RPC task.
    pub fn record(&self, stats: &protocol::Statistics) {
        self.counters
            .received
            .fetch_add(stats.events.len() as u64, Ordering::Relaxed);
        let rows: Vec<_> = stats.events.iter().take(EVENTS).filter_map(row).collect();
        self.counters
            .dropped
            .fetch_add((stats.events.len() - rows.len()) as u64, Ordering::Relaxed);
        let maps = [
            &stats.by_host,
            &stats.by_executable,
            &stats.by_address,
            &stats.by_port,
            &stats.by_uid,
        ];
        let summaries: [Vec<(String, i64)>; 5] = std::array::from_fn(|index| {
            maps[index]
                .iter()
                .take(SUMMARY)
                .filter(|(key, _)| {
                    !key.is_empty() && key.len() <= if index == 1 { 4096 } else { 253 }
                })
                .map(|(key, count)| {
                    (
                        crate::model::display_text(key),
                        i64::try_from(*count).unwrap_or(i64::MAX),
                    )
                })
                .collect()
        });
        self.counters.summary_dropped.fetch_add(
            (maps.iter().map(|m| m.len()).sum::<usize>()
                - summaries.iter().map(Vec::len).sum::<usize>()) as u64,
            Ordering::Relaxed,
        );
        let batch = Batch {
            identity: self.identity.lock().expect("history identity").clone(),
            daemon_version: text(&stats.daemon_version, 128),
            uptime: stats.uptime,
            rules: stats.rules,
            connections: stats.connections,
            dropped: stats.dropped,
            rows,
            summaries,
        };
        if let Err(error) = self.sender.try_send(batch) {
            let batch = error.into_inner();
            self.counters.summary_dropped.fetch_add(
                batch.summaries.iter().map(Vec::len).sum::<usize>() as u64,
                Ordering::Relaxed,
            );
            self.counters
                .dropped
                .fetch_add(batch.rows.len() as u64, Ordering::Relaxed);
        }
    }
    pub fn status(&self) -> Status {
        let c = &self.counters;
        Status {
            ready: c.ready.load(Ordering::Relaxed),
            received: c.received.load(Ordering::Relaxed),
            persisted: c.persisted.load(Ordering::Relaxed),
            duplicates: c.duplicates.load(Ordering::Relaxed),
            dropped: c.dropped.load(Ordering::Relaxed),
            errors: c.errors.load(Ordering::Relaxed),
            summary_dropped: c.summary_dropped.load(Ordering::Relaxed),
            last_write_at: c.last_write_at.load(Ordering::Relaxed),
        }
    }
}
fn text(value: &str, limit: usize) -> String {
    if value.len() > limit {
        String::new()
    } else {
        crate::model::display_text(value)
    }
}
fn row(event: &protocol::Event) -> Option<Row> {
    if event.unixnano <= 0 {
        return None;
    }
    let c = event.connection.as_ref()?;
    if c.process_path.len() > 4096
        || c.dst_host.len() > 253
        || c.protocol.is_empty()
        || c.protocol.len() > 16
        || !c
            .protocol
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
    {
        return None;
    }
    let rule = event.rule.as_ref();
    Some(Row {
        timestamp: event.unixnano,
        action: rule
            .map(|r| r.action.as_str())
            .filter(|a| matches!(*a, "allow" | "deny" | "reject"))
            .unwrap_or("unknown")
            .into(),
        protocol: c.protocol.to_ascii_lowercase(),
        src_ip: if c.src_ip.is_empty() {
            String::new()
        } else {
            c.src_ip.parse::<std::net::IpAddr>().ok()?.to_string()
        },
        src_port: u16::try_from(c.src_port).ok()?,
        dst_ip: c.dst_ip.parse::<std::net::IpAddr>().ok()?.to_string(),
        host: crate::model::display_text(&c.dst_host),
        dst_port: u16::try_from(c.dst_port).ok()?,
        uid: c.user_id,
        pid: c.process_id,
        process: crate::model::display_text(&c.process_path),
        rule: rule
            .map(|r| {
                if r.name.len() <= 512 {
                    crate::model::display_text(&r.name)
                } else {
                    format!("sha256:{:x}", Sha256::digest(r.name.as_bytes()))
                }
            })
            .unwrap_or_default(),
    })
}
fn timestamp(ns: i64) -> String {
    Local
        .timestamp_nanos(ns)
        .format("%Y-%m-%d %H:%M:%S%.6f")
        .to_string()
}
struct Writer {
    db: Connection,
    path: PathBuf,
    node: String,
    inode: (u64, u64),
    schema: i64,
    users: HashMap<String, String>,
    _lease: File,
}
fn metadata(path: &Path) -> Result<std::fs::Metadata> {
    let meta = std::fs::symlink_metadata(path)?;
    ensure!(
        meta.is_file()
            && !meta.file_type().is_symlink()
            && meta.uid() == std::fs::metadata("/proc/self")?.uid()
            && meta.mode() & 0o022 == 0,
        "history file must be a regular owned file without shared write permissions"
    );
    Ok(meta)
}
impl Writer {
    fn open(config: &OpenSnitchDatabase) -> Result<Self> {
        let meta = metadata(&config.path)?;
        let parent = std::fs::symlink_metadata(
            config
                .path
                .parent()
                .ok_or_else(|| anyhow::anyhow!("missing parent"))?,
        )?;
        ensure!(
            parent.is_dir() && parent.uid() == meta.uid() && parent.mode() & 0o022 == 0,
            "unsafe history directory"
        );
        let lease_path = config.path.with_file_name(format!(
            ".{}.snitchgate.lock",
            config.path.file_name().unwrap().to_string_lossy()
        ));
        let lease = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&lease_path)?;
        ensure!(
            metadata(&lease_path)?.mode() & 0o077 == 0,
            "history lease must be private"
        );
        // SAFETY: flock operates on this live, owned descriptor; the File retains the lease.
        ensure!(
            unsafe { libc::flock(lease.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0,
            "history writer already active"
        );
        let mut db = Connection::open_with_flags(
            &config.path,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )?;
        db.busy_timeout(Duration::from_millis(100))?;
        let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        validate(&tx)?;
        let schema = tx.query_row("PRAGMA schema_version", [], |r| r.get(0))?;
        tx.commit()?;
        // Match the GUI's local UID labels without starting external processes or copying passwd records.
        let users = std::fs::read_to_string("/etc/passwd")
            .ok()
            .filter(|s| s.len() <= 1024 * 1024)
            .map(|s| {
                s.lines()
                    .filter_map(|line| {
                        let fields: Vec<_> = line.split(':').take(3).collect();
                        (fields.len() == 3
                            && fields[0].len() <= 128
                            && fields[2].parse::<u32>().is_ok())
                        .then(|| (fields[2].into(), format!("{} ({})", fields[0], fields[2])))
                    })
                    .collect()
            })
            .unwrap_or_default();
        Ok(Self {
            db,
            path: config.path.clone(),
            node: config.node.clone(),
            inode: (meta.dev(), meta.ino()),
            schema,
            users,
            _lease: lease,
        })
    }
    fn write(&mut self, batch: &Batch) -> Result<u64> {
        let meta = metadata(&self.path)?;
        ensure!(
            self.inode == (meta.dev(), meta.ino()),
            "history file changed"
        );
        let tx = self
            .db
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let schema: i64 = tx.query_row("PRAGMA schema_version", [], |r| r.get(0))?;
        ensure!(schema == self.schema, "history schema changed");
        ensure!(
            tx.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))? == 3,
            "history version changed"
        );
        let mut inserted = 0;
        {
            let mut insert = tx.prepare("INSERT OR IGNORE INTO connections(time,node,action,protocol,src_ip,src_port,dst_ip,dst_host,dst_port,uid,pid,process,process_args,process_cwd,rule) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,'','',?13)")?;
            let mut seen_rule = tx.prepare(
                "UPDATE rules SET time=?1 WHERE node=?2 AND name=?3 AND (time IS NULL OR time<?1)",
            )?;
            for row in &batch.rows {
                let time = timestamp(row.timestamp);
                inserted += insert.execute(params![
                    time,
                    self.node,
                    row.action,
                    row.protocol,
                    row.src_ip,
                    row.src_port.to_string(),
                    row.dst_ip,
                    row.host,
                    row.dst_port.to_string(),
                    row.uid.to_string(),
                    row.pid.to_string(),
                    row.process,
                    row.rule
                ])? as u64;
                if !row.rule.is_empty() {
                    seen_rule.execute(params![time, self.node, row.rule])?;
                }
            }
        }
        let days = batch.uptime / 86400;
        let seconds = batch.uptime % 86400;
        let uptime = format!(
            "{}{}:{:02}:{:02}",
            if days == 0 {
                String::new()
            } else {
                format!("{days} day{}, ", if days == 1 { "" } else { "s" })
            },
            seconds / 3600,
            (seconds / 60) % 60,
            seconds % 60
        );
        tx.execute("INSERT INTO nodes(addr,hostname,daemon_version,daemon_uptime,daemon_rules,cons,cons_dropped,version,status,last_connection) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,'✓ online',?9) ON CONFLICT(addr) DO UPDATE SET hostname=CASE WHEN excluded.hostname='' THEN nodes.hostname ELSE excluded.hostname END,daemon_version=excluded.daemon_version,daemon_uptime=excluded.daemon_uptime,daemon_rules=excluded.daemon_rules,cons=excluded.cons,cons_dropped=excluded.cons_dropped,version=CASE WHEN excluded.version='' THEN nodes.version ELSE excluded.version END,status=excluded.status,last_connection=excluded.last_connection",params![self.node,batch.identity.hostname,batch.daemon_version,uptime,batch.rules.to_string(),batch.connections.to_string(),batch.dropped.to_string(),batch.identity.version,timestamp((now() as i64).saturating_mul(1_000_000_000))])?;
        for (index, rows) in batch.summaries.iter().enumerate() {
            let mut statement = tx.prepare(SUMMARY_SQL[index])?;
            for (what, hits) in rows {
                let what = if index == 4 {
                    self.users.get(what).unwrap_or(what)
                } else {
                    what
                };
                statement.execute(params![what, hits])?;
            }
        }
        tx.commit()?;
        Ok(inserted)
    }
}
fn columns(db: &Connection, table: &str, expected: &[(&str, &str)]) -> Result<()> {
    let kind: String = db.query_row(
        "SELECT type FROM sqlite_master WHERE name=?1",
        [table],
        |r| r.get(0),
    )?;
    ensure!(kind == "table", "history object must be a table");
    let sql: String = db.query_row(
        "SELECT lower(sql) FROM sqlite_master WHERE name=?1",
        [table],
        |r| r.get(0),
    )?;
    ensure!(
        !sql.contains("check") && !sql.contains("references") && !sql.contains("collate"),
        "unexpected history constraints"
    );
    ensure!(db.query_row("SELECT count(*) FROM pragma_table_xinfo(?1) WHERE \"notnull\"!=0 OR dflt_value IS NOT NULL OR hidden!=0", [table], |r|r.get::<_,i64>(0))? == 0, "unexpected history columns");
    let columns: Vec<(String, String)> = db
        .prepare("SELECT name,type FROM pragma_table_info(?1) ORDER BY cid")?
        .query_map([table], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    ensure!(
        columns.len() == expected.len()
            && columns
                .iter()
                .zip(expected)
                .all(|((name, ty), (n, t))| name == n && ty.eq_ignore_ascii_case(t)),
        "unsupported history table schema"
    );
    let triggers: i64 = db.query_row(
        "SELECT count(*) FROM sqlite_master WHERE type='trigger' AND tbl_name=?1",
        [table],
        |r| r.get(0),
    )?;
    ensure!(triggers == 0, "unexpected history triggers");
    Ok(())
}
fn validate(db: &Connection) -> Result<()> {
    ensure!(
        db.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))? == 3,
        "unsupported OpenSnitch history version"
    );
    columns(
        db,
        "connections",
        &[
            "time",
            "node",
            "action",
            "protocol",
            "src_ip",
            "src_port",
            "dst_ip",
            "dst_host",
            "dst_port",
            "uid",
            "pid",
            "process",
            "process_args",
            "process_cwd",
            "rule",
        ]
        .map(|name| (name, "TEXT")),
    )?;
    columns(
        db,
        "nodes",
        &[
            "addr",
            "hostname",
            "daemon_version",
            "daemon_uptime",
            "daemon_rules",
            "cons",
            "cons_dropped",
            "version",
            "status",
            "last_connection",
        ]
        .map(|name| (name, "TEXT")),
    )?;
    for table in TABLES {
        columns(db, table, &[("what", "TEXT"), ("hits", "INTEGER")])?;
    }
    // Rules are an existing GUI cache: update only last-seen time, never invent inventory.
    for name in ["time", "node", "name"] {
        let ty: String = db.query_row(
            "SELECT type FROM pragma_table_info('rules') WHERE name=?1",
            [name],
            |r| r.get(0),
        )?;
        ensure!(ty.eq_ignore_ascii_case("TEXT"), "unsupported rules cache");
    }
    ensure!(db.query_row("SELECT count(*) FROM sqlite_master WHERE (type='trigger' AND tbl_name='rules') OR (type='view' AND name='rules')",[],|r|r.get::<_,i64>(0))? == 0, "unexpected rules cache");
    let unique: i64 = db.query_row("SELECT count(*) FROM pragma_index_list('connections') WHERE \"unique\"=1 AND (SELECT group_concat(name,',') FROM (SELECT name FROM pragma_index_info(pragma_index_list.name) ORDER BY seqno))='node,action,protocol,src_ip,src_port,dst_ip,dst_port,uid,pid,process,process_args'",[],|r|r.get(0))?;
    ensure!(unique == 1, "unsupported connection uniqueness");
    for table in ["nodes", "hosts", "procs", "addrs", "ports", "users"] {
        let key = if table == "nodes" { "addr" } else { "what" };
        ensure!(
            db.query_row(
                "SELECT pk FROM pragma_table_info(?1) WHERE name=?2",
                params![table, key],
                |r| r.get::<_, i64>(0)
            )? == 1,
            "unsupported history key"
        );
    }
    Ok(())
}

#[cfg(test)]
#[path = "opensnitch_db_tests.rs"]
mod tests;

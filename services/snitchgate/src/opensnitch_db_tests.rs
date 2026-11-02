use super::*;
const SCHEMA: &str = include_str!("../tests/opensnitch-v3.sql");
fn fixture(root: &Path) -> OpenSnitchDatabase {
    let path = root.join("ui.sqlite3");
    Connection::open(&path)
        .unwrap()
        .execute_batch(SCHEMA)
        .unwrap();
    OpenSnitchDatabase {
        path,
        node: "unix:/local".into(),
    }
}
fn stats() -> protocol::Statistics {
    protocol::Statistics {
        daemon_version: "1.7.2".into(),
        uptime: 86401,
        rules: 2,
        connections: 101,
        dropped: 9,
        by_host: HashMap::from([("example.invalid".into(), 12)]),
        by_executable: HashMap::from([("/usr/bin/example".into(), 12)]),
        by_address: HashMap::from([("2001:db8::1".into(), 12)]),
        by_port: HashMap::from([("443".into(), 12)]),
        by_uid: HashMap::from([("1000".into(), 12)]),
        events: vec![protocol::Event {
            unixnano: 1793577600123456000,
            connection: Some(protocol::Connection {
                protocol: "tcp6".into(),
                src_ip: "::1".into(),
                src_port: 43210,
                dst_ip: "2001:db8::1".into(),
                dst_host: "example.invalid".into(),
                dst_port: 443,
                user_id: 1000,
                process_id: 123,
                process_path: "/usr/bin/example".into(),
                process_args: vec!["PRIVATE_ARG".into()],
                process_cwd: "PRIVATE_CWD".into(),
                process_env: HashMap::from([("KEY".into(), "PRIVATE_ENV".into())]),
                ..Default::default()
            }),
            rule: Some(protocol::Rule {
                name: "fixture-rule".into(),
                action: "allow".into(),
                ..Default::default()
            }),
            ..Default::default()
        }],
        ..Default::default()
    }
}
fn batch() -> Batch {
    let stats = stats();
    Batch {
        identity: Identity {
            hostname: "fixture-host".into(),
            version: "fixture-kernel".into(),
        },
        daemon_version: stats.daemon_version,
        uptime: stats.uptime,
        rules: stats.rules,
        connections: stats.connections,
        dropped: stats.dropped,
        rows: stats.events.iter().filter_map(row).collect(),
        summaries: std::array::from_fn(|i| {
            if i == 0 {
                vec![("example.invalid".into(), 12)]
            } else {
                vec![]
            }
        }),
    }
}
#[test]
fn preserves_history_and_schema_matches_gui_dedup_and_refreshes_summaries() {
    let root = tempfile::tempdir().unwrap();
    let config = fixture(root.path());
    let db = Connection::open(&config.path).unwrap();
    db.execute_batch("INSERT INTO connections(time,node,process_args) VALUES('2026-10-25 12:59:43.932424','archive','OLD_PRIVATE_ARG'); INSERT INTO rules(node,name,time) VALUES('unix:/local','fixture-rule','2020-01-01');").unwrap();
    let schema: i64 = db
        .query_row("PRAGMA schema_version", [], |r| r.get(0))
        .unwrap();
    let mut writer = Writer::open(&config).unwrap();
    assert_eq!(writer.write(&batch()).unwrap(), 1);
    assert_eq!(writer.write(&batch()).unwrap(), 0);
    drop(writer);
    let mut restarted = Writer::open(&config).unwrap();
    assert_eq!(restarted.write(&batch()).unwrap(), 0);
    let old: String = db
        .query_row(
            "SELECT process_args FROM connections WHERE node='archive'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(old, "OLD_PRIVATE_ARG");
    let values: (String,String,String,String,String,String) = db.query_row("SELECT time,protocol,src_ip,process_args,process_cwd,rule FROM connections WHERE node='unix:/local'",[],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?))).unwrap();
    assert_eq!(
        values,
        (
            timestamp(1793577600123456000),
            "tcp6".into(),
            "::1".into(),
            "".into(),
            "".into(),
            "fixture-rule".into()
        )
    );
    assert_eq!(
        db.query_row("SELECT count(*) FROM connections", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        2
    );
    assert_eq!(
        db.query_row(
            "SELECT hits FROM hosts WHERE what='example.invalid'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        12
    );
    assert_eq!(
        db.query_row(
            "SELECT hostname||'/'||daemon_uptime||'/'||cons FROM nodes",
            [],
            |r| r.get::<_, String>(0)
        )
        .unwrap(),
        "fixture-host/1 day, 0:00:01/101"
    );
    assert_eq!(
        db.query_row("SELECT time FROM rules", [], |r| r.get::<_, String>(0))
            .unwrap(),
        values.0
    );
    assert_eq!(
        db.query_row("PRAGMA schema_version", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        schema
    );
    assert_eq!(
        db.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        3
    );
}
#[test]
fn refuses_missing_future_changed_unsafe_and_concurrent_writer_targets() {
    let root = tempfile::tempdir().unwrap();
    let mut config = fixture(root.path());
    let mut writer = Writer::open(&config).unwrap();
    assert!(Writer::open(&config).is_err());
    let db = Connection::open(&config.path).unwrap();
    db.execute_batch(
        "CREATE TRIGGER unknown_writer AFTER INSERT ON connections BEGIN DELETE FROM hosts; END;",
    )
    .unwrap();
    assert!(writer.write(&batch()).is_err());
    drop(writer);
    assert!(Writer::open(&config).is_err());
    db.execute_batch("DROP TRIGGER unknown_writer; PRAGMA user_version=4;")
        .unwrap();
    assert!(Writer::open(&config).is_err());
    db.execute_batch("PRAGMA user_version=3;").unwrap();
    let mut writer = Writer::open(&config).unwrap();
    db.execute_batch("PRAGMA user_version=4;").unwrap();
    assert!(writer.write(&batch()).is_err());
    drop(writer);
    let target = config.path.clone();
    config.path = root.path().join("link.sqlite3");
    std::os::unix::fs::symlink(&target, &config.path).unwrap();
    assert!(Writer::open(&config).is_err());
    config.path = root.path().join("missing.sqlite3");
    assert!(Writer::open(&config).is_err());
    assert!(!config.path.exists());
    config.path = root.path().join("constraints.sqlite3");
    Connection::open(&config.path)
        .unwrap()
        .execute_batch(&SCHEMA.replacen(
            " UNIQUE(node,action",
            " CHECK(pid!='123'), UNIQUE(node,action",
            1,
        ))
        .unwrap();
    assert!(
        Writer::open(&config).is_err(),
        "custom constraints must not silently discard observations"
    );
}
#[tokio::test]
async fn grpc_stats_write_history_without_approvals_and_locked_history_cannot_prevent_a_decision() {
    use crate::{
        broker::Broker,
        config::Config,
        model::{Action, Duration as RuleDuration},
        server,
        store::Store,
    };
    let root = tempfile::tempdir().unwrap();
    let config = fixture(root.path());
    let db = Connection::open(&config.path).unwrap();
    let broker = Broker::new(Config::default(), Store::memory(100).unwrap(), false).unwrap();
    broker.opensnitch_database_start(config.clone());
    let path = root.path().join("wire/opensnitch.sock");
    let service = tokio::spawn(server::serve(
        crate::ipc::bind(&path).unwrap(),
        broker.clone(),
    ));
    let mut client = server::connect(&path).await.unwrap();
    client
        .subscribe(protocol::ClientConfig {
            name: "fixture-host".into(),
            version: "fixture-kernel".into(),
            config: "PRIVATE_CONFIG".into(),
            ..Default::default()
        })
        .await
        .unwrap();
    client
        .ping(protocol::PingRequest {
            id: 1,
            stats: Some(stats()),
        })
        .await
        .unwrap();
    for _ in 0..100 {
        if broker
            .snapshot()
            .unwrap()
            .health
            .opensnitch_database
            .unwrap()
            .persisted
            == 1
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(
        broker
            .snapshot()
            .unwrap()
            .health
            .opensnitch_database
            .unwrap()
            .persisted,
        1
    );
    assert!(broker.snapshot().unwrap().history.is_empty());
    assert_eq!(
        db.query_row("SELECT hostname FROM nodes", [], |r| r.get::<_, String>(0))
            .unwrap(),
        "fixture-host"
    );
    for table in TABLES {
        assert_eq!(
            db.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            1
        );
    }
    db.execute_batch("BEGIN EXCLUSIVE").unwrap();
    tokio::time::timeout(
        Duration::from_secs(1),
        client.ping(protocol::PingRequest {
            id: 2,
            stats: Some(stats()),
        }),
    )
    .await
    .unwrap()
    .unwrap();
    let mut decision_client = client.clone();
    let asking = tokio::spawn(async move {
        decision_client
            .ask_rule(protocol::Connection {
                protocol: "tcp".into(),
                dst_ip: "192.0.2.1".into(),
                dst_port: 443,
                process_path: "/usr/bin/example".into(),
                ..Default::default()
            })
            .await
            .unwrap()
            .into_inner()
    });
    let mut selected = None;
    for _ in 0..100 {
        selected = broker
            .snapshot()
            .unwrap()
            .pending
            .first()
            .map(|r| r.id.clone());
        if selected.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    broker
        .decide(&selected.unwrap(), Action::Deny, RuleDuration::Once)
        .unwrap();
    let reply = tokio::time::timeout(Duration::from_secs(1), asking)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(reply.action, "deny");
    assert_eq!(reply.duration, "once");
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert!(
        broker
            .snapshot()
            .unwrap()
            .health
            .opensnitch_database
            .unwrap()
            .errors
            > 0
    );
    db.execute_batch("ROLLBACK").unwrap();
    tokio::time::sleep(Duration::from_millis(1100)).await;
    client
        .ping(protocol::PingRequest {
            id: 3,
            stats: Some(stats()),
        })
        .await
        .unwrap();
    for _ in 0..100 {
        if broker
            .snapshot()
            .unwrap()
            .health
            .opensnitch_database
            .unwrap()
            .ready
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(
        broker
            .snapshot()
            .unwrap()
            .health
            .opensnitch_database
            .unwrap()
            .ready
    );
    assert_eq!(
        db.query_row("SELECT count(*) FROM connections", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        1
    );
    service.abort();
    service.await.unwrap_err();
}

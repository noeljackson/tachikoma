//! Transparent local GUI fallback with a private, non-authoritative history outbox.
use crate::{credentials, ipc, model, protocol, service};
use anyhow::{Context, Result, ensure};
use approval_client::Client;
use approval_protocol::{Effect, Mode, ReviewState, SubmitRequest};
use clap::Args;
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::json;
use std::{
    os::unix::fs::MetadataExt,
    path::PathBuf,
    pin::Pin,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio_stream::{
    Stream,
    wrappers::{ReceiverStream, UnixListenerStream},
};
use tonic::{Request, Response, Status};

pub const QUEUE: &str = "network.opensnitch-popup";
pub const SCHEMA: &str = "network.popup-result.v1";
const MAX_OUTBOX: i64 = 10_000;

#[derive(Args)]
pub struct Options {
    #[arg(long)]
    pub listen_socket: PathBuf,
    #[arg(long)]
    pub abstract_name: Option<String>,
    #[arg(long)]
    pub gui_socket: PathBuf,
    #[arg(long)]
    pub approval_socket: PathBuf,
    #[arg(long)]
    pub state_dir: PathBuf,
    #[arg(long, default_value = "POPUP_PRODUCER_TOKEN")]
    pub credential_env: String,
}

struct Journal {
    db: Connection,
}
impl Journal {
    fn open(path: &std::path::Path) -> Result<Self> {
        let db = crate::approvals::private_database(path)?;
        db.busy_timeout(Duration::from_secs(1))?;
        db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; CREATE TABLE IF NOT EXISTS popup_outbox(id TEXT PRIMARY KEY,body TEXT NOT NULL);")?;
        Ok(Self { db })
    }
    fn record(&self, request: &SubmitRequest) -> Result<()> {
        let count: i64 = self
            .db
            .query_row("SELECT count(*) FROM popup_outbox", [], |r| r.get(0))?;
        ensure!(count < MAX_OUTBOX, "popup history outbox full");
        self.db.execute(
            "INSERT INTO popup_outbox VALUES(?1,?2)",
            params![request.idempotency_key, serde_json::to_string(request)?],
        )?;
        Ok(())
    }
    fn next(&self) -> Result<Option<SubmitRequest>> {
        let value: Option<String> = self
            .db
            .query_row(
                "SELECT body FROM popup_outbox ORDER BY rowid LIMIT 1",
                [],
                |r| r.get(0),
            )
            .optional()?;
        value
            .map(|s| serde_json::from_str(&s).map_err(Into::into))
            .transpose()
    }
    fn ack(&self, id: &str) -> Result<()> {
        self.db
            .execute("DELETE FROM popup_outbox WHERE id=?1", [id])?;
        Ok(())
    }
}

fn capture(raw: &protocol::Connection) -> Result<model::Connection> {
    ensure!(
        raw.process_path.len() <= 4096
            && raw.dst_host.len() <= 253
            && !raw.process_path.chars().any(char::is_control),
        "invalid popup connection metadata"
    );
    let protocol = match raw.protocol.to_ascii_lowercase().as_str() {
        "tcp" => "tcp",
        "udp" => "udp",
        "icmp" => "icmp",
        "icmp6" | "icmpv6" => "icmp6",
        _ => "other",
    };
    let host = if raw.dst_host.is_ascii()
        && raw
            .dst_host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
    {
        raw.dst_host.clone()
    } else {
        String::new()
    };
    Ok(model::Connection {
        process: raw.process_path.clone(),
        pid: raw.process_id,
        uid: raw.user_id,
        protocol: protocol.into(),
        ip: raw.dst_ip.parse()?,
        host,
        port: u16::try_from(raw.dst_port)?,
        url_hints: vec![],
        context: Default::default(),
    })
}

fn record(connection: &model::Connection, rule: &protocol::Rule) -> SubmitRequest {
    let id = uuid::Uuid::new_v4().to_string();
    let action = match rule.action.as_str() {
        "allow" => "allow",
        "deny" => "deny",
        "reject" => "reject",
        _ => "other",
    };
    let duration = match rule.duration.as_str() {
        "once" | "30s" | "5m" | "15m" | "30m" | "1h" | "12h" | "until restart" | "always" => {
            rule.duration.as_str()
        }
        _ => "other",
    };
    SubmitRequest {
        queue: QUEUE.into(),
        producer_request_id: id.clone(),
        nonce: id.clone(),
        idempotency_key: id,
        schema_id: SCHEMA.into(),
        subject: json!({"origin":"opensnitch-popup","process":connection.process,"pid":connection.pid,"uid":connection.uid,"protocol":connection.protocol,"ip":connection.ip.to_string(),"host":connection.host,"port":connection.port,"result":action,"duration":duration,"observed_at_ms":model::now().saturating_mul(1000)}),
        evidence: json!({"source":"OpenSnitch popup response","authority":"Historical observation only. This is not a tachikoma approval or a kernel enforcement acknowledgement.","interaction":"The wire protocol cannot distinguish an explicit click from the GUI timeout action.","scope":"Captured connection only. Original rule name and operators are not exported because they may contain arguments or credentials."}),
        action_ids: vec!["record".into()],
        // Stable across retries after an outage. The queue settles this immediately
        // as Observe; the server independently caps the effective deadline.
        expires_at_ms: 4_102_444_800_000,
        summary: format!(
            "OpenSnitch {action}: {} → {}:{}",
            connection.process,
            if connection.host.is_empty() {
                connection.ip.to_string()
            } else {
                connection.host.clone()
            },
            connection.port
        )
        .chars()
        .take(256)
        .collect(),
    }
}

#[derive(Clone)]
struct Relay {
    gui: protocol::ui_client::UiClient<tonic::transport::Channel>,
    journal: Arc<Mutex<Journal>>,
    capacity: Arc<tokio::sync::Semaphore>,
}
#[tonic::async_trait]
impl protocol::ui_server::Ui for Relay {
    async fn ping(
        &self,
        request: Request<protocol::PingRequest>,
    ) -> Result<Response<protocol::PingReply>, Status> {
        self.gui.clone().ping(request.into_inner()).await
    }
    async fn subscribe(
        &self,
        request: Request<protocol::ClientConfig>,
    ) -> Result<Response<protocol::ClientConfig>, Status> {
        self.gui.clone().subscribe(request.into_inner()).await?;
        // Register the GUI node without accepting a default-action/config rewrite.
        Ok(Response::new(protocol::ClientConfig {
            name: "opensnitch-popup-relay".into(),
            ..Default::default()
        }))
    }
    async fn post_alert(
        &self,
        request: Request<protocol::Alert>,
    ) -> Result<Response<protocol::MsgResponse>, Status> {
        self.gui.clone().post_alert(request.into_inner()).await
    }
    async fn ask_rule(
        &self,
        request: Request<protocol::Connection>,
    ) -> Result<Response<protocol::Rule>, Status> {
        let permit = self
            .capacity
            .clone()
            .try_acquire_owned()
            .map_err(|_| Status::resource_exhausted("popup relay busy"))?;
        let raw = request.into_inner();
        let captured =
            capture(&raw).map_err(|_| Status::invalid_argument("invalid connection metadata"))?;
        let mut gui = self.gui.clone();
        let journal = self.journal.clone();
        // A dropped daemon RPC cannot cancel the audit of a popup already shown.
        tokio::spawn(async move {
            let _permit = permit;
            let reply = tokio::time::timeout(Duration::from_secs(120), gui.ask_rule(raw))
                .await
                .map_err(|_| Status::deadline_exceeded("popup deadline"))??;
            let event = record(&captured, reply.get_ref());
            journal
                .lock()
                .map_err(|_| Status::internal("popup journal unavailable"))?
                .record(&event)
                .map_err(|_| Status::unavailable("popup response could not be audited"))?;
            Ok(reply)
        })
        .await
        .map_err(|_| Status::unavailable("popup relay interrupted"))?
    }
    type NotificationsStream =
        Pin<Box<dyn Stream<Item = Result<protocol::Notification, Status>> + Send>>;
    async fn notifications(
        &self,
        request: Request<tonic::Streaming<protocol::NotificationReply>>,
    ) -> Result<Response<Self::NotificationsStream>, Status> {
        let mut inbound = request.into_inner();
        let (sender, receiver) = tokio::sync::mpsc::channel(16);
        tokio::spawn(async move {
            loop {
                let item =
                    tokio::select! { _=sender.closed()=>break, item=inbound.message()=>item };
                match item {
                    Ok(Some(item)) => {
                        if sender.send(item).await.is_err() {
                            break;
                        }
                    }
                    _ => break,
                }
            }
        });
        let reply = self
            .gui
            .clone()
            .notifications(ReceiverStream::new(receiver))
            .await?;
        Ok(Response::new(Box::pin(reply.into_inner())))
    }
}

async fn connect_gui(
    path: PathBuf,
) -> Result<protocol::ui_client::UiClient<tonic::transport::Channel>> {
    let uid = std::fs::metadata("/proc/self")?.uid();
    let channel = tonic::transport::Endpoint::try_from("http://[::]:50051")?
        .connect_with_connector(tower::service_fn(move |_| {
            let path = path.clone();
            async move {
                let stream = tokio::net::UnixStream::connect(path).await?;
                if stream.peer_cred()?.uid() != uid {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::PermissionDenied,
                        "unexpected GUI owner",
                    ));
                }
                Ok::<_, std::io::Error>(hyper_util::rt::TokioIo::new(stream))
            }
        }))
        .await?;
    Ok(protocol::ui_client::UiClient::new(channel))
}

async fn flush(client: &Client, journal: &Mutex<Journal>) -> Result<()> {
    let queue = client
        .queues()
        .await?
        .into_iter()
        .find(|q| q.manifest.id == QUEUE)
        .context("popup archive queue missing")?
        .manifest;
    ensure!(
        queue.mode == Mode::Policy
            && queue.schema_id == SCHEMA
            && queue.service_kind == "snitchgate.popup"
            && queue.assessment.is_none()
            && queue.actions.len() == 1
            && queue.actions[0].id == "record"
            && queue.actions[0].effect == Effect::Observe
            && queue.rules.len() == 1
            && queue.rules[0].action == "record"
            && queue.rules[0].equals == [("origin".into(), json!("opensnitch-popup"))].into(),
        "popup archive must be observation-only"
    );
    for _ in 0..32 {
        let next = journal
            .lock()
            .map_err(|_| anyhow::anyhow!("popup journal unavailable"))?
            .next()?;
        let Some(request) = next else { break };
        let review = client.submit(&request).await?;
        ensure!(
            review.state == ReviewState::Observed
                && review
                    .decision
                    .as_ref()
                    .is_some_and(|d| !d.authoritative && d.action.effect == Effect::Observe),
            "popup archive did not record an observation"
        );
        journal
            .lock()
            .map_err(|_| anyhow::anyhow!("popup journal unavailable"))?
            .ack(&request.idempotency_key)?;
    }
    client.heartbeat("OpenSnitch popup history relay available; responses are observations, not central approvals").await?;
    Ok(())
}

pub async fn run(args: Options) -> Result<()> {
    ensure!(
        [
            &args.listen_socket,
            &args.gui_socket,
            &args.approval_socket,
            &args.state_dir
        ]
        .iter()
        .all(|p| p.is_absolute()),
        "popup paths must be absolute"
    );
    ipc::private_dir(&args.state_dir)?;
    if let Some(parent) = args.listen_socket.parent() {
        ipc::private_dir(parent)?;
    }
    let journal = Arc::new(Mutex::new(Journal::open(
        &args.state_dir.join("popup.sqlite3"),
    )?));
    let relay = Relay {
        gui: connect_gui(args.gui_socket).await?,
        journal: journal.clone(),
        capacity: Arc::new(tokio::sync::Semaphore::new(128)),
    };
    let listener = ipc::bind(&args.listen_socket)?;
    let history = tokio::spawn(async move {
        loop {
            let result = async {
                let client = Client::new(
                    "http://tachikoma.local",
                    Some(&args.approval_socket),
                    credentials::read(&args.credential_env)?.trim().to_owned(),
                )?;
                flush(&client, &journal).await
            }
            .await;
            let _ = service::notify(if result.is_ok() {
                "STATUS=Popup relay ready; history synchronized"
            } else {
                "STATUS=Popup relay ready; history queued locally"
            });
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    });
    let mut local = tokio::spawn(
        tonic::transport::Server::builder()
            .add_service(
                protocol::ui_server::UiServer::new(relay.clone())
                    .max_decoding_message_size(16 * 1024 * 1024),
            )
            .serve_with_incoming(UnixListenerStream::new(listener)),
    );
    let mut abstract_server = tokio::spawn(async move {
        match args.abstract_name {
            Some(name) => {
                let incoming = crate::server::abstract_incoming(&name)?;
                tonic::transport::Server::builder()
                    .add_service(
                        protocol::ui_server::UiServer::new(relay)
                            .max_decoding_message_size(16 * 1024 * 1024),
                    )
                    .serve_with_incoming(incoming)
                    .await?;
                Ok::<_, anyhow::Error>(())
            }
            None => std::future::pending::<Result<()>>().await,
        }
    });
    service::notify("READY=1\nSTATUS=Popup relay ready; history queued locally")?;
    let mut stop = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let result = tokio::select! {
        r=&mut local=>r.context("popup listener stopped")?.map_err(Into::into),
        r=&mut abstract_server=>r.context("popup abstract listener stopped")?,
        _=stop.recv()=>Ok(()),
        _=tokio::signal::ctrl_c()=>Ok(()),
    };
    history.abort();
    local.abort();
    abstract_server.abort();
    ipc::remove_socket(&args.listen_socket);
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    #[test]
    fn outbox_is_private_stable_and_excludes_rule_and_argument_secrets() {
        let root = tempfile::tempdir().unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let path = root.path().join("popup.sqlite3");
        let journal = Journal::open(&path).unwrap();
        let connection = model::Connection::try_from(protocol::Connection {
            process_path: "/usr/bin/example".into(),
            process_args: vec!["SECRET_ARG".into()],
            dst_ip: "127.0.0.1".into(),
            dst_port: 8443,
            protocol: "tcp".into(),
            ..Default::default()
        })
        .unwrap();
        let rule = protocol::Rule {
            name: "allow-SECRET_NAME".into(),
            description: "SECRET_DESCRIPTION".into(),
            action: "allow".into(),
            duration: "once".into(),
            operator: Some(protocol::Operator {
                operand: "process.command".into(),
                data: "SECRET_OPERATOR".into(),
                ..Default::default()
            }),
            ..Default::default()
        };
        let event = record(&connection, &rule);
        journal.record(&event).unwrap();
        let copy = journal.next().unwrap().unwrap();
        let encoded = serde_json::to_string(&copy).unwrap();
        assert!(!encoded.contains("SECRET"));
        assert_eq!(copy.idempotency_key, event.idempotency_key);
        assert_eq!(copy.subject["result"], "allow");
        assert_eq!(copy.subject["duration"], "once");
        assert_eq!(std::fs::metadata(path).unwrap().mode() & 0o777, 0o600);
        drop(journal);
        let journal = Journal::open(&root.path().join("popup.sqlite3")).unwrap();
        assert_eq!(
            journal.next().unwrap().unwrap().idempotency_key,
            event.idempotency_key
        );
        journal.ack(&event.idempotency_key).unwrap();
        assert!(journal.next().unwrap().is_none());
    }
    #[derive(Clone)]
    struct Gui;
    fn gui_rule() -> protocol::Rule {
        protocol::Rule {
            name: "allow-SECRET-NAME".into(),
            description: "SECRET-DESCRIPTION".into(),
            action: "allow".into(),
            duration: "once".into(),
            enabled: true,
            operator: Some(protocol::Operator {
                operand: "process.command".into(),
                data: "SECRET-COMMAND".into(),
                ..Default::default()
            }),
            ..Default::default()
        }
    }
    #[tonic::async_trait]
    impl protocol::ui_server::Ui for Gui {
        async fn ping(
            &self,
            r: Request<protocol::PingRequest>,
        ) -> Result<Response<protocol::PingReply>, Status> {
            Ok(Response::new(protocol::PingReply {
                id: r.into_inner().id,
            }))
        }
        async fn subscribe(
            &self,
            _: Request<protocol::ClientConfig>,
        ) -> Result<Response<protocol::ClientConfig>, Status> {
            Ok(Response::new(protocol::ClientConfig {
                config: "SECRET-CONFIG".into(),
                ..Default::default()
            }))
        }
        async fn post_alert(
            &self,
            r: Request<protocol::Alert>,
        ) -> Result<Response<protocol::MsgResponse>, Status> {
            Ok(Response::new(protocol::MsgResponse {
                id: r.into_inner().id,
            }))
        }
        async fn ask_rule(
            &self,
            r: Request<protocol::Connection>,
        ) -> Result<Response<protocol::Rule>, Status> {
            assert_eq!(r.get_ref().process_args, ["SECRET-ARGV"]);
            Ok(Response::new(gui_rule()))
        }
        type NotificationsStream =
            Pin<Box<dyn Stream<Item = Result<protocol::Notification, Status>> + Send>>;
        async fn notifications(
            &self,
            r: Request<tonic::Streaming<protocol::NotificationReply>>,
        ) -> Result<Response<Self::NotificationsStream>, Status> {
            let (tx, rx) = tokio::sync::mpsc::channel(1);
            let mut inbound = r.into_inner();
            tokio::spawn(async move {
                let reply = inbound.message().await.unwrap().unwrap();
                tx.send(Ok(protocol::Notification {
                    id: reply.id,
                    ..Default::default()
                }))
                .await
                .unwrap();
            });
            Ok(Response::new(Box::pin(ReceiverStream::new(rx))))
        }
    }
    #[tokio::test]
    async fn gui_wire_response_is_forwarded_audited_and_survives_central_outage() {
        let root = tempfile::tempdir().unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let gui_path = root.path().join("gui.sock");
        let listener = ipc::bind(&gui_path).unwrap();
        let gui = tokio::spawn(
            tonic::transport::Server::builder()
                .add_service(protocol::ui_server::UiServer::new(Gui))
                .serve_with_incoming(UnixListenerStream::new(listener)),
        );
        let journal = Arc::new(Mutex::new(
            Journal::open(&root.path().join("popup.sqlite3")).unwrap(),
        ));
        let relay = Relay {
            gui: connect_gui(gui_path).await.unwrap(),
            journal: journal.clone(),
            capacity: Arc::new(tokio::sync::Semaphore::new(2)),
        };
        let relay_path = root.path().join("relay.sock");
        let listener = ipc::bind(&relay_path).unwrap();
        let proxy = tokio::spawn(
            tonic::transport::Server::builder()
                .add_service(protocol::ui_server::UiServer::new(relay))
                .serve_with_incoming(UnixListenerStream::new(listener)),
        );
        let mut client = crate::server::connect(&relay_path).await.unwrap();
        assert!(
            client
                .subscribe(protocol::ClientConfig::default())
                .await
                .unwrap()
                .get_ref()
                .config
                .is_empty()
        );
        assert_eq!(
            client
                .ping(protocol::PingRequest {
                    id: 77,
                    stats: None
                })
                .await
                .unwrap()
                .get_ref()
                .id,
            77
        );
        let mut notifications = client
            .notifications(tokio_stream::iter([protocol::NotificationReply {
                id: 81,
                ..Default::default()
            }]))
            .await
            .unwrap()
            .into_inner();
        assert_eq!(notifications.message().await.unwrap().unwrap().id, 81);
        let reply = client
            .ask_rule(protocol::Connection {
                process_path: "/usr/bin/example".into(),
                process_args: vec!["SECRET-ARGV".into()],
                dst_ip: "127.0.0.1".into(),
                dst_port: 443,
                protocol: "icmp".into(),
                ..Default::default()
            })
            .await
            .unwrap()
            .into_inner();
        assert_eq!(reply, gui_rule());
        let event = journal.lock().unwrap().next().unwrap().unwrap();
        assert!(!serde_json::to_string(&event).unwrap().contains("SECRET"));
        assert_eq!(event.subject["protocol"], "icmp");
        let absent = Client::new(
            "http://fixture",
            Some(&root.path().join("missing.sock")),
            "a".repeat(64),
        )
        .unwrap();
        assert!(flush(&absent, &journal).await.is_err());
        assert_eq!(
            journal
                .lock()
                .unwrap()
                .next()
                .unwrap()
                .unwrap()
                .idempotency_key,
            event.idempotency_key
        );
        // An audit failure must not turn into an unrecorded allow.
        journal
            .lock()
            .unwrap()
            .db
            .execute_batch("DROP TABLE popup_outbox;")
            .unwrap();
        assert!(
            client
                .ask_rule(protocol::Connection {
                    process_path: "/usr/bin/example".into(),
                    process_args: vec!["SECRET-ARGV".into()],
                    dst_ip: "127.0.0.1".into(),
                    dst_port: 443,
                    protocol: "tcp".into(),
                    ..Default::default()
                })
                .await
                .is_err()
        );
        proxy.abort();
        gui.abort();
    }
    #[tokio::test]
    async fn outbox_retries_same_event_and_rejects_an_authoritative_archive() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let root = tempfile::tempdir().unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let journal = Mutex::new(Journal::open(&root.path().join("popup.sqlite3")).unwrap());
        let captured = model::Connection::try_from(protocol::Connection {
            process_path: "/usr/bin/example".into(),
            protocol: "tcp".into(),
            dst_ip: "127.0.0.1".into(),
            dst_port: 443,
            ..Default::default()
        })
        .unwrap();
        let event = record(&captured, &gui_rule());
        journal.lock().unwrap().record(&event).unwrap();
        let path = root.path().join("api.sock");
        let listener = tokio::net::UnixListener::bind(&path).unwrap();
        // 0=unsafe archive, 1=transient API failure, 2=success.
        let mode = Arc::new(AtomicUsize::new(0));
        let server_mode = mode.clone();
        let attempts = Arc::new(Mutex::new(Vec::<serde_json::Value>::new()));
        let received = attempts.clone();
        let expected = event.clone();
        let task = tokio::spawn(async move {
            loop {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut bytes = Vec::new();
                let mut byte = [0u8; 1];
                while !bytes.ends_with(b"\r\n\r\n") && bytes.len() < 65536 {
                    stream.read_exact(&mut byte).await.unwrap();
                    bytes.push(byte[0]);
                }
                let header = String::from_utf8(bytes).unwrap();
                let path = header
                    .lines()
                    .next()
                    .unwrap()
                    .split_whitespace()
                    .nth(1)
                    .unwrap();
                let size = header
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .map(|n| n.trim().parse::<usize>().unwrap())
                    })
                    .unwrap_or(0);
                let mut body = vec![0; size];
                stream.read_exact(&mut body).await.unwrap();
                let current = server_mode.load(Ordering::SeqCst);
                let mut queue: approval_protocol::QueueManifest = serde_json::from_str(
                    include_str!("../../../contrib/adapters/opensnitch-popup.queue.json"),
                )
                .unwrap();
                let mut status = "200 OK";
                let response = if path.ends_with("/queues") {
                    if current == 0 {
                        queue.actions[0].effect = Effect::Permit;
                    }
                    json!([{"manifest":queue,"revision":1,"pending":0,"settled":0}])
                } else if path.ends_with("/requests") {
                    let input: serde_json::Value = serde_json::from_slice(&body).unwrap();
                    assert_eq!(input, serde_json::to_value(&expected).unwrap());
                    received.lock().unwrap().push(input);
                    if current == 1 {
                        status = "503 Unavailable";
                        json!({"error":"fixture"})
                    } else {
                        json!({"id":"recorded-event","producer":"popup","service_kind":"snitchgate.popup","revision":1,"manifest_revision":1,"request":expected,"subject_digest":"subject","evidence_digest":"evidence","created_at_ms":1,"expires_at_ms":4102444800000i64,"mode":"policy","state":"observed","actions":queue.actions,"assessment":null,"receipt":null,
                        "decision":{"id":"observed-decision","request_id":"recorded-event","producer":"popup","queue":QUEUE,"request_revision":1,"manifest_revision":1,"nonce":expected.nonce,"subject_digest":"subject","evidence_digest":"evidence","action":queue.actions[0],"actor":"policy:record","issued_at_ms":1,"expires_at_ms":4102444800000i64,"authoritative":false}})
                    }
                } else {
                    json!(null)
                };
                let body = serde_json::to_vec(&response).unwrap();
                stream.write_all(format!("HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",body.len()).as_bytes()).await.unwrap();
                stream.write_all(&body).await.unwrap();
            }
        });
        let client = Client::new("http://fixture", Some(&path), "a".repeat(64)).unwrap();
        assert!(flush(&client, &journal).await.is_err());
        assert!(attempts.lock().unwrap().is_empty());
        mode.store(1, Ordering::SeqCst);
        assert!(flush(&client, &journal).await.is_err());
        assert!(journal.lock().unwrap().next().unwrap().is_some());
        mode.store(2, Ordering::SeqCst);
        flush(&client, &journal).await.unwrap();
        assert!(journal.lock().unwrap().next().unwrap().is_none());
        assert_eq!(attempts.lock().unwrap().len(), 2);
        task.abort();
    }
}

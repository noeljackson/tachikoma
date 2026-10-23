//! Administrative and producer operations through the public adapter API.
use anyhow::{Context, Result, ensure};
use approval_client::Client;
use approval_protocol::*;
use clap::Subcommand;
use serde::{Serialize, de::DeserializeOwned};
use std::{
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
};

#[derive(Debug, Subcommand)]
pub enum Command {
    Health,
    /// Import legacy proposal metadata as non-authoritative history; leave source data untouched.
    ImportLegacy {
        database: PathBuf,
    },
    Tui {
        #[arg(long)]
        queue: Option<String>,
    },
    Queues,
    PutQueue {
        manifest: PathBuf,
    },
    Enroll {
        spec: PathBuf,
        #[arg(long)]
        credential_out: PathBuf,
    },
    UpdatePrincipal {
        spec: PathBuf,
        #[arg(long)]
        credential_out: PathBuf,
    },
    Revoke {
        id: String,
    },
    Rotate {
        id: String,
        #[arg(long)]
        credential_out: PathBuf,
    },
    Submit {
        request: PathBuf,
    },
    List {
        #[arg(long)]
        history: bool,
    },
    Show {
        id: String,
    },
    Decide {
        id: String,
        action: String,
        #[arg(long, default_value_t = 1)]
        revision: i64,
    },
    Cancel {
        id: String,
    },
    Conversation {
        id: String,
    },
    Chat {
        id: String,
        message: String,
        #[arg(long)]
        model: Option<String>,
        #[arg(long)]
        idempotency_key: String,
    },
    CancelChat {
        id: String,
    },
    Receipt {
        id: String,
        receipt: PathBuf,
    },
}
fn read<T: DeserializeOwned>(path: &Path) -> Result<T> {
    let metadata = std::fs::metadata(path)?;
    ensure!(
        metadata.is_file() && metadata.len() <= MAX_BODY_BYTES as u64,
        "input must be a bounded JSON file"
    );
    serde_json::from_slice(&std::fs::read(path)?).context("invalid approval JSON")
}
fn print<T: Serialize>(value: &T) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}
pub fn default_credential() -> PathBuf {
    dirs::state_dir()
        .unwrap_or_else(|| PathBuf::from(".local/state"))
        .join("tachikoma/approval/admin.token")
}
pub async fn run(
    socket: &Path,
    endpoint: Option<&str>,
    credential: Option<&Path>,
    command: Command,
) -> Result<()> {
    let default = default_credential();
    let endpoint = endpoint.unwrap_or("http://tachikoma.local");
    let client = Client::from_credential_file(
        endpoint,
        (!endpoint.starts_with("https://")).then_some(socket),
        credential.unwrap_or(&default),
    )?;
    match command {
        Command::Tui { queue } => approval_tui::run(client, queue).await,
        Command::ImportLegacy { database } => import_legacy(&client, &database).await,
        Command::Health => print(&client.health().await?),
        Command::Queues => print(&client.queues().await?),
        Command::PutQueue { manifest } => print(&client.put_queue(&read(&manifest)?).await?),
        Command::Enroll {
            spec,
            credential_out,
        } => {
            let spec = read(&spec)?;
            save_credential(&credential_out, client.enroll(&spec)).await
        }
        Command::UpdatePrincipal {
            spec,
            credential_out,
        } => save_credential(&credential_out, client.update_principal(&read(&spec)?)).await,
        Command::Rotate { id, credential_out } => {
            save_credential(&credential_out, client.rotate(&id)).await
        }
        Command::Revoke { id } => {
            client.revoke(&id).await?;
            println!("Principal revoked; pending requests cancelled");
            Ok(())
        }
        Command::Submit { request } => print(&client.submit(&read(&request)?).await?),
        Command::List { history } => print(
            &client
                .list_state(if history { "history" } else { "pending" })
                .await?,
        ),
        Command::Show { id } => print(&client.get(&id).await?),
        Command::Decide {
            id,
            action,
            revision,
        } => print(
            &client
                .decide(
                    &id,
                    &DecideRequest {
                        revision,
                        action_id: action,
                    },
                )
                .await?,
        ),
        Command::Cancel { id } => print(&client.cancel(&id).await?),
        Command::Conversation { id } => print(&client.conversation(&id).await?),
        Command::Chat {
            id,
            message,
            model,
            idempotency_key,
        } => print(
            &client
                .send_message(
                    &id,
                    &SendMessage {
                        message,
                        model,
                        idempotency_key,
                    },
                )
                .await?,
        ),
        Command::CancelChat { id } => print(&client.cancel_conversation(&id).await?),
        Command::Receipt { id, receipt } => print(&client.receipt(&id, &read(&receipt)?).await?),
    }
}

async fn save_credential(
    path: &Path,
    operation: impl std::future::Future<Output = Result<EnrolledPrincipal>>,
) -> Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .context("credential output needs an explicit private parent directory")?;
    if !parent.exists() {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(parent)?;
    }
    let meta = std::fs::symlink_metadata(parent)?;
    ensure!(
        meta.is_dir()
            && !meta.file_type().is_symlink()
            && meta.uid() == std::fs::metadata("/proc/self")?.uid()
            && meta.mode() & 0o077 == 0,
        "credential output directory must be private and owned by this user"
    );
    let mut output = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    match operation.await {
        Ok(enrolled) => {
            use std::io::Write;
            output.write_all(enrolled.credential.as_bytes())?;
            output.sync_all()?;
            println!("Credential saved privately for {}", enrolled.principal.id);
            Ok(())
        }
        Err(error) => {
            drop(output);
            let _ = std::fs::remove_file(path);
            Err(error)
        }
    }
}

async fn import_legacy(client: &Client, path: &Path) -> Result<()> {
    use serde_json::json;
    use sha2::{Digest, Sha256};
    ensure!(
        client.identity().await?.role == Role::Admin,
        "legacy import requires administrator authority"
    );
    let database =
        rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let count: i64 = database.query_row("SELECT count(*) FROM proposals", [], |r| r.get(0))?;
    ensure!(count <= 10_000, "legacy import exceeds the per-run limit");
    let rows=database.prepare("SELECT id,adapter,action,state,risk,created_at,updated_at FROM proposals ORDER BY id")?.query_map([],|r| Ok(json!({"legacy_id":r.get::<_,String>(0)?,"adapter":r.get::<_,String>(1)?,"action":r.get::<_,String>(2)?,"legacy_state":r.get::<_,String>(3)?,"risk":r.get::<_,String>(4)?,"created_at":r.get::<_,String>(5)?,"updated_at":r.get::<_,String>(6)?})))?.collect::<std::result::Result<Vec<_>,_>>()?;
    let queue = "legacy.archive";
    let fields = [
        "legacy_id",
        "adapter",
        "action",
        "legacy_state",
        "risk",
        "created_at",
        "updated_at",
    ]
    .into_iter()
    .map(|key| {
        (
            key.into(),
            Shape::String {
                max_len: 1024,
                choices: vec![],
            },
        )
    })
    .collect();
    let manifest = QueueManifest {
        id: queue.into(),
        title: "Legacy proposal archive".into(),
        service_kind: "legacy".into(),
        schema_id: "legacy.record.v1".into(),
        subject_shape: Shape::Object {
            fields,
            required: vec![],
        },
        actions: vec![ReviewAction {
            id: "archive".into(),
            label: "Archive historical metadata".into(),
            effect: Effect::Observe,
            scope: json!({"mode":"historical-reference-only"}),
        }],
        max_pending: 16,
        deadline_cap_ms: 86_400_000,
        scheduling_class: SchedulingClass::Background,
        mode: Mode::Manual,
        rules: vec![],
        assessment: None,
    };
    if let Some(existing) = client
        .queues()
        .await?
        .into_iter()
        .find(|q| q.manifest.id == queue)
    {
        ensure!(
            serde_json::to_value(existing.manifest)? == serde_json::to_value(&manifest)?,
            "archive manifest differs; inspect it before importing"
        );
    } else {
        client.put_queue(&manifest).await?;
    }
    let source = format!(
        "{:x}",
        Sha256::digest(path.canonicalize()?.as_os_str().as_encoded_bytes())
    );
    for subject in rows {
        let key = format!(
            "{:x}",
            Sha256::digest(canonical_bytes(&(source.as_str(), &subject))?)
        );
        let request = SubmitRequest {
            queue: queue.into(),
            producer_request_id: key.clone(),
            nonce: key.clone(),
            idempotency_key: key,
            schema_id: "legacy.record.v1".into(),
            subject,
            evidence: json!({"import":"Metadata only. Original SQLite, AI analysis and JSONL files remain authoritative historical records at their original paths. Historical states grant no permission."}),
            action_ids: vec!["archive".into()],
            expires_at_ms: 4_102_444_800_000,
            summary: "Imported legacy proposal metadata".into(),
        };
        let review = client.submit(&request).await?;
        if review.state == ReviewState::Pending {
            client
                .decide(
                    &review.id,
                    &DecideRequest {
                        revision: review.revision,
                        action_id: "archive".into(),
                    },
                )
                .await?;
        }
    }
    println!(
        "Archived {count} historical metadata records. Source database and transcripts were preserved."
    );
    Ok(())
}

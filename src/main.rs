use anyhow::Result;
use axum::{
    Form, Router,
    extract::{Path, State},
    http::StatusCode,
    response::Redirect,
    routing::{get, post},
};
use clap::Parser;
use connectrpc::Router as ConnectRouter;
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tachikoma::proto::tachikoma::v1::{
    AutomationPolicyServiceExt, ProposalServiceExt, StatusServiceExt,
};

#[derive(Debug, Parser)]
#[command(name = "tachikomad", about = "Tachikoma local proposal daemon")]
struct Args {
    /// SQLite database path. Defaults to the XDG state directory.
    #[arg(long)]
    database: Option<std::path::PathBuf>,

    /// Loopback-only HTTP address for the web UI.
    #[arg(long, default_value = "127.0.0.1:7447")]
    listen: std::net::SocketAddr,

    /// User-owned Unix socket for the Connect and gRPC API.
    #[arg(long)]
    rpc_socket: Option<std::path::PathBuf>,

    /// OpenSnitch UI history database to read. Omit to leave the adapter disabled.
    #[arg(long)]
    opensnitch_history: Option<std::path::PathBuf>,

    /// Seconds between OpenSnitch history scans when the adapter is enabled.
    #[arg(long, default_value_t = 60)]
    poll_seconds: u64,
}

#[derive(Clone)]
struct WebState {
    store: Arc<Mutex<tachikoma::store::Store>>,
    csrf_token: String,
}

#[derive(serde::Deserialize)]
struct ReviewForm {
    csrf_token: String,
}

fn default_database() -> std::path::PathBuf {
    dirs::state_dir()
        .unwrap_or_else(|| std::path::PathBuf::from(".local/state"))
        .join("tachikoma")
        .join("tachikoma.sqlite3")
}

fn default_rpc_socket() -> std::path::PathBuf {
    dirs::runtime_dir()
        .unwrap_or_else(|| {
            dirs::state_dir().unwrap_or_else(|| std::path::PathBuf::from(".local/state"))
        })
        .join("tachikoma")
        .join("tachikoma.sock")
}

async fn bind_rpc_socket(path: &std::path::Path) -> Result<tokio::net::UnixListener> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("RPC socket path has no parent: {}", path.display()))?;
    std::fs::create_dir_all(parent)?;
    if let Ok(metadata) = std::fs::symlink_metadata(path) {
        if !metadata.file_type().is_socket() {
            anyhow::bail!(
                "refusing to replace non-socket RPC path: {}",
                path.display()
            );
        }
        std::fs::remove_file(path)?;
    }
    let listener = tokio::net::UnixListener::bind(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(listener)
}

fn unix_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

fn csrf_token() -> Result<String> {
    let mut token = [0_u8; 32];
    getrandom::fill(&mut token).map_err(|error| anyhow::anyhow!("generate CSRF token: {error}"))?;
    Ok(token.iter().map(|byte| format!("{byte:02x}")).collect())
}

async fn review_proposal(
    State(state): State<WebState>,
    Path((id, decision)): Path<(String, String)>,
    Form(form): Form<ReviewForm>,
) -> Result<Redirect, StatusCode> {
    if form.csrf_token != state.csrf_token {
        return Err(StatusCode::FORBIDDEN);
    }
    let decision = match decision.as_str() {
        "approve" => "approved",
        "reject" => "rejected",
        _ => return Err(StatusCode::NOT_FOUND),
    };
    state
        .store
        .lock()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .transition(&id, decision, &unix_seconds().to_string())
        .map_err(|_| StatusCode::CONFLICT)?;
    Ok(Redirect::to("/"))
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let args = Args::parse();
    let database = args.database.unwrap_or_else(default_database);
    let rpc_socket = args.rpc_socket.unwrap_or_else(default_rpc_socket);
    let store = Arc::new(Mutex::new(tachikoma::store::Store::open(&database)?));
    let status_api = Arc::new(tachikoma::rpc::StatusApi {
        store: Arc::clone(&store),
        opensnitch_enabled: args.opensnitch_history.is_some(),
    });
    if let Some(history) = args.opensnitch_history {
        let scanner_store = Arc::clone(&store);
        let poll_seconds = args.poll_seconds.max(1);
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(poll_seconds));
            loop {
                interval.tick().await;
                match tachikoma::opensnitch::recent_connections(&history, 100) {
                    Ok(signals) => {
                        let created_at = unix_seconds().to_string();
                        let expires_at = unix_seconds() + 7 * 24 * 60 * 60;
                        for signal in signals {
                            let Some(proposal) = tachikoma::opensnitch::denial_proposal(
                                &signal,
                                tachikoma::rpc::new_proposal_id(),
                                created_at.clone(),
                                expires_at,
                            ) else {
                                continue;
                            };
                            let mut proposal = proposal;
                            let store = scanner_store.lock().expect("store lock");
                            if let Err(error) = store.apply_automatic_policy(&mut proposal) {
                                tracing::warn!(%error, "could not evaluate OpenSnitch automation policy");
                                continue;
                            }
                            match store.create_if_absent(&proposal) {
                                Ok(true) => {
                                    tracing::info!(proposal_id = %proposal.id, "created OpenSnitch review proposal")
                                }
                                Ok(false) => {}
                                Err(error) => {
                                    tracing::warn!(%error, "could not persist OpenSnitch proposal")
                                }
                            }
                        }
                    }
                    Err(error) => {
                        tracing::warn!(path = %history.display(), %error, "could not read OpenSnitch history")
                    }
                }
            }
        });
    }
    let web_state = WebState {
        store: Arc::clone(&store),
        csrf_token: csrf_token()?,
    };
    let proposal_api = Arc::new(tachikoma::rpc::ProposalApi { store });
    let connect = AutomationPolicyServiceExt::register(
        Arc::clone(&proposal_api),
        ProposalServiceExt::register(proposal_api, status_api.register(ConnectRouter::new())),
    );
    let web_app = Router::new()
        .route(
            "/",
            get(move |State(state): State<WebState>| async move {
                let store = Arc::clone(&state.store);
                tachikoma::web::dashboard(
                    store
                        .lock()
                        .expect("store lock")
                        .list(None)
                        .unwrap_or_default(),
                    state.csrf_token,
                )
            }),
        )
        .route("/proposals/{id}/{decision}", post(review_proposal))
        .route("/health", get(|| async { "OK" }))
        .with_state(web_state);
    let rpc_app = Router::new().fallback_service(connect.into_axum_service());
    let rpc_listener = bind_rpc_socket(&rpc_socket).await?;
    let rpc_path = rpc_socket.display().to_string();
    tokio::spawn(async move {
        if let Err(error) = axum::serve(rpc_listener, rpc_app).await {
            tracing::error!(%error, path = %rpc_path, "Tachikoma RPC socket stopped");
        }
    });
    let web_listener = tokio::net::TcpListener::bind(args.listen).await?;
    tracing::info!(path = %database.display(), web_address = %args.listen, rpc_socket = %rpc_socket.display(), "Tachikoma daemon listening");
    axum::serve(web_listener, web_app).await?;
    Ok(())
}

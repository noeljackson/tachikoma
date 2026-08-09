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
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tachikoma::proto::tachikoma::v1::{
    AnalysisServiceExt, AutomationPolicyServiceExt, InvestigationServiceExt, ProposalServiceExt,
    StatusServiceExt,
};
use tokio::sync::watch;

#[derive(Debug, Parser)]
#[command(name = "tachikomad", about = "Tachikoma local proposal daemon")]
struct Args {
    /// JSON configuration file. Defaults to the XDG config directory.
    #[arg(long)]
    config: Option<PathBuf>,

    /// SQLite database path. Defaults to the XDG state directory.
    #[arg(long)]
    database: Option<PathBuf>,

    /// Loopback-only HTTP address for the web UI.
    #[arg(long)]
    listen: Option<std::net::SocketAddr>,

    /// User-owned Unix socket for the Connect and gRPC API.
    #[arg(long)]
    rpc_socket: Option<PathBuf>,

    /// OpenSnitch UI history database to read. Omit to leave the adapter disabled.
    #[arg(long)]
    opensnitch_history: Option<PathBuf>,

    /// Seconds between OpenSnitch history scans when the adapter is enabled.
    #[arg(long)]
    poll_seconds: Option<u64>,
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
    let config_path = args
        .config
        .clone()
        .unwrap_or_else(tachikoma::config::Config::default_path);
    let config_file = if args.config.is_some() {
        tachikoma::config::Config::load(&config_path)?
    } else {
        tachikoma::config::Config::load_or_default(&config_path)?
    };
    let overrides = tachikoma::config::Overrides {
        database: args.database.clone(),
        rpc_socket: args.rpc_socket.clone(),
        listen: args.listen,
        opensnitch_history: args.opensnitch_history.clone(),
        poll_seconds: args.poll_seconds,
    };
    let initial_config = config_file.resolve(&overrides)?;
    let database = initial_config.database.clone();
    let rpc_socket = initial_config.rpc_socket.clone();
    let store = Arc::new(Mutex::new(tachikoma::store::Store::open(&database)?));
    store
        .lock()
        .map_err(|_| anyhow::anyhow!("proposal store lock poisoned"))?
        .recover_running_analyses(&unix_seconds().to_string())?;
    let opensnitch_enabled = Arc::new(AtomicBool::new(initial_config.opensnitch.active()));
    let ai_enabled = Arc::new(AtomicBool::new(initial_config.ai.enabled));
    let (config_tx, config_rx) = watch::channel(initial_config.clone());
    let analysis = Arc::new(tachikoma::analysis::AnalysisScheduler::new(
        Arc::clone(&store),
        config_rx.clone(),
    ));
    let investigations = Arc::new(tachikoma::investigation::InvestigationManager::new(
        Arc::clone(&store),
        config_rx.clone(),
    ));
    investigations.reconcile()?;
    let status_api = Arc::new(tachikoma::rpc::StatusApi {
        store: Arc::clone(&store),
        opensnitch_enabled: Arc::clone(&opensnitch_enabled),
        ai_enabled: Arc::clone(&ai_enabled),
    });
    let mut opensnitch_config_rx = config_rx.clone();
    let scanner_store = Arc::clone(&store);
    let scanner_analysis = Arc::clone(&analysis);
    tokio::spawn(async move {
        loop {
            let settings = opensnitch_config_rx.borrow().opensnitch.clone();
            if !settings.active() {
                if opensnitch_config_rx.changed().await.is_err() {
                    break;
                }
                continue;
            }

            if let Some(history) = settings.history.clone() {
                match tachikoma::opensnitch::recent_connections(&history, settings.limit) {
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
                            let created = {
                                let store = scanner_store.lock().expect("store lock");
                                if let Err(error) = store.apply_automatic_policy(&mut proposal) {
                                    tracing::warn!(%error, "could not evaluate OpenSnitch automation policy");
                                    continue;
                                }
                                match store.create_if_absent(&proposal) {
                                    Ok(created) => created,
                                    Err(error) => {
                                        tracing::warn!(%error, "could not persist OpenSnitch proposal");
                                        false
                                    }
                                }
                            };
                            if created {
                                tracing::info!(proposal_id = %proposal.id, "created OpenSnitch review proposal");
                                if let Err(error) =
                                    scanner_analysis.schedule_automatic(&proposal.id).await
                                {
                                    tracing::debug!(proposal_id = %proposal.id, %error, "no automatic AI analysis scheduled");
                                }
                            }
                        }
                    }
                    Err(error) => {
                        tracing::warn!(path = %history.display(), %error, "could not read OpenSnitch history")
                    }
                }
            }

            let delay = tokio::time::sleep(Duration::from_secs(settings.poll_seconds));
            tokio::pin!(delay);
            tokio::select! {
                _ = &mut delay => {},
                changed = opensnitch_config_rx.changed() => {
                    if changed.is_err() {
                        break;
                    }
                }
            }
        }
    });
    let reload_path = config_path.clone();
    let reload_overrides = overrides.clone();
    let reload_tx = config_tx.clone();
    let reload_status = Arc::clone(&opensnitch_enabled);
    let reload_ai_status = Arc::clone(&ai_enabled);
    let running_config = initial_config.clone();
    tokio::spawn(async move {
        let mut fingerprint = match tachikoma::config::fingerprint(&reload_path) {
            Ok(fingerprint) => fingerprint,
            Err(error) => {
                tracing::warn!(path = %reload_path.display(), %error, "could not inspect Tachikoma config for reload");
                return;
            }
        };
        let mut applied = running_config;
        let mut interval = tokio::time::interval(Duration::from_secs(1));
        loop {
            interval.tick().await;
            let current_fingerprint = match tachikoma::config::fingerprint(&reload_path) {
                Ok(fingerprint) => fingerprint,
                Err(error) => {
                    tracing::warn!(path = %reload_path.display(), %error, "could not inspect Tachikoma config for reload");
                    continue;
                }
            };
            if current_fingerprint == fingerprint {
                continue;
            }
            fingerprint = current_fingerprint;

            let candidate = match tachikoma::config::Config::load_or_default(&reload_path)
                .and_then(|config| config.resolve(&reload_overrides))
            {
                Ok(candidate) => candidate,
                Err(error) => {
                    tracing::warn!(path = %reload_path.display(), %error, "keeping previous Tachikoma config after reload failure");
                    continue;
                }
            };

            if candidate.database != applied.database
                || candidate.rpc_socket != applied.rpc_socket
                || candidate.listen != applied.listen
            {
                tracing::warn!(
                    "database, rpc_socket, and listen config changes require a daemon restart"
                );
            }
            let mut next_applied = candidate.clone();
            next_applied.database = applied.database.clone();
            next_applied.rpc_socket = applied.rpc_socket.clone();
            next_applied.listen = applied.listen;
            let opensnitch_changed = candidate.opensnitch != applied.opensnitch;
            let ai_changed = candidate.ai != applied.ai;
            if (opensnitch_changed || ai_changed) && reload_tx.send(next_applied.clone()).is_err() {
                break;
            }
            if opensnitch_changed {
                reload_status.store(candidate.opensnitch.active(), Ordering::Relaxed);
                tracing::info!(
                    enabled = candidate.opensnitch.active(),
                    path = ?candidate.opensnitch.history,
                    poll_seconds = candidate.opensnitch.poll_seconds,
                    limit = candidate.opensnitch.limit,
                    "reloaded OpenSnitch config"
                );
            }
            if ai_changed {
                reload_ai_status.store(candidate.ai.enabled, Ordering::Relaxed);
                tracing::info!(
                    enabled = candidate.ai.enabled,
                    "reloaded AI analysis config"
                );
            }
            if opensnitch_changed || ai_changed {
                applied = next_applied;
            }
        }
    });
    let web_state = WebState {
        store: Arc::clone(&store),
        csrf_token: csrf_token()?,
    };
    let proposal_api = Arc::new(tachikoma::rpc::ProposalApi {
        store: Arc::clone(&store),
        analysis: Arc::clone(&analysis),
        investigations,
    });
    let connect = InvestigationServiceExt::register(
        Arc::clone(&proposal_api),
        AnalysisServiceExt::register(
            Arc::clone(&proposal_api),
            AutomationPolicyServiceExt::register(
                Arc::clone(&proposal_api),
                ProposalServiceExt::register(
                    proposal_api,
                    status_api.register(ConnectRouter::new()),
                ),
            ),
        ),
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
    let web_listener = tokio::net::TcpListener::bind(initial_config.listen).await?;
    tracing::info!(path = %database.display(), config = %config_path.display(), web_address = %initial_config.listen, rpc_socket = %rpc_socket.display(), "Tachikoma daemon listening");
    axum::serve(web_listener, web_app).await?;
    Ok(())
}

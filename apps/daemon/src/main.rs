//! Generic approval daemon: no domain adapters, provider credentials or tools.
use anyhow::{Context, Result, ensure};
use axum::{Router, response::Redirect, routing::get};
use clap::Parser;
use std::{
    os::{
        fd::AsRawFd,
        unix::fs::{DirBuilderExt, FileTypeExt, MetadataExt, OpenOptionsExt},
    },
    path::{Path, PathBuf},
};
#[derive(Parser)]
struct Args {
    /// State anchor. Approval data is isolated in its parent's approval/ directory.
    #[arg(long)]
    database: Option<PathBuf>,
    #[arg(long)]
    rpc_socket: Option<PathBuf>,
    #[arg(long, default_value = "127.0.0.1:7447")]
    listen: std::net::SocketAddr,
    /// Allow a non-loopback web bind only behind a deliberately configured proxy.
    #[arg(long)]
    allow_container_bind: bool,
}
fn private_parent(path: &Path) -> Result<()> {
    let parent = path.parent().context("path needs a parent directory")?;
    if !parent.exists() {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(parent)?;
    }
    let meta = std::fs::symlink_metadata(parent)?;
    ensure!(
        meta.is_dir() && !meta.file_type().is_symlink(),
        "invalid runtime directory"
    );
    Ok(())
}
async fn shutdown() {
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .expect("install termination handler");
    tokio::select! { _=tokio::signal::ctrl_c()=>{}, _=terminate.recv()=>{} }
}
#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    ensure!(
        args.listen.ip().is_loopback() || args.allow_container_bind,
        "web listener must be loopback"
    );
    let state = dirs::state_dir()
        .context("XDG state directory unavailable")?
        .join("tachikoma");
    let database = args
        .database
        .unwrap_or_else(|| state.join("tachikoma.sqlite3"));
    let runtime = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| state.join("run"));
    let socket = args
        .rpc_socket
        .unwrap_or_else(|| runtime.join("tachikoma/tachikoma.sock"));
    ensure!(socket.is_absolute(), "socket path must be absolute");
    private_parent(&socket)?;
    let lock_path = socket.with_extension("lock");
    if let Ok(meta) = std::fs::symlink_metadata(&lock_path) {
        ensure!(
            meta.is_file()
                && meta.uid() == std::fs::metadata("/proc/self")?.uid()
                && meta.mode() & 0o077 == 0,
            "invalid socket lock"
        );
    }
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(lock_path)?;
    // Held for the entire daemon lifetime; never unlink another running instance.
    ensure!(
        unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0,
        "another daemon owns this socket"
    );
    if let Ok(meta) = std::fs::symlink_metadata(&socket) {
        ensure!(
            meta.file_type().is_socket() && meta.uid() == std::fs::metadata("/proc/self")?.uid(),
            "refusing to replace an unowned or non-socket path"
        );
        ensure!(
            std::os::unix::net::UnixStream::connect(&socket).is_err(),
            "socket already in use"
        );
        std::fs::remove_file(&socket)?;
    }
    let approval = database
        .parent()
        .context("database needs a parent")?
        .join("approval");
    let api = control_plane::Api::new(control_plane::Store::open(
        &approval.join("approval.sqlite3"),
        &approval.join("admin.token"),
    )?);
    let listener = tokio::net::UnixListener::bind(&socket)?;
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600))?;
    let web_listener = tokio::net::TcpListener::bind(args.listen).await?;
    let web = Router::new()
        .route("/", get(|| async { Redirect::to("/approval/") }))
        .route("/health", get(|| async { "OK" }))
        .nest("/approval/", approval_web::router(socket.clone()));
    eprintln!("Tachikoma approval API ready; web {}", args.listen);
    let api_server = axum::serve(listener, api.router()).with_graceful_shutdown(shutdown());
    let web_server = axum::serve(web_listener, web).with_graceful_shutdown(shutdown());
    tokio::try_join!(api_server, web_server)?;
    std::fs::remove_file(socket)?;
    drop(lock);
    Ok(())
}

//! Optional file adapters. No rules, decisions, commands, file contents or raw arguments are emitted.
mod normalize;
use anyhow::{Context, Result, ensure};
use approval_client::Client;
use approval_protocol::*;
use clap::{Parser, ValueEnum};
use std::{
    fs::File,
    io::{BufRead, BufReader, Read, Seek, SeekFrom},
    os::unix::fs::MetadataExt,
    path::PathBuf,
    time::Duration,
};

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum Format {
    Jsonl,
    Tetragon,
    Suricata,
}
#[derive(Parser)]
struct Args {
    #[arg(long)]
    socket: PathBuf,
    #[arg(long)]
    credential: PathBuf,
    #[arg(long)]
    source: String,
    #[arg(long)]
    queue: String,
    #[arg(long, value_enum)]
    format: Format,
    #[arg(long)]
    input: PathBuf,
    #[arg(long)]
    tetragon_policy: Vec<String>,
    /// Import a bounded recent tail once. Otherwise follow new events, starting at EOF.
    #[arg(long)]
    once: bool,
}
async fn bounded<T>(future: impl std::future::Future<Output = Result<T>>) -> Result<T> {
    tokio::time::timeout(Duration::from_secs(5), future)
        .await
        .context("source API timed out")?
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    ensure!(
        !matches!(args.format, Format::Tetragon) || !args.tetragon_policy.is_empty(),
        "Tetragon format requires at least one explicit tracing policy name"
    );
    let client = Client::from_credential_file(
        "http://tachikoma.local",
        Some(&args.socket),
        &args.credential,
    )?;
    let mut reader: Option<BufReader<File>> = None;
    let mut first = true;
    let mut heartbeat = std::time::Instant::now() - Duration::from_secs(30);
    let mut published = 0usize;
    let mut rejected = 0usize;
    loop {
        if reader.is_none() {
            match File::open(&args.input) {
                Ok(mut file) => {
                    ensure!(
                        file.metadata()?.is_file(),
                        "source input must be a regular file"
                    );
                    if first {
                        let end = file.metadata()?.len();
                        let start = if args.once {
                            end.saturating_sub(4 * 1024 * 1024)
                        } else {
                            end
                        };
                        file.seek(SeekFrom::Start(start))?;
                        let mut buffered = BufReader::new(file);
                        if args.once && start > 0 {
                            let mut skip = Vec::new();
                            buffered
                                .by_ref()
                                .take(65_537)
                                .read_until(b'\n', &mut skip)?;
                        }
                        reader = Some(buffered);
                        first = false;
                    } else {
                        reader = Some(BufReader::new(file));
                    }
                }
                Err(_) => {
                    let _ = bounded(client.source_heartbeat(
                        &args.source,
                        &SourceHeartbeat {
                            ready: false,
                            detail: "Input unavailable; coverage gap".into(),
                        },
                    ))
                    .await;
                    if args.once {
                        anyhow::bail!("source input unavailable");
                    }
                    tokio::select! {_=tokio::signal::ctrl_c()=>break,_=tokio::time::sleep(Duration::from_secs(2))=>{}}
                    continue;
                }
            }
        }
        let buffered = reader.as_mut().context("source reader unavailable")?;
        let position = buffered.stream_position()?;
        let mut line = Vec::new();
        let count = buffered
            .by_ref()
            .take(65_537)
            .read_until(b'\n', &mut line)?;
        if count > 65_536 {
            let _ = bounded(client.source_heartbeat(
                &args.source,
                &SourceHeartbeat {
                    ready: false,
                    detail: "Oversized source record; collection stopped".into(),
                },
            ))
            .await;
            anyhow::bail!("source record exceeds limit");
        }
        if count > 0 && line.last() == Some(&b'\n') {
            let value = serde_json::from_slice(&line);
            match value
                .ok()
                .and_then(|v| {
                    normalize::observation(args.format, &args.queue, v, &args.tetragon_policy).ok()
                })
                .flatten()
            {
                Some(observation) => {
                    match bounded(client.publish_observation(&args.source, &observation)).await {
                        Ok(_) => published += 1,
                        Err(_) => {
                            buffered.seek(SeekFrom::Start(position))?;
                            let _ = bounded(client.source_heartbeat(
                                &args.source,
                                &SourceHeartbeat {
                                    ready: false,
                                    detail: "Publish failed; record retained for retry".into(),
                                },
                            ))
                            .await;
                            if args.once {
                                anyhow::bail!("source publish failed");
                            }
                            tokio::time::sleep(Duration::from_secs(2)).await;
                            continue;
                        }
                    }
                }
                None => {
                    rejected += 1;
                }
            }
        } else {
            if count > 0 {
                buffered.seek(SeekFrom::Start(position))?;
            }
            if args.once {
                break;
            }
            if let Ok(metadata) = std::fs::metadata(&args.input) {
                let current = buffered.get_ref().metadata()?;
                if metadata.ino() != current.ino()
                    || metadata.dev() != current.dev()
                    || metadata.len() < position
                {
                    reader = None;
                }
            } else {
                reader = None;
            }
            tokio::select! {_=tokio::signal::ctrl_c()=>break,_=tokio::time::sleep(Duration::from_millis(200))=>{}}
        }
        if heartbeat.elapsed() >= Duration::from_secs(10) {
            let detail = format!(
                "Watching configured metadata input; {published} published, {rejected} ignored; partial coverage"
            );
            let _ = bounded(client.source_heartbeat(
                &args.source,
                &SourceHeartbeat {
                    ready: reader.is_some(),
                    detail,
                },
            ))
            .await;
            heartbeat = std::time::Instant::now();
        }
        if args.once && published >= 200 {
            break;
        }
    }
    // One-shot import is not a continuously healthy sensor.
    let _ = bounded(client.source_heartbeat(
        &args.source,
        &SourceHeartbeat {
            ready: false,
            detail: format!("Collector stopped; {published} published, {rejected} ignored"),
        },
    ))
    .await;
    println!(
        "Metadata observations published: {published}; ignored: {rejected}. No decisions issued."
    );
    Ok(())
}

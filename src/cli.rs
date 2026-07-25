use std::io;
use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand};
use connectrpc::client::{ClientConfig, ServiceTransport};
use http::{Request, header::HOST};
use hyper::client::conn::http1;
use hyper_util::rt::TokioIo;
use tachikoma::proto::tachikoma::v1::{
    ApproveProposalRequest, CreateProposalRequest, GetStatusRequest, ListProposalsRequest,
    ProposalServiceClient, RejectProposalRequest, StatusServiceClient,
};
use tachikoma::{
    adapter::ObservationAdapter,
    kubernetes::{KubernetesCommand, KubernetesCommandAdapter},
};

#[derive(Debug, Parser)]
#[command(name = "tachikoma", about = "Tachikoma Connect API client")]
struct Args {
    /// User-owned Tachikoma Connect/gRPC Unix socket.
    #[arg(long)]
    rpc_socket: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Show daemon and adapter status.
    Status,
    /// List proposals in the durable queue.
    Queue,
    /// Approve one reviewable proposal.
    Approve { id: String },
    /// Reject one reviewable proposal.
    Reject {
        id: String,
        #[arg(long, default_value = "rejected from terminal client")]
        reason: String,
    },
    /// Turn a kubectl command observation into a review-only proposal.
    SuggestKubectl {
        #[arg(long)]
        context: Option<String>,
        #[arg(long)]
        namespace: Option<String>,
        #[arg(required = true, trailing_var_arg = true)]
        arguments: Vec<String>,
    },
}

fn default_rpc_socket() -> PathBuf {
    dirs::runtime_dir()
        .unwrap_or_else(|| dirs::state_dir().unwrap_or_else(|| PathBuf::from(".local/state")))
        .join("tachikoma")
        .join("tachikoma.sock")
}

fn io_error(error: impl std::fmt::Display) -> io::Error {
    io::Error::other(error.to_string())
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let socket = args.rpc_socket.unwrap_or_else(default_rpc_socket);
    let connector = tower::service_fn(
        move |mut request: Request<connectrpc::client::ClientBody>| {
            let socket = socket.clone();
            async move {
                request
                    .headers_mut()
                    .insert(HOST, http::HeaderValue::from_static("tachikoma.local"));
                let stream = tokio::net::UnixStream::connect(socket).await?;
                let (mut sender, connection) = http1::handshake(TokioIo::new(stream))
                    .await
                    .map_err(io_error)?;
                tokio::spawn(async move {
                    if let Err(error) = connection.await {
                        tracing::debug!(%error, "Tachikoma CLI HTTP connection ended");
                    }
                });
                sender.send_request(request).await.map_err(io_error)
            }
        },
    );
    let transport = ServiceTransport::new(connector);
    let config = ClientConfig::new("http://tachikoma.local".parse()?);

    match args.command {
        Command::Status => {
            let response = StatusServiceClient::new(transport, config)
                .get_status(GetStatusRequest::default())
                .await?
                .into_owned();
            println!("Tachikoma {}", response.version);
            for adapter in response.adapters {
                println!(
                    "{}: {} ({})",
                    adapter.name,
                    if adapter.enabled {
                        "enabled"
                    } else {
                        "disabled"
                    },
                    adapter.detail
                );
            }
        }
        Command::Queue => {
            let response = ProposalServiceClient::new(transport, config)
                .list_proposals(ListProposalsRequest::default())
                .await?
                .into_owned();
            for proposal in response.proposals {
                println!(
                    "{}\t{:?}\t{}\t{}\t{}",
                    proposal.id, proposal.state, proposal.adapter, proposal.action, proposal.risk
                );
            }
        }
        Command::Approve { id } => {
            let proposal = ProposalServiceClient::new(transport, config)
                .approve_proposal(ApproveProposalRequest {
                    id,
                    ..Default::default()
                })
                .await?
                .into_owned()
                .proposal
                .into_option()
                .ok_or_else(|| anyhow::anyhow!("server returned no proposal"))?;
            println!("{} approved", proposal.id);
        }
        Command::Reject { id, reason } => {
            let proposal = ProposalServiceClient::new(transport, config)
                .reject_proposal(RejectProposalRequest {
                    id,
                    reason,
                    ..Default::default()
                })
                .await?
                .into_owned()
                .proposal
                .into_option()
                .ok_or_else(|| anyhow::anyhow!("server returned no proposal"))?;
            println!("{} rejected", proposal.id);
        }
        Command::SuggestKubectl {
            context,
            namespace,
            arguments,
        } => {
            let draft = KubernetesCommandAdapter
                .draft(&KubernetesCommand {
                    arguments,
                    context,
                    namespace,
                })
                .expect("required command arguments produce a draft");
            let proposal = ProposalServiceClient::new(transport, config)
                .create_proposal(CreateProposalRequest {
                    adapter: draft.adapter,
                    action: draft.action,
                    risk: draft.risk,
                    evidence_json: draft.evidence_json,
                    preview: draft.preview,
                    rollback: draft.rollback,
                    idempotency_key: draft.idempotency_key,
                    ..Default::default()
                })
                .await?
                .into_owned()
                .proposal
                .into_option()
                .ok_or_else(|| anyhow::anyhow!("server returned no proposal"))?;
            println!("{} queued for review", proposal.id);
        }
    }
    Ok(())
}

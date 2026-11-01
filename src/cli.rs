mod approval_cli;

use std::io;
use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand};
use connectrpc::client::{ClientConfig, ServiceTransport};
use http::{Request, header::HOST};
use hyper::client::conn::http1;
use hyper_util::rt::TokioIo;
use tachikoma::proto::tachikoma::v1::{
    AnalysisServiceClient, AnalyzeProposalRequest, ApproveProposalRequest, AutomationPolicy,
    AutomationPolicyServiceClient, CancelTurnRequest, CreateProposalRequest,
    GetInvestigationRequest, GetStatusRequest, InvestigationServiceClient, ListAnalysesRequest,
    ListModelsRequest, ListPoliciesRequest, ListProposalsRequest, OpenInvestigationRequest,
    ProposalServiceClient, QueueRemediationDraftRequest, RejectProposalRequest, SendMessageRequest,
    SetActiveModelRequest, StatusServiceClient, UpsertPolicyRequest,
};
use tachikoma::{
    adapter::ObservationAdapter,
    kubernetes::{KubernetesCommand, KubernetesCommandAdapter},
};

#[derive(Debug, Parser)]
#[command(
    name = "tachikoma",
    about = "Tachikoma approval client (legacy commands retained for migration)"
)]
struct Args {
    /// User-owned Tachikoma Connect/gRPC Unix socket.
    #[arg(long)]
    rpc_socket: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Public approval API administration and producer operations.
    Approval {
        #[arg(long)]
        credential: Option<PathBuf>,
        #[arg(long)]
        endpoint: Option<String>,
        #[command(subcommand)]
        command: approval_cli::Command,
    },
    /// Show daemon and adapter status.
    Status,
    /// List proposals in the durable queue.
    Queue {
        /// Show proposals from this adapter only.
        #[arg(long)]
        adapter: Option<String>,
    },
    /// Open the terminal UI with service queues and proposal investigations.
    Tui {
        /// Start in this adapter's queue.
        #[arg(long)]
        adapter: Option<String>,
        /// Connect to the separately started legacy proposal daemon.
        #[arg(long)]
        legacy: bool,
    },
    /// List compiled and custom models with local authentication status.
    Models,
    /// Open or resume the primary chat investigation for a proposal.
    Investigate {
        id: String,
        #[arg(long)]
        model: Option<String>,
    },
    /// Send one message to a durable investigation.
    Chat { id: String, message: String },
    /// Switch an idle investigation to another qualified model.
    Model { id: String, model: String },
    /// Cancel the active turn in an investigation.
    Cancel { id: String },
    /// Queue the latest remediation draft as a linked proposal.
    Remediate { id: String },
    /// Start an AI investigation for one proposal.
    Analyze {
        id: String,
        #[arg(long)]
        profile: Option<String>,
        #[arg(long)]
        force: bool,
    },
    /// List durable AI investigations for one proposal.
    Analysis { id: String },
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
    /// List or configure narrowly-scoped automation policies.
    Policy {
        #[command(subcommand)]
        command: PolicyCommand,
    },
}

#[derive(Debug, Subcommand)]
enum PolicyCommand {
    List,
    Upsert {
        id: String,
        #[arg(long)]
        adapter: String,
        #[arg(long)]
        action: String,
        /// Non-empty JSON object that must exactly match proposal evidence.
        #[arg(long)]
        scope: String,
        #[arg(long, default_value = "review")]
        mode: String,
        #[arg(long, default_value = "low")]
        risk_ceiling: String,
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

fn print_latest_assistant(investigation: &tachikoma::proto::tachikoma::v1::Investigation) {
    let content = investigation.events.iter().rev().find_map(|event| {
        let value: serde_json::Value = serde_json::from_str(&event.json).ok()?;
        (event.kind == "message" && value["message"]["role"].as_str() == Some("assistant")).then(
            || {
                value["message"]["content"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string()
            },
        )
    });
    println!(
        "{}",
        content.unwrap_or_else(|| "(no assistant response)".into())
    );
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let socket = args.rpc_socket.unwrap_or_else(default_rpc_socket);
    let approval_socket = socket.clone();
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
    let transport = || ServiceTransport::new(connector.clone());
    let config = ClientConfig::new("http://tachikoma.local".parse()?);

    match args.command {
        Command::Approval {
            credential,
            endpoint,
            command,
        } => {
            approval_cli::run(
                &approval_socket,
                endpoint.as_deref(),
                credential.as_deref(),
                command,
            )
            .await?;
        }
        Command::Status => {
            let response = StatusServiceClient::new(transport(), config.clone())
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
        Command::Queue { adapter } => {
            let response = ProposalServiceClient::new(transport(), config.clone())
                .list_proposals(ListProposalsRequest {
                    adapter: adapter.unwrap_or_default(),
                    ..Default::default()
                })
                .await?
                .into_owned();
            for proposal in response.proposals {
                println!(
                    "{}\t{:?}\t{}\t{}\t{}",
                    proposal.id, proposal.state, proposal.adapter, proposal.action, proposal.risk
                );
            }
        }
        Command::Tui { adapter, legacy } => {
            let credential = approval_cli::default_credential();
            if !legacy {
                let client = approval_client::Client::from_credential_file(
                    "http://tachikoma.local",
                    Some(&approval_socket),
                    &credential,
                )?;
                approval_tui::run(client, adapter).await?;
                return Ok(());
            }
            let response = ProposalServiceClient::new(transport(), config.clone())
                .list_proposals(ListProposalsRequest::default())
                .await?
                .into_owned();
            let models = InvestigationServiceClient::new(transport(), config.clone())
                .list_models(ListModelsRequest::default())
                .await?
                .into_owned()
                .models;
            let status = StatusServiceClient::new(transport(), config.clone())
                .get_status(GetStatusRequest::default())
                .await?
                .into_owned();
            let mut app =
                tachikoma::tui::App::new(response.proposals, models, status.adapters, adapter);
            let mut session = tachikoma::tui::Session::new()?;
            loop {
                match session.run(&mut app)? {
                    tachikoma::tui::Action::Quit => break,
                    tachikoma::tui::Action::RefreshQueue => {
                        let response = ProposalServiceClient::new(transport(), config.clone())
                            .list_proposals(ListProposalsRequest::default())
                            .await?
                            .into_owned();
                        let status = StatusServiceClient::new(transport(), config.clone())
                            .get_status(GetStatusRequest::default())
                            .await?
                            .into_owned();
                        app.update_queue(response.proposals, status.adapters);
                    }
                    tachikoma::tui::Action::OpenProposal(proposal_id) => {
                        let response = InvestigationServiceClient::new(transport(), config.clone())
                            .open_investigation(OpenInvestigationRequest {
                                proposal_id,
                                ..Default::default()
                            })
                            .await;
                        let response = match response {
                            Ok(response) => response.into_owned(),
                            Err(error) => {
                                app.notice = Some(error.to_string());
                                continue;
                            }
                        };
                        app.set_investigation(
                            response.investigation.into_option().ok_or_else(|| {
                                anyhow::anyhow!("server returned no investigation")
                            })?,
                        );
                    }
                    tachikoma::tui::Action::SendMessage {
                        investigation_id,
                        message,
                    } => {
                        let response = InvestigationServiceClient::new(transport(), config.clone())
                            .send_message(SendMessageRequest {
                                investigation_id,
                                message,
                                ..Default::default()
                            })
                            .await?
                            .into_owned();
                        app.set_investigation(
                            response.investigation.into_option().ok_or_else(|| {
                                anyhow::anyhow!("server returned no investigation")
                            })?,
                        );
                    }
                    tachikoma::tui::Action::SetModel {
                        investigation_id,
                        model,
                    } => {
                        let response = InvestigationServiceClient::new(transport(), config.clone())
                            .set_active_model(SetActiveModelRequest {
                                investigation_id,
                                model,
                                ..Default::default()
                            })
                            .await?
                            .into_owned();
                        app.set_investigation(
                            response.investigation.into_option().ok_or_else(|| {
                                anyhow::anyhow!("server returned no investigation")
                            })?,
                        );
                    }
                    tachikoma::tui::Action::CancelTurn(investigation_id) => {
                        let response = InvestigationServiceClient::new(transport(), config.clone())
                            .cancel_turn(CancelTurnRequest {
                                investigation_id,
                                ..Default::default()
                            })
                            .await?
                            .into_owned();
                        app.set_investigation(
                            response.investigation.into_option().ok_or_else(|| {
                                anyhow::anyhow!("server returned no investigation")
                            })?,
                        );
                    }
                    tachikoma::tui::Action::QueueRemediation(investigation_id) => {
                        let proposal = InvestigationServiceClient::new(transport(), config.clone())
                            .queue_remediation_draft(QueueRemediationDraftRequest {
                                investigation_id,
                                ..Default::default()
                            })
                            .await?
                            .into_owned()
                            .proposal
                            .into_option()
                            .ok_or_else(|| anyhow::anyhow!("server returned no proposal"))?;
                        app.proposals.insert(0, proposal);
                    }
                    tachikoma::tui::Action::Refresh(investigation_id) => {
                        let response = InvestigationServiceClient::new(transport(), config.clone())
                            .get_investigation(GetInvestigationRequest {
                                investigation_id,
                                ..Default::default()
                            })
                            .await?
                            .into_owned();
                        app.set_investigation(
                            response.investigation.into_option().ok_or_else(|| {
                                anyhow::anyhow!("server returned no investigation")
                            })?,
                        );
                    }
                }
            }
        }
        Command::Models => {
            let response = InvestigationServiceClient::new(transport(), config.clone())
                .list_models(ListModelsRequest::default())
                .await?
                .into_owned();
            for model in response.models {
                println!(
                    "{}/{}\t{}\t{}\t{}",
                    model.provider,
                    model.id,
                    if model.authenticated {
                        "ready"
                    } else {
                        "no-auth"
                    },
                    model.credential_source,
                    model.usage_scope
                );
            }
        }
        Command::Investigate { id, model } => {
            let investigation = InvestigationServiceClient::new(transport(), config.clone())
                .open_investigation(OpenInvestigationRequest {
                    proposal_id: id,
                    model: model.unwrap_or_default(),
                    ..Default::default()
                })
                .await?
                .into_owned()
                .investigation
                .into_option()
                .ok_or_else(|| anyhow::anyhow!("server returned no investigation"))?;
            println!(
                "{}\t{}/{}\t{}",
                investigation.id,
                investigation.active_provider,
                investigation.active_model,
                investigation.status
            );
        }
        Command::Chat { id, message } => {
            let mut investigation = InvestigationServiceClient::new(transport(), config.clone())
                .send_message(SendMessageRequest {
                    investigation_id: id.clone(),
                    message,
                    ..Default::default()
                })
                .await?
                .into_owned()
                .investigation
                .into_option()
                .ok_or_else(|| anyhow::anyhow!("server returned no investigation"))?;
            while investigation.status == "running" {
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                investigation = InvestigationServiceClient::new(transport(), config.clone())
                    .get_investigation(GetInvestigationRequest {
                        investigation_id: id.clone(),
                        ..Default::default()
                    })
                    .await?
                    .into_owned()
                    .investigation
                    .into_option()
                    .ok_or_else(|| anyhow::anyhow!("server returned no investigation"))?;
            }
            print_latest_assistant(&investigation);
        }
        Command::Model { id, model } => {
            let investigation = InvestigationServiceClient::new(transport(), config.clone())
                .set_active_model(SetActiveModelRequest {
                    investigation_id: id,
                    model,
                    ..Default::default()
                })
                .await?
                .into_owned()
                .investigation
                .into_option()
                .ok_or_else(|| anyhow::anyhow!("server returned no investigation"))?;
            println!(
                "{}/{}",
                investigation.active_provider, investigation.active_model
            );
        }
        Command::Cancel { id } => {
            let investigation = InvestigationServiceClient::new(transport(), config.clone())
                .cancel_turn(CancelTurnRequest {
                    investigation_id: id,
                    ..Default::default()
                })
                .await?
                .into_owned()
                .investigation
                .into_option()
                .ok_or_else(|| anyhow::anyhow!("server returned no investigation"))?;
            println!("{} {}", investigation.id, investigation.status);
        }
        Command::Remediate { id } => {
            let proposal = InvestigationServiceClient::new(transport(), config.clone())
                .queue_remediation_draft(QueueRemediationDraftRequest {
                    investigation_id: id,
                    ..Default::default()
                })
                .await?
                .into_owned()
                .proposal
                .into_option()
                .ok_or_else(|| anyhow::anyhow!("server returned no proposal"))?;
            println!("{} queued for review", proposal.id);
        }
        Command::Analyze { id, profile, force } => {
            let response = AnalysisServiceClient::new(transport(), config.clone())
                .analyze_proposal(AnalyzeProposalRequest {
                    proposal_id: id,
                    profile_id: profile.unwrap_or_default(),
                    force,
                    ..Default::default()
                })
                .await?
                .into_owned();
            let run = response
                .run
                .into_option()
                .ok_or_else(|| anyhow::anyhow!("server returned no analysis run"))?;
            println!("{} {}", run.id, run.state);
        }
        Command::Analysis { id } => {
            let response = AnalysisServiceClient::new(transport(), config.clone())
                .list_analyses(ListAnalysesRequest {
                    proposal_id: id,
                    ..Default::default()
                })
                .await?
                .into_owned();
            for run in response.runs {
                println!(
                    "{}\t{}\t{}\t{}\t{}\t{}",
                    run.id, run.state, run.provider, run.finding, run.priority, run.rationale
                );
            }
        }
        Command::Approve { id } => {
            let proposal = ProposalServiceClient::new(transport(), config.clone())
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
            let proposal = ProposalServiceClient::new(transport(), config.clone())
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
            let proposal = ProposalServiceClient::new(transport(), config.clone())
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
        Command::Policy { command } => match command {
            PolicyCommand::List => {
                let response = AutomationPolicyServiceClient::new(transport(), config.clone())
                    .list_policies(ListPoliciesRequest::default())
                    .await?
                    .into_owned();
                for policy in response.policies {
                    println!(
                        "{}\t{}\t{}\t{}\t{}\t{}",
                        policy.id,
                        policy.adapter,
                        policy.action,
                        policy.mode,
                        policy.risk_ceiling,
                        policy.scope
                    );
                }
            }
            PolicyCommand::Upsert {
                id,
                adapter,
                action,
                scope,
                mode,
                risk_ceiling,
            } => {
                let response = AutomationPolicyServiceClient::new(transport(), config.clone())
                    .upsert_policy(UpsertPolicyRequest {
                        policy: Some(AutomationPolicy {
                            id,
                            adapter,
                            action,
                            scope,
                            mode,
                            risk_ceiling,
                            ..Default::default()
                        })
                        .into(),
                        ..Default::default()
                    })
                    .await?
                    .into_owned();
                let policy = response
                    .policy
                    .into_option()
                    .ok_or_else(|| anyhow::anyhow!("server returned no policy"))?;
                println!("{} saved", policy.id);
            }
        },
    }
    Ok(())
}

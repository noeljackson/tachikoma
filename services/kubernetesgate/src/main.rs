//! Read-only Kubernetes observation producer. It never loads kubeconfig or runs kubectl.
use anyhow::{Result, ensure};
use approval_client::Client;
use approval_protocol::*;
use clap::{Parser, Subcommand};
use serde_json::json;
use std::{
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};
#[derive(Parser)]
struct Args {
    #[arg(long)]
    socket: Option<PathBuf>,
    #[arg(long)]
    credential: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    /// Print the read-only queue manifest for administrator registration.
    Manifest,
    /// Record an observation; never contacts a Kubernetes cluster or runs the command.
    Suggest {
        #[arg(long)]
        context: Option<String>,
        #[arg(long)]
        namespace: Option<String>,
        #[arg(required = true, trailing_var_arg = true)]
        arguments: Vec<String>,
    },
}
fn manifest() -> QueueManifest {
    let text = || Shape::String {
        max_len: 1024,
        choices: vec![],
    };
    QueueManifest{id:"kubernetes.observe".into(),title:"Kubernetes observations".into(),service_kind:"kubernetesgate".into(),schema_id:"kubernetes.observation.v1".into(),subject_shape:Shape::Object{fields:[("context".into(),text()),("namespace".into(),text()),("verb".into(),Shape::String{max_len:16,choices:vec!["get".into(),"describe".into(),"logs".into(),"events".into(),"diff".into()]}),("arguments".into(),Shape::Array{items:Box::new(text()),max_items:64}),("observation_only".into(),Shape::Boolean)].into(),required:vec!["context".into(),"namespace".into(),"verb".into(),"arguments".into(),"observation_only".into()]},actions:vec![ReviewAction{id:"acknowledge".into(),label:"Acknowledge observation".into(),effect:Effect::Observe,scope:json!({"mode":"observation-only"})}],max_pending:256,deadline_cap_ms:3_600_000,scheduling_class:SchedulingClass::Background,mode:Mode::Manual,rules:vec![],assessment:Some(AssessmentProfile{instructions:"Explain this read-only Kubernetes command observation and its uncertainty. No command has been executed; never propose an executed result or authorize changes.".into(),labels:[("unknown".into(),"Observation only; execution state is unknown".into())].into(),subject_fields:vec!["context".into(),"namespace".into(),"verb".into()],evidence_fields:vec![],jev:false,llm:true,models:vec![],..Default::default()})}
}
fn sanitized(arguments: &[String]) -> Result<Vec<String>> {
    ensure!(
        !arguments.is_empty() && arguments.len() <= 64,
        "provide a bounded read-only command observation"
    );
    ensure!(
        ["get", "describe", "logs", "events", "diff"].contains(&arguments[0].as_str()),
        "only read-only observations are supported; execution is not implemented"
    );
    ensure!(
        !arguments
            .iter()
            .any(|v| ["secret", "secrets"].contains(&v.as_str())),
        "Secret resource observations are excluded"
    );
    let sensitive = [
        "--token",
        "--password",
        "--client-key",
        "--certificate-key",
        "--from-literal",
        "--env",
        "--server",
    ];
    let mut output = vec![];
    let mut hide_next = false;
    for argument in arguments {
        ensure!(
            argument.len() <= 1024 && !argument.chars().any(char::is_control),
            "invalid command observation argument"
        );
        if hide_next {
            output.push("<redacted>".into());
            hide_next = false;
            continue;
        }
        if let Some(name) = sensitive
            .iter()
            .find(|name| argument == **name || argument.starts_with(&format!("{name}=")))
        {
            output.push(format!("{name}=<redacted>"));
            hide_next = !argument.contains('=');
        } else {
            output.push(argument.clone());
        }
    }
    Ok(output)
}
#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    match args.command {
        Command::Manifest => println!("{}", serde_json::to_string_pretty(&manifest())?),
        Command::Suggest {
            context,
            namespace,
            arguments,
        } => {
            let arguments = sanitized(&arguments)?;
            let socket = args
                .socket
                .or_else(|| std::env::var_os("TACHIKOMA_SOCKET").map(PathBuf::from))
                .ok_or_else(|| anyhow::anyhow!("set --socket or TACHIKOMA_SOCKET"))?;
            let credential = args
                .credential
                .or_else(|| std::env::var_os("TACHIKOMA_CREDENTIAL_FILE").map(PathBuf::from))
                .ok_or_else(|| anyhow::anyhow!("set --credential or TACHIKOMA_CREDENTIAL_FILE"))?;
            let client =
                Client::from_credential_file("http://tachikoma.local", Some(&socket), &credential)?;
            let identity = client.identity().await?;
            ensure!(
                identity.role == Role::Producer && identity.service_kind == "kubernetesgate",
                "use an enrolled Kubernetesgate producer credential"
            );
            let id = uuid::Uuid::new_v4().to_string();
            let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis() as i64;
            let subject = json!({"context":context.unwrap_or_default(),"namespace":namespace.unwrap_or_default(),"verb":arguments[0],"arguments":arguments,"observation_only":true});
            let request = SubmitRequest {
                queue: "kubernetes.observe".into(),
                producer_request_id: id.clone(),
                nonce: id.clone(),
                idempotency_key: id,
                schema_id: "kubernetes.observation.v1".into(),
                summary: format!(
                    "Kubernetes {} observation; no command executed",
                    subject["verb"].as_str().unwrap_or("unknown")
                ),
                subject,
                evidence: json!({"execution":"No kubeconfig was loaded, no cluster was contacted, and kubectl was not executed."}),
                action_ids: vec!["acknowledge".into()],
                expires_at_ms: now + 3_600_000,
            };
            let review = client.submit(&request).await?;
            let _ = client
                .heartbeat("read-only observation producer; cluster execution disabled")
                .await;
            println!("{}", serde_json::to_string_pretty(&review)?);
        }
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn accepts_observations_but_redacts_credentials_and_rejects_changes() {
        let args = vec!["get".into(), "pods".into(), "--token=private".into()];
        let result = sanitized(&args).unwrap();
        assert!(!result.join(" ").contains("private"));
        assert!(sanitized(&["apply".into(), "-f".into(), "file".into()]).is_err());
        assert!(sanitized(&["get".into(), "secrets".into()]).is_err());
    }
}

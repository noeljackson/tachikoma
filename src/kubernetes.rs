//! Kubernetes command observation adapter.
//!
//! This is deliberately `kube-rs`-ready rather than a cluster client: it turns
//! an already-observed `kubectl` command into a reviewable suggestion and never
//! loads credentials, contacts a cluster, or executes the command.

use serde_json::json;

use crate::adapter::{ObservationAdapter, ProposalDraft};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KubernetesCommand {
    pub arguments: Vec<String>,
    pub context: Option<String>,
    pub namespace: Option<String>,
}

pub struct KubernetesCommandAdapter;

impl ObservationAdapter for KubernetesCommandAdapter {
    type Observation = KubernetesCommand;

    fn name(&self) -> &'static str {
        "kubernetes"
    }

    fn draft(&self, command: &KubernetesCommand) -> Option<ProposalDraft> {
        let verb = command.arguments.first()?;
        let read_only = matches!(
            verb.as_str(),
            "get" | "describe" | "logs" | "events" | "diff"
        );
        let command_line = format!("kubectl {}", command.arguments.join(" "));
        Some(ProposalDraft {
            adapter: self.name().into(),
            action: if read_only {
                "review_kubectl_observation".into()
            } else {
                "review_kubectl_change".into()
            },
            risk: if read_only {
                "low".into()
            } else {
                "high".into()
            },
            evidence_json: json!({
                "tool": "kubectl",
                "arguments": command.arguments,
                "context": command.context,
                "namespace": command.namespace,
            })
            .to_string(),
            preview: format!(
                "Review Kubernetes command suggestion: `{command_line}`. Tachikoma will not contact a cluster or execute it."
            ),
            rollback:
                "No command has been run; rejecting or expiring this proposal changes nothing."
                    .into(),
            idempotency_key: format!(
                "kubectl:{}:{}:{}",
                command.context.as_deref().unwrap_or_default(),
                command.namespace.as_deref().unwrap_or_default(),
                command.arguments.join("\u{1f}")
            ),
        })
    }
}

#[cfg(test)]
mod tests {
    use crate::adapter::ObservationAdapter;

    use super::{KubernetesCommand, KubernetesCommandAdapter};

    #[test]
    fn commands_become_review_only_drafts_with_risk_by_verb() {
        let adapter = KubernetesCommandAdapter;
        let read = adapter
            .draft(&KubernetesCommand {
                arguments: vec!["get".into(), "pods".into()],
                context: Some("development".into()),
                namespace: Some("default".into()),
            })
            .expect("draft");
        assert_eq!(read.risk, "low");
        assert!(read.preview.contains("will not contact a cluster"));

        let change = adapter
            .draft(&KubernetesCommand {
                arguments: vec!["apply".into(), "-f".into(), "deployment.yaml".into()],
                context: None,
                namespace: None,
            })
            .expect("draft");
        assert_eq!(change.risk, "high");
    }
}

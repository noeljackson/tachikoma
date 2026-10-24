use super::{clipped, epoch_ms, visible_action};
use approval_protocol::*;
use serde_json::{Value, json};

fn escaped(value: &str, limit: usize) -> String {
    let mut output = String::new();
    let mut length = 0;
    for character in clipped(value, limit).chars() {
        let replacement = match character {
            '&' => "&amp;".into(),
            '<' => "&lt;".into(),
            '>' => "&gt;".into(),
            '`' => "ˋ".into(),
            _ => character.to_string(),
        };
        let width = replacement.chars().count();
        if length + width > limit {
            break;
        }
        output.push_str(&replacement);
        length += width;
    }
    output
}

fn markup(value: String) -> Value {
    json!({"type":"mrkdwn","text":value,"verbatim":true})
}
fn field(label: &str, value: &str) -> Value {
    markup(format!(
        "*{label}*\n{}",
        escaped(
            if value.is_empty() {
                "Not reported"
            } else {
                value
            },
            650
        )
    ))
}
fn section(value: &str) -> Value {
    json!({"type":"section","text":{"type":"plain_text","text":clipped(value,2900),"emoji":false}})
}
fn context(value: String) -> Value {
    json!({"type":"context","elements":[markup(value)]})
}
fn scalar(value: &Value) -> String {
    match value {
        Value::String(value) => value.clone(),
        Value::Null => String::new(),
        _ => value.to_string(),
    }
}
fn confidence(value: Option<f64>) -> String {
    value
        .filter(|value| value.is_finite())
        .map(|value| format!("{:.0}% confidence", value * 100.))
        .unwrap_or_else(|| "Confidence unavailable".into())
}
fn scope(action: &ReviewAction, network: bool) -> String {
    if network && action.scope == json!({"duration":"once"}) {
        "This connection only · once".into()
    } else if network && action.scope == json!({"duration":"five-minutes"}) {
        "Matching process and destination · 5 minutes".into()
    } else {
        format!("Exact scope: {}", action.scope)
    }
}
pub(super) fn card(delivery: &Delivery) -> Value {
    let review = &delivery.review;
    let active = review.state == ReviewState::Pending && review.expires_at_ms > epoch_ms();
    let network =
        review.service_kind == "snitchgate" || review.request.schema_id == "network.connect.v1";
    let status = match review.state {
        ReviewState::Pending if active => "⏳ Approval needed",
        ReviewState::Pending | ReviewState::Expired => "⌛ Request expired",
        ReviewState::Allowed => "✓ Allow recorded",
        ReviewState::Denied => "⛔ Deny recorded",
        ReviewState::Observed => "Observation recorded",
        ReviewState::Cancelled => "Request cancelled",
    };
    let title = if network {
        format!("{status} · Network access")
    } else {
        format!("{status} · {}", clipped(&review.service_kind, 60))
    };
    let mut blocks =
        vec![json!({"type":"header","text":{"type":"plain_text","text":clipped(&title,150)}})];
    if network {
        let subject = &review.request.subject;
        let host = subject["host"]
            .as_str()
            .filter(|host| !host.is_empty())
            .unwrap_or_else(|| subject["ip"].as_str().unwrap_or("Unknown destination"));
        let destination = format!("{}:{}", host, scalar(&subject["port"]));
        let process = subject["process"]
            .as_str()
            .unwrap_or("Process attribution unavailable");
        let application = process
            .rsplit('/')
            .next()
            .filter(|value| !value.is_empty())
            .unwrap_or("Unknown process");
        blocks.push(json!({"type":"section","fields":[field("Destination",&destination),field("Application",application),field("Transport",&format!("{} · {}",scalar(&subject["protocol"]),scalar(&subject["ip"]))),field("Process",&format!("PID {} · UID {}",scalar(&subject["pid"]),scalar(&subject["uid"]))) ]}));
        blocks.push(context(format!(
            "*Executable*  `{}`",
            escaped(process, 1000)
        )));
        let evidence = &review.request.evidence;
        let process_context = &evidence["process_context"];
        let mut details = vec![];
        if let Some(directory) = process_context["working_directory"].as_str() {
            details.push(field("Working directory", directory));
        }
        if let Some(lifecycle) = process_context["lifecycle_event"].as_str() {
            details.push(field("Lifecycle", lifecycle));
        }
        if !details.is_empty() {
            blocks.push(json!({"type":"section","fields":details}));
        }
        if let Some(hints) = evidence["url_hints"]
            .as_array()
            .filter(|hints| !hints.is_empty())
        {
            let hints = hints
                .iter()
                .filter_map(Value::as_str)
                .take(3)
                .map(|hint| clipped(hint, 750))
                .collect::<Vec<_>>()
                .join("\n");
            blocks.push(section(&format!(
                "Resource hints (reported by the process)\n{hints}"
            )));
        }
        blocks.push(context("Process metadata and URL hints are unverified. Approval covers the connection; encrypted file contents are unavailable.".into()));
    } else {
        blocks.push(section(&review.request.summary));
        let fields = review
            .request
            .subject
            .as_object()
            .map(|subject| {
                subject
                    .iter()
                    .filter(|(_, value)| !value.is_object() && !value.is_array())
                    .take(6)
                    .map(|(name, value)| field(&escaped(name, 60), &scalar(value)))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        if !fields.is_empty() {
            blocks.push(json!({"type":"section","fields":fields}));
        }
    }
    blocks.push(json!({"type":"divider"}));
    if let Some(assessment) = &review.assessment {
        blocks.push(json!({"type":"section","fields":[field("Classification",&assessment.category.replace('_'," ")),field("Conviction",&confidence(assessment.confidence))]}));
        if !assessment.rationale.trim().is_empty() {
            blocks.push(section(&clipped(&assessment.rationale, 1200)));
        }
        let judgments = assessment
            .judgments
            .iter()
            .filter(|(name, _)| name.as_str() != "category")
            .take(4)
            .map(|(name, judgment)| {
                format!(
                    "{}: {} ({:.0}%)",
                    escaped(name, 60),
                    escaped(&judgment.choice, 80),
                    judgment.confidence * 100.
                )
            })
            .collect::<Vec<_>>()
            .join("  ·  ");
        if !judgments.is_empty() {
            blocks.push(context(judgments));
        }
        let models = [
            ("Jev", assessment.jev_model.as_str()),
            ("LLM", assessment.llm_model.as_str()),
        ]
        .into_iter()
        .filter(|(_, model)| !model.is_empty())
        .map(|(label, model)| format!("{label}: {}", escaped(model, 180)))
        .collect::<Vec<_>>()
        .join("  ·  ");
        if !models.is_empty() {
            blocks.push(context(models));
        }
    } else {
        blocks.push(context(
            "Classification pending or not requested. Human review remains required.".into(),
        ));
    }
    if let Some(decision) = &review.decision {
        blocks.push(json!({"type":"divider"}));
        blocks.push(section(&format!(
            "{} · by {}\n{}",
            decision.action.label,
            decision.actor,
            scope(&decision.action, network)
        )));
        let outcome = review
            .receipt
            .as_ref()
            .map(|receipt| format!("Producer outcome: {:?} · {}", receipt.state, receipt.reason))
            .unwrap_or_else(|| {
                "Producer outcome not reported. Recording a decision does not confirm enforcement."
                    .into()
            });
        blocks.push(context(escaped(&outcome, 1500)));
    } else if active {
        blocks.push(context(format!("Expires <!date^{}^{{time_secs}} ({{date_short}})|at the request deadline> · Classification is advisory",review.expires_at_ms/1000)));
        blocks.push(json!({"type":"divider"}));
        for action in &review.actions {
            blocks.push(section(&if visible_action(action) {
                format!("{} · {}", action.label, scope(action, network))
            } else {
                format!(
                    "{} · Scope too large for Slack. Inspect the full action in Tachikoma.",
                    action.label
                )
            }));
        }
        let mut primary = false;
        let mut danger = false;
        let buttons = review.actions.iter().enumerate().filter(|(_,action)|visible_action(action)).map(|(index,action)| {
            let mut button = json!({"type":"button","text":{"type":"plain_text","text":clipped(&action.label,70)},"action_id":format!("tachikoma_decide_{index}"),"value":json!({"delivery":delivery.id,"nonce":delivery.nonce,"action":action.id}).to_string()});
            if action.effect == Effect::Permit && action.scope == json!({"duration":"once"}) && !primary { button["style"] = json!("primary"); primary = true; }
            if action.effect == Effect::Block && !danger { button["style"] = json!("danger"); danger = true; }
            button
        }).collect::<Vec<_>>();
        for chunk in buttons.chunks(5) {
            blocks.push(json!({"type":"actions","elements":chunk}));
        }
    }
    blocks.push(context(format!(
        "{}  ·  {}  ·  `{}`",
        escaped(&review.request.queue, 120),
        escaped(&review.producer, 120),
        escaped(&review.id, 128)
    )));
    json!({"text":clipped(&format!("{status}: {}",review.request.summary),500),"blocks":blocks,"mrkdwn":false,"parse":"none","unfurl_links":false,"unfurl_media":false})
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn escaping_respects_slack_limits_after_expansion_and_keeps_entities_intact() {
        let value = escaped(&"<&>`".repeat(1000), 650);
        assert!(value.chars().count() <= 650);
        assert!(!value.contains('<') && !value.contains('>') && !value.contains('`'));
        assert!(!value.ends_with("&am"));
        let field = field("Destination", &"<&>".repeat(1000));
        assert!(field["text"].as_str().unwrap().chars().count() < 2000);
    }
}

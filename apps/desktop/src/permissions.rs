//! Presentation of the network permission schema; enforcement stays with its producer.
use crate::state::text;
use approval_protocol::{Effect, Review, ReviewAction};
use serde_json::{Value, json};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Field {
    Source,
    Destination,
    Protocol,
    Port,
    Duration,
}
impl Field {
    pub const ALL: [Self; 5] = [
        Self::Source,
        Self::Destination,
        Self::Protocol,
        Self::Port,
        Self::Duration,
    ];
    pub fn key(self) -> &'static str {
        match self {
            Self::Source => "source",
            Self::Destination => "destination",
            Self::Protocol => "protocol",
            Self::Port => "port",
            Self::Duration => "duration",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Source => "SOURCE",
            Self::Destination => "DESTINATION",
            Self::Protocol => "PROTOCOL",
            Self::Port => "PORT",
            Self::Duration => "DURATION",
        }
    }
    pub fn options(
        self,
        review: &Review,
        scope: &Value,
    ) -> Vec<(&'static str, String, Option<&'static str>)> {
        let choices: &[(&str, &str)] = match self {
            Self::Source => &[
                ("executable", "Executable"),
                ("all-applications", "All applications"),
            ],
            Self::Destination => &[
                ("host", "Hostname"),
                ("domain", "Hostname and subdomains"),
                ("ip", "IP address"),
                ("any", "Any destination"),
            ],
            Self::Protocol => &[("this", "Current protocol"), ("any", "TCP and UDP")],
            Self::Port => &[("this", "Current port"), ("any", "Any port")],
            Self::Duration => &[
                ("five-minutes", "5 minutes"),
                ("one-hour", "1 hour"),
                ("until-restart", "Until firewall restart"),
                ("forever", "Forever"),
            ],
        };
        choices
            .iter()
            .map(|&(value, label)| {
                let reason = match (self, value) {
                    (Self::Source, "executable") if !executable(review) => {
                        Some("Executable path unavailable")
                    }
                    (Self::Source, "all-applications") if scope["destination"] == "any" => {
                        Some("Choose a destination first")
                    }
                    (Self::Destination, "host" | "domain") if hostname(review).is_none() => {
                        Some("Hostname unavailable")
                    }
                    (Self::Destination, "any") if scope["source"] == "all-applications" => {
                        Some("Choose an executable first")
                    }
                    _ => None,
                };
                let label = match (self, value) {
                    (Self::Protocol, "this") => text(
                        review.request.subject["protocol"]
                            .as_str()
                            .unwrap_or("Unknown"),
                    )
                    .to_uppercase(),
                    (Self::Port, "this") => review.request.subject["port"].to_string(),
                    _ => label.to_owned(),
                };
                (value, label, reason)
            })
            .collect()
    }
}
pub fn executable(review: &Review) -> bool {
    review.request.subject["process"]
        .as_str()
        .is_some_and(|value| value.starts_with('/'))
}
pub fn hostname(review: &Review) -> Option<String> {
    let host = review.request.subject["host"]
        .as_str()?
        .trim_end_matches('.')
        .to_ascii_lowercase();
    (host.len() <= 253
        && host.parse::<std::net::IpAddr>().is_err()
        && host.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || c == b'-')
        }))
    .then_some(host)
}
pub fn draft(review: &Review, effect: &Effect) -> Option<ReviewAction> {
    let action = review.actions.iter().find(|action| {
        &action.effect == effect
            && action.selectable()
            && matches!(action.id.as_str(), "allow-rule" | "deny-rule")
    })?;
    action.resolve_scope(Some(&json!({"kind":"network-rule", "source":"executable",
        "destination":if hostname(review).is_some() {"host"} else {"ip"},
        "protocol":"this", "port":"this", "duration":"five-minutes"})))
}
pub fn error(review: &Review, action: &ReviewAction) -> Option<&'static str> {
    if action.scope["kind"] != "network-rule" {
        return None;
    }
    if !review.actions.iter().any(|offered| offered.permits(action)) {
        return Some("This permission is not offered by the service.");
    }
    if action.scope["source"] == "executable" && !executable(review) {
        return Some(
            "Executable path unavailable. Select All applications and a specific destination, or approve this request once.",
        );
    }
    if matches!(
        action.scope["destination"].as_str(),
        Some("host" | "domain")
    ) && hostname(review).is_none()
    {
        return Some("Hostname unavailable. Select the IP address instead.");
    }
    if action.scope["source"] == "all-applications" && action.scope["destination"] == "any" {
        return Some("Select an executable or a specific destination.");
    }
    None
}
pub fn summary(review: &Review, action: &ReviewAction) -> String {
    let subject = &review.request.subject;
    let source = if action.scope["source"] == "all-applications" {
        format!("all applications run by UID {}", subject["uid"])
    } else {
        text(
            subject["process"]
                .as_str()
                .filter(|p| !p.is_empty())
                .unwrap_or("an unidentified executable"),
        )
    };
    let destination = match action.scope["destination"].as_str() {
        Some("any") => "any destination".into(),
        Some("host") => hostname(review).unwrap_or_else(|| "an unavailable hostname".into()),
        Some("domain") => format!(
            "{} and its subdomains",
            hostname(review).unwrap_or_else(|| "an unavailable hostname".into())
        ),
        _ => text(subject["ip"].as_str().unwrap_or("an unavailable address")),
    };
    let protocol = if action.scope["protocol"] == "any" {
        "TCP and UDP".into()
    } else {
        text(subject["protocol"].as_str().unwrap_or("unknown protocol")).to_uppercase()
    };
    let port = if action.scope["port"] == "any" {
        "any port".into()
    } else {
        format!("port {}", subject["port"])
    };
    let duration = match action.scope["duration"].as_str() {
        Some("one-hour") => "for 1 hour",
        Some("until-restart") => "until OpenSnitch restarts",
        Some("forever") => "forever, including across restarts",
        _ => "for 5 minutes",
    };
    format!(
        "{} connections from {} to {} on {}, using {}, {}.",
        if action.effect == Effect::Permit {
            "Allow"
        } else {
            "Deny"
        },
        source,
        destination,
        port,
        protocol,
        duration
    )
}

//! Human-selected network scopes. All operands come from the captured connection.
use crate::{
    model::{Connection, Duration},
    protocol::Operator,
};
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Permission {
    pub kind: String,
    pub source: String,
    pub destination: String,
    pub protocol: String,
    pub port: String,
    pub duration: String,
}
pub fn hostname(connection: &Connection) -> Option<String> {
    let host = connection.host.trim_end_matches('.').to_ascii_lowercase();
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
fn exact(operand: &str, data: String) -> Operator {
    Operator {
        r#type: "simple".into(),
        operand: operand.into(),
        data,
        sensitive: true,
        list: vec![],
    }
}
impl Permission {
    pub fn duration(&self) -> Result<Duration> {
        Ok(match self.duration.as_str() {
            "five-minutes" => Duration::FiveMinutes,
            "one-hour" => Duration::OneHour,
            "until-restart" => Duration::UntilRestart,
            "forever" => Duration::Forever,
            _ => anyhow::bail!("unsupported reusable duration"),
        })
    }
    pub fn operators(&self, connection: &Connection) -> Result<Vec<Operator>> {
        ensure!(self.kind == "network-rule", "unsupported network scope");
        self.duration()?;
        ensure!(
            matches!(self.source.as_str(), "executable" | "all-applications"),
            "unsupported source"
        );
        ensure!(
            self.source != "all-applications" || self.destination != "any",
            "choose an executable or a destination"
        );
        let mut operators = vec![exact("user.id", connection.uid.to_string())];
        if self.source == "executable" {
            ensure!(
                connection.process.starts_with('/'),
                "executable path unavailable"
            );
            operators.push(exact("process.path", connection.process.clone()));
        }
        match self.destination.as_str() {
            "ip" => operators.push(exact("dest.ip", connection.ip.to_string())),
            "host" | "domain" => {
                let host =
                    hostname(connection).ok_or_else(|| anyhow::anyhow!("hostname unavailable"))?;
                if self.destination == "host" {
                    let mut operator = exact("dest.host", host);
                    operator.sensitive = false;
                    operators.push(operator);
                } else {
                    // Literal DNS labels only; dots are escaped and both ends anchored.
                    let mut operator = exact(
                        "dest.host",
                        format!("^([a-z0-9-]+\\.)*{}$", host.replace('.', "\\.")),
                    );
                    operator.r#type = "regexp".into();
                    operator.sensitive = false;
                    operators.push(operator);
                }
            }
            "any" => {}
            _ => anyhow::bail!("unsupported destination"),
        }
        match self.protocol.as_str() {
            "this" => operators.push(exact("protocol", connection.protocol.clone())),
            "any" => {
                let mut operator = exact("protocol", "^(tcp|udp)$".into());
                operator.r#type = "regexp".into();
                operators.push(operator);
            }
            _ => anyhow::bail!("unsupported protocol"),
        }
        match self.port.as_str() {
            "this" => operators.push(exact("dest.port", connection.port.to_string())),
            "any" => {}
            _ => anyhow::bail!("unsupported port"),
        }
        Ok(operators)
    }
}

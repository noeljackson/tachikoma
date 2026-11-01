use super::Format;
use anyhow::{Context, Result, ensure};
use approval_protocol::{Observation, canonical_bytes};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::time::{SystemTime, UNIX_EPOCH};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}
fn timestamp(value: &Value) -> Result<i64> {
    let raw = value.as_str().context("missing observation timestamp")?;
    let mut normalized = raw.to_owned();
    let bytes = raw.as_bytes();
    if bytes.len() >= 5
        && matches!(bytes[bytes.len() - 5], b'+' | b'-')
        && bytes[bytes.len() - 4..].iter().all(u8::is_ascii_digit)
    {
        normalized.insert(normalized.len() - 2, ':');
    }
    let raw = normalized.as_str();
    let time = OffsetDateTime::parse(raw, &Rfc3339).context("invalid timestamp")?;
    i64::try_from(time.unix_timestamp_nanos() / 1_000_000).context("timestamp out of range")
}
fn text(value: &Value, limit: usize) -> Option<String> {
    value
        .as_str()
        .filter(|s| s.len() <= limit && !s.chars().any(char::is_control))
        .map(str::to_owned)
}
fn process(value: &Value) -> Value {
    json!({"pid":value["pid"].as_u64().filter(|n|*n<=u32::MAX as u64),"path":text(&value["binary"],1024),"exec_id":text(&value["exec_id"],512),"start_time":text(&value["start_time"],64),"uid":value["uid"].as_u64().filter(|n|*n<=u32::MAX as u64)})
}
pub fn observation(
    format: Format,
    queue: &str,
    value: Value,
    policies: &[String],
) -> Result<Option<Observation>> {
    let event = match format {
        Format::Jsonl => {
            let observation: Observation = serde_json::from_value(value)?;
            ensure!(observation.queue == queue, "input queue mismatch");
            observation
        }
        Format::Tetragon => {
            let observed_at_ms = timestamp(&value["time"])?;
            let (capability, data) = if let Some(probe) = value.get("process_kprobe") {
                if !probe["policy_name"]
                    .as_str()
                    .is_some_and(|p| policies.iter().any(|name| name == p))
                {
                    return Ok(None);
                }
                (
                    "host.sensitive_file_open",
                    json!({"process":process(&probe["process"]),"parent":process(&probe["parent"]),"operation":"open_attempt","coverage":"selected credential paths; successful read not established"}),
                )
            } else if let Some(exec) = value.get("process_exec") {
                let path = exec["process"]["binary"].as_str().unwrap_or("");
                let unlinked = exec["process"]["binary_properties"]["file"]["inode"]["links"]
                    .as_u64()
                    == Some(0);
                if !unlinked
                    && !path.starts_with("/tmp/")
                    && !path.starts_with("/var/tmp/")
                    && !path.starts_with("/dev/shm/")
                    && !path.contains("memfd:")
                    && !(path.starts_with("/proc/") && path.contains("/fd/"))
                {
                    return Ok(None);
                }
                (
                    "host.unusual_execution",
                    json!({"process":process(&exec["process"]),"parent":process(&exec["parent"]),"unlinked":unlinked}),
                )
            } else {
                return Ok(None);
            };
            // Some real kernel events have no attributed executable. Do not
            // invent it or block all subsequent records on a schema rejection.
            if data["process"]["pid"].is_null()
                || data["process"]["path"]
                    .as_str()
                    .is_none_or(|p| p.is_empty())
            {
                return Ok(None);
            }
            Observation {
                id: String::new(),
                queue: queue.into(),
                capability: capability.into(),
                observed_at_ms,
                data,
            }
        }
        Format::Suricata => {
            if value["event_type"] != "alert" {
                return Ok(None);
            }
            let observed_at_ms = timestamp(&value["timestamp"])?;
            let data = json!({"source_ip":text(&value["src_ip"],64),"source_port":value["src_port"].as_u64(),"destination_ip":text(&value["dest_ip"],64),"destination_port":value["dest_port"].as_u64(),"protocol":text(&value["proto"],16).map(|p|p.to_ascii_lowercase()),"signature_id":value["alert"]["signature_id"].as_u64(),"severity":value["alert"]["severity"].as_u64(),"flow_id":value["flow_id"].as_u64().map(|id|id.to_string())});
            Observation {
                id: String::new(),
                queue: queue.into(),
                capability: "network.alert".into(),
                observed_at_ms,
                data,
            }
        }
    };
    let mut event = event;
    fn omit_missing(value: &mut Value) {
        if let Some(object) = value.as_object_mut() {
            object.retain(|_, v| !v.is_null());
            for v in object.values_mut() {
                omit_missing(v);
            }
        }
    }
    omit_missing(&mut event.data);
    if event.observed_at_ms < now() - 3_600_000 || event.observed_at_ms > now() + 2000 {
        return Ok(None);
    }
    if event.id.is_empty() {
        let hash = format!("{:x}", Sha256::digest(canonical_bytes(&event)?));
        Ok(Some(Observation { id: hash, ..event }))
    } else {
        Ok(Some(event))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn time() -> String {
        OffsetDateTime::now_utc().format(&Rfc3339).unwrap()
    }
    #[test]
    fn tetragon_adapter_drops_arguments_environment_and_sensitive_paths() {
        let event = json!({"time":time(),"process_kprobe":{"policy_name":"noelarchy-credentials","process":{"pid":42,"binary":"/usr/bin/node","exec_id":"stable-id","arguments":"--token=private-secret","environment_variables":{"TOKEN":"private-secret"}},"parent":{"pid":40,"binary":"/usr/bin/npm"},"args":[{"file_arg":{"path":"/home/private/.aws/credentials"}}]}});
        let observation = observation(
            Format::Tetragon,
            "network.egress",
            event,
            &["noelarchy-credentials".into()],
        )
        .unwrap()
        .unwrap();
        let serialized = serde_json::to_string(&observation).unwrap();
        assert!(!serialized.contains("private-secret") && !serialized.contains("/home/private"));
        assert_eq!(observation.data["operation"], "open_attempt");
    }
    #[test]
    fn suricata_adapter_drops_payload_and_preserves_flow_join_fields() {
        let event = json!({"timestamp":time().replace('Z',"+0000"),"event_type":"alert","src_ip":"192.0.2.1","src_port":44321,"dest_ip":"203.0.113.3","dest_port":443,"proto":"TCP","alert":{"signature_id":123,"severity":1},"payload":"private-payload","http":{"authorization":"private-token"}});
        let observation = observation(
            Format::Suricata,
            "network.egress",
            event,
            &["noelarchy-credentials".into()],
        )
        .unwrap()
        .unwrap();
        let encoded = serde_json::to_string(&observation).unwrap();
        assert!(!encoded.contains("private"));
        assert_eq!(observation.data["source_port"], 44321);
        assert_eq!(observation.data["protocol"], "tcp");
    }
    #[test]
    fn stale_records_are_not_retimed_and_ids_are_replay_stable() {
        let event = json!({"timestamp":time(),"event_type":"alert","alert":{"signature_id":123}});
        let a = observation(Format::Suricata, "q", event.clone(), &[])
            .unwrap()
            .unwrap();
        let b = observation(Format::Suricata, "q", event, &[])
            .unwrap()
            .unwrap();
        assert_eq!(a.id, b.id);
        let event = json!({"timestamp":"2000-01-01T00:00:00Z","event_type":"alert"});
        assert!(
            observation(Format::Suricata, "q", event, &[])
                .unwrap()
                .is_none()
        );
    }
    #[test]
    fn unattributed_tetragon_records_do_not_invent_an_executable_or_stall_ingestion() {
        let event = json!({"time":time(),"process_kprobe":{"policy_name":"credentials","process":{"pid":42},"parent":{"binary":"/usr/bin/node"}}});
        assert!(
            observation(Format::Tetragon, "q", event, &["credentials".into()])
                .unwrap()
                .is_none()
        );
    }
}

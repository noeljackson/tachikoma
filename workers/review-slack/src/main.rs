//! Generic Slack transport. Approval authority and durable delivery records live in Tachikoma.
mod notify;
mod socket;
use anyhow::{Context, Result, anyhow, ensure};
use approval_client::Client;
use approval_protocol::*;
use clap::Parser;
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::sync::mpsc;
use tokio_tungstenite::{connect_async_with_config, tungstenite::protocol::WebSocketConfig};

#[derive(Parser)]
struct Args {
    #[arg(long)]
    socket: PathBuf,
    #[arg(long)]
    credential: PathBuf,
    #[arg(long)]
    bot_token_file: Option<PathBuf>,
    #[arg(long)]
    app_token_file: Option<PathBuf>,
    /// Explicit operator-supplied initial channel; otherwise register via an authorized mention.
    #[arg(long, requires = "register_actor")]
    channel: Option<String>,
    #[arg(long, requires = "channel")]
    register_actor: Option<String>,
}
struct Slack {
    http: reqwest::Client,
    bot: String,
    app: String,
}
async fn response_json(mut response: reqwest::Response) -> Result<Value> {
    ensure!(response.status().is_success(), "Slack API status failed");
    let mut bytes = vec![];
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| anyhow!("Slack response read failed"))?
    {
        ensure!(
            bytes.len() + chunk.len() <= 65_536,
            "Slack response exceeds limit"
        );
        bytes.extend_from_slice(&chunk);
    }
    let value: Value =
        serde_json::from_slice(&bytes).map_err(|_| anyhow!("invalid Slack response"))?;
    if value["ok"] != true {
        let code = value["error"]
            .as_str()
            .filter(|code| {
                [
                    "invalid_blocks",
                    "not_in_channel",
                    "channel_not_found",
                    "missing_scope",
                    "ratelimited",
                    "invalid_auth",
                    "token_revoked",
                    "msg_too_long",
                ]
                .contains(code)
            })
            .unwrap_or("rejected");
        anyhow::bail!("Slack rejected API request ({code})");
    }
    Ok(value)
}
impl Slack {
    async fn api(&self, method: &str, body: Value, app: bool) -> Result<Value> {
        let response = self
            .http
            .post(format!("https://slack.com/api/{method}"))
            .bearer_auth(if app { &self.app } else { &self.bot })
            .json(&body)
            .timeout(Duration::from_secs(10))
            .send()
            .await
            .map_err(|_| anyhow!("Slack API request unavailable"))?;
        response_json(response).await
    }
    async fn connection(&self, events: mpsc::Sender<Value>, connected: &AtomicBool) -> Result<()> {
        let ticket = self.api("apps.connections.open", json!({}), true).await?;
        let url = ticket["url"]
            .as_str()
            .context("Slack socket ticket missing")?;
        let parsed =
            reqwest::Url::parse(url).map_err(|_| anyhow!("invalid Slack socket ticket"))?;
        ensure!(
            parsed.scheme() == "wss"
                && parsed.username().is_empty()
                && parsed.password().is_none()
                && parsed.port_or_known_default() == Some(443)
                && parsed
                    .host_str()
                    .is_some_and(|h| h == "slack.com" || h.ends_with(".slack.com")),
            "untrusted Slack socket destination"
        );
        let limits = WebSocketConfig::default()
            .max_message_size(Some(65_536))
            .max_frame_size(Some(65_536));
        let (mut stream, _) = tokio::time::timeout(
            Duration::from_secs(10),
            connect_async_with_config(url, Some(limits), true),
        )
        .await
        .map_err(|_| anyhow!("Slack socket timeout"))?
        .map_err(|_| anyhow!("Slack socket unavailable"))?;
        connected.store(true, Ordering::Relaxed);
        let result = socket::forward_socket(&mut stream, events).await;
        connected.store(false, Ordering::Relaxed);
        result
    }
}
fn secret(path: Option<PathBuf>, name: &str) -> Result<String> {
    let path = path.or_else(|| std::env::var_os(format!("{name}_FILE")).map(PathBuf::from));
    if let Some(path) = path {
        use std::os::unix::fs::MetadataExt;
        let file = std::fs::symlink_metadata(&path)?;
        let uid = std::fs::metadata("/proc/self")?.uid();
        let private = file.uid() == uid && file.mode() & 0o077 == 0;
        let systemd = std::env::var_os("CREDENTIALS_DIRECTORY")
            .map(PathBuf::from)
            .filter(|root| path.parent() == Some(root.as_path()))
            .and_then(|root| std::fs::symlink_metadata(root).ok())
            .is_some_and(|root| {
                file.uid() == 0
                    && file.gid() == 0
                    && file.mode() & 0o037 == 0
                    && root.is_dir()
                    && !root.file_type().is_symlink()
                    && root.uid() == 0
                    && root.gid() == 0
                    && root.mode() & 0o027 == 0
            });
        ensure!(
            file.is_file() && file.len() <= 65_536 && (private || systemd),
            "Slack credential must be a protected regular file"
        );
        return Ok(std::fs::read_to_string(path)?.trim().into());
    }
    std::env::var(name).context("Slack credential unavailable")
}
fn clipped(value: &str, limit: usize) -> String {
    display_text(value).chars().take(limit).collect()
}
fn card(delivery: &Delivery) -> Value {
    let review = &delivery.review;
    let active = review.state == ReviewState::Pending && review.expires_at_ms > epoch_ms();
    let assessment = review
        .assessment
        .as_ref()
        .map(|a| {
            format!(
                "{} — {}\nJev: {} / LLM: {}\n{}",
                a.category,
                a.confidence
                    .map(|c| format!("{:.1}% confidence", 100.0 * c))
                    .unwrap_or_else(|| "confidence unavailable".into()),
                a.jev_model,
                a.llm_model,
                format_args!(
                    "{}\n{}",
                    a.judgments
                        .iter()
                        .filter(|(id, _)| id.as_str() != "category")
                        .map(|(id, j)| format!(
                            "{}: {} ({:.0}%)",
                            id,
                            j.choice,
                            j.confidence * 100.0
                        ))
                        .collect::<Vec<_>>()
                        .join(" · "),
                    a.rationale
                )
            )
        })
        .unwrap_or_else(|| {
            "Classification pending or not requested. Human review remains required.".into()
        });
    let outcome = review
        .decision
        .as_ref()
        .map(|d| {
            format!(
                "Decision: {} by {}\nProducer outcome: {}",
                d.action.label,
                d.actor,
                review
                    .receipt
                    .as_ref()
                    .map(|r| format!("{:?}: {}", r.state, r.reason))
                    .unwrap_or_else(|| "not reported".into())
            )
        })
        .unwrap_or_else(|| format!("Deadline: {} (UTC epoch ms)", review.expires_at_ms));
    let mut blocks = vec![
        json!({"type":"header","text":{"type":"plain_text","text":clipped(&format!("{} / {}",review.service_kind,review.request.queue),140)}}),
        json!({"type":"section","text":{"type":"plain_text","text":clipped(&review.request.summary,2900)}}),
        json!({"type":"section","text":{"type":"plain_text","text":clipped(&format!("{}\n{}",review.request.subject,review.request.evidence),2900)}}),
        json!({"type":"section","text":{"type":"plain_text","text":clipped(&assessment,2900)}}),
        json!({"type":"section","text":{"type":"plain_text","text":clipped(&outcome,2900)}}),
    ];
    if active {
        for action in &review.actions {
            let detail = if visible_action(action) {
                format!(
                    "{} — {:?}\nExact scope: {}",
                    action.label, action.effect, action.scope
                )
            } else {
                format!(
                    "{}: scope too large for a Slack card. Inspect and decide in the central TUI or web view.",
                    action.label
                )
            };
            blocks.push(
                json!({"type":"section","text":{"type":"plain_text","text":clipped(&detail,2900)}}),
            );
        }
        // Slack requires action_id to be unique within its containing actions block.
        let elements=review.actions.iter().enumerate().filter(|(_,a)| visible_action(a)).map(|(index,action)|json!({"type":"button","text":{"type":"plain_text","text":clipped(&action.label,70)},"action_id":format!("tachikoma_decide_{index}"),"value":json!({"delivery":delivery.id,"nonce":delivery.nonce,"action":action.id}).to_string()})).collect::<Vec<_>>();
        if !elements.is_empty() {
            blocks.push(json!({"type":"actions","elements":elements}));
        }
    }
    json!({"text":format!("Tachikoma review {}",review.id),"blocks":blocks,"mrkdwn":false,"parse":"none","unfurl_links":false,"unfurl_media":false})
}
fn visible_action(action: &ReviewAction) -> bool {
    action.scope.to_string().len() <= 1800
}
fn epoch_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}
enum Incoming {
    Route(RouteRegistration),
    Button {
        delivery: String,
        reply: DeliveryReply,
    },
    Text {
        actor: String,
        channel: String,
        thread: String,
        permit: bool,
    },
}
fn parse(envelope: &Value, team: &str, bot: &str) -> Option<Incoming> {
    let p = &envelope["payload"];
    if envelope["type"] == "events_api" && p["team_id"] == team {
        let e = &p["event"];
        if !e["subtype"].is_null() || !e["bot_id"].is_null() || e["hidden"] == true {
            return None;
        }
        let actor = e["user"].as_str()?.to_owned();
        let channel = e["channel"].as_str()?.to_owned();
        let text = e["text"].as_str()?.trim();
        if e["type"] == "app_mention"
            && e["thread_ts"].is_null()
            && text == format!("<@{bot}> approvals here")
        {
            let (s, u) = e["ts"].as_str()?.split_once('.')?;
            if u.len() != 6 {
                return None;
            }
            let at = s
                .parse::<i64>()
                .ok()?
                .checked_mul(1_000_000)?
                .checked_add(u.parse::<i64>().ok()?)?;
            return Some(Incoming::Route(RouteRegistration {
                actor,
                channel,
                event_micros: at,
            }));
        }
        if e["type"] == "message" {
            let permit = match text.to_ascii_lowercase().as_str() {
                "yes" => true,
                "no" => false,
                _ => return None,
            };
            return Some(Incoming::Text {
                actor,
                channel,
                thread: e["thread_ts"].as_str()?.into(),
                permit,
            });
        }
    }
    if envelope["type"] == "interactive" && p["type"] == "block_actions" && p["team"]["id"] == team
    {
        let actions = p["actions"].as_array()?;
        if actions.len() != 1
            || actions[0]["action_id"]
                .as_str()
                .and_then(|id| id.strip_prefix("tachikoma_decide_"))
                .and_then(|id| id.parse::<usize>().ok())
                .is_none_or(|index| index >= 16)
        {
            return None;
        }
        let value: Value = serde_json::from_str(actions[0]["value"].as_str()?).ok()?;
        return Some(Incoming::Button {
            delivery: value["delivery"].as_str()?.into(),
            reply: DeliveryReply {
                actor: p["user"]["id"].as_str()?.into(),
                channel: p["container"]["channel_id"].as_str()?.into(),
                message: p["container"]["message_ts"].as_str()?.into(),
                nonce: value["nonce"].as_str()?.into(),
                action_id: value["action"].as_str()?.into(),
            },
        });
    }
    None
}
async fn tick(
    client: &Client,
    slack: &Slack,
    sent: &mut std::collections::HashMap<String, String>,
) -> Result<()> {
    if client.route().await?.is_none() {
        return Ok(());
    }
    let deliveries = client.deliveries().await?;
    ensure!(
        !deliveries
            .iter()
            .any(|d| d.message.is_none() && d.review.state == ReviewState::Pending),
        "Unconfirmed Slack delivery; use the central inbox"
    );
    // New requests have priority. A failed reservation never falls through to an
    // untracked post, and each tick performs at most one Slack write.
    for review in client.list().await? {
        if review.mode == Mode::Shadow
            || deliveries
                .iter()
                .any(|d| d.review.id == review.id && d.review.revision == review.revision)
        {
            continue;
        }
        match client
            .reserve(&ReserveDelivery {
                request_id: review.id,
                revision: review.revision,
            })
            .await
        {
            Ok(delivery) if delivery.status == "newly_reserved" => {
                let mut body = card(&delivery);
                let fingerprint = body.to_string();
                body["channel"] = delivery.channel.clone().into();
                let value = slack.api("chat.postMessage", body, false).await?;
                let message = value["ts"]
                    .as_str()
                    .context("Slack post acknowledgement missing")?;
                client.delivered(&delivery.id, message).await?;
                sent.insert(delivery.id, fingerprint);
                return Ok(());
            }
            _ => break,
        }
    }
    for delivery in &deliveries {
        if let Some(message) = &delivery.message {
            let mut body = card(delivery);
            let fingerprint = body.to_string();
            if sent.get(&delivery.id) == Some(&fingerprint) {
                continue;
            }
            body["channel"] = delivery.channel.clone().into();
            body["ts"] = message.clone().into();
            slack.api("chat.update", body, false).await?;
            sent.insert(delivery.id.clone(), fingerprint);
            break;
        }
    }
    let known = deliveries
        .iter()
        .map(|d| d.id.as_str())
        .collect::<std::collections::HashSet<_>>();
    sent.retain(|id, _| known.contains(id.as_str()));
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let client = Client::from_credential_file(
        "http://tachikoma.local",
        Some(&args.socket),
        &args.credential,
    )?;
    let identity = client.identity().await?;
    ensure!(
        identity.role == Role::Transport,
        "credential is not a review transport"
    );
    let bot = secret(args.bot_token_file, "SLACK_BOT_TOKEN")?;
    let app = secret(args.app_token_file, "SLACK_APP_TOKEN")?;
    ensure!(
        bot.starts_with("xoxb-") && app.starts_with("xapp-"),
        "invalid Slack credential types"
    );
    let slack = Arc::new(Slack {
        http: reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .build()?,
        bot,
        app,
    });
    let auth = slack.api("auth.test", json!({}), false).await?;
    let team = auth["team_id"]
        .as_str()
        .context("missing Slack workspace")?
        .to_owned();
    let bot = auth["user_id"]
        .as_str()
        .context("missing Slack bot identity")?
        .to_owned();
    ensure!(
        identity.realm == format!("slack:{team}"),
        "Slack workspace does not match the transport credential realm"
    );
    if let (Some(channel), Some(actor)) = (args.channel, args.register_actor) {
        client
            .register_route(&RouteRegistration {
                channel,
                actor,
                event_micros: epoch_ms().saturating_mul(1000),
            })
            .await?;
    }
    let (events, mut incoming) = mpsc::channel(64);
    let connected = Arc::new(AtomicBool::new(false));
    let connection = Arc::clone(&connected);
    let reader = Arc::clone(&slack);
    let worker = tokio::spawn(async move {
        let mut backoff = 1;
        loop {
            let _ = reader.connection(events.clone(), &connection).await;
            connection.store(false, Ordering::Relaxed);
            let _ = notify::notify("STATUS=Slack reconnecting");
            tokio::time::sleep(Duration::from_secs(backoff)).await;
            backoff = (backoff * 2).min(30);
        }
    });
    let mut sent = std::collections::HashMap::new();
    let mut interval = tokio::time::interval(Duration::from_secs(1));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    loop {
        tokio::select! {
            _=tokio::signal::ctrl_c()=>break,
            _=term.recv()=>break,
            _=interval.tick()=>{
                let ready=connected.load(Ordering::Relaxed)&&client.route().await.ok().flatten().is_some();
                if ready {
                    match tick(&client,&slack,&mut sent).await {
                        Ok(()) => { let _=client.heartbeat("Slack connected; approval route available").await;let _=notify::notify("READY=1\nSTATUS=Slack connected"); },
                        Err(error) => { eprintln!("Slack delivery unavailable: {error}");let _=client.heartbeat("Slack delivery or approval API unavailable").await;let _=notify::notify("STATUS=Slack delivery or approval API unavailable"); }
                    }
                } else { let _=client.heartbeat("Slack disconnected or route unavailable").await;let _=notify::notify("STATUS=Slack reconnecting"); }
            },
            Some(envelope)=incoming.recv()=>{match parse(&envelope,&team,&bot){Some(Incoming::Route(route))=>{let _=client.register_route(&route).await;},Some(Incoming::Button{delivery,reply})=>{let _=client.reply(&delivery,&reply).await;},Some(Incoming::Text{actor,channel,thread,permit})=>{if let Ok(deliveries)=client.deliveries().await&& let Some(delivery)=deliveries.iter().find(|d|d.channel==channel&&d.message.as_deref()==Some(thread.as_str())){let actions=delivery.review.actions.iter().filter(|a|visible_action(a) && if permit{a.effect==Effect::Permit}else{a.effect==Effect::Block}).collect::<Vec<_>>();let selected=if permit{actions.iter().find(|a|a.scope==json!({"duration":"once"})).copied().or_else(||(actions.len()==1).then(||actions[0]))}else{(actions.len()==1).then(||actions[0])};if let Some(action)=selected{let _=client.reply(&delivery.id,&DeliveryReply{actor,channel,message:thread,nonce:delivery.nonce.clone(),action_id:action.id.clone()}).await;}}},None=>{}}
            }
        }
    }
    worker.abort();
    let _ = notify::notify("STOPPING=1\nSTATUS=Slack stopped");
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ignores_free_floating_yes_edited_messages_and_wrong_workspaces() {
        let value = json!({"type":"events_api","payload":{"team_id":"T1","event":{"type":"message","user":"U1","channel":"C1","text":"yes"}}});
        assert!(parse(&value, "T1", "B1").is_none());
        let mut value = value;
        value["payload"]["event"]["thread_ts"] = "1.000001".into();
        assert!(parse(&value, "T1", "B1").is_some());
        assert!(parse(&value, "OTHER", "B1").is_none());
        value["payload"]["event"]["subtype"] = "message_changed".into();
        assert!(parse(&value, "T1", "B1").is_none());
    }
    #[test]
    fn approval_buttons_parse_opaque_bindings_and_wrong_workspaces_fail() {
        let envelope = json!({"type":"interactive","payload":{"type":"block_actions","team":{"id":"T1"},"user":{"id":"U1"},"container":{"channel_id":"C1","message_ts":"1.000001"},"actions":[{"action_id":"tachikoma_decide_0","value":json!({"delivery":"delivery-1","nonce":"nonce-1234","action":"allow-once"}).to_string()}]}});
        let Some(Incoming::Button { delivery, reply }) = parse(&envelope, "T1", "B1") else {
            panic!("expected button")
        };
        assert_eq!(delivery, "delivery-1");
        assert_eq!(reply.channel, "C1");
        assert_eq!(reply.message, "1.000001");
        assert_eq!(reply.nonce, "nonce-1234");
        assert_eq!(reply.actor, "U1");
        assert!(parse(&envelope, "OTHER", "B1").is_none());
        assert!(!visible_action(&ReviewAction {
            id: "large".into(),
            label: "Large".into(),
            effect: Effect::Permit,
            scope: json!({"value":"a".repeat(1900)})
        }));
    }
    #[test]
    fn card_actions_have_unique_slack_identifiers_and_show_exact_scopes() {
        let actions = vec![
            ReviewAction {
                id: "allow".into(),
                label: "Allow once".into(),
                effect: Effect::Permit,
                scope: json!({"duration":"once"}),
            },
            ReviewAction {
                id: "deny".into(),
                label: "Deny once".into(),
                effect: Effect::Block,
                scope: json!({"duration":"once"}),
            },
        ];
        let request = SubmitRequest {
            queue: "network.egress".into(),
            producer_request_id: "fixture".into(),
            nonce: "fixture-nonce-1234".into(),
            idempotency_key: "fixture".into(),
            schema_id: "fixture".into(),
            subject: json!({}),
            evidence: json!({}),
            action_ids: vec!["allow".into(), "deny".into()],
            expires_at_ms: epoch_ms() + 60_000,
            summary: "Fixture".into(),
        };
        let review = Review {
            id: "fixture".into(),
            producer: "fixture".into(),
            service_kind: "snitchgate".into(),
            revision: 1,
            manifest_revision: 1,
            subject_digest: String::new(),
            evidence_digest: String::new(),
            created_at_ms: epoch_ms(),
            expires_at_ms: epoch_ms() + 60_000,
            mode: Mode::Manual,
            state: ReviewState::Pending,
            actions,
            request,
            decision: None,
            assessment: None,
            receipt: None,
        };
        let delivery = Delivery {
            id: "fixture".into(),
            realm: "slack:T1".into(),
            channel: "C1".into(),
            message: None,
            nonce: "nonce".into(),
            review,
            status: "newly_reserved".into(),
        };
        let body = card(&delivery);
        let buttons = body["blocks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|b| b["type"] == "actions")
            .unwrap()["elements"]
            .as_array()
            .unwrap();
        let identifiers = buttons
            .iter()
            .map(|b| b["action_id"].as_str().unwrap())
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(buttons.len(), 2);
        assert_eq!(identifiers.len(), 2);
        assert!(body["blocks"].as_array().unwrap().iter().any(|b| {
            b["text"]["text"]
                .as_str()
                .is_some_and(|s| s.contains("Exact scope: {\"duration\":\"once\"}"))
        }));
    }
}

//! Desktop presentation only. Decisions still go through the public reviewer API.
use crate::{backend::Backend, state::now_ms};
use anyhow::{Result, ensure};
use approval_protocol::{DecideRequest, Effect, Mode, Review, ReviewAction, ReviewState};
use futures_util::StreamExt;
use std::{collections::HashMap, time::Duration};
use tokio::sync::mpsc;
use zbus::{Connection, zvariant::Value};

const NAME: &str = "dev.noel.tachikoma";
const PATH: &str = "/dev/noel/tachikoma";
pub const READY: &str = "Desktop notifications available";

#[derive(Debug)]
pub struct Open {
    pub request: String,
    pub token: String,
}
struct Launcher(mpsc::Sender<Open>);
#[zbus::interface(name = "dev.noel.tachikoma")]
impl Launcher {
    async fn activate(&self, request: &str) -> zbus::fdo::Result<()> {
        if request.len() > 128 || request.chars().any(char::is_control) {
            return Err(zbus::fdo::Error::InvalidArgs("Invalid request".into()));
        }
        self.0
            .try_send(Open {
                request: request.into(),
                token: String::new(),
            })
            .map_err(|_| zbus::fdo::Error::Failed("Desktop busy".into()))
    }
}
/// Claim a session-local instance or forward a launcher invocation to its window.
pub async fn instance(
    sender: mpsc::Sender<Open>,
    background: bool,
    request: &str,
) -> Result<Option<Connection>> {
    let bus = Connection::session().await?;
    bus.object_server().at(PATH, Launcher(sender)).await?;
    match bus
        .request_name_with_flags(NAME, zbus::fdo::RequestNameFlags::DoNotQueue.into())
        .await
    {
        Ok(_) => Ok(Some(bus)),
        Err(zbus::Error::NameTaken) => {
            if !background {
                let proxy = zbus::Proxy::new(&bus, NAME, PATH, NAME).await?;
                proxy.call::<_, _, ()>("Activate", &(request,)).await?;
            }
            Ok(None)
        }
        Err(error) => Err(error.into()),
    }
}

#[zbus::proxy(
    interface = "org.freedesktop.Notifications",
    default_service = "org.freedesktop.Notifications",
    default_path = "/org/freedesktop/Notifications"
)]
trait Notifications {
    fn get_capabilities(&self) -> zbus::Result<Vec<String>>;
    #[allow(clippy::too_many_arguments)]
    fn notify(
        &self,
        app_name: &str,
        replaces_id: u32,
        app_icon: &str,
        summary: &str,
        body: &str,
        actions: &[String],
        hints: HashMap<&str, Value<'_>>,
        expire_timeout: i32,
    ) -> zbus::Result<u32>;
    fn close_notification(&self, id: u32) -> zbus::Result<()>;
    #[zbus(signal)]
    fn action_invoked(&self, id: u32, action_key: String) -> zbus::Result<()>;
    #[zbus(signal)]
    fn notification_closed(&self, id: u32, reason: u32) -> zbus::Result<()>;
    #[zbus(signal)]
    fn activation_token(&self, id: u32, activation_token: String) -> zbus::Result<()>;
}

struct Prompt {
    id: u32,
    review: Review,
    content: String,
    visible: bool,
    token: String,
}
fn pending(review: &Review, now: i64) -> bool {
    review.state == ReviewState::Pending
        && review.mode != Mode::Shadow
        && review.expires_at_ms > now
}
/// Only this schema's exact, one-connection actions can appear in a notification.
fn quick_action<'a>(review: &'a Review, key: &str) -> Option<&'a ReviewAction> {
    let effect = match key {
        "allow-once" => Effect::Permit,
        "deny-once" => Effect::Block,
        _ => return None,
    };
    (review.request.schema_id == "network.connect.v1" && review.service_kind == "snitchgate")
        .then_some(())?;
    review.actions.iter().find(|action| {
        action.id == key
            && action.effect == effect
            && action.scope == serde_json::json!({"duration":"once"})
            && review.request.action_ids.contains(&action.id)
    })
}
fn clean(value: &str, limit: usize) -> String {
    value
        .chars()
        .filter(|c| !c.is_control())
        .take(limit)
        .collect()
}
fn content(review: &Review) -> (String, String, Vec<String>) {
    let title = if review.request.schema_id == "network.connect.v1" {
        let subject = &review.request.subject;
        let process = subject["process"]
            .as_str()
            .and_then(|path| std::path::Path::new(path).file_name())
            .and_then(|name| name.to_str())
            .filter(|name| !name.is_empty())
            .unwrap_or("Unknown process");
        let host = subject["host"]
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .or_else(|| {
                subject["ip"]
                    .as_str()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
            })
            .unwrap_or("Unknown destination");
        let host = clean(host, 110);
        let destination = if host.contains(':') && !host.starts_with('[') {
            format!("[{host}]")
        } else {
            host
        };
        let port = subject["port"]
            .as_u64()
            .map(|p| p.to_string())
            .unwrap_or_else(|| "?".into());
        format!("{} → {destination}:{port}", clean(process, 40))
    } else {
        clean(&review.request.summary, 160)
    };
    let mut actions = vec!["default".into(), "Review".into()];
    for (key, label) in [("allow-once", "Allow once"), ("deny-once", "Deny once")] {
        if quick_action(review, key).is_some() {
            actions.extend([key.into(), label.into()]);
        }
    }
    (title, String::new(), actions)
}
async fn show(proxy: &NotificationsProxy<'_>, review: &Review, replaces: u32) -> Result<Prompt> {
    let (title, body, actions) = content(review);
    let mut hints = HashMap::from([
        ("desktop-entry", Value::from(NAME)),
        ("category", Value::from("network")),
        ("urgency", Value::from(2u8)),
        ("transient", Value::from(true)),
        ("suppress-sound", Value::from(replaces != 0)),
    ]);
    // A shell may ignore expiry. We also explicitly close at the original deadline.
    let remaining = review
        .expires_at_ms
        .saturating_sub(now_ms())
        .clamp(1, i32::MAX as i64) as i32;
    if review.request.schema_id != "network.connect.v1" {
        hints.remove("category");
    }
    let id = proxy
        .notify(
            "tachikoma",
            replaces,
            "security-high",
            &title,
            &body,
            &actions,
            hints,
            remaining,
        )
        .await?;
    ensure!(id != 0, "notification server returned no identifier");
    Ok(Prompt {
        id,
        review: review.clone(),
        content: serde_json::to_string(&(title, body, actions))?,
        visible: true,
        token: String::new(),
    })
}
fn signature(review: &Review) -> String {
    serde_json::to_string(&content(review)).unwrap_or_default()
}
fn same_binding(before: &Review, current: &Review) -> bool {
    before.id == current.id
        && before.revision == current.revision
        && before.manifest_revision == current.manifest_revision
        && before.subject_digest == current.subject_digest
        && before.evidence_digest == current.evidence_digest
        && before.request.nonce == current.request.nonce
        && before.expires_at_ms == current.expires_at_ms
}
async fn act(backend: &Backend, prompt: &Prompt, key: &str) -> Result<()> {
    ensure!(
        pending(&prompt.review, now_ms()),
        "request no longer pending"
    );
    let action = quick_action(&prompt.review, key)
        .ok_or_else(|| anyhow::anyhow!("review in the application"))?;
    let client = backend.client()?;
    let current = client.get(&prompt.review.id).await?;
    ensure!(
        pending(&current, now_ms()) && same_binding(&prompt.review, &current),
        "request changed"
    );
    ensure!(
        quick_action(&current, key)
            .is_some_and(|a| a.effect == action.effect && a.scope == action.scope),
        "action changed"
    );
    client
        .decide(
            &current.id,
            &DecideRequest {
                revision: current.revision,
                action_id: action.id.clone(),
                scope: None,
            },
        )
        .await?;
    Ok(())
}

/// Reconnect when the shell, API or session bus is unavailable. Never change policy.
pub async fn run(backend: Backend, sender: mpsc::Sender<Open>) {
    loop {
        if serve(&backend, &sender).await.is_err() {
            // No request facts, tokens or raw server response bodies in logs.
            eprintln!("Desktop notifications unavailable; retrying");
            if let Ok(client) = backend.client() {
                let _ = client.heartbeat("Desktop notifications unavailable").await;
            }
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}
async fn serve(backend: &Backend, sender: &mpsc::Sender<Open>) -> Result<()> {
    let bus = Connection::session().await?;
    let proxy = NotificationsProxy::new(&bus).await?;
    ensure!(
        proxy
            .get_capabilities()
            .await?
            .iter()
            .any(|c| c == "actions"),
        "notification actions unavailable"
    );
    // Pin to the current daemon's unique owner. Ignore signals from other clients.
    let owner = zbus::fdo::DBusProxy::new(&bus)
        .await?
        .get_name_owner("org.freedesktop.Notifications".try_into()?)
        .await?;
    let proxy = NotificationsProxy::builder(&bus)
        .destination(owner.clone())?
        .build()
        .await?;
    let mut actions = proxy.receive_action_invoked().await?;
    let mut closed = proxy.receive_notification_closed().await?;
    let mut tokens = proxy.receive_activation_token().await?;
    let client = backend.client()?;
    let identity = client.identity().await?;
    ensure!(
        identity.id == "local-desktop" && identity.role == approval_protocol::Role::Reviewer,
        "desktop reviewer required"
    );
    let mut prompts: HashMap<String, Prompt> = HashMap::new();
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let outcome: Result<()> = async {
        loop {
            tokio::select! {
                _ = tick.tick() => {
                    tokio::time::timeout(Duration::from_secs(4), async {
                        // A unique-owner probe also detects shell restarts with no pending requests.
                        ensure!(zbus::fdo::DBusProxy::new(&bus).await?.get_name_owner("org.freedesktop.Notifications".try_into()?).await? == owner, "notification server changed");
                        proxy.get_capabilities().await?;
                        let reviews = client.list().await?;
                        let live: HashMap<_, _> = reviews.into_iter().filter(|r| pending(r, now_ms())).map(|r| (r.id.clone(), r)).collect();
                        let stale: Vec<_> = prompts.iter().filter(|(id, p)| !live.get(*id).is_some_and(|r| same_binding(&p.review, r))).map(|(id, _)| id.clone()).collect();
                        for id in stale { if let Some(p) = prompts.remove(&id) && p.id != 0 { let _ = proxy.close_notification(p.id).await; } }
                        for (id, review) in live {
                            match prompts.get(&id) {
                                Some(p) if !p.visible || p.content == signature(&review) => (),
                                Some(p) => { let next = show(&proxy, &review, p.id).await?; prompts.insert(id, next); },
                                None => { let next = show(&proxy, &review, 0).await?;
                                    for old in prompts.values_mut().filter(|p| p.id == next.id) { old.id = 0; }
                                    prompts.insert(id, next); }
                            }
                        }
                        client.heartbeat(READY).await?;
                        Ok::<_, anyhow::Error>(())
                    }).await??;
                }
                event = tokens.next() => {
                    let event = event.ok_or_else(|| anyhow::anyhow!("notification stream closed"))?;
                    let args = event.args()?;
                    if let Some(prompt) = prompts.values_mut().find(|p| p.id == args.id) { prompt.token = args.activation_token.clone(); }
                }
                event = closed.next() => {
                    let event = event.ok_or_else(|| anyhow::anyhow!("notification stream closed"))?;
                    let args = event.args()?;
                    if let Some(prompt) = prompts.values_mut().find(|p| p.id == args.id) { prompt.visible = false; }
                }
                event = actions.next() => {
                    let event = event.ok_or_else(|| anyhow::anyhow!("notification stream closed"))?;
                    let args = event.args()?;
                    if let Some(prompt) = prompts.values_mut().find(|p| p.id == args.id) {
                        if args.action_key == "default" {
                            let _ = sender.try_send(Open { request: prompt.review.id.clone(), token: std::mem::take(&mut prompt.token) });
                        } else if quick_action(&prompt.review, &args.action_key).is_some() {
                            if tokio::time::timeout(Duration::from_secs(4), act(backend, prompt, &args.action_key)).await.is_ok_and(|r| r.is_ok()) {
                                prompt.review.state = ReviewState::Observed; // local guard against duplicate signals; server owns actual result
                            } else {
                                let _ = sender.try_send(Open { request: prompt.review.id.clone(), token: std::mem::take(&mut prompt.token) });
                            }
                        } else { continue; }
                        let _ = proxy.close_notification(prompt.id).await;
                        prompt.visible = false;
                    }
                }
            }
        }
    }.await;
    for prompt in prompts.values() {
        if prompt.id != 0 {
            let _ = proxy.close_notification(prompt.id).await;
        }
    }
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn quick_actions_are_exact_and_never_expand_permission() {
        let mut review = crate::backend::demo_review();
        assert!(quick_action(&review, "allow-once").is_some());
        assert!(quick_action(&review, "allow-forever").is_none());
        review
            .actions
            .iter_mut()
            .find(|a| a.id == "allow-once")
            .unwrap()
            .scope = serde_json::json!({"duration":"once","match":"executable"});
        assert!(quick_action(&review, "allow-once").is_none());
        review.request.schema_id = "other.v1".into();
        assert!(quick_action(&review, "deny-once").is_none());
    }
    #[test]
    fn presentation_is_a_short_subject_with_details_left_in_the_app() {
        let mut review = crate::backend::demo_review();
        review.request.subject["process"] = "/usr/bin/zsh\u{001b}".into();
        review.request.subject["host"] = "api.z.ai".into();
        let (title, body, actions) = content(&review);
        assert_eq!(title, "zsh → api.z.ai:443");
        assert!(body.is_empty());
        assert_eq!(actions.len(), 6);
        let original = signature(&review);
        review.assessment.as_mut().unwrap().rationale = "New classification details".into();
        assert_eq!(signature(&review), original);
        review.request.subject["host"] = "".into();
        review.request.subject["ip"] = "::1".into();
        assert_eq!(content(&review).0, "zsh → [::1]:443");
        let mut changed = review.clone();
        changed.evidence_digest = "new".into();
        assert!(!same_binding(&review, &changed));
        changed = review.clone();
        changed.expires_at_ms = now_ms() - 1;
        assert!(!pending(&changed, now_ms()));
        changed.state = ReviewState::Allowed;
        assert!(!pending(&changed, now_ms()));
    }
}

#[cfg(test)]
mod bus_tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use zbus::{object_server::SignalEmitter, zvariant::OwnedValue};
    #[derive(Default)]
    struct Captured {
        replaces: Vec<u32>,
        timeouts: Vec<i32>,
        actions: Vec<String>,
    }
    struct Shell(Arc<Mutex<Captured>>);
    #[zbus::interface(name = "org.freedesktop.Notifications")]
    impl Shell {
        fn get_capabilities(&self) -> Vec<String> {
            vec!["actions".into(), "body".into()]
        }
        #[allow(clippy::too_many_arguments)]
        fn notify(
            &self,
            _app_name: &str,
            replaces_id: u32,
            _app_icon: &str,
            _summary: &str,
            _body: &str,
            actions: Vec<String>,
            _hints: HashMap<String, OwnedValue>,
            expire_timeout: i32,
        ) -> u32 {
            let mut captured = self.0.lock().unwrap();
            captured.replaces.push(replaces_id);
            captured.timeouts.push(expire_timeout);
            captured.actions = actions;
            42
        }
        fn close_notification(&self, _id: u32) {}
        #[zbus(signal)]
        async fn action_invoked(
            emitter: &SignalEmitter<'_>,
            id: u32,
            action_key: &str,
        ) -> zbus::Result<()>;
        #[zbus(signal)]
        async fn notification_closed(
            emitter: &SignalEmitter<'_>,
            id: u32,
            reason: u32,
        ) -> zbus::Result<()>;
    }
    #[tokio::test]
    async fn private_bus_replaces_prompts_preserves_deadlines_and_delivers_actions() {
        let captured = Arc::new(Mutex::new(Captured::default()));
        // Tests run inside Docker under dbus-run-session, never on the workstation bus.
        let shell = zbus::connection::Builder::session()
            .unwrap()
            .name("org.freedesktop.Notifications")
            .unwrap()
            .serve_at("/org/freedesktop/Notifications", Shell(captured.clone()))
            .unwrap()
            .build()
            .await
            .unwrap();
        let bus = Connection::session().await.unwrap();
        let proxy = NotificationsProxy::new(&bus).await.unwrap();
        assert!(
            proxy
                .get_capabilities()
                .await
                .unwrap()
                .contains(&"actions".into())
        );
        let mut events = proxy.receive_action_invoked().await.unwrap();
        let mut review = crate::backend::demo_review();
        review.expires_at_ms = now_ms() + 30_000;
        let first = show(&proxy, &review, 0).await.unwrap();
        review.assessment.as_mut().unwrap().rationale = "Updated classification".into();
        let second = show(&proxy, &review, first.id).await.unwrap();
        assert_eq!(first.content, second.content);
        {
            let data = captured.lock().unwrap();
            assert_eq!(data.replaces, [0, 42]);
            assert!(data.timeouts[1] <= data.timeouts[0]);
            assert!(data.actions.contains(&"allow-once".into()));
            assert!(!data.actions.contains(&"allow-forever".into()));
        }
        let interface = shell
            .object_server()
            .interface::<_, Shell>("/org/freedesktop/Notifications")
            .await
            .unwrap();
        Shell::action_invoked(interface.signal_emitter(), 42, "deny-once")
            .await
            .unwrap();
        let event = tokio::time::timeout(Duration::from_secs(2), events.next())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(event.args().unwrap().action_key, "deny-once");
        proxy.close_notification(second.id).await.unwrap();
        let (sender, mut receiver) = mpsc::channel(2);
        let instance = instance(sender.clone(), true, "").await.unwrap().unwrap();
        assert!(
            super::instance(sender, false, &review.id)
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(receiver.recv().await.unwrap().request, review.id);
        drop(instance);
    }
}

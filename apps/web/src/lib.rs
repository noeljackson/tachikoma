//! Local authenticated review frontend. All data and decisions use the public API.
use approval_client::Client;
use approval_protocol::*;
use axum::{
    Form, Router,
    body::Body,
    extract::{DefaultBodyLimit, Path, Query, State},
    http::{HeaderMap, HeaderValue, Request, StatusCode},
    middleware::{self, Next},
    response::{Html, IntoResponse, Redirect, Response},
    routing::{get, post},
};
use serde::Deserialize;
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
#[derive(Clone)]
struct Session {
    client: Client,
    csrf: String,
    actor: String,
    expires: Instant,
}
#[derive(Clone)]
struct Web {
    socket: PathBuf,
    sessions: Arc<Mutex<HashMap<String, Session>>>,
}
fn random() -> Option<String> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).ok()?;
    Some(bytes.iter().map(|b| format!("{b:02x}")).collect())
}
fn escape(value: &str) -> String {
    display_text(value)
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}
fn page(title: &str, content: &str, refresh: bool) -> Html<String> {
    Html(format!(
        "<!doctype html><html><head><meta charset=utf-8><meta name=viewport content='width=device-width,initial-scale=1'>{}<title>{}</title><style>body{{font:16px system-ui;background:#10171f;color:#dce5ed;margin:2rem auto;max-width:1100px;padding:0 1rem}}a{{color:#80d8cf}}article{{border:1px solid #435268;border-radius:8px;padding:1rem;margin:1rem 0}}pre{{white-space:pre-wrap;overflow-wrap:anywhere;background:#192430;padding:1rem}}button,input,select{{font:inherit;padding:.6rem;margin:.3rem}}small{{color:#a9bacb}}nav{{display:flex;gap:1rem;flex-wrap:wrap}}form{{display:inline}}label{{display:block}}code{{overflow-wrap:anywhere}}</style></head><body><h1>{}</h1>{}</body></html>",
        if refresh {
            "<meta http-equiv=refresh content=10>"
        } else {
            ""
        },
        escape(title),
        escape(title),
        content
    ))
}
fn allowed_host(headers: &HeaderMap) -> bool {
    headers
        .get("host")
        .and_then(|v| v.to_str().ok())
        .and_then(|host| host.parse::<axum::http::uri::Authority>().ok())
        .is_some_and(|host| {
            let name = host.host().trim_start_matches('[').trim_end_matches(']');
            name.eq_ignore_ascii_case("localhost")
                || name
                    .parse::<std::net::IpAddr>()
                    .is_ok_and(|ip| ip.is_loopback())
        })
}
async fn guard(request: Request<Body>, next: Next) -> Response {
    if !allowed_host(request.headers()) {
        return StatusCode::FORBIDDEN.into_response();
    }
    if request.method() != axum::http::Method::GET && request.method() != axum::http::Method::HEAD {
        let host = request
            .headers()
            .get("host")
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default();
        let origin = request
            .headers()
            .get("origin")
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default();
        if origin != format!("http://{host}") {
            return StatusCode::FORBIDDEN.into_response();
        }
    }
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    headers.insert("cache-control", HeaderValue::from_static("no-store"));
    headers.insert("x-frame-options", HeaderValue::from_static("DENY"));
    headers.insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    headers.insert("referrer-policy", HeaderValue::from_static("no-referrer"));
    headers.insert("content-security-policy",HeaderValue::from_static("default-src 'none'; style-src 'unsafe-inline'; form-action 'self'; frame-ancestors 'none'; base-uri 'none'"));
    response
}
fn session(state: &Web, headers: &HeaderMap) -> Option<Session> {
    let cookie = headers
        .get("cookie")?
        .to_str()
        .ok()?
        .split(';')
        .find_map(|v| v.trim().strip_prefix("tachikoma_session="))?;
    let mut sessions = state.sessions.lock().ok()?;
    sessions.retain(|_, s| s.expires > Instant::now());
    sessions.get(cookie).cloned()
}
pub fn router(socket: PathBuf) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/login", post(login))
        .route("/logout", post(logout))
        .route("/requests/{id}/decide", post(decide))
        .route("/requests/{id}/conversation", get(conversation).post(chat))
        .route("/requests/{id}/conversation/cancel", post(cancel_chat))
        .layer(DefaultBodyLimit::max(4096))
        .layer(middleware::from_fn(guard))
        .with_state(Web {
            socket,
            sessions: Arc::new(Mutex::new(HashMap::new())),
        })
}
#[derive(Deserialize)]
struct Filter {
    queue: Option<String>,
    view: Option<String>,
}
async fn index(
    State(state): State<Web>,
    headers: HeaderMap,
    Query(filter): Query<Filter>,
) -> Response {
    let Some(session) = session(&state, &headers) else {
        return page("Tachikoma review","<p>Sign in with an enrolled reviewer credential. Credentials are sent only to this local endpoint and are not stored in the browser.</p><form method=post action=login><label>Reviewer credential <input name=credential type=password autocomplete=off required maxlength=64></label><button>Sign in</button></form><p><small>The credential is provided by your Tachikoma administrator. The private local administrator credential is in the daemon’s approval state directory.</small></p>",false).into_response();
    };
    let history = filter.view.as_deref() == Some("history");
    let health_view = filter.view.as_deref() == Some("health");
    let result = async {
        Ok::<_, anyhow::Error>((
            session.client.health().await?,
            session
                .client
                .list_state(if history { "history" } else { "pending" })
                .await?,
        ))
    }
    .await;
    let(health,reviews)=match result{Ok(value)=>value,Err(_)=>return (StatusCode::SERVICE_UNAVAILABLE,page("Review unavailable","<p>The approval API is unavailable. No decision was made. Retry after reconnection.</p>",false)).into_response()};
    let mut html = format!(
        "<p>Signed in as <strong>{}</strong></p><nav><a href='./'>Inbox</a><a href='?view=history'>History</a><a href='?view=health'>Health</a><a href='./'>All queues</a></nav><form method=post action=logout><input type=hidden name=csrf value='{}'><button>Sign out</button></form>",
        escape(&session.actor),
        session.csrf
    );
    for queue in &health.queues {
        html.push_str(&format!(
            "<p><a href='?queue={}&amp;view={}'>{} / {}</a> — {} pending, {} settled</p>",
            escape(&queue.manifest.id),
            if history { "history" } else { "inbox" },
            escape(&queue.manifest.service_kind),
            escape(&queue.manifest.id),
            queue.pending,
            queue.settled
        ));
    }
    if health_view {
        html.push_str(&format!(
            "<h2>Workers</h2><p>{} ready jobs; {} leased jobs</p>",
            health.ready_jobs, health.leased_jobs
        ));
        for service in &health.services {
            html.push_str(&format!("<article><strong>{} / {}</strong><p>{}</p><small>Last heartbeat UTC epoch ms: {}</small></article>",escape(&service.service_kind),escape(&service.id),escape(&service.detail),service.last_seen_ms));
        }
    } else {
        let reviews = reviews
            .iter()
            .filter(|r| filter.queue.as_ref().is_none_or(|q| *q == r.request.queue))
            .collect::<Vec<_>>();
        if reviews.is_empty() {
            html.push_str("<p>No requests in this view. A registered producer must submit a request before review is available.</p>");
        }
        for review in reviews {
            html.push_str(&render_review(review, &session.csrf));
        }
    }
    page("Tachikoma", &html, true).into_response()
}
fn render_review(review: &Review, csrf: &str) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64;
    let mut out = format!(
        "<article><h2>{}</h2><p>{} / {} — {:?} — {:?}</p><code>{}</code><p>Deadline: {} (UTC epoch ms); {} seconds remaining</p>",
        escape(&review.request.summary),
        escape(&review.service_kind),
        escape(&review.request.queue),
        review.state,
        review.mode,
        escape(&review.id),
        review.expires_at_ms,
        (review.expires_at_ms - now).max(0) / 1000
    );
    if let Some(a) = &review.assessment {
        out.push_str(&format!(
            "<h3>Classification</h3><p>{} — {} — {}</p><p>Jev: {} / LLM: {}</p><pre>{}</pre>",
            escape(&a.category),
            a.confidence
                .map(|v| format!("{:.1}% confidence", 100.0 * v))
                .unwrap_or_else(|| "confidence unavailable".into()),
            escape(&a.state),
            escape(&a.jev_model),
            escape(&a.llm_model),
            escape(&a.rationale)
        ));
    } else {
        out.push_str(
            "<p>Classification pending or not requested. Human review remains required.</p>",
        );
    }
    out.push_str(&format!(
        "<details><summary>Subject and evidence</summary><pre>{}</pre><pre>{}</pre></details>",
        escape(&serde_json::to_string_pretty(&review.request.subject).unwrap_or_default()),
        escape(&serde_json::to_string_pretty(&review.request.evidence).unwrap_or_default())
    ));
    if review.state == ReviewState::Pending && review.expires_at_ms > now {
        for action in &review.actions {
            out.push_str(&format!("<p><strong>{}</strong> — {:?}</p><pre>{}</pre><form method=post action='requests/{}/decide'><input type=hidden name=csrf value='{}'><input type=hidden name=revision value='{}'><button name=action_id value='{}'>{}</button></form>",escape(&action.label),action.effect,escape(&action.scope.to_string()),escape(&review.id),csrf,review.revision,escape(&action.id),escape(&action.label)));
        }
    }
    if let Some(decision) = &review.decision {
        out.push_str(&format!(
            "<p>Decision: {} by {}</p>",
            escape(&decision.action.label),
            escape(&decision.actor)
        ));
    }
    if let Some(receipt) = &review.receipt {
        out.push_str(&format!(
            "<p>Producer-reported outcome: {:?} — {}</p>",
            receipt.state,
            escape(&receipt.reason)
        ));
    } else if review.decision.is_some() {
        out.push_str("<p>Producer outcome not reported.</p>");
    }
    out.push_str(&format!(
        "<p><a href='/approval/requests/{}/conversation'>Investigate this request</a></p>",
        escape(&review.id)
    ));
    out.push_str("</article>");
    out
}
async fn conversation(
    State(state): State<Web>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    let Some(session) = session(&state, &headers) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let result = async {
        Ok::<_, anyhow::Error>((
            session.client.conversation(&id).await?,
            session.client.health().await?,
        ))
    }
    .await;
    let Ok((chat, health)) = result else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let Some(key) = random() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let mut html = format!(
        "<nav><a href='/approval/'>Inbox</a><a href=''>Refresh conversation</a></nav><p>{} — {} — {}</p><p>This conversation cannot change decisions or execute actions.</p>",
        escape(&chat.queue),
        escape(&id),
        escape(&chat.status)
    );
    for message in &chat.messages {
        html.push_str(&format!(
            "<article><strong>{} / {}</strong><pre>{}</pre></article>",
            escape(&message.role),
            escape(&message.model),
            escape(&message.content)
        ));
    }
    if chat.status == "running" {
        html.push_str(&format!("<form method=post action='/approval/requests/{}/conversation/cancel'><input type=hidden name=csrf value='{}'><button>Cancel turn</button></form>",escape(&id),session.csrf));
    } else {
        let mut models = String::from("<option value=''>Worker default</option>");
        if let Some(profile) = health
            .queues
            .iter()
            .find(|q| q.manifest.id == chat.queue)
            .and_then(|q| q.manifest.assessment.as_ref())
        {
            for model in &profile.models {
                models.push_str(&format!(
                    "<option value='{}'>{}</option>",
                    escape(model),
                    escape(model)
                ));
            }
        }
        html.push_str(&format!("<form method=post action='/approval/requests/{}/conversation'><input type=hidden name=csrf value='{}'><input type=hidden name=idempotency_key value='{}'><label>Model <select name=model>{}</select></label><label>Message <input name=message required maxlength=2048 autocomplete=off></label><button>Send</button></form>",escape(&id),session.csrf,key,models));
    }
    page("Investigation", &html, false).into_response()
}
#[derive(Deserialize)]
struct ChatForm {
    csrf: String,
    message: String,
    model: String,
    idempotency_key: String,
}
async fn chat(
    State(state): State<Web>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Form(input): Form<ChatForm>,
) -> Response {
    let Some(session) = session(&state, &headers) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    if input.csrf != session.csrf {
        return StatusCode::FORBIDDEN.into_response();
    }
    let model = (!input.model.is_empty()).then_some(input.model);
    match session
        .client
        .send_message(
            &id,
            &SendMessage {
                message: input.message,
                model,
                idempotency_key: input.idempotency_key,
            },
        )
        .await
    {
        Ok(_) => Redirect::to(&format!("/approval/requests/{id}/conversation")).into_response(),
        Err(_) => (
            StatusCode::CONFLICT,
            "Message rejected or delivery uncertain; refresh the conversation.",
        )
            .into_response(),
    }
}
async fn cancel_chat(
    State(state): State<Web>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Form(input): Form<Logout>,
) -> Response {
    let Some(session) = session(&state, &headers) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    if input.csrf != session.csrf {
        return StatusCode::FORBIDDEN.into_response();
    }
    match session.client.cancel_conversation(&id).await {
        Ok(_) => Redirect::to(&format!("/approval/requests/{id}/conversation")).into_response(),
        Err(_) => StatusCode::CONFLICT.into_response(),
    }
}
#[derive(Deserialize)]
struct Login {
    credential: String,
}
async fn login(State(state): State<Web>, Form(input): Form<Login>) -> Response {
    let Ok(client) = Client::new(
        "http://tachikoma.local",
        Some(&state.socket),
        input.credential,
    ) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let Ok(identity) = client.identity().await else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    if !matches!(identity.role, Role::Reviewer | Role::Admin) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let (Some(token), Some(csrf)) = (random(), random()) else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    {
        let Ok(mut sessions) = state.sessions.lock() else {
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        };
        sessions.retain(|_, s| s.expires > Instant::now());
        if sessions.len() >= 128 {
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
        sessions.insert(
            token.clone(),
            Session {
                client,
                csrf,
                actor: identity.id,
                expires: Instant::now() + Duration::from_secs(3600),
            },
        );
    }
    let mut response = Redirect::to("./").into_response();
    let cookie = format!(
        "tachikoma_session={token}; Path=/approval/; HttpOnly; SameSite=Strict; Max-Age=3600"
    );
    if let Ok(value) = HeaderValue::from_str(&cookie) {
        response.headers_mut().insert("set-cookie", value);
    }
    response
}
#[derive(Deserialize)]
struct ReviewForm {
    csrf: String,
    revision: i64,
    action_id: String,
}
async fn decide(
    State(state): State<Web>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Form(input): Form<ReviewForm>,
) -> Response {
    let Some(session) = session(&state, &headers) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    if input.csrf != session.csrf {
        return StatusCode::FORBIDDEN.into_response();
    }
    match session.client.decide(&id,&DecideRequest{revision:input.revision,action_id:input.action_id}).await{Ok(_)=>Redirect::to("/approval/").into_response(),Err(_)=>(StatusCode::CONFLICT,page("Decision rejected","<p>The request may have expired, changed, or settled elsewhere. <a href='/approval/'>Return to the current inbox</a>.</p>",false)).into_response()}
}
#[derive(Deserialize)]
struct Logout {
    csrf: String,
}
async fn logout(
    State(state): State<Web>,
    headers: HeaderMap,
    Form(input): Form<Logout>,
) -> Response {
    let Some(session) = session(&state, &headers) else {
        return Redirect::to("./").into_response();
    };
    if input.csrf != session.csrf {
        return StatusCode::FORBIDDEN.into_response();
    }
    if let Some(cookie) = headers
        .get("cookie")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| {
            v.split(';')
                .find_map(|v| v.trim().strip_prefix("tachikoma_session="))
        })
        && let Ok(mut sessions) = state.sessions.lock()
    {
        sessions.remove(cookie);
    }
    let mut response = Redirect::to("./").into_response();
    response.headers_mut().insert(
        "set-cookie",
        HeaderValue::from_static(
            "tachikoma_session=; Path=/approval/; HttpOnly; SameSite=Strict; Max-Age=0",
        ),
    );
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn host_checks_reject_rebinding_and_rendering_escapes_markup() {
        let mut h = HeaderMap::new();
        h.insert("host", HeaderValue::from_static("127.0.0.1:7447"));
        assert!(allowed_host(&h));
        h.insert("host", HeaderValue::from_static("evil.example:7447"));
        assert!(!allowed_host(&h));
        assert_eq!(escape("<script>\"'&"), "&lt;script&gt;&quot;&#39;&amp;");
    }
}

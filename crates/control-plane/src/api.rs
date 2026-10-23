use crate::store::{Fault, Principal, Store};
use approval_protocol::*;
use axum::{
    Json, Router,
    body::Bytes,
    extract::{DefaultBodyLimit, Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::{
    Deserialize, Deserializer,
    de::{self, DeserializeOwned, MapAccess, SeqAccess, Visitor},
};
use serde_json::Value;
use std::{
    fmt,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::sync::Notify;

#[derive(Clone)]
pub struct Api {
    store: Arc<Mutex<Store>>,
    changed: Arc<Notify>,
}
impl Api {
    pub fn new(store: Store) -> Self {
        Self {
            store: Arc::new(Mutex::new(store)),
            changed: Arc::new(Notify::new()),
        }
    }
    pub fn router(self) -> Router {
        let routes = Router::new()
            .route("/identity", get(identity))
            .route("/queues", get(queues).post(put_queue))
            .route("/principals", post(enroll))
            .route("/principals/update", post(update_principal))
            .route("/principals/{id}/revoke", post(revoke))
            .route("/principals/{id}/rotate", post(rotate))
            .route("/requests", get(list).post(submit))
            .route("/requests/{id}", get(request))
            .route("/requests/{id}/decision", post(decide))
            .route("/requests/{id}/cancel", post(cancel))
            .route("/requests/{id}/receipt", post(receipt))
            .route(
                "/requests/{id}/conversation",
                get(conversation).post(send_message),
            )
            .route(
                "/requests/{id}/conversation/cancel",
                post(cancel_conversation),
            )
            .route("/events", get(events))
            .route("/jobs/claim", post(claim))
            .route("/jobs/{id}/complete", post(complete))
            .route("/jobs/{id}/fail", post(fail))
            .route("/heartbeat", post(heartbeat))
            .route("/health", get(health))
            .route("/transport/route", get(route).post(register_route))
            .route("/transport/deliveries", get(deliveries).post(reserve))
            .route("/transport/deliveries/{id}/delivered", post(delivered))
            .route("/transport/deliveries/{id}/reply", post(reply))
            .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
            .with_state(self);
        Router::new().nest(API_PREFIX, routes)
    }
    async fn call<T: Send + 'static>(
        &self,
        headers: &HeaderMap,
        changed: bool,
        operation: impl FnOnce(&mut Store, &Principal) -> crate::store::Result<T> + Send + 'static,
    ) -> crate::store::Result<T> {
        let token = headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .unwrap_or_default()
            .to_owned();
        let store = Arc::clone(&self.store);
        let result = tokio::task::spawn_blocking(move || {
            let mut store = store.lock().map_err(|_| Fault {
                status: 503,
                code: "store_unavailable",
            })?;
            let principal = store.authenticate(&token)?;
            operation(&mut store, &principal)
        })
        .await
        .map_err(|_| Fault {
            status: 503,
            code: "store_unavailable",
        })?;
        if changed && result.is_ok() {
            self.changed.notify_waiters();
        }
        result
    }
}
impl IntoResponse for Fault {
    fn into_response(self) -> Response {
        (
            StatusCode::from_u16(self.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
            Json(ApiError {
                code: self.code.into(),
                message: self.code.replace('_', " "),
            }),
        )
            .into_response()
    }
}
type Reply<T> = std::result::Result<Json<T>, Fault>;

// serde_json::Value normally accepts duplicate object keys. Reject them recursively
// before decoding the public envelope, so hashing and consent have one meaning.
struct Unique(Value);
impl<'de> Deserialize<'de> for Unique {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        struct UniqueVisitor;
        impl<'de> Visitor<'de> for UniqueVisitor {
            type Value = Unique;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("JSON without duplicate object keys")
            }
            fn visit_bool<E: de::Error>(self, v: bool) -> std::result::Result<Unique, E> {
                Ok(Unique(Value::Bool(v)))
            }
            fn visit_i64<E: de::Error>(self, v: i64) -> std::result::Result<Unique, E> {
                Ok(Unique(v.into()))
            }
            fn visit_u64<E: de::Error>(self, v: u64) -> std::result::Result<Unique, E> {
                Ok(Unique(v.into()))
            }
            fn visit_f64<E: de::Error>(self, v: f64) -> std::result::Result<Unique, E> {
                serde_json::Number::from_f64(v)
                    .map(|v| Unique(Value::Number(v)))
                    .ok_or_else(|| E::custom("non-finite number"))
            }
            fn visit_str<E: de::Error>(self, v: &str) -> std::result::Result<Unique, E> {
                Ok(Unique(Value::String(v.into())))
            }
            fn visit_string<E: de::Error>(self, v: String) -> std::result::Result<Unique, E> {
                Ok(Unique(Value::String(v)))
            }
            fn visit_unit<E: de::Error>(self) -> std::result::Result<Unique, E> {
                Ok(Unique(Value::Null))
            }
            fn visit_none<E: de::Error>(self) -> std::result::Result<Unique, E> {
                Ok(Unique(Value::Null))
            }
            fn visit_seq<A: SeqAccess<'de>>(
                self,
                mut seq: A,
            ) -> std::result::Result<Unique, A::Error> {
                let mut values = vec![];
                while let Some(Unique(value)) = seq.next_element()? {
                    values.push(value);
                }
                Ok(Unique(Value::Array(values)))
            }
            fn visit_map<A: MapAccess<'de>>(
                self,
                mut map: A,
            ) -> std::result::Result<Unique, A::Error> {
                let mut values = serde_json::Map::new();
                while let Some(key) = map.next_key::<String>()? {
                    if values.contains_key(&key) {
                        return Err(de::Error::custom("duplicate object key"));
                    }
                    let Unique(value) = map.next_value()?;
                    values.insert(key, value);
                }
                Ok(Unique(Value::Object(values)))
            }
        }
        deserializer.deserialize_any(UniqueVisitor)
    }
}
fn parse<T: DeserializeOwned>(bytes: &[u8]) -> std::result::Result<T, Fault> {
    if bytes.len() > MAX_BODY_BYTES {
        return Err(Fault::bad());
    }
    let mut deserializer = serde_json::Deserializer::from_slice(bytes);
    let Unique(value) = Unique::deserialize(&mut deserializer).map_err(|_| Fault::bad())?;
    deserializer.end().map_err(|_| Fault::bad())?;
    serde_json::from_value(value).map_err(|_| Fault::bad())
}
async fn queues(State(api): State<Api>, headers: HeaderMap) -> Reply<Vec<QueueInfo>> {
    api.call(&headers, false, |s, p| s.queues(p))
        .await
        .map(Json)
}
async fn put_queue(State(api): State<Api>, headers: HeaderMap, body: Bytes) -> Reply<QueueInfo> {
    let input = parse(&body)?;
    api.call(&headers, true, move |s, p| s.put_queue(p, input))
        .await
        .map(Json)
}
async fn enroll(
    State(api): State<Api>,
    headers: HeaderMap,
    body: Bytes,
) -> Reply<EnrolledPrincipal> {
    let input = parse(&body)?;
    api.call(&headers, true, move |s, p| s.enroll(p, input))
        .await
        .map(Json)
}
async fn submit(State(api): State<Api>, headers: HeaderMap, body: Bytes) -> Reply<Review> {
    let input = parse(&body)?;
    api.call(&headers, true, move |s, p| s.submit(p, input))
        .await
        .map(Json)
}
#[derive(Deserialize)]
struct ListQuery {
    before: Option<i64>,
    state: Option<String>,
}
async fn list(
    State(api): State<Api>,
    headers: HeaderMap,
    Query(query): Query<ListQuery>,
) -> Reply<Reviews> {
    api.call(&headers, false, move |s, p| {
        s.list(
            p,
            query.before.unwrap_or(i64::MAX),
            query.state.as_deref().unwrap_or("pending"),
        )
    })
    .await
    .map(Json)
}
async fn request(
    State(api): State<Api>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Reply<Review> {
    api.call(&headers, false, move |s, p| s.get(p, &id))
        .await
        .map(Json)
}
async fn decide(
    State(api): State<Api>,
    headers: HeaderMap,
    Path(id): Path<String>,
    body: Bytes,
) -> Reply<Review> {
    let input = parse(&body)?;
    api.call(&headers, true, move |s, p| s.decide(p, &id, input))
        .await
        .map(Json)
}
async fn cancel(
    State(api): State<Api>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Reply<Review> {
    api.call(&headers, true, move |s, p| s.cancel(p, &id))
        .await
        .map(Json)
}
async fn receipt(
    State(api): State<Api>,
    headers: HeaderMap,
    Path(id): Path<String>,
    body: Bytes,
) -> Reply<Review> {
    let input = parse(&body)?;
    api.call(&headers, true, move |s, p| s.receipt(p, &id, input))
        .await
        .map(Json)
}
#[derive(Deserialize)]
struct EventQuery {
    after: Option<i64>,
    wait_ms: Option<u32>,
}
async fn events(
    State(api): State<Api>,
    headers: HeaderMap,
    Query(query): Query<EventQuery>,
) -> Reply<Events> {
    let after = query.after.unwrap_or(0);
    if after < 0 {
        return Err(Fault::bad());
    }
    let wait = Duration::from_millis(u64::from(query.wait_ms.unwrap_or(0).min(30_000)));
    let start = Instant::now();
    loop {
        let result = api
            .call(&headers, false, move |s, p| s.events(p, after))
            .await?;
        if !result.events.is_empty() || start.elapsed() >= wait || result.cursor != after {
            return Ok(Json(result));
        }
        let pause = Duration::from_millis(200).min(wait.saturating_sub(start.elapsed()));
        tokio::select! {_=api.changed.notified()=>{},_=tokio::time::sleep(pause)=>{}}
    }
}
async fn claim(State(api): State<Api>, headers: HeaderMap, body: Bytes) -> Reply<Option<Job>> {
    let input = parse(&body)?;
    api.call(&headers, true, move |s, p| s.claim(p, input))
        .await
        .map(Json)
}
async fn complete(
    State(api): State<Api>,
    headers: HeaderMap,
    Path(id): Path<String>,
    body: Bytes,
) -> Reply<Review> {
    let input = parse(&body)?;
    api.call(&headers, true, move |s, p| s.complete(p, &id, input))
        .await
        .map(Json)
}
async fn fail(
    State(api): State<Api>,
    headers: HeaderMap,
    Path(id): Path<String>,
    body: Bytes,
) -> Reply<()> {
    let input = parse(&body)?;
    api.call(&headers, true, move |s, p| s.fail(p, &id, input))
        .await
        .map(Json)
}
async fn heartbeat(State(api): State<Api>, headers: HeaderMap, body: Bytes) -> Reply<()> {
    let input: Heartbeat = parse(&body)?;
    api.call(&headers, true, move |s, p| s.heartbeat(p, &input.detail))
        .await
        .map(Json)
}
async fn health(State(api): State<Api>, headers: HeaderMap) -> Reply<Health> {
    api.call(&headers, false, |s, p| s.health(p))
        .await
        .map(Json)
}
async fn route(State(api): State<Api>, headers: HeaderMap) -> Reply<Option<ReviewRoute>> {
    api.call(&headers, false, |s, p| s.route(p)).await.map(Json)
}
async fn register_route(
    State(api): State<Api>,
    headers: HeaderMap,
    body: Bytes,
) -> Reply<ReviewRoute> {
    let input = parse(&body)?;
    api.call(&headers, true, move |s, p| s.register_route(p, input))
        .await
        .map(Json)
}
async fn reserve(State(api): State<Api>, headers: HeaderMap, body: Bytes) -> Reply<Delivery> {
    let input = parse(&body)?;
    api.call(&headers, true, move |s, p| s.reserve_delivery(p, input))
        .await
        .map(Json)
}
async fn deliveries(State(api): State<Api>, headers: HeaderMap) -> Reply<Vec<Delivery>> {
    api.call(&headers, false, |s, p| s.deliveries(p))
        .await
        .map(Json)
}
async fn delivered(
    State(api): State<Api>,
    headers: HeaderMap,
    Path(id): Path<String>,
    body: Bytes,
) -> Reply<Delivery> {
    let input = parse(&body)?;
    api.call(&headers, true, move |s, p| s.delivered(p, &id, input))
        .await
        .map(Json)
}
async fn reply(
    State(api): State<Api>,
    headers: HeaderMap,
    Path(id): Path<String>,
    body: Bytes,
) -> Reply<Review> {
    let input = parse(&body)?;
    api.call(&headers, true, move |s, p| s.delivery_reply(p, &id, input))
        .await
        .map(Json)
}

async fn identity(State(api): State<Api>, headers: HeaderMap) -> Reply<PrincipalSpec> {
    api.call(&headers, false, |_, p| Ok(p.spec.clone()))
        .await
        .map(Json)
}

async fn revoke(State(api): State<Api>, headers: HeaderMap, Path(id): Path<String>) -> Reply<()> {
    api.call(&headers, true, move |s, p| s.revoke(p, &id))
        .await
        .map(Json)
}
async fn rotate(
    State(api): State<Api>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Reply<EnrolledPrincipal> {
    api.call(&headers, true, move |s, p| s.rotate(p, &id))
        .await
        .map(Json)
}

async fn conversation(
    State(api): State<Api>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Reply<Conversation> {
    api.call(&headers, false, move |s, p| s.conversation(p, &id))
        .await
        .map(Json)
}
async fn send_message(
    State(api): State<Api>,
    headers: HeaderMap,
    Path(id): Path<String>,
    body: Bytes,
) -> Reply<Conversation> {
    let input = parse(&body)?;
    api.call(&headers, true, move |s, p| s.send_message(p, &id, input))
        .await
        .map(Json)
}
async fn cancel_conversation(
    State(api): State<Api>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Reply<Conversation> {
    api.call(&headers, true, move |s, p| s.cancel_conversation(p, &id))
        .await
        .map(Json)
}

async fn update_principal(
    State(api): State<Api>,
    headers: HeaderMap,
    body: Bytes,
) -> Reply<EnrolledPrincipal> {
    let spec = parse(&body)?;
    api.call(&headers, true, move |s, p| s.update_principal(p, spec))
        .await
        .map(Json)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_duplicate_keys_and_trailing_documents() {
        assert!(parse::<Value>(br#"{"subject":{"name":"one","name":"two"}}"#).is_err());
        assert!(parse::<Value>(b"{} {}").is_err());
        assert!(parse::<Value>(br#"{"subject":{"name":"one"}}"#).is_ok());
    }
}

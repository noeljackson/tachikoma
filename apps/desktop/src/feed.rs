//! One authenticated change subscription shared by the window and notifications.
use crate::backend::Backend;
use anyhow::Result;
use approval_client::Client;
use approval_protocol::*;
use futures_util::{StreamExt, stream};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::sync::{mpsc, watch};

pub const HISTORY_LIMIT: usize = 5000;
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Query {
    pub search: String,
    pub queues: Option<Vec<String>>,
}
impl Query {
    pub fn matches(&self, review: &Review) -> bool {
        self.queues
            .as_ref()
            .is_none_or(|q| q.contains(&review.request.queue))
            && (self.search.is_empty()
                || format!(
                    "{} {} {} {}",
                    review.request.summary, review.request.queue, review.request.subject, review.id
                )
                .to_lowercase()
                .contains(&self.search.to_lowercase()))
    }
}
#[derive(Clone, Default)]
pub struct Data {
    pub connected: bool,
    pub queues: Vec<QueueInfo>,
    pub pending: Vec<Review>,
    pub history: Vec<Review>,
    pub next_before: Option<i64>,
    pub cursor: i64,
    pub epochs: BTreeMap<String, i64>,
    pub health: Option<Health>,
    pub sources: Vec<SourceInfo>,
    pub query: Query,
}
pub struct Shared {
    pub updates: watch::Sender<Arc<Data>>,
    pub query: watch::Sender<Query>,
    more: mpsc::Sender<()>,
    receiver: Mutex<Option<mpsc::Receiver<()>>>,
    pub started: AtomicBool,
    more_queued: AtomicBool,
    pub client: Mutex<Option<Client>>,
}
impl Default for Shared {
    fn default() -> Self {
        let (updates, _) = watch::channel(Arc::new(Data::default()));
        let (query, _) = watch::channel(Query::default());
        let (more, receiver) = mpsc::channel(1);
        Self {
            updates,
            query,
            more,
            receiver: Mutex::new(Some(receiver)),
            started: AtomicBool::new(false),
            more_queued: AtomicBool::new(false),
            client: Mutex::new(None),
        }
    }
}
impl Shared {
    pub fn more(&self) {
        if !self.more_queued.swap(true, Ordering::AcqRel) && self.more.try_send(()).is_err() {
            self.more_queued.store(false, Ordering::Release);
        }
    }
    fn publish(&self, data: &Data) {
        self.updates.send_replace(Arc::new(data.clone()));
    }
}
fn trim(data: &mut Data, capacity: usize) {
    data.history.sort_by(|a, b| {
        b.created_at_ms
            .cmp(&a.created_at_ms)
            .then_with(|| a.id.cmp(&b.id))
    });
    data.history.dedup_by(|a, b| a.id == b.id);
    data.history.truncate(capacity.min(HISTORY_LIMIT));
}
pub fn apply(data: &mut Data, review: Review, query: &Query, sequence: i64, capacity: usize) {
    data.pending.retain(|r| r.id != review.id);
    data.history.retain(|r| r.id != review.id);
    data.epochs.insert(review.id.clone(), sequence);
    while data.epochs.len() > 8192 {
        data.epochs.pop_first();
    }
    if review.state == ReviewState::Pending {
        data.pending.push(review);
    } else if query.matches(&review) {
        data.history.push(review);
        trim(data, capacity);
    }
}
pub async fn run(backend: Backend) {
    let Some(mut more) = backend
        .shared
        .receiver
        .lock()
        .expect("feed receiver")
        .take()
    else {
        return;
    };
    loop {
        if connected(&backend, &mut more).await.is_err() {
            backend.shared.client.lock().expect("client cache").take();
            let mut data = (**backend.shared.updates.borrow()).clone();
            data.connected = false;
            backend.shared.publish(&data);
            backend.shared.more_queued.store(false, Ordering::Release);
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    }
}
async fn connected(backend: &Backend, more: &mut mpsc::Receiver<()>) -> Result<()> {
    let client = backend.client()?;
    let inbox = client.inbox().await?;
    let mut query = backend.shared.query.subscribe();
    let mut selected = query.borrow_and_update().clone();
    let mut data = Data {
        connected: true,
        queues: inbox.queues,
        pending: inbox.pending,
        history: inbox.history.reviews,
        next_before: inbox.history.next_before,
        cursor: inbox.cursor,
        ..Default::default()
    };
    data.pending
        .retain(|r| r.expires_at_ms > crate::state::now_ms());
    if selected != Query::default() {
        let page = client
            .history_page(None, &selected.search, selected.queues.as_deref())
            .await?;
        data.history = page.reviews;
        data.next_before = page.next_before;
    }
    data.query = selected.clone();
    backend.shared.publish(&data);
    let mut capacity = 100;
    let mut health = tokio::time::interval(Duration::from_secs(5));
    health.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        let mut changes = Box::pin(client.events(data.cursor, 30_000));
        loop {
            tokio::select! {
                response=&mut changes => {
                    let events=response?;
                    let ids: BTreeSet<_>=events.events.iter().map(|event|event.request_id.clone()).collect();
                    let updates:Vec<_>=stream::iter(ids).map(|id| {let client=client.clone(); async move {client.get(&id).await}}).buffer_unordered(8).collect().await;
                    for review in updates { apply(&mut data,review?,&selected,events.cursor,capacity); }
                    data.cursor=events.cursor;
                    if !events.events.is_empty() { backend.shared.publish(&data); }
                    break;
                }
                _=query.changed() => {
                    // Coalesce typing before issuing a server-side search; pending approvals remain complete.
                    tokio::time::sleep(Duration::from_millis(150)).await;
                    selected=query.borrow_and_update().clone();
                    let page=client.history_page(None,&selected.search,selected.queues.as_deref()).await?;
                    data.history=page.reviews;data.next_before=page.next_before;data.query=selected.clone();capacity=100;
                    backend.shared.publish(&data);
                }
                Some(())=more.recv() => {
                    if data.history.len()<HISTORY_LIMIT && let Some(before)=data.next_before {
                        let page=client.history_page(Some(before),&selected.search,selected.queues.as_deref()).await?;
                        data.history.extend(page.reviews);data.next_before=page.next_before;
                        capacity=data.history.len().max(100);trim(&mut data,capacity);
                        backend.shared.publish(&data);
                    }
                    backend.shared.more_queued.store(false,Ordering::Release);
                }
                _=health.tick() => {
                    let current=client.health().await?; data.queues=current.queues.clone(); data.health=Some(current);
                    if let Ok(sources)=client.sources().await {data.sources=sources;}
                    backend.shared.publish(&data);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn deltas_replace_pending_and_refresh_assessment_even_without_authority_revision_change() {
        let mut review = crate::backend::demo_review();
        let mut data = Data::default();
        let query = Query::default();
        apply(&mut data, review.clone(), &query, 1, 100);
        review.assessment.as_mut().unwrap().rationale = "Changed assessment".into();
        apply(&mut data, review.clone(), &query, 2, 100);
        assert_eq!(data.epochs[&review.id], 2);
        assert_eq!(data.pending.len(), 1);
        review.state = ReviewState::Denied;
        apply(&mut data, review.clone(), &query, 3, 100);
        assert!(data.pending.is_empty());
        assert_eq!(data.history.len(), 1);
        apply(&mut data, review, &query, 4, 100);
        assert_eq!(data.history.len(), 1);
    }
}

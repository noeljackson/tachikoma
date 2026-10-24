use crate::{backend::Backend, state::*};
use approval_protocol::*;
use gpui::{
    App, Context, Div, ElementId, Entity, FocusHandle, Focusable, KeyBinding, SharedString,
    Stateful, Subscription, Window, actions, div, prelude::*, px, rgb,
};
use gpui_elements::editable_text::{
    EditableTextState, StringStorage, TextChanged, text_area, text_input,
};
use std::{collections::BTreeMap, sync::Arc, time::Duration};

const BACKGROUND: u32 = 0x0c1220;
const PANEL: u32 = 0x131d2e;
const BORDER: u32 = 0x263449;
const MUTED: u32 = 0x94a3b8;
const WHITE: u32 = 0xe5edf8;
const ACCENT: u32 = 0x7dd3fc;
const GREEN: u32 = 0x86efac;
const RED: u32 = 0xfda4af;
const AMBER: u32 = 0xfcd34d;

actions!(
    desktop,
    [
        Next, Previous, FocusLeft, FocusRight, Refresh, Inbox, History, Health, Search, Escape,
        Send
    ]
);
pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("up", Previous, Some("ReviewNavigation")),
        KeyBinding::new("down", Next, Some("ReviewNavigation")),
        KeyBinding::new("left", FocusLeft, Some("ReviewNavigation")),
        KeyBinding::new("right", FocusRight, Some("ReviewNavigation")),
        KeyBinding::new("ctrl-r", Refresh, Some("Tachikoma")),
        KeyBinding::new("ctrl-1", Inbox, Some("Tachikoma")),
        KeyBinding::new("ctrl-2", History, Some("Tachikoma")),
        KeyBinding::new("ctrl-3", Health, Some("Tachikoma")),
        KeyBinding::new("ctrl-f", Search, Some("Tachikoma")),
        KeyBinding::new("escape", Escape, Some("Tachikoma")),
        KeyBinding::new("ctrl-enter", Send, Some("Investigation")),
    ]);
}
pub struct Desktop {
    state: State,
    backend: Backend,
    runtime: Arc<tokio::runtime::Runtime>,
    navigation_focus: FocusHandle,
    detail_scroll: gpui::ScrollHandle,
    request_scroll: gpui::ScrollHandle,
    filter: Entity<EditableTextState>,
    draft: Entity<EditableTextState>,
    model: Entity<EditableTextState>,
    drafts: BTreeMap<String, String>,
    pending_message: Option<(String, SendMessage)>,
    _subscriptions: Vec<Subscription>,
}
impl Desktop {
    pub fn new(
        backend: Backend,
        runtime: Arc<tokio::runtime::Runtime>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let filter = cx.new(|cx| EditableTextState::new(StringStorage::default(), cx));
        let draft = cx.new(|cx| EditableTextState::new(StringStorage::default(), cx));
        let model = cx.new(|cx| EditableTextState::new(StringStorage::default(), cx));
        let subscriptions = vec![cx.subscribe(&filter, |_, _, _: &TextChanged, cx| cx.notify())];
        let navigation_focus = cx.focus_handle();
        window.focus(&navigation_focus, cx);
        cx.spawn(async move |this, cx| {
            loop {
                if this.update(cx, |this, cx| this.refresh(cx)).is_err() {
                    break;
                }
                cx.background_executor().timer(Duration::from_secs(2)).await;
            }
        })
        .detach();
        Self {
            state: State::default(),
            backend,
            runtime,
            navigation_focus,
            detail_scroll: gpui::ScrollHandle::new(),
            request_scroll: gpui::ScrollHandle::new(),
            filter,
            draft,
            model,
            drafts: BTreeMap::new(),
            pending_message: None,
            _subscriptions: subscriptions,
        }
    }
    fn refresh(&mut self, cx: &mut Context<Self>) {
        if self.state.refreshing {
            return;
        }
        self.state.refreshing = true;
        let selected = self.state.selected.clone();
        let target = selected.clone();
        let backend = self.backend.clone();
        let task = self
            .runtime
            .spawn(async move { backend.snapshot(selected.as_deref()).await });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            if this.update(cx, |this, cx| {
                this.state.refreshing = false;
                match result {
                    Ok(Ok(snapshot)) => { this.state.apply(snapshot, target.as_deref()); if this.backend.demo { this.state.notice = "Offline example · read only · no approval service connection".into(); } },
                    _ => { this.state.connected = false; this.state.notice = "Disconnected. Check tachikoma.service; approval controls are disabled.".into(); }
                }
                cx.notify();
            }).is_err() { eprintln!("Approval window closed during refresh"); }
        }).detach();
    }
    fn select(&mut self, id: String, cx: &mut Context<Self>) {
        if self.state.selected.as_ref() != Some(&id) {
            if let Some(previous) = &self.state.selected {
                self.drafts
                    .insert(previous.clone(), self.draft.read(cx).as_str().to_owned());
            }
            let saved = self.drafts.get(&id).cloned().unwrap_or_default();
            self.draft.update(cx, |draft, cx| draft.emplace(&saved, cx));
            self.state.select(id);
            self.detail_scroll.set_offset(gpui::point(px(0.), px(0.)));
        }
        self.refresh(cx);
        cx.notify();
    }
    fn page(&mut self, page: Page, window: &mut Window, cx: &mut Context<Self>) {
        self.state.page = page;
        self.state.confirmation = None;
        self.state.pane = Pane::Requests;
        let filter = self.filter.read(cx).as_str();
        if let Some(review) = self.state.rows(filter).first() {
            self.select(review.id.clone(), cx);
        }
        window.focus(&self.navigation_focus, cx);
        cx.notify();
    }
    fn navigate(&mut self, offset: isize, cx: &mut Context<Self>) {
        if self.state.pane == Pane::Queues {
            let queues = &self.state.snapshot.queues;
            let index = self
                .state
                .queue
                .as_ref()
                .and_then(|id| queues.iter().position(|queue| &queue.manifest.id == id))
                .map(|index| index + 1)
                .unwrap_or(0);
            let next = index.saturating_add_signed(offset).min(queues.len());
            self.state.queue = next
                .checked_sub(1)
                .and_then(|index| queues.get(index))
                .map(|queue| queue.manifest.id.clone());
            self.state.confirmation = None;
        } else if self.state.pane == Pane::Requests {
            let rows = self.state.rows(self.filter.read(cx).as_str());
            let index = rows
                .iter()
                .position(|review| Some(&review.id) == self.state.selected.as_ref())
                .unwrap_or(0);
            let next = index
                .saturating_add_signed(offset)
                .min(rows.len().saturating_sub(1));
            if let Some(review) = rows.get(next) {
                let id = review.id.clone();
                self.select(id, cx);
                self.state.pane = Pane::Requests;
                self.request_scroll.scroll_to_item(next);
            }
        }
        if self.state.pane == Pane::Details {
            let mut position = self.detail_scroll.offset();
            position.y -= px(offset as f32 * 56.);
            self.detail_scroll.set_offset(position);
        }
        cx.notify();
    }
    fn confirm(&mut self, cx: &mut Context<Self>) {
        let Some(confirmation) = self.state.confirmation.clone() else {
            return;
        };
        if self.backend.demo || !self.state.can_confirm(&confirmation, now_ms()) {
            self.state.confirmation = None;
            self.state.notice = "Request changed, expired or unavailable. No decision sent.".into();
            cx.notify();
            return;
        }
        self.state.busy = true;
        self.state.confirmation = None;
        let backend = self.backend.clone();
        let task = self.runtime.spawn(async move {
            backend
                .decide(&confirmation.review, &confirmation.action)
                .await
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            if this
                .update(cx, |this, cx| {
                    this.state.busy = false;
                    this.state.notice = if matches!(result, Ok(Ok(()))) {
                        "Decision recorded. The producer reports enforcement separately.".into()
                    } else {
                        "Decision not confirmed. Refresh before trying again.".into()
                    };
                    this.refresh(cx);
                    cx.notify();
                })
                .is_err()
            {
                eprintln!("Approval window closed while recording a decision");
            }
        })
        .detach();
        cx.notify();
    }
    fn send(&mut self, cx: &mut Context<Self>) {
        if self.state.busy || !self.state.connected || self.backend.demo {
            return;
        }
        let Some(id) = self.state.selected.clone() else {
            return;
        };
        let message = self.draft.read(cx).as_str().trim().to_owned();
        if message.is_empty() {
            return;
        }
        let model = self.model.read(cx).as_str().trim().to_owned();
        let model = (!model.is_empty()).then_some(model);
        // Preserve the exact key after an uncertain send so a retry cannot duplicate a turn.
        let input = self
            .pending_message
            .as_ref()
            .filter(|(request_id, input)| {
                request_id == &id && input.message == message && input.model == model
            })
            .map(|(_, input)| input.clone())
            .unwrap_or_else(|| SendMessage {
                message,
                model,
                idempotency_key: uuid::Uuid::new_v4().to_string(),
            });
        self.pending_message = Some((id.clone(), input.clone()));
        self.state.busy = true;
        let backend = self.backend.clone();
        let target = id.clone();
        let task = self
            .runtime
            .spawn(async move { backend.message(&id, &input).await });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            if this.update(cx, |this, cx| {
                this.state.busy = false;
                match result {
                    Ok(Ok(conversation)) => {
                        this.drafts.remove(&target);
                        this.pending_message = None;
                        if this.state.selected.as_ref() == Some(&target) {
                            this.draft.update(cx, |draft, cx| draft.emplace("", cx));
                            this.state.snapshot.conversation = Some(conversation);
                        }
                        this.state.notice = "Investigation queued. Approval remains a separate action.".into();
                    }
                    _ => this.state.notice = "Message delivery uncertain. Retry keeps the same message key; your draft is saved.".into(),
                }
                cx.notify();
            }).is_err() { eprintln!("Investigation window closed during send"); }
        }).detach();
        cx.notify();
    }
    fn cancel(&mut self, cx: &mut Context<Self>) {
        if self.state.busy || self.backend.demo {
            return;
        }
        let Some(id) = self.state.selected.clone() else {
            return;
        };
        self.state.busy = true;
        let target = id.clone();
        let backend = self.backend.clone();
        let task = self.runtime.spawn(async move { backend.cancel(&id).await });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            if this
                .update(cx, |this, cx| {
                    this.state.busy = false;
                    if let Ok(Ok(conversation)) = result {
                        if this.state.selected.as_ref() == Some(&target) {
                            this.state.snapshot.conversation = Some(conversation);
                        }
                        this.state.notice = "Turn cancelled; your draft is kept.".into();
                    } else {
                        this.state.notice =
                            "Cancellation not confirmed. Refresh to check the turn.".into();
                    }
                    cx.notify();
                })
                .is_err()
            {
                eprintln!("Investigation window closed during cancellation");
            }
        })
        .detach();
        cx.notify();
    }
    fn sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let mut queues = vec![
            button(
                "all-queues",
                format!("All queues    {}", self.state.snapshot.pending.len()),
                if self.state.queue.is_none() {
                    ACCENT
                } else {
                    MUTED
                },
            )
            .w_full()
            .on_click(cx.listener(|this, _, window, cx| {
                this.state.queue = None;
                this.state.pane = Pane::Queues;
                this.state.confirmation = None;
                window.focus(&this.navigation_focus, cx);
                cx.notify();
            }))
            .into_any_element(),
        ];
        for queue in &self.state.snapshot.queues {
            let id = queue.manifest.id.clone();
            let selected = self.state.queue.as_ref() == Some(&id);
            queues.push(
                button(
                    SharedString::from(format!("queue-{id}")),
                    format!("{}    {}", queue.manifest.title, queue.pending),
                    if selected { ACCENT } else { MUTED },
                )
                .w_full()
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.state.queue = Some(id.clone());
                    this.state.pane = Pane::Queues;
                    this.state.confirmation = None;
                    window.focus(&this.navigation_focus, cx);
                    cx.notify();
                }))
                .into_any_element(),
            );
        }
        div()
            .flex()
            .flex_col()
            .w(px(210.))
            .flex_shrink_0()
            .h_full()
            .p_4()
            .gap_2()
            .border_r_1()
            .border_color(rgb(if self.state.pane == Pane::Queues {
                ACCENT
            } else {
                BORDER
            }))
            .child(label("WORKSPACE"))
            .child(
                div()
                    .text_lg()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child("Local approvals"),
            )
            .child(div().mt_6().mb_2().child(label("SERVICE QUEUES")))
            .child(
                div()
                    .id("queue-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .children(queues),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(MUTED))
                    .child("← → Change pane\n↑ ↓ Navigate\nCtrl+F Search\nCtrl+R Refresh"),
            )
            .child(
                div()
                    .mt_3()
                    .pt_3()
                    .border_t_1()
                    .border_color(rgb(BORDER))
                    .text_xs()
                    .text_color(rgb(MUTED))
                    .child("Requests from every adapter.\nOne shared approval trail."),
            )
    }
    fn inbox(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let rows = self.state.rows(self.filter.read(cx).as_str());
        let count = rows.len();
        let mut elements = Vec::new();
        for review in rows {
            let id = review.id.clone();
            let selected = self.state.selected.as_ref() == Some(&id);
            let summary = text(&review.request.summary);
            let assessment = review
                .assessment
                .as_ref()
                .map(|assessment| {
                    format!(
                        "{} · {}",
                        text(&assessment.category).replace('_', " "),
                        percent(assessment.confidence)
                    )
                })
                .unwrap_or_else(|| "Awaiting classification".into());
            elements.push(
                div()
                    .id(SharedString::from(format!("request-{id}")))
                    .p_4()
                    .mb_2()
                    .rounded_lg()
                    .cursor_pointer()
                    .bg(rgb(if selected { 0x20334b } else { PANEL }))
                    .border_1()
                    .border_color(rgb(if selected { ACCENT } else { BORDER }))
                    .hover_style(|style| style.bg(rgb(0x20334b)))
                    .child(div().text_xs().text_color(rgb(MUTED)).child(format!(
                        "{}  ·  {}",
                        text(&review.request.queue),
                        status(review)
                    )))
                    .child(
                        div()
                            .mt_2()
                            .text_sm()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child(summary),
                    )
                    .child(
                        div()
                            .mt_2()
                            .text_xs()
                            .text_color(rgb(AMBER))
                            .child(assessment),
                    )
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.select(id.clone(), cx);
                        this.state.pane = Pane::Requests;
                        window.focus(&this.navigation_focus, cx);
                    }))
                    .into_any_element(),
            );
        }
        div()
            .w(px(350.))
            .flex_shrink_0()
            .h_full()
            .flex()
            .flex_col()
            .p_4()
            .gap_3()
            .border_r_1()
            .border_color(rgb(if self.state.pane == Pane::Requests {
                ACCENT
            } else {
                BORDER
            }))
            .child(
                div()
                    .flex()
                    .justify_between()
                    .child(label(if self.state.page == Page::History {
                        "HISTORY"
                    } else {
                        "INBOX"
                    }))
                    .child(label(&format!("{count} requests"))),
            )
            .child(
                text_input("request-filter")
                    .state(self.filter.downgrade())
                    .placeholder("Filter requests…")
                    .p_3()
                    .rounded_lg()
                    .bg(rgb(PANEL))
                    .border_1()
                    .border_color(rgb(BORDER))
                    .w_full()
                    .min_h(px(42.))
                    .text_sm(),
            )
            .child(
                div()
                    .id("request-scroll")
                    .track_scroll(&self.request_scroll)
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .children(elements)
                    .when(count == 0, |element| {
                        element.child(div().py_8().text_color(rgb(MUTED)).text_sm().child(
                            if self.state.connected {
                                "No matching requests. New requests appear here automatically."
                            } else {
                                "Waiting for the approval service…"
                            },
                        ))
                    }),
            )
    }
    fn details(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let review = self.state.review().cloned();
        let mut content = Vec::new();
        if let Some(review) = review {
            content.push(
                div()
                    .child(label(&format!(
                        "{} / {}",
                        text(&review.service_kind),
                        text(&review.request.queue)
                    )))
                    .child(
                        div()
                            .mt_2()
                            .text_xl()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child(text(&review.request.summary)),
                    )
                    .child(div().mt_2().text_sm().text_color(rgb(MUTED)).child(format!(
                        "{} · {} · {}",
                        status(&review),
                        text(&review.producer),
                        text(&review.id)
                    )))
                    .into_any_element(),
            );
            let mut tabs = vec![];
            for (detail, title) in [
                (Detail::Overview, "Overview"),
                (Detail::Evidence, "Evidence"),
                (Detail::Policy, "Rules"),
                (Detail::Investigation, "Investigate"),
            ] {
                tabs.push(
                    button(
                        title,
                        title,
                        if detail == self.state.detail {
                            ACCENT
                        } else {
                            MUTED
                        },
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.state.detail = detail;
                        this.state.pane = Pane::Details;
                        cx.notify();
                    }))
                    .into_any_element(),
                );
            }
            content.push(
                div()
                    .flex()
                    .flex_wrap()
                    .gap_2()
                    .py_3()
                    .children(tabs)
                    .into_any_element(),
            );
            match self.state.detail {
                Detail::Overview => {
                    if let Some(process) = review.request.subject["process"].as_str() {
                        let subject = &review.request.subject;
                        let host = subject["host"]
                            .as_str()
                            .filter(|host| !host.is_empty())
                            .unwrap_or_else(|| {
                                subject["ip"].as_str().unwrap_or("Unknown destination")
                            });
                        content.push(
                            div()
                                .p_4()
                                .rounded_lg()
                                .bg(rgb(PANEL))
                                .border_1()
                                .border_color(rgb(BORDER))
                                .flex()
                                .flex_col()
                                .gap_3()
                                .child(label("CONNECTION"))
                                .child(div().text_lg().child(format!("{host}:{}", subject["port"])))
                                .child(div().text_sm().text_color(rgb(MUTED)).child(format!(
                                    "{}  ·  {}  ·  PID {}  ·  UID {}",
                                    subject["protocol"].as_str().unwrap_or(""),
                                    subject["ip"].as_str().unwrap_or(""),
                                    subject["pid"],
                                    subject["uid"]
                                )))
                                .child(div().text_sm().child(text(if process.is_empty() {
                                    "Process attribution unavailable"
                                } else {
                                    process
                                })))
                                .into_any_element(),
                        );
                        let context = &review.request.evidence["process_context"];
                        if let Some(directory) = context["working_directory"].as_str() {
                            content.push(
                                section("Reported working directory", text(directory))
                                    .into_any_element(),
                            );
                        }
                        if let Some(hints) = review.request.evidence["url_hints"]
                            .as_array()
                            .filter(|hints| !hints.is_empty())
                        {
                            content.push(
                                section(
                                    "Resource hints · unverified",
                                    hints
                                        .iter()
                                        .filter_map(serde_json::Value::as_str)
                                        .map(text)
                                        .collect::<Vec<_>>()
                                        .join("\n"),
                                )
                                .into_any_element(),
                            );
                        }
                    } else {
                        content.push(
                            section("Subject", json_text(&review.request.subject))
                                .into_any_element(),
                        );
                    }
                    if let Some(assessment) = &review.assessment {
                        content.push(div().p_4().rounded_lg().bg(rgb(0x1d2940)).border_1().border_color(rgb(BORDER))
                            .child(label("CLASSIFICATION"))
                            .child(div().mt_2().text_lg().text_color(rgb(AMBER)).child(format!("{}  ·  {}", text(&assessment.category).replace('_', " "), percent(assessment.confidence))))
                            .child(div().mt_3().text_sm().child(text(&assessment.rationale)))
                            .children(assessment.judgments.iter().map(|(name, judgment)| div().mt_2().text_sm().text_color(rgb(MUTED)).child(format!("{}: {} · {}", text(name), text(&judgment.choice), percent(Some(judgment.confidence))))))
                            .child(div().mt_3().text_xs().text_color(rgb(MUTED)).child(format!("Jev: {}  ·  LLM: {}", text(&assessment.jev_model), text(&assessment.llm_model))))
                            .child(div().mt_2().text_xs().text_color(rgb(MUTED)).child("Classification is evidence for review; it does not grant access."))
                            .into_any_element());
                    } else {
                        content.push(section("Classification", "Assessment pending or not requested. You can inspect evidence or start an investigation.".into()).into_any_element());
                    }
                    if let Some(decision) = &review.decision {
                        content.push(
                            section(
                                "Recorded decision",
                                format!(
                                    "{} · by {}\nExact scope: {}",
                                    text(&decision.action.label),
                                    text(&decision.actor),
                                    json_text(&decision.action.scope)
                                ),
                            )
                            .into_any_element(),
                        );
                        content.push(section("Producer outcome", review.receipt.as_ref().map(|receipt| format!("{:?}: {}",receipt.state,text(&receipt.reason))).unwrap_or_else(|| "Not reported. A recorded decision does not confirm enforcement.".into())).into_any_element());
                    }
                }
                Detail::Evidence => {
                    content.push(
                        section(
                            "Subject · immutable request facts",
                            json_text(&review.request.subject),
                        )
                        .into_any_element(),
                    );
                    content.push(
                        section(
                            "Evidence · provided by the adapter",
                            json_text(&review.request.evidence),
                        )
                        .into_any_element(),
                    );
                    content.push(section("Evidence identity", format!("Subject: {}\nEvidence: {}\nRequest revision: {}\nManifest revision: {}",review.subject_digest,review.evidence_digest,review.revision,review.manifest_revision)).into_any_element());
                }
                Detail::Policy => {
                    content.push(section("Rule evaluation · shadow only", "These previews explain matches across configured evidence sources. They do not authorize an action. Missing sensor data does not establish safety.".into()).into_any_element());
                    if let Some(preview) = &self.state.snapshot.preview {
                        content.push(
                            section(
                                "Preview",
                                format!(
                                    "{:?}\n{}\nPolicy revision {}",
                                    preview.outcome,
                                    text(&preview.reason),
                                    preview.policy_revision
                                ),
                            )
                            .into_any_element(),
                        );
                        for trace in &preview.traces {
                            content.push(
                                section(
                                    &format!("{} · {:?}", text(&trace.rule), trace.state),
                                    format!(
                                        "{}\n{}",
                                        text(&trace.reason),
                                        json_text(&trace.observations)
                                    ),
                                )
                                .into_any_element(),
                            );
                        }
                    } else {
                        content.push(section("No preview available", "This queue may not have an evidence policy, or its preview is unavailable.".into()).into_any_element());
                    }
                }
                Detail::Investigation => {
                    content.push(label("INVESTIGATION · ADVISORY ONLY").into_any_element());
                    if let Some(conversation) = &self.state.snapshot.conversation {
                        content.push(
                            div()
                                .text_xs()
                                .text_color(rgb(MUTED))
                                .child(format!(
                                    "{} · {}",
                                    text(&conversation.status),
                                    text(&conversation.active_model)
                                ))
                                .into_any_element(),
                        );
                        for message in &conversation.messages {
                            content.push(
                                section(
                                    &format!("{} · {}", text(&message.role), text(&message.model)),
                                    text(&message.content),
                                )
                                .into_any_element(),
                            );
                        }
                    } else {
                        content.push(section("Ask about this request", "Ask why it was classified this way, inspect risks, or compare the available choices. A model response cannot approve a request.".into()).into_any_element());
                    }
                    content.push(
                        div()
                            .key_context("Investigation")
                            .flex()
                            .flex_col()
                            .gap_3()
                            .child(
                                text_input("investigation-model")
                                    .state(self.model.downgrade())
                                    .placeholder("Model override (optional)")
                                    .p_3()
                                    .min_h(px(40.))
                                    .rounded_lg()
                                    .bg(rgb(PANEL))
                                    .border_1()
                                    .border_color(rgb(BORDER)),
                            )
                            .child(
                                text_area("investigation-draft")
                                    .state(self.draft.downgrade())
                                    .placeholder("Ask a question…  Ctrl+Enter to send")
                                    .p_3()
                                    .w_full()
                                    .min_h(px(100.))
                                    .max_h(px(220.))
                                    .overflow_y_scroll()
                                    .rounded_lg()
                                    .bg(rgb(PANEL))
                                    .border_1()
                                    .border_color(rgb(BORDER)),
                            )
                            .child(
                                div()
                                    .flex()
                                    .gap_3()
                                    .child(
                                        button("send-message", "Send question", ACCENT)
                                            .when(
                                                self.state.busy
                                                    || !self.state.connected
                                                    || self.backend.demo,
                                                |element| element.opacity(0.4),
                                            )
                                            .on_click(cx.listener(|this, _, _, cx| this.send(cx))),
                                    )
                                    .child(
                                        button("cancel-turn", "Cancel turn", MUTED).on_click(
                                            cx.listener(|this, _, _, cx| this.cancel(cx)),
                                        ),
                                    ),
                            )
                            .into_any_element(),
                    );
                }
            }
            if !self.state.snapshot.detail_error.is_empty() {
                content.push(
                    section(
                        "Some details unavailable",
                        self.state.snapshot.detail_error.clone(),
                    )
                    .into_any_element(),
                );
            }
            if review.state == ReviewState::Pending {
                let active = self.state.connected
                    && !self.state.busy
                    && review.expires_at_ms > now_ms()
                    && !self.backend.demo;
                content.push(
                    div()
                        .mt_3()
                        .pt_4()
                        .border_t_1()
                        .border_color(rgb(BORDER))
                        .child(label(if active {
                            "REVIEW ACTIONS"
                        } else {
                            "ACTIONS UNAVAILABLE"
                        }))
                        .into_any_element(),
                );
                for action in &review.actions {
                    let confirmation = Confirmation {
                        review: review.clone(),
                        action: action.clone(),
                    };
                    let color = match action.effect {
                        Effect::Permit => GREEN,
                        Effect::Block => RED,
                        Effect::Observe => ACCENT,
                    };
                    content.push(
                        button(
                            SharedString::from(format!("action-{}", action.id)),
                            text(&action.label),
                            color,
                        )
                        .when(!active, |element| element.opacity(0.4))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if !this.backend.demo && this.state.can_confirm(&confirmation, now_ms())
                            {
                                this.state.confirmation = Some(confirmation.clone());
                            }
                            cx.notify();
                        }))
                        .into_any_element(),
                    );
                }
            }
            if let Some(confirmation) = &self.state.confirmation {
                content.push(
                    div()
                        .p_4()
                        .rounded_lg()
                        .bg(rgb(0x302a1c))
                        .border_1()
                        .border_color(rgb(AMBER))
                        .flex()
                        .flex_col()
                        .gap_3()
                        .child(label("CONFIRM EXACT ACTION"))
                        .child(
                            div()
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .child(text(&confirmation.action.label)),
                        )
                        .child(div().text_sm().child(format!(
                            "{}\nEffect: {:?}\n{}",
                            text(&confirmation.review.request.summary),
                            confirmation.action.effect,
                            json_text(&confirmation.action.scope)
                        )))
                        .child(
                            div()
                                .flex()
                                .gap_3()
                                .child(
                                    button("confirm-action", "Confirm decision", AMBER)
                                        .on_click(cx.listener(|this, _, _, cx| this.confirm(cx))),
                                )
                                .child(button("dismiss-action", "Cancel", MUTED).on_click(
                                    cx.listener(|this, _, _, cx| {
                                        this.state.confirmation = None;
                                        cx.notify();
                                    }),
                                )),
                        )
                        .into_any_element(),
                );
            }
        } else {
            content.push(div().p_8().text_color(rgb(MUTED)).child("Select a request to inspect classification, evidence, rule traces and available actions.").into_any_element());
        }
        div()
            .id("detail-scroll")
            .track_scroll(&self.detail_scroll)
            .flex_1()
            .min_w_0()
            .h_full()
            .overflow_y_scroll()
            .p_6()
            .flex()
            .flex_col()
            .gap_3()
            .border_l_1()
            .border_color(rgb(if self.state.pane == Pane::Details {
                ACCENT
            } else {
                BORDER
            }))
            .children(content)
    }
    fn health(&self) -> impl IntoElement {
        let mut content = vec![div().text_xl().font_weight(gpui::FontWeight::SEMIBOLD).child("Services & evidence sources").into_any_element(),
            section("Coverage", "Each service publishes its own status. A ready source means its reader is available; it does not prove every event was observed.".into()).into_any_element()];
        if let Some(health) = &self.state.snapshot.health {
            content.push(
                section(
                    "Assessment work",
                    format!(
                        "{} ready jobs · {} leased jobs",
                        health.ready_jobs, health.leased_jobs
                    ),
                )
                .into_any_element(),
            );
            for service in &health.services {
                content.push(
                    section(
                        &service.id,
                        format!(
                            "{}\n{}\nHeartbeat: {}",
                            text(&service.service_kind),
                            text(&service.detail),
                            age(service.last_seen_ms)
                        ),
                    )
                    .into_any_element(),
                );
            }
        }
        for source in &self.state.snapshot.sources {
            let fresh = source.last_seen_ms > 0
                && now_ms().saturating_sub(source.last_seen_ms) <= source.spec.max_age_ms;
            content.push(
                section(
                    &format!(
                        "{} · {}",
                        text(&source.spec.title),
                        if !source.spec.enabled {
                            "disabled"
                        } else if !source.ready {
                            "unavailable"
                        } else if !fresh {
                            "stale"
                        } else {
                            "reader ready"
                        }
                    ),
                    format!(
                        "{}\n{}\nCapabilities: {}\nHeartbeat: {}\n{}",
                        text(&source.spec.coverage),
                        text(&source.detail),
                        source
                            .spec
                            .capabilities
                            .keys()
                            .cloned()
                            .collect::<Vec<_>>()
                            .join(", "),
                        age(source.last_seen_ms),
                        source.spec.queues.join(", ")
                    ),
                )
                .into_any_element(),
            );
        }
        div()
            .id("health-scroll")
            .flex_1()
            .h_full()
            .overflow_y_scroll()
            .p_6()
            .flex()
            .flex_col()
            .gap_4()
            .children(content)
    }
}
impl Render for Desktop {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut tabs = vec![];
        for (page, title) in [
            (Page::Inbox, "Inbox"),
            (Page::History, "History"),
            (Page::Health, "Health"),
        ] {
            tabs.push(
                button(
                    title,
                    title,
                    if self.state.page == page {
                        ACCENT
                    } else {
                        MUTED
                    },
                )
                .on_click(cx.listener(move |this, _, window, cx| this.page(page, window, cx)))
                .into_any_element(),
            );
        }
        let connected = self.state.connected;
        let status = if self.backend.demo {
            "OFFLINE DEMO · READ ONLY"
        } else if connected {
            "LIVE · LOCAL SERVICE"
        } else {
            "DISCONNECTED"
        };
        div()
            .key_context("Tachikoma")
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(BACKGROUND))
            .text_color(rgb(WHITE))
            .font_family("Inter")
            .text_size(px(14.))
            .on_action(cx.listener(|this, _: &Refresh, _, cx| this.refresh(cx)))
            .on_action(
                cx.listener(|this, _: &Inbox, window, cx| this.page(Page::Inbox, window, cx)),
            )
            .on_action(
                cx.listener(|this, _: &History, window, cx| this.page(Page::History, window, cx)),
            )
            .on_action(
                cx.listener(|this, _: &Health, window, cx| this.page(Page::Health, window, cx)),
            )
            .on_action(cx.listener(|this, _: &Search, window, cx| {
                window.focus(&this.filter.read(cx).focus_handle(cx), cx)
            }))
            .on_action(cx.listener(|this, _: &Escape, window, cx| {
                this.state.confirmation = None;
                window.focus(&this.navigation_focus, cx);
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &Send, _, cx| this.send(cx)))
            .child(
                div()
                    .h(px(72.))
                    .flex_shrink_0()
                    .px_6()
                    .flex()
                    .items_center()
                    .justify_between()
                    .border_b_1()
                    .border_color(rgb(BORDER))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_5()
                            .child(
                                div()
                                    .text_lg()
                                    .font_weight(gpui::FontWeight::BOLD)
                                    .text_color(rgb(ACCENT))
                                    .child("TACHIKOMA"),
                            )
                            .children(tabs),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(if connected { GREEN } else { AMBER }))
                            .child(status),
                    ),
            )
            .child(
                div()
                    .key_context("ReviewNavigation")
                    .track_focus(&self.navigation_focus)
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .on_action(cx.listener(|this, _: &Next, _, cx| this.navigate(1, cx)))
                    .on_action(cx.listener(|this, _: &Previous, _, cx| this.navigate(-1, cx)))
                    .on_action(cx.listener(|this, _: &FocusLeft, _, cx| {
                        this.state.pane = match this.state.pane {
                            Pane::Details => Pane::Requests,
                            _ => Pane::Queues,
                        };
                        cx.notify();
                    }))
                    .on_action(cx.listener(|this, _: &FocusRight, _, cx| {
                        this.state.pane = match this.state.pane {
                            Pane::Queues => Pane::Requests,
                            _ => Pane::Details,
                        };
                        cx.notify();
                    }))
                    .child(self.sidebar(cx))
                    .when(self.state.page == Page::Health, |element| {
                        element.child(self.health())
                    })
                    .when(self.state.page != Page::Health, |element| {
                        element.child(self.inbox(cx)).child(self.details(cx))
                    }),
            )
            .child(
                div()
                    .min_h(px(38.))
                    .flex_shrink_0()
                    .px_5()
                    .py_2()
                    .border_t_1()
                    .border_color(rgb(BORDER))
                    .text_xs()
                    .text_color(rgb(MUTED))
                    .child(text(&self.state.notice)),
            )
    }
}
fn label(value: &str) -> Div {
    div()
        .text_xs()
        .text_color(rgb(MUTED))
        .child(value.to_owned())
}
fn button(id: impl Into<ElementId>, title: impl Into<SharedString>, color: u32) -> Stateful<Div> {
    div()
        .id(id)
        .px_3()
        .py_2()
        .rounded_md()
        .text_sm()
        .cursor_pointer()
        .text_color(rgb(color))
        .bg(rgb(PANEL))
        .border_1()
        .border_color(rgb(BORDER))
        .hover_style(|style| style.bg(rgb(0x263449)))
        .child(title.into())
}
fn section(title: &str, body: String) -> Div {
    div()
        .p_4()
        .rounded_lg()
        .bg(rgb(PANEL))
        .border_1()
        .border_color(rgb(BORDER))
        .child(label(title))
        .child(
            div()
                .mt_2()
                .text_sm()
                .whitespace_normal()
                .child(text(&body)),
        )
}
fn json_text(value: &impl serde::Serialize) -> String {
    serde_json::to_string_pretty(value)
        .unwrap_or_else(|_| "Unable to display structured data".into())
}
fn percent(value: Option<f64>) -> String {
    value
        .filter(|value| value.is_finite())
        .map(|value| format!("{:.0}% confidence", value * 100.))
        .unwrap_or_else(|| "confidence unavailable".into())
}
fn age(at: i64) -> String {
    if at <= 0 {
        "not reported".into()
    } else {
        format!("{}s ago", now_ms().saturating_sub(at) / 1000)
    }
}
fn status(review: &Review) -> String {
    if review.state == ReviewState::Pending && review.expires_at_ms <= now_ms() {
        return "Expired".into();
    }
    if review.state == ReviewState::Pending {
        return format!(
            "Needs review · {}s left",
            review.expires_at_ms.saturating_sub(now_ms()) / 1000
        );
    }
    match review.state {
        ReviewState::Pending => "Needs review",
        ReviewState::Allowed => "Allow recorded",
        ReviewState::Denied => "Deny recorded",
        ReviewState::Observed => "Observed",
        ReviewState::Expired => "Expired",
        ReviewState::Cancelled => "Cancelled",
    }
    .into()
}

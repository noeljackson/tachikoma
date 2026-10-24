use crate::{backend::Backend, state::*};
use crate::{components::Ui, design::Appearance, icons::Icon};
use approval_protocol::*;
use gpui::{
    App, Context, Div, Entity, FocusHandle, Focusable, KeyBinding, SharedString, Subscription,
    Window, actions, div, prelude::*, px, rgb,
};
use gpui_elements::editable_text::{
    EditableTextState, StringStorage, TextChanged, actions::DEFAULT_INPUT_CONTEXT, text_area,
    text_input,
};
use std::{collections::BTreeMap, path::PathBuf, sync::Arc, time::Duration};

actions!(
    desktop,
    [
        Next,
        Previous,
        FocusLeft,
        FocusRight,
        Refresh,
        Inbox,
        History,
        Health,
        Search,
        Escape,
        Send,
        CycleAppearance
    ]
);
pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("up", Previous, Some("ReviewNavigation")),
        KeyBinding::new("down", Next, Some("ReviewNavigation")),
        KeyBinding::new("left", FocusLeft, Some("ReviewNavigation")),
        KeyBinding::new("right", FocusRight, Some("ReviewNavigation")),
        KeyBinding::new("ctrl-r", Refresh, Some("DecisionUI")),
        KeyBinding::new("ctrl-shift-t", CycleAppearance, Some("DecisionUI")),
        KeyBinding::new("ctrl-1", Inbox, Some("DecisionUI")),
        KeyBinding::new("ctrl-2", History, Some("DecisionUI")),
        KeyBinding::new("ctrl-3", Health, Some("DecisionUI")),
        KeyBinding::new("ctrl-f", Search, Some("DecisionUI")),
        KeyBinding::new("escape", Escape, Some("DecisionUI")),
        // Escape leaves an editor through our navigation focus rather than clearing focus.
        KeyBinding::new("escape", Escape, Some(DEFAULT_INPUT_CONTEXT)),
        KeyBinding::new("ctrl-enter", Send, Some("Investigation")),
    ]);
}
pub struct Desktop {
    state: State,
    ui: Ui,
    appearance: Appearance,
    preferences_path: PathBuf,
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
        appearance_override: Option<Appearance>,
        preferences_path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let filter = cx.new(|cx| EditableTextState::new(StringStorage::default(), cx));
        let draft = cx.new(|cx| EditableTextState::new(StringStorage::default(), cx));
        let model = cx.new(|cx| EditableTextState::new(StringStorage::default(), cx));
        let mut subscriptions =
            vec![cx.subscribe(&filter, |_, _, _: &TextChanged, cx| cx.notify())];
        let appearance = appearance_override
            .unwrap_or_else(|| Appearance::load(&preferences_path).unwrap_or_default());
        let ui = Ui::new(appearance.resolve(window.appearance()));
        subscriptions.push(cx.observe_window_appearance(window, |this, window, cx| {
            if this.appearance == Appearance::System {
                this.ui = Ui::new(this.appearance.resolve(window.appearance()));
                cx.notify();
            }
        }));
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
        let mut state = State::default();
        if !backend.demo {
            state.page = Page::History;
        }
        Self {
            state,
            ui,
            appearance,
            preferences_path,
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
    fn set_appearance(
        &mut self,
        appearance: Appearance,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.appearance = appearance;
        self.ui = Ui::new(appearance.resolve(window.appearance()));
        if appearance.save(&self.preferences_path).is_err() {
            self.state.notice =
                "Appearance changed for this window, but the preference could not be saved.".into();
        }
        cx.notify();
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
                    _ => { this.state.connected = false; this.state.notice = "Disconnected. Start the local approval service to review requests.".into(); }
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
            let queue = next
                .checked_sub(1)
                .and_then(|index| queues.get(index))
                .map(|queue| queue.manifest.id.clone());
            self.select_queue(queue, cx);
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
    fn select_queue(&mut self, queue: Option<String>, cx: &mut Context<Self>) {
        if let Some(previous) = &self.state.selected {
            self.drafts
                .insert(previous.clone(), self.draft.read(cx).as_str().to_owned());
        }
        self.state.queue = queue;
        self.state.confirmation = None;
        let id = self
            .state
            .rows(self.filter.read(cx).as_str())
            .first()
            .map(|review| review.id.clone());
        if let Some(id) = id {
            self.select(id, cx);
        } else {
            self.state.selected = None;
            self.state.snapshot.selected = None;
            self.state.snapshot.preview = None;
            self.state.snapshot.conversation = None;
            self.draft.update(cx, |draft, cx| draft.emplace("", cx));
        }
        self.state.pane = Pane::Queues;
        cx.notify();
    }
    fn sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = self.ui.colors;
        let metrics = self.ui.metrics;
        let total = self
            .state
            .snapshot
            .queues
            .iter()
            .map(|queue| {
                if self.state.page == Page::History {
                    queue.settled
                } else {
                    queue.pending
                }
            })
            .sum::<u64>();
        let mut queues = vec![
            self.ui
                .icon_button(
                    "all-queues",
                    Icon::Layers,
                    "All queues",
                    self.state.queue.is_none(),
                )
                .w_full()
                .justify_between()
                .child(self.ui.caption(&total.to_string()))
                .on_click(cx.listener(|this, _, window, cx| {
                    this.select_queue(None, cx);
                    window.focus(&this.navigation_focus, cx);
                }))
                .into_any_element(),
        ];
        for queue in &self.state.snapshot.queues {
            let id = queue.manifest.id.clone();
            let count = if self.state.page == Page::History {
                queue.settled
            } else {
                queue.pending
            };
            let icon = match queue.manifest.service_kind.as_str() {
                "snitchgate" => Icon::Shield,
                "kubernetesgate" => Icon::Boxes,
                _ => Icon::Workflow,
            };
            queues.push(
                self.ui
                    .icon_button(
                        SharedString::from(format!("queue-{id}")),
                        icon,
                        &queue.manifest.title,
                        self.state.queue.as_ref() == Some(&id),
                    )
                    .w_full()
                    .justify_between()
                    .child(self.ui.caption(&count.to_string()))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.select_queue(Some(id.clone()), cx);
                        window.focus(&this.navigation_focus, cx);
                    }))
                    .into_any_element(),
            );
        }
        div()
            .w(px(metrics.sidebar_width))
            .flex_shrink_0()
            .h_full()
            .bg(rgb(colors.sidebar))
            .border_r_1()
            .border_color(rgb(colors.border))
            .p(px(metrics.space[2]))
            .flex()
            .flex_col()
            .gap(px(metrics.space[1]))
            .child(
                div()
                    .px(px(metrics.space[1]))
                    .py(px(metrics.space[2]))
                    .child(self.ui.caption("QUEUES")),
            )
            .child(
                div()
                    .id("queue-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .gap(px(metrics.space[1]))
                    .children(queues),
            )
            .child(
                self.ui
                    .icon_button(
                        "source-health",
                        Icon::Server,
                        "Sources & services",
                        self.state.page == Page::Health,
                    )
                    .w_full()
                    .on_click(
                        cx.listener(|this, _, window, cx| this.page(Page::Health, window, cx)),
                    ),
            )
            .child(
                div()
                    .pt(px(metrics.space[2]))
                    .border_t_1()
                    .border_color(rgb(colors.border))
                    .text_size(px(metrics.caption))
                    .text_color(rgb(colors.muted))
                    .child(
                        "← → Switch pane · ↑ ↓ Navigate\nCtrl+F Search\nCtrl+Shift+T Appearance",
                    ),
            )
    }
    fn inbox(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = self.ui.colors;
        let metrics = self.ui.metrics;
        let rows = self.state.rows(self.filter.read(cx).as_str());
        let count = rows.len();
        let mut elements = vec![];
        for review in &rows {
            let id = review.id.clone();
            let selected = self.state.selected.as_ref() == Some(&id);
            let name = if review.request.schema_id == "network.connect.v1" {
                format!(
                    "{} → {}",
                    application_name(State::application(review)),
                    destination(review)
                )
            } else {
                text(&review.request.summary)
            };
            let destination = self
                .state
                .snapshot
                .queues
                .iter()
                .find(|queue| queue.manifest.id == review.request.queue)
                .map(|queue| queue.manifest.title.clone())
                .unwrap_or_else(|| review.request.queue.clone());
            let color = match review.state {
                ReviewState::Allowed => colors.positive,
                ReviewState::Denied => colors.negative,
                ReviewState::Pending => colors.warning,
                _ => colors.muted,
            };
            let marker = match review.state {
                ReviewState::Allowed => Icon::Check,
                ReviewState::Denied => Icon::Close,
                ReviewState::Pending => Icon::Clock,
                _ => Icon::Help,
            };
            elements.push(
                div()
                    .id(SharedString::from(format!("request-{id}")))
                    .px(px(metrics.space[2]))
                    .py(px(metrics.space[2]))
                    .cursor_pointer()
                    .bg(rgb(if selected {
                        colors.selected
                    } else {
                        colors.panel
                    }))
                    .border_b_1()
                    .border_color(rgb(colors.separator))
                    .hover_style(move |style| style.bg(rgb(colors.hover)))
                    .flex()
                    .items_center()
                    .gap(px(metrics.space[2]))
                    .child(
                        div()
                            .w(px(18.))
                            .flex_shrink_0()
                            .text_size(px(metrics.title))
                            .text_color(rgb(color))
                            .child(marker.view(color, metrics.icon)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap(px(metrics.space[0]))
                            .child(
                                div()
                                    .flex()
                                    .justify_between()
                                    .gap(px(metrics.space[1]))
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .truncate()
                                            .text_size(px(metrics.body))
                                            .font_weight(gpui::FontWeight::SEMIBOLD)
                                            .child(name),
                                    )
                                    .child(
                                        self.ui
                                            .caption(&relative_time(review.created_at_ms))
                                            .flex_shrink_0(),
                                    ),
                            )
                            .child(
                                div()
                                    .text_size(px(metrics.body))
                                    .overflow_hidden()
                                    .whitespace_nowrap()
                                    .child(destination),
                            )
                            .child(
                                div()
                                    .text_size(px(metrics.caption))
                                    .text_color(rgb(color))
                                    .child(status(review)),
                            ),
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
            .w(px(metrics.list_width))
            .flex_shrink_0()
            .h_full()
            .flex()
            .flex_col()
            .bg(rgb(colors.panel))
            .border_r_1()
            .border_color(rgb(colors.border))
            .child(
                div()
                    .px(px(metrics.space[3]))
                    .pt(px(metrics.space[3]))
                    .pb(px(metrics.space[2]))
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .text_size(px(metrics.title))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child(if self.state.page == Page::Inbox {
                                "Pending Decisions"
                            } else {
                                "Decision History"
                            }),
                    )
                    .child(self.ui.caption(&format!("{count}"))),
            )
            .child(
                div()
                    .px(px(metrics.space[2]))
                    .pb(px(metrics.space[2]))
                    .child(
                        text_input("request-filter")
                            .placeholder_color(rgb(colors.muted))
                            .caret_color(gpui::rgb_to_hsla(rgb(colors.text)))
                            .state(self.filter.downgrade())
                            .placeholder("Search decisions, subjects or evidence")
                            .px(px(metrics.space[2]))
                            .py(px(metrics.space[1]))
                            .rounded(px(metrics.control_radius))
                            .bg(rgb(colors.control))
                            .border_1()
                            .border_color(rgb(colors.border))
                            .w_full()
                            .min_h(px(36.))
                            .text_size(px(metrics.body)),
                    ),
            )
            .child(activity_chart(&rows, self.ui))
            .child(
                div()
                    .px(px(metrics.space[3]))
                    .py(px(metrics.space[1]))
                    .border_b_1()
                    .border_color(rgb(colors.border))
                    .flex()
                    .justify_between()
                    .child(self.ui.caption("DECISION / SUBJECT"))
                    .child(self.ui.caption("LATEST")),
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
                        element.child(
                            div()
                                .p(px(metrics.space[3]))
                                .text_size(px(metrics.body))
                                .text_color(rgb(colors.muted))
                                .child(if self.state.connected {
                                    "No requests in this view. New requests appear automatically."
                                } else {
                                    "Connecting to the local approval service…"
                                }),
                        )
                    }),
            )
    }
    fn details(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = self.ui.colors;
        let metrics = self.ui.metrics;
        let review = self.state.review().cloned();
        let mut content = Vec::new();
        if let Some(review) = review {
            content.push(
                div()
                    .child(
                        self.ui.caption(
                            self.state
                                .snapshot
                                .queues
                                .iter()
                                .find(|queue| queue.manifest.id == review.request.queue)
                                .map(|queue| queue.manifest.title.as_str())
                                .unwrap_or(&review.request.queue),
                        ),
                    )
                    .child(
                        div()
                            .mt(px(metrics.space[1]))
                            .text_size(px(metrics.title))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child(review_title(&review)),
                    )
                    .child(
                        div()
                            .mt(px(metrics.space[1]))
                            .text_size(px(metrics.body))
                            .text_color(rgb(colors.muted))
                            .child(format!("{} · {}", status(&review), destination(&review))),
                    )
                    .into_any_element(),
            );
            let mut tabs = vec![];
            for (detail, title, icon) in [
                (Detail::Overview, "Overview", Icon::File),
                (Detail::Evidence, "Evidence", Icon::Evidence),
                (Detail::Policy, "Policy", Icon::Shield),
                (Detail::Investigation, "Investigate", Icon::Chat),
            ] {
                tabs.push(
                    self.ui
                        .icon_button(title, icon, title, self.state.detail == detail)
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
                    .gap(px(metrics.space[1]))
                    .py(px(metrics.space[2]))
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
                                .p(px(metrics.space[3]))
                                .rounded(px(metrics.panel_radius))
                                .bg(rgb(colors.panel))
                                .border_1()
                                .border_color(rgb(colors.border))
                                .flex()
                                .flex_col()
                                .gap(px(metrics.space[2]))
                                .child(self.ui.caption("CONNECTION"))
                                .child(div().text_size(px(metrics.title)).child(format!(
                                    "{}:{}",
                                    text(host),
                                    subject["port"]
                                )))
                                .child(
                                    div()
                                        .text_size(px(metrics.body))
                                        .text_color(rgb(colors.muted))
                                        .child(format!(
                                            "{}  ·  {}  ·  PID {}  ·  UID {}",
                                            subject["protocol"].as_str().unwrap_or(""),
                                            subject["ip"].as_str().unwrap_or(""),
                                            subject["pid"],
                                            subject["uid"]
                                        )),
                                )
                                .child(div().text_size(px(metrics.body)).child(text(
                                    if process.is_empty() {
                                        "Process attribution unavailable"
                                    } else {
                                        process
                                    },
                                )))
                                .into_any_element(),
                        );
                        let context = &review.request.evidence["process_context"];
                        if let Some(directory) = context["working_directory"].as_str() {
                            content.push(
                                self.ui
                                    .section("Reported working directory", text(directory))
                                    .into_any_element(),
                            );
                        }
                        if let Some(hints) = review.request.evidence["url_hints"]
                            .as_array()
                            .filter(|hints| !hints.is_empty())
                        {
                            content.push(
                                self.ui
                                    .section(
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
                            self.ui
                                .section("Subject", json_text(&review.request.subject))
                                .into_any_element(),
                        );
                    }
                    if let Some(assessment) = &review.assessment {
                        content.push(div().p(px(metrics.space[3])).rounded(px(metrics.panel_radius)).bg(rgb(colors.info_surface)).border_1().border_color(rgb(colors.border))
                            .child(self.ui.caption("ASSESSMENT"))
                            .child(div().mt(px(metrics.space[1])).text_size(px(metrics.title)).text_color(rgb(colors.warning)).child(format!("{}  ·  {}", text(&assessment.category).replace('_', " "), percent(assessment.confidence))))
                            .child(div().mt(px(metrics.space[2])).text_size(px(metrics.body)).child(text(&assessment.rationale)))
                            .children(assessment.judgments.iter().map(|(name, judgment)| div().mt(px(metrics.space[1])).text_size(px(metrics.body)).text_color(rgb(colors.muted)).child(format!("{}: {} · {}", text(name), text(&judgment.choice), percent(Some(judgment.confidence))))))
                            .child(div().mt(px(metrics.space[2])).text_size(px(metrics.caption)).text_color(rgb(colors.muted)).child(format!("Jev: {}  ·  LLM: {}", text(&assessment.jev_model), text(&assessment.llm_model))))
                            .child(div().mt(px(metrics.space[1])).text_size(px(metrics.caption)).text_color(rgb(colors.muted)).child("Classification is evidence for review; it does not grant access."))
                            .into_any_element());
                    } else {
                        content.push(self.ui.section("Classification", "Assessment pending or not requested. You can inspect evidence or start an investigation.".into()).into_any_element());
                    }
                    if let Some(decision) = &review.decision {
                        content.push(
                            self.ui
                                .section(
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
                        content.push(self.ui.section("Producer outcome", review.receipt.as_ref().map(|receipt| format!("{:?}: {}",receipt.state,text(&receipt.reason))).unwrap_or_else(|| "Not reported. A recorded decision does not confirm enforcement.".into())).into_any_element());
                    }
                }
                Detail::Evidence => {
                    content.push(
                        self.ui
                            .section(
                                "Subject · immutable request facts",
                                json_text(&review.request.subject),
                            )
                            .into_any_element(),
                    );
                    content.push(
                        self.ui
                            .section(
                                "Evidence · provided by the adapter",
                                json_text(&review.request.evidence),
                            )
                            .into_any_element(),
                    );
                    content.push(self.ui.section("Evidence identity", format!("Subject: {}\nEvidence: {}\nRequest revision: {}\nManifest revision: {}",review.subject_digest,review.evidence_digest,review.revision,review.manifest_revision)).into_any_element());
                }
                Detail::Policy => {
                    content.push(self.ui.section("Rule evaluation · shadow only", "These previews explain matches across configured evidence sources. They do not authorize an action. Missing sensor data does not establish safety.".into()).into_any_element());
                    if let Some(preview) = &self.state.snapshot.preview {
                        content.push(
                            self.ui
                                .section(
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
                                self.ui
                                    .section(
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
                        content.push(self.ui.section("No preview available", "This queue may not have an evidence policy, or its preview is unavailable.".into()).into_any_element());
                    }
                }
                Detail::Investigation => {
                    content.push(
                        self.ui
                            .caption("INVESTIGATION · ADVISORY ONLY")
                            .into_any_element(),
                    );
                    if let Some(conversation) = &self.state.snapshot.conversation {
                        content.push(
                            div()
                                .text_size(px(metrics.caption))
                                .text_color(rgb(colors.muted))
                                .child(format!(
                                    "{} · {}",
                                    text(&conversation.status),
                                    text(&conversation.active_model)
                                ))
                                .into_any_element(),
                        );
                        for message in &conversation.messages {
                            content.push(
                                self.ui
                                    .section(
                                        &format!(
                                            "{} · {}",
                                            text(&message.role),
                                            text(&message.model)
                                        ),
                                        text(&message.content),
                                    )
                                    .into_any_element(),
                            );
                        }
                    } else {
                        content.push(self.ui.section("Ask about this request", "Ask why it was classified this way, inspect risks, or compare the available choices. A model response cannot approve a request.".into()).into_any_element());
                    }
                    content.push(
                        div()
                            .key_context("Investigation")
                            .flex()
                            .flex_col()
                            .gap(px(metrics.space[2]))
                            .child(
                                text_input("investigation-model")
                                    .placeholder_color(rgb(colors.muted))
                                    .caret_color(gpui::rgb_to_hsla(rgb(colors.text)))
                                    .state(self.model.downgrade())
                                    .placeholder("Model override (optional)")
                                    .p(px(metrics.space[2]))
                                    .min_h(px(40.))
                                    .rounded(px(metrics.panel_radius))
                                    .bg(rgb(colors.panel))
                                    .border_1()
                                    .border_color(rgb(colors.border)),
                            )
                            .child(
                                text_area("investigation-draft")
                                    .placeholder_color(rgb(colors.muted))
                                    .caret_color(gpui::rgb_to_hsla(rgb(colors.text)))
                                    .state(self.draft.downgrade())
                                    .placeholder("Ask a question…  Ctrl+Enter to send")
                                    .p(px(metrics.space[2]))
                                    .w_full()
                                    .min_h(px(100.))
                                    .max_h(px(220.))
                                    .overflow_y_scroll()
                                    .rounded(px(metrics.panel_radius))
                                    .bg(rgb(colors.panel))
                                    .border_1()
                                    .border_color(rgb(colors.border)),
                            )
                            .child(
                                div()
                                    .flex()
                                    .gap(px(metrics.space[2]))
                                    .child(
                                        self.ui
                                            .button("send-message", "Send question", colors.accent)
                                            .when(
                                                self.state.busy
                                                    || !self.state.connected
                                                    || self.backend.demo,
                                                |element| element.opacity(0.4),
                                            )
                                            .on_click(cx.listener(|this, _, _, cx| this.send(cx))),
                                    )
                                    .child(
                                        self.ui
                                            .button("cancel-turn", "Cancel turn", colors.muted)
                                            .on_click(
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
                    self.ui
                        .section(
                            "Some details unavailable",
                            self.state.snapshot.detail_error.clone(),
                        )
                        .into_any_element(),
                );
            }
            let mut permissions = Vec::new();
            if review.state == ReviewState::Pending {
                let active = self.state.connected
                    && !self.state.busy
                    && review.expires_at_ms > now_ms()
                    && !self.backend.demo;
                permissions.push(self.ui.caption("PERMISSION").into_any_element());
                if review.request.schema_id == "network.connect.v1" {
                    let any_available = review
                        .actions
                        .iter()
                        .any(|action| action.scope["match"] == "executable");
                    let mut targets = vec![];
                    for (any, title) in [(false, "This IP and port"), (true, "Any destination")] {
                        targets.push(
                            self.ui
                                .segment(title, title, self.state.permission_any_destination == any)
                                .when(any && !any_available, |element| element.opacity(0.4))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    if any && !any_available {
                                        return;
                                    }
                                    this.state.permission_any_destination = any;
                                    if any && this.state.permission_duration == "once" {
                                        this.state.permission_duration = "five-minutes".into();
                                    }
                                    this.state.confirmation = None;
                                    cx.notify();
                                }))
                                .into_any_element(),
                        );
                    }
                    permissions.push(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(metrics.space[1]))
                            .child(self.ui.caption("Apply to"))
                            .child(
                                div()
                                    .flex()
                                    .flex_wrap()
                                    .gap(px(metrics.space[1]))
                                    .children(targets),
                            )
                            .into_any_element(),
                    );
                    let mut durations = vec![];
                    for (duration, title) in [
                        ("once", "Once"),
                        ("five-minutes", "5 minutes"),
                        ("until-restart", "Until restart"),
                        ("forever", "Forever"),
                    ] {
                        let available = review.actions.iter().any(|action| {
                            action.scope["duration"] == duration
                                && (duration == "once"
                                    || (action.scope["match"] == "executable")
                                        == self.state.permission_any_destination)
                        });
                        durations.push(
                            self.ui
                                .segment(title, title, self.state.permission_duration == duration)
                                .when(!available, |element| element.opacity(0.4))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    if available {
                                        this.state.permission_duration = duration.into();
                                        if duration == "once" {
                                            this.state.permission_any_destination = false;
                                        }
                                        this.state.confirmation = None;
                                        cx.notify();
                                    }
                                }))
                                .into_any_element(),
                        );
                    }
                    permissions.push(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(metrics.space[1]))
                            .child(self.ui.caption("Duration"))
                            .child(
                                div()
                                    .flex()
                                    .flex_wrap()
                                    .gap(px(metrics.space[1]))
                                    .children(durations),
                            )
                            .into_any_element(),
                    );
                    let scope = if self.state.permission_any_destination {
                        serde_json::json!({"duration":self.state.permission_duration,"match":"executable"})
                    } else {
                        serde_json::json!({"duration":self.state.permission_duration})
                    };
                    let mut decisions = vec![];
                    for (effect, title, color) in [
                        (Effect::Block, "Deny", colors.negative),
                        (Effect::Permit, "Allow", colors.positive),
                    ] {
                        let confirmation = review
                            .actions
                            .iter()
                            .find(|action| action.effect == effect && action.scope == scope)
                            .map(|action| Confirmation {
                                review: review.clone(),
                                action: action.clone(),
                            });
                        let available = active && confirmation.is_some();
                        decisions.push(
                            self.ui
                                .button(title, title, color)
                                .px(px(metrics.space[4]))
                                .when(!available, |element| element.opacity(0.4))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    if let Some(confirmation) = &confirmation
                                        && !this.backend.demo
                                        && this.state.can_confirm(confirmation, now_ms())
                                    {
                                        this.state.confirmation = Some(confirmation.clone());
                                        this.detail_scroll.scroll_to_bottom();
                                    }
                                    cx.notify();
                                }))
                                .into_any_element(),
                        );
                    }
                    permissions.push(
                        div()
                            .flex()
                            .gap(px(metrics.space[2]))
                            .pt(px(metrics.space[1]))
                            .children(decisions)
                            .into_any_element(),
                    );
                } else {
                    for action in &review.actions {
                        let confirmation = Confirmation {
                            review: review.clone(),
                            action: action.clone(),
                        };
                        permissions.push(
                            self.ui
                                .button(
                                    SharedString::from(format!("action-{}", action.id)),
                                    text(&action.label),
                                    if action.effect == Effect::Block {
                                        colors.negative
                                    } else {
                                        colors.positive
                                    },
                                )
                                .when(!active, |element| element.opacity(0.4))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    if !this.backend.demo
                                        && this.state.can_confirm(&confirmation, now_ms())
                                    {
                                        this.state.confirmation = Some(confirmation.clone());
                                        this.detail_scroll.scroll_to_bottom();
                                    }
                                    cx.notify();
                                }))
                                .into_any_element(),
                        );
                    }
                }
            }
            content.splice(2..2, permissions);
            if let Some(confirmation) = &self.state.confirmation {
                content.push(
                    div()
                        .p(px(metrics.space[3]))
                        .rounded(px(metrics.panel_radius))
                        .bg(rgb(colors.warning_surface))
                        .border_1()
                        .border_color(rgb(colors.warning))
                        .flex()
                        .flex_col()
                        .gap(px(metrics.space[2]))
                        .child(self.ui.caption("Confirm Permission"))
                        .child(
                            div()
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .child(text(&confirmation.action.label)),
                        )
                        .child(div().text_size(px(metrics.body)).child(format!(
                            "{}\nEffect: {:?}\n{}",
                            text(&confirmation.review.request.summary),
                            confirmation.action.effect,
                            action_details(&confirmation.review, &confirmation.action)
                        )))
                        .child(
                            div()
                                .flex()
                                .gap(px(metrics.space[2]))
                                .child(
                                    self.ui
                                        .button(
                                            "confirm-action",
                                            "Confirm decision",
                                            colors.warning,
                                        )
                                        .on_click(cx.listener(|this, _, _, cx| this.confirm(cx))),
                                )
                                .child(
                                    self.ui
                                        .button("dismiss-action", "Cancel", colors.muted)
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.state.confirmation = None;
                                            cx.notify();
                                        })),
                                ),
                        )
                        .into_any_element(),
                );
            }
        } else {
            content.push(div().p(px(metrics.space[5])).text_color(rgb(colors.muted)).child("Select a decision to inspect its subject, assessment, evidence and available actions.").into_any_element());
        }
        div()
            .id("detail-scroll")
            .track_scroll(&self.detail_scroll)
            .flex_1()
            .min_w_0()
            .h_full()
            .overflow_y_scroll()
            .p(px(metrics.space[3]))
            .flex()
            .flex_col()
            .gap(px(metrics.space[2]))
            .border_l_1()
            .border_color(rgb(if self.state.pane == Pane::Details {
                colors.accent
            } else {
                colors.border
            }))
            .children(content)
    }
    fn health(&self) -> impl IntoElement {
        let metrics = self.ui.metrics;
        let mut content = vec![div().text_size(px(metrics.title)).font_weight(gpui::FontWeight::SEMIBOLD).child("Sources & Service Status").into_any_element(),
            self.ui.section("Coverage", "Each service publishes its own status. A ready source means its reader is available; it does not prove every event was observed.".into()).into_any_element()];
        if let Some(health) = &self.state.snapshot.health {
            content.push(
                self.ui
                    .section(
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
                    self.ui
                        .section(
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
                self.ui
                    .section(
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
            .p(px(metrics.space[3]))
            .flex()
            .flex_col()
            .gap(px(metrics.space[3]))
            .children(content)
    }
}
impl Render for Desktop {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = self.ui.colors;
        let metrics = self.ui.metrics;
        let mut tabs = vec![];
        for (page, title, icon) in [
            (Page::Inbox, "Pending", Icon::Inbox),
            (Page::History, "History", Icon::History),
            (Page::Health, "Sources", Icon::Server),
        ] {
            let label = if page == Page::Inbox {
                format!("Pending  {}", self.state.snapshot.pending.len())
            } else {
                title.to_owned()
            };
            tabs.push(
                self.ui
                    .icon_button(title, icon, &label, self.state.page == page)
                    .on_click(cx.listener(move |this, _, window, cx| this.page(page, window, cx)))
                    .into_any_element(),
            );
        }
        let mut appearances = vec![];
        for (appearance, title, icon) in [
            (Appearance::System, "System", Icon::System),
            (Appearance::Light, "Light", Icon::Sun),
            (Appearance::Dark, "Dark", Icon::Moon),
        ] {
            appearances.push(
                self.ui
                    .icon_button(title, icon, title, self.appearance == appearance)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.set_appearance(appearance, window, cx)
                    }))
                    .into_any_element(),
            );
        }
        let connected = self.state.connected;
        let status = if self.backend.demo {
            "Offline example · read only"
        } else if connected {
            "Connected"
        } else {
            "Disconnected"
        };
        div()
            .key_context("DecisionUI")
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(colors.canvas))
            .text_color(rgb(colors.text))
            .font_family("Inter")
            .text_size(px(metrics.body))
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
            .on_action(cx.listener(|this, _: &CycleAppearance, window, cx| {
                this.set_appearance(this.appearance.next(), window, cx)
            }))
            .child(
                div()
                    .h(px(metrics.toolbar_height))
                    .flex_shrink_0()
                    .px(px(metrics.space[3]))
                    .flex()
                    .items_center()
                    .justify_between()
                    .border_b_1()
                    .border_color(rgb(colors.border))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(metrics.space[1]))
                            .children(tabs),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(metrics.space[1]))
                            .children(appearances),
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
                    .min_h(px(28.))
                    .flex_shrink_0()
                    .px_5()
                    .py(px(metrics.space[1]))
                    .border_t_1()
                    .border_color(rgb(colors.border))
                    .text_size(px(metrics.caption))
                    .text_color(rgb(colors.muted))
                    .flex()
                    .items_center()
                    .gap(px(metrics.space[1]))
                    .child(
                        if connected {
                            Icon::Connected
                        } else {
                            Icon::Failure
                        }
                        .view(
                            if connected {
                                colors.positive
                            } else {
                                colors.warning
                            },
                            metrics.icon,
                        ),
                    )
                    .child(format!("{} · {}", status, text(&self.state.notice))),
            )
    }
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

fn action_details(review: &Review, action: &ReviewAction) -> String {
    let mut detail = json_text(&action.scope);
    if review.request.schema_id == "network.connect.v1" {
        let subject = &review.request.subject;
        let destination = if action.scope["match"] == "executable" {
            "Any IPv4/IPv6 address, any port, TCP and UDP".into()
        } else {
            format!(
                "IP {} · port {} · {}",
                subject["ip"].as_str().unwrap_or(""),
                subject["port"],
                subject["protocol"].as_str().unwrap_or("")
            )
        };
        let lifetime = match action.scope["duration"].as_str() {
            Some("forever") => "Forever · persisted by OpenSnitch across restarts",
            Some("until-restart") => "Until the OpenSnitch daemon restarts",
            Some("five-minutes") => "Five minutes",
            _ => "This request once",
        };
        detail = format!(
            "Executable path: {}\nUser: {}\nDestinations: {}\nDuration: {}\n\n{}",
            subject["process"].as_str().unwrap_or("Unattributed"),
            subject["uid"],
            destination,
            lifetime,
            detail
        );
        if action.scope["duration"] == "forever" || action.scope["match"] == "executable" {
            detail.push_str("\n\nThis matches the path, including future programs installed there. It does not verify the file contents or authorize other executable paths.");
        }
    }
    detail
}

fn application_name(path: &str) -> String {
    text(
        std::path::Path::new(path)
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or(path),
    )
}
fn destination(review: &Review) -> String {
    let subject = &review.request.subject;
    if let Some(ip) = subject["ip"].as_str() {
        let host = subject["host"]
            .as_str()
            .filter(|host| !host.is_empty())
            .unwrap_or(ip);
        format!("{}:{}", text(host), subject["port"])
    } else {
        text(&review.request.summary)
    }
}
fn relative_time(at: i64) -> String {
    let seconds = now_ms().saturating_sub(at) / 1000;
    if seconds < 60 {
        format!("{seconds}s ago")
    } else if seconds < 3600 {
        format!("{}m ago", seconds / 60)
    } else if seconds < 86400 {
        format!("{}h ago", seconds / 3600)
    } else {
        format!("{}d ago", seconds / 86400)
    }
}
fn activity_chart(reviews: &[&Review], ui: Ui) -> Div {
    let colors = ui.colors;
    let mut buckets = [0usize; 30];
    let now = now_ms();
    for review in reviews {
        let age = now.saturating_sub(review.created_at_ms);
        if (0..3600000).contains(&age) {
            buckets[29 - (age / 120000) as usize] += 1;
        }
    }
    let max = buckets.iter().copied().max().unwrap_or(1).max(1);
    div()
        .px_4()
        .pb_3()
        .flex()
        .flex_col()
        .gap_2()
        .child(
            div()
                .flex()
                .justify_between()
                .child(ui.caption("RECORDED DECISIONS · LAST HOUR"))
                .child(ui.caption(&buckets.iter().sum::<usize>().to_string())),
        )
        .child(
            div()
                .h(px(28.))
                .flex()
                .items_end()
                .gap(px(3.))
                .children(buckets.into_iter().map(|count| {
                    div()
                        .flex_1()
                        .h(px(if count == 0 {
                            2.
                        } else {
                            4. + 24. * count as f32 / max as f32
                        }))
                        .rounded_sm()
                        .bg(rgb(if count == 0 {
                            colors.chart_empty
                        } else {
                            colors.chart
                        }))
                })),
        )
}

fn review_title(review: &Review) -> String {
    if review.request.schema_id == "network.connect.v1" {
        format!(
            "{} → {}",
            application_name(State::application(review)),
            destination(review)
        )
    } else {
        text(&review.request.summary)
    }
}

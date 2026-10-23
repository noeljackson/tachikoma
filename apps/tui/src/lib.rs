//! Generic review UI. It depends only on the public adapter client and contracts.
use anyhow::Result;
use approval_client::Client;
use approval_protocol::*;
use crossterm::event::{Event, EventStream, KeyCode, KeyEventKind, KeyModifiers};
use futures_util::StreamExt;
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Text},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Tabs, Wrap},
};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}
#[derive(Default)]
struct App {
    reviews: Vec<Review>,
    history: Vec<Review>,
    health: Option<Health>,
    queue: Option<String>,
    selected: Option<String>,
    tab: usize,
    search: String,
    searching: bool,
    scroll: u16,
    action_index: usize,
    action_menu: bool,
    action_target: Option<(String, i64)>,
    selection_lost: bool,
    notice: String,
    connected: bool,
    chat: Option<Conversation>,
    chat_input: String,
    chat_model: Option<String>,
}
impl App {
    fn rows(&self) -> Vec<&Review> {
        let query = self.search.to_lowercase();
        let rows = if self.tab == 1 {
            &self.history
        } else {
            &self.reviews
        };
        rows.iter()
            .filter(|r| {
                self.queue
                    .as_ref()
                    .is_none_or(|q| *q == r.request.queue || *q == r.service_kind)
            })
            .filter(|r| {
                format!(
                    "{} {} {} {} {}",
                    r.id, r.service_kind, r.request.queue, r.request.summary, r.request.subject
                )
                .to_lowercase()
                .contains(&query)
            })
            .collect()
    }
    fn sync(&mut self) {
        let rows = self.rows();
        if let Some(selected) = &self.selected {
            if !rows.iter().any(|r| r.id == *selected) {
                self.selected = None;
                self.selection_lost = true;
                self.action_menu = false;
                self.action_target = None;
                self.scroll = 0;
                self.action_index = 0;
            }
        } else if !self.selection_lost {
            self.selected = rows.first().map(|r| r.id.clone());
        }
    }
    fn navigate(&mut self, delta: isize) {
        let rows = self.rows();
        let current = self
            .selected
            .as_ref()
            .and_then(|id| rows.iter().position(|r| r.id == *id));
        let index = current
            .map(|index| {
                index
                    .saturating_add_signed(delta)
                    .min(rows.len().saturating_sub(1))
            })
            .unwrap_or(0);
        self.selected = rows.get(index).map(|r| r.id.clone());
        self.selection_lost = false;
        self.scroll = 0;
        self.action_index = 0;
    }
    fn queues(&self) -> Vec<Option<String>> {
        let mut names = std::collections::BTreeSet::new();
        if let Some(health) = &self.health {
            for q in &health.queues {
                names.insert(q.manifest.id.clone());
            }
        }
        for r in self.reviews.iter().chain(&self.history) {
            names.insert(r.request.queue.clone());
        }
        if let Some(queue) = &self.queue {
            names.insert(queue.clone());
        }
        std::iter::once(None)
            .chain(names.into_iter().map(Some))
            .collect()
    }
    fn cycle(&mut self, back: bool) {
        let queues = self.queues();
        let current = queues.iter().position(|q| *q == self.queue).unwrap_or(0);
        let next = if back {
            (current + queues.len() - 1) % queues.len()
        } else {
            (current + 1) % queues.len()
        };
        self.queue = queues[next].clone();
        self.selected = None;
        self.selection_lost = false;
        self.sync();
    }
    fn selected(&self) -> Option<&Review> {
        let id = self.selected.as_ref()?;
        self.rows().into_iter().find(|r| r.id == *id)
    }
    async fn refresh(&mut self, client: &Client) {
        let result = async {
            Ok::<_, anyhow::Error>((
                client.list().await?,
                client.list_state("history").await?,
                client.health().await?,
            ))
        }
        .await;
        match result {
            Ok((reviews, history, health)) => {
                self.reviews = reviews;
                self.history = history;
                self.health = Some(health);
                self.connected = true;
                self.sync();
            }
            Err(_) => {
                self.connected = false;
                self.notice =
                    "Disconnected. Decisions are disabled until a fresh snapshot is available."
                        .into();
            }
        }
    }
    async fn refresh_chat(&mut self, client: &Client) {
        if let Some(chat) = &self.chat {
            match client.conversation(&chat.request_id).await {
                Ok(chat) => self.chat = Some(chat),
                Err(_) => self.notice = "Investigation unavailable; no action was taken.".into(),
            }
        }
    }
    async fn open_chat(&mut self, client: &Client) {
        let Some(review) = self.selected() else {
            return;
        };
        match client.conversation(&review.id).await {
            Ok(chat) => {
                self.chat = Some(chat);
                self.chat_input.clear();
                self.chat_model = None;
                self.scroll = 0;
            }
            Err(_) => self.notice = "Investigation unavailable for this request.".into(),
        }
    }
    fn cycle_model(&mut self) {
        let Some(chat) = &self.chat else {
            return;
        };
        let models = self
            .health
            .as_ref()
            .and_then(|h| h.queues.iter().find(|q| q.manifest.id == chat.queue))
            .and_then(|q| q.manifest.assessment.as_ref())
            .map(|p| p.models.clone())
            .unwrap_or_default();
        let choices = std::iter::once(None)
            .chain(models.into_iter().map(Some))
            .collect::<Vec<_>>();
        let index = choices
            .iter()
            .position(|m| m == &self.chat_model)
            .unwrap_or(0);
        self.chat_model = choices[(index + 1) % choices.len()].clone();
    }
    async fn send_chat(&mut self, client: &Client) {
        let Some(chat) = &self.chat else {
            return;
        };
        if self.chat_input.trim().is_empty() || chat.status == "running" {
            return;
        }
        let input = SendMessage {
            message: self.chat_input.clone(),
            model: self.chat_model.clone(),
            idempotency_key: format!(
                "tui-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos()
            ),
        };
        match client.send_message(&chat.request_id, &input).await {
            Ok(chat) => {
                self.chat = Some(chat);
                self.chat_input.clear();
                self.notice = "Investigation queued. It cannot change the decision.".into();
            }
            Err(_) => self.notice =
                "Message rejected or delivery uncertain. Refresh the conversation before retrying."
                    .into(),
        }
    }
    async fn decide(&mut self, client: &Client, permit: bool) {
        if !self.connected || self.tab != 0 {
            return;
        }
        let Some(review) = self.selected() else {
            return;
        };
        if review.expires_at_ms <= now() {
            self.notice = "Request expired; no decision sent.".into();
            return;
        }
        let actions = review
            .actions
            .iter()
            .filter(|a| {
                if permit {
                    a.effect == Effect::Permit
                } else {
                    a.effect == Effect::Block
                }
            })
            .collect::<Vec<_>>();
        let Some(action) = actions.get(if permit {
            self.action_index % actions.len().max(1)
        } else {
            0
        }) else {
            self.notice = "This request does not offer that action.".into();
            return;
        };
        let action_id = action.id.clone();
        self.send_action(client, action_id).await;
    }
    async fn send_action(&mut self, client: &Client, action_id: String) {
        if !self.connected || self.tab != 0 {
            return;
        }
        let Some(review) = self.selected() else {
            return;
        };
        if review.expires_at_ms <= now() {
            self.notice = "Request expired; no decision sent.".into();
            return;
        }
        let Some(action) = review.actions.iter().find(|a| a.id == action_id) else {
            return;
        };
        let id = review.id.clone();
        let revision = review.revision;
        let action_id = action.id.clone();
        let label = action.label.clone();
        self.notice = match client
            .decide(
                &id,
                &DecideRequest {
                    revision,
                    action_id,
                },
            )
            .await
        {
            Ok(review) => format!(
                "{} recorded for {}. Enforcement: {}",
                display_text(&label),
                id,
                if review.receipt.is_some() {
                    "producer reported"
                } else {
                    "not yet reported"
                }
            ),
            Err(_) => {
                "Decision rejected. Request may have expired, changed, or settled elsewhere.".into()
            }
        };
        self.refresh(client).await;
    }
}
fn panel(title: impl Into<String>) -> Block<'static> {
    Block::default().title(title.into()).borders(Borders::ALL)
}
fn draw(frame: &mut Frame, app: &App) {
    if frame.area().width < 70 || frame.area().height < 18 {
        frame.render_widget(
            Paragraph::new("Tachikoma needs at least 70 x 18. Resize, or press q to quit."),
            frame.area(),
        );
        return;
    }
    let vertical = Layout::vertical([
        Constraint::Length(2),
        Constraint::Min(8),
        Constraint::Length(4),
    ])
    .split(frame.area());
    frame.render_widget(
        Tabs::new(["1 Inbox", "2 History", "3 Health"])
            .select(app.tab)
            .highlight_style(
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
        vertical[0],
    );
    let columns =
        Layout::horizontal([Constraint::Length(26), Constraint::Min(20)]).split(vertical[1]);
    let queues = app.queues();
    let items = queues
        .iter()
        .map(|queue| {
            let label = queue.as_deref().unwrap_or("All");
            let count = app
                .health
                .as_ref()
                .map(|h| {
                    h.queues
                        .iter()
                        .filter(|q| {
                            queue.as_ref().is_none_or(|id| {
                                *id == q.manifest.id || *id == q.manifest.service_kind
                            })
                        })
                        .map(|q| if app.tab == 1 { q.settled } else { q.pending })
                        .sum::<u64>()
                })
                .unwrap_or(0);
            ListItem::new(display_text(&format!("{label} ({count})")))
        })
        .collect::<Vec<_>>();
    frame.render_stateful_widget(
        List::new(items)
            .block(panel(" Service / queue "))
            .highlight_style(Style::default().fg(Color::Cyan))
            .highlight_symbol("▶ "),
        columns[0],
        &mut ListState::default().with_selected(queues.iter().position(|q| *q == app.queue)),
    );
    if let Some(chat) = &app.chat {
        let areas = Layout::vertical([Constraint::Min(4), Constraint::Length(5)]).split(columns[1]);
        let mut lines = vec![
            Line::raw(display_text(&format!(
                "{} / {} — {}",
                chat.queue, chat.request_id, chat.status
            ))),
            Line::raw("Advisory conversation. No tools or permission to act."),
        ];
        for message in &chat.messages {
            lines.push(Line::raw(display_text(&format!(
                "\n{} / {}\n{}",
                message.role, message.model, message.content
            ))));
        }
        frame.render_widget(
            Paragraph::new(Text::from(lines))
                .wrap(Wrap { trim: false })
                .scroll((app.scroll, 0))
                .block(panel(" Investigate ")),
            areas[0],
        );
        frame.render_widget(
            Paragraph::new(display_text(&app.chat_input))
                .wrap(Wrap { trim: false })
                .block(panel(format!(
                    " Message — model: {} ",
                    app.chat_model.as_deref().unwrap_or("worker default")
                ))),
            areas[1],
        );
    } else if app.tab == 2 {
        let mut lines = vec![Line::raw(if app.connected {
            "Control plane connected"
        } else {
            "DISCONNECTED — review disabled"
        })];
        if let Some(health) = &app.health {
            lines.push(Line::raw(format!(
                "Jobs: {} ready, {} leased",
                health.ready_jobs, health.leased_jobs
            )));
            for service in &health.services {
                lines.push(Line::raw(display_text(&format!(
                    "{} / {} — heartbeat {}s ago — {}",
                    service.service_kind,
                    service.id,
                    (now() - service.last_seen_ms).max(0) / 1000,
                    service.detail
                ))));
            }
            for queue in &health.queues {
                lines.push(Line::raw(display_text(&format!(
                    "{} — {:?} — {} pending, {} settled",
                    queue.manifest.id, queue.manifest.mode, queue.pending, queue.settled
                ))));
            }
        }
        frame.render_widget(
            Paragraph::new(Text::from(lines))
                .wrap(Wrap { trim: false })
                .scroll((app.scroll, 0))
                .block(panel(" Health ")),
            columns[1],
        );
    } else {
        let areas = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Percentage(40), Constraint::Percentage(60)])
            .split(columns[1]);
        let rows = app.rows();
        if rows.is_empty() {
            frame.render_widget(Paragraph::new("No requests in this view.\nRegister a producer and submit a request through the public approval API.\nTab changes queue; / searches; 3 shows health.").wrap(Wrap{trim:false}).block(panel(" Requests ")),areas[0]);
        } else {
            let items = rows
                .iter()
                .map(|r| {
                    let age = if r.state == ReviewState::Pending {
                        format!("{}s left", (r.expires_at_ms - now()).max(0) / 1000)
                    } else {
                        format!("{:?}", r.state)
                    };
                    let assessment = r
                        .assessment
                        .as_ref()
                        .map(|a| {
                            format!(
                                "{} {}",
                                a.category,
                                a.confidence
                                    .map(|c| format!("{:.0}%", 100.0 * c))
                                    .unwrap_or_default()
                            )
                        })
                        .unwrap_or_else(|| "assessment pending/not requested".into());
                    ListItem::new(display_text(&format!(
                        "{} | {} | {} | {}",
                        r.request.summary, age, r.request.queue, assessment
                    )))
                })
                .collect::<Vec<_>>();
            let selected = app
                .selected
                .as_ref()
                .and_then(|id| rows.iter().position(|r| r.id == *id));
            frame.render_stateful_widget(
                List::new(items)
                    .block(panel(" Requests "))
                    .highlight_style(Style::default().fg(Color::Cyan))
                    .highlight_symbol("▶ "),
                areas[0],
                &mut ListState::default().with_selected(selected),
            );
        }
        let text=app.selected().map(|review|{
            let permits=review.actions.iter().filter(|a|a.effect==Effect::Permit).collect::<Vec<_>>();let action=permits.get(app.action_index%permits.len().max(1));
            let mut scope=action.map(|a|format!("ALLOW CHOICE: {}\n{}",a.label,a.scope)).unwrap_or_else(||"No permit action offered.".into());
            if app.action_menu { scope.push_str("\n\nAvailable actions (arrows select, Enter commits):\n"); scope.push_str(&review.actions.iter().enumerate().map(|(index, action)| format!("{} {} — {:?} — {}", if index == app.action_index % review.actions.len().max(1) { ">" } else { " " }, action.label, action.effect, action.scope)).collect::<Vec<_>>().join("\n")); }
            let assessment=review.assessment.as_ref().map(|a|format!("Classification: {} {:?}\nJev: {} / LLM: {}\n{}\n{}",a.category,a.confidence,a.jev_model,a.llm_model,a.state,a.rationale)).unwrap_or_else(||"No completed assessment. Human review is still required.".into());
            let outcome=review.decision.as_ref().map(|d|format!("Decision: {} by {}\nScope: {}\nProducer outcome: {}",d.action.label,d.actor,d.action.scope,review.receipt.as_ref().map(|r|format!("{:?}: {}",r.state,r.reason)).unwrap_or_else(||"not reported".into()))).unwrap_or_default();
            let subject=serde_json::to_string_pretty(&review.request.subject).unwrap_or_default();let evidence=serde_json::to_string_pretty(&review.request.evidence).unwrap_or_default();
            display_text(&format!("{} / {}\n{} — revision {} — {:?}\nMode: {:?}\n\n{}\n\n{}\n\n{}\n\nSubject:\n{}\nEvidence:\n{}",review.service_kind,review.request.queue,review.id,review.revision,review.state,review.mode,scope,assessment,outcome,subject,evidence))
        }).unwrap_or_else(||"The control plane records scoped decisions; each service validates and enforces them locally.\nUse `tachikoma approval --help` for enrollment and submissions.".into());
        frame.render_widget(
            Paragraph::new(text)
                .wrap(Wrap { trim: false })
                .scroll((app.scroll, 0))
                .block(panel(" Inspect / exact decision scope ")),
            areas[1],
        );
    }
    let footer = if app.chat.is_some() {
        format!(
            "Enter send | F2 model | PgUp/PgDn scroll | Ctrl-C cancel turn | Esc back\n{}",
            display_text(&app.notice)
        )
    } else if app.searching {
        format!(
            "Search: {} | Enter apply / Esc clear",
            display_text(&app.search)
        )
    } else {
        format!(
            "Tab queue | 1/2/3 view | ↑/↓ or j/k select | i investigate | Enter actions | a allow / d deny | t scope | / search | PgUp/PgDn details | q quit\n{}",
            display_text(&app.notice)
        )
    };
    frame.render_widget(
        Paragraph::new(footer)
            .wrap(Wrap { trim: false })
            .block(panel(if app.connected {
                " Connected "
            } else {
                " Disconnected: actions disabled "
            })),
        vertical[2],
    );
}
struct TerminalGuard;
impl Drop for TerminalGuard {
    fn drop(&mut self) {
        ratatui::restore();
    }
}
pub async fn run(client: Client, queue: Option<String>) -> Result<()> {
    let mut app = App {
        queue,
        ..Default::default()
    };
    app.refresh(&client).await;
    let mut terminal = ratatui::init();
    let _guard = TerminalGuard;
    let mut keys = EventStream::new();
    let mut refresh = tokio::time::interval(Duration::from_secs(2));
    loop {
        app.sync();
        terminal.draw(|frame| draw(frame, &app))?;
        tokio::select! {
            _=refresh.tick()=>{ app.refresh(&client).await; app.refresh_chat(&client).await; },
            event=keys.next()=>{let Some(event)=event else{break};let Event::Key(key)=event? else{continue};
                if key.kind!=KeyEventKind::Press{continue}
                if app.chat.is_some() {
                    match key.code {
                        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                            if let Some(chat) = &app.chat && let Ok(chat) = client.cancel_conversation(&chat.request_id).await { app.chat = Some(chat); }
                        },
                        KeyCode::Esc => { app.chat = None; app.chat_input.clear(); app.scroll = 0; },
                        KeyCode::Enter => app.send_chat(&client).await,
                        KeyCode::F(2) => app.cycle_model(),
                        KeyCode::PageDown => app.scroll = app.scroll.saturating_add(5),
                        KeyCode::PageUp => app.scroll = app.scroll.saturating_sub(5),
                        KeyCode::Backspace => { app.chat_input.pop(); },
                        KeyCode::Char(c) if !c.is_control() && app.chat_input.len() + c.len_utf8() <= 2048 => app.chat_input.push(c),
                        _ => {}
                    }
                    continue;
                }
                if key.code==KeyCode::Char('c')&&key.modifiers.contains(KeyModifiers::CONTROL){break}
                if app.action_menu {
                    match key.code {
                        KeyCode::Esc => { app.action_menu = false; app.action_target = None; app.action_index = 0; },
                        KeyCode::Up => app.action_index = app.action_index.saturating_sub(1),
                        KeyCode::Down => app.action_index = app.action_index.saturating_add(1),
                        KeyCode::Enter => {
                            let action = app.selected().filter(|r| app.action_target.as_ref() == Some(&(r.id.clone(), r.revision)))
                                .and_then(|r| r.actions.get(app.action_index % r.actions.len().max(1))).map(|a| a.id.clone());
                            app.action_menu = false; app.action_target = None; app.action_index = 0;
                            if let Some(action) = action { app.send_action(&client, action).await; }
                            else { app.notice = "Request changed; explicitly select a current request.".into(); }
                        },
                        _ => {}
                    }
                    continue;
                }
                if app.searching{match key.code{KeyCode::Char('i')=>app.open_chat(&client).await,KeyCode::Enter=>app.searching=false,KeyCode::Esc=>{app.search.clear();app.searching=false},KeyCode::Backspace=>{app.search.pop();},KeyCode::Char(c)if !c.is_control()&&app.search.len()<256=>app.search.push(c),_=>{}}app.selected=None;continue}
                match key.code{KeyCode::Enter=>{app.action_target=app.selected().map(|r|(r.id.clone(),r.revision));app.action_menu=app.action_target.is_some();app.action_index=0;app.scroll=0;},KeyCode::Char('q')|KeyCode::Esc=>break,KeyCode::Tab=>app.cycle(false),KeyCode::BackTab=>app.cycle(true),KeyCode::Char(c@'1'..='3')=>{app.tab=(c as u8-b'1')as usize;app.selected=None;app.selection_lost=false;app.scroll=0;},KeyCode::Up|KeyCode::Char('k')=>app.navigate(-1),KeyCode::Down|KeyCode::Char('j')=>app.navigate(1),KeyCode::PageDown=>app.scroll=app.scroll.saturating_add(5),KeyCode::PageUp=>app.scroll=app.scroll.saturating_sub(5),KeyCode::Char('/')=>app.searching=true,KeyCode::Char('t')=>app.action_index=app.action_index.saturating_add(1),KeyCode::Char('a')=>app.decide(&client,true).await,KeyCode::Char('d')=>app.decide(&client,false).await,KeyCode::Char('r')=>app.refresh(&client).await,_=>{}}
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn empty_view_and_disconnected_controls_render() {
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 40)).unwrap();
        terminal.draw(|frame| draw(frame, &App::default())).unwrap();
        let output = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect::<String>();
        assert!(output.contains("No requests in this view"));
        assert!(output.contains("actions disabled"));
    }
    fn review(id: &str) -> Review {
        Review {
            id: id.into(),
            producer: "fixture".into(),
            service_kind: "example".into(),
            revision: 1,
            manifest_revision: 1,
            subject_digest: String::new(),
            evidence_digest: String::new(),
            created_at_ms: now(),
            expires_at_ms: now() + 60_000,
            mode: Mode::Manual,
            state: ReviewState::Pending,
            actions: vec![],
            request: SubmitRequest {
                queue: "fixture".into(),
                producer_request_id: id.into(),
                nonce: "fixture-nonce-123456".into(),
                idempotency_key: id.into(),
                schema_id: "fixture".into(),
                subject: serde_json::json!({}),
                evidence: serde_json::json!({}),
                action_ids: vec![],
                expires_at_ms: now() + 60_000,
                summary: id.into(),
            },
            decision: None,
            assessment: None,
            receipt: None,
        }
    }
    #[test]
    fn refresh_never_retargets_an_action_after_the_selected_request_settles() {
        let mut app = App {
            reviews: vec![review("first"), review("second")],
            connected: true,
            ..Default::default()
        };
        app.sync();
        assert_eq!(app.selected.as_deref(), Some("first"));
        app.action_menu = true;
        app.action_target = Some(("first".into(), 1));
        app.reviews.remove(0);
        app.sync();
        assert!(app.selected.is_none());
        assert!(!app.action_menu);
        assert!(app.action_target.is_none());
        app.sync();
        assert!(app.selected.is_none());
        app.navigate(1);
        assert_eq!(app.selected.as_deref(), Some("second"));
    }
}

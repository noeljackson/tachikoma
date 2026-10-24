//! Focused review navigation over the public approval API.
mod input;
mod state;
use anyhow::{Result, anyhow};
use approval_client::Client;
use approval_protocol::*;
use crossterm::event::{DisableBracketedPaste, EnableBracketedPaste, Event, EventStream};
use futures_util::StreamExt;
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Text},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Tabs, Wrap},
};
use state::{App, Chat, ChatFocus, Command, Focus, Menu};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}
async fn bounded<T>(future: impl std::future::Future<Output = Result<T>>) -> Result<T> {
    tokio::time::timeout(Duration::from_secs(3), future)
        .await
        .map_err(|_| anyhow!("approval API timed out"))?
}
impl App {
    async fn refresh(&mut self, client: &Client) {
        let result = bounded(async {
            tokio::try_join!(client.list(), client.list_state("history"), client.health())
        })
        .await;
        match result {
            Ok((reviews, history, health)) => {
                self.reviews = reviews;
                self.history = history;
                self.health = Some(health);
                if !self.connected && self.notice.starts_with("Disconnected") {
                    self.notice = "Reconnected.".into();
                }
                self.connected = true;
                self.sync();
                let selected = self.selected.clone();
                let (sources, preview) = tokio::join!(bounded(client.sources()), async {
                    if let Some(id) = selected {
                        bounded(client.policy_preview(&id)).await.ok()
                    } else {
                        None
                    }
                });
                self.sources = sources.unwrap_or_default();
                self.preview = preview;
            }
            Err(_) => {
                self.connected = false;
                self.notice =
                    "Disconnected. Review actions are disabled until reconnection.".into();
            }
        }
    }
    async fn refresh_chat(&mut self, client: &Client) {
        if let Some(chat) = &mut self.chat {
            match bounded(client.conversation(&chat.conversation.request_id)).await {
                Ok(conversation) => chat.conversation = conversation,
                Err(_) => self.notice = "Conversation unavailable. Your draft is kept.".into(),
            }
        }
    }
    async fn execute(&mut self, client: &Client, command: Command) -> bool {
        match command {
            Command::Quit=>return true,
            Command::Refresh=>{self.refresh(client).await;self.refresh_chat(client).await;},
            Command::OpenChat(id)=>match bounded(client.conversation(&id)).await {
                Ok(conversation)=>{
                    self.chat=Some(Chat {conversation,input:Default::default(),model:None,focus:ChatFocus::Composer,pending_send:None});
                    self.focus=Focus::Details;self.scroll=0;
                },
                Err(_)=>self.notice="Investigation unavailable for this request.".into()
            },
            Command::SendChat {id,input}=>match bounded(client.send_message(&id,&input)).await {
                Ok(conversation)=>if let Some(chat)=&mut self.chat && chat.conversation.request_id==id {
                    chat.conversation=conversation;chat.input.clear();chat.pending_send=None;
                    self.notice="Investigation queued. Decisions remain separate.".into();
                },
                Err(_)=>self.notice="Message delivery uncertain. Enter retries the same message; your draft is kept.".into()
            },
            Command::CancelChat(id)=>match bounded(client.cancel_conversation(&id)).await {
                Ok(conversation)=>if let Some(chat)=&mut self.chat && chat.conversation.request_id==id {
                    chat.conversation=conversation;chat.pending_send=None;self.notice="Turn cancelled. Your draft is kept.".into();
                },
                Err(_)=>self.notice="Cancellation not confirmed. Refresh to check the turn.".into()
            },
            Command::Decide {target,action}=>{
                if !self.action_current(&target,&action) {self.notice="Request changed or expired. No decision sent.".into();return false;}
                let result=bounded(client.decide(&target.id,&DecideRequest {revision:target.revision,action_id:action.id})).await;
                self.notice=match result {
                    Ok(_)=>format!("{} recorded. Waiting for the producer outcome.",display_text(&action.label)),
                    Err(_)=>"Decision not confirmed. Refresh the request before trying again.".into()
                };
                self.refresh(client).await;
            }
        }
        false
    }
}
fn panel(title: impl Into<String>, focused: bool) -> Block<'static> {
    Block::default()
        .title(title.into())
        .borders(Borders::ALL)
        .border_style(Style::default().fg(if focused {
            Color::Cyan
        } else {
            Color::DarkGray
        }))
        .title_style(if focused {
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::Gray)
        })
}
fn selection(focused: bool) -> Style {
    if focused {
        Style::default()
            .bg(Color::DarkGray)
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::Gray)
    }
}
fn draw_queues(frame: &mut Frame, area: Rect, app: &App) {
    let queues = app.queues();
    let focused = app.focus == Focus::Queues && app.chat.is_none();
    let selected = if focused {
        &app.queue_cursor
    } else {
        &app.queue
    };
    let items = queues
        .iter()
        .map(|queue| {
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
            ListItem::new(display_text(&format!(
                "{} ({count})",
                queue.as_deref().unwrap_or("All queues")
            )))
        })
        .collect::<Vec<_>>();
    frame.render_stateful_widget(
        List::new(items)
            .block(panel(" Queues ", focused))
            .highlight_style(selection(focused))
            .highlight_symbol("▶ "),
        area,
        &mut ListState::default().with_selected(queues.iter().position(|q| q == selected)),
    );
}
fn draw_requests(frame: &mut Frame, area: Rect, app: &App) {
    let focused = app.focus == Focus::Requests;
    let rows = app.rows();
    let title = if app.tab == 1 {
        " History "
    } else {
        " Requests "
    };
    if rows.is_empty() {
        let text = if !app.filter.text.is_empty() {
            "No matching requests.\n/ edits the filter; Esc clears it."
        } else if app.tab == 1 {
            "No previous requests in this queue.\n← selects another queue."
        } else {
            "Nothing waiting for review.\nExisting rules keep handling traffic.\n2 opens History; 3 shows Health."
        };
        frame.render_widget(
            Paragraph::new(text)
                .wrap(Wrap { trim: false })
                .block(panel(title, focused)),
            area,
        );
        return;
    }
    let items = rows
        .iter()
        .map(|review| {
            let status = if review.state == ReviewState::Pending {
                format!("{}s left", (review.expires_at_ms - now()).max(0) / 1000)
            } else {
                format!("{:?}", review.state)
            };
            let opinion = review
                .assessment
                .as_ref()
                .map(|a| {
                    format!(
                        "{} {}",
                        a.category,
                        a.confidence
                            .map(|c| format!("{:.0}%", c * 100.0))
                            .unwrap_or_default()
                    )
                })
                .unwrap_or_else(|| "assessment pending".into());
            ListItem::new(vec![
                Line::raw(display_text(&review.request.summary)),
                Line::styled(
                    display_text(&format!("{status} · {opinion}")),
                    Style::default().fg(Color::Gray),
                ),
            ])
        })
        .collect::<Vec<_>>();
    let selected = app
        .selected
        .as_ref()
        .and_then(|id| rows.iter().position(|r| r.id == *id));
    frame.render_stateful_widget(
        List::new(items)
            .block(panel(title, focused))
            .highlight_style(selection(focused))
            .highlight_symbol("▶ "),
        area,
        &mut ListState::default().with_selected(selected),
    );
}
fn details(app: &App) -> String {
    let Some(review) = app.selected() else {
        return if app.selection_lost {
            "The selected request settled or changed.\nReturn to Requests and select another before acting.".into()
        } else {
            "Select a request to inspect its facts, classification and exact action scopes.".into()
        };
    };
    let classification = review
        .assessment
        .as_ref()
        .map(|a| {
            format!(
                "Classification: {} — {}\nJev: {}\nLLM: {}\n{}",
                a.category,
                a.confidence
                    .map(|c| format!("{:.0}% confidence", c * 100.0))
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
    let policy = app
        .preview
        .as_ref()
        .filter(|p| p.request_id == review.id)
        .map(|p| {
            format!(
                "\nEvidence policy — shadow only\n{:?} · revision {}\n{}\n{}\n",
                p.outcome,
                p.policy_revision,
                p.reason,
                p.traces
                    .iter()
                    .map(|t| format!("{}: {:?} — {}", t.rule, t.state, t.reason))
                    .collect::<Vec<_>>()
                    .join("\n")
            )
        })
        .unwrap_or_default();
    let outcome = review
        .decision
        .as_ref()
        .map(|d| {
            format!(
                "Decision: {} by {}\nExact scope: {}\nProducer outcome: {}\n",
                d.action.label,
                d.actor,
                d.action.scope,
                review
                    .receipt
                    .as_ref()
                    .map(|r| format!("{:?}: {}", r.state, r.reason))
                    .unwrap_or_else(|| "not reported".into())
            )
        })
        .unwrap_or_default();
    let actions = review
        .actions
        .iter()
        .map(|a| format!("{} — {:?}\n{}", a.label, a.effect, a.scope))
        .collect::<Vec<_>>()
        .join("\n\n");
    display_text(&format!(
        "{}\n{} · {:?} · {:?}\n{}s remaining\n\n{}{}\n\n{}\nActions and exact scopes:\n{}\n\nSubject:\n{}\n\nEvidence:\n{}\n\nRequest: {} / revision {}",
        review.request.summary,
        review.request.queue,
        review.state,
        review.mode,
        (review.expires_at_ms - now()).max(0) / 1000,
        classification,
        policy,
        outcome,
        actions,
        serde_json::to_string_pretty(&review.request.subject).unwrap_or_default(),
        serde_json::to_string_pretty(&review.request.evidence).unwrap_or_default(),
        review.id,
        review.revision
    ))
}
fn draw_details(frame: &mut Frame, area: Rect, app: &App) {
    frame.render_widget(
        Paragraph::new(details(app))
            .wrap(Wrap { trim: false })
            .scroll((app.scroll, 0))
            .block(panel(
                " Details / exact scope ",
                app.focus == Focus::Details,
            )),
        area,
    );
}
fn draw_health(frame: &mut Frame, area: Rect, app: &App) {
    let mut lines = vec![Line::raw(if app.connected {
        "Connected to Tachikoma"
    } else {
        "Disconnected — review disabled"
    })];
    if let Some(health) = &app.health {
        lines.push(Line::raw(format!(
            "Jobs: {} ready, {} running",
            health.ready_jobs, health.leased_jobs
        )));
        lines.push(Line::raw("Services across your queues:"));
        for service in &health.services {
            let heartbeat = if service.last_seen_ms == 0 {
                "not connected".into()
            } else {
                format!("{}s ago", (now() - service.last_seen_ms).max(0) / 1000)
            };
            lines.push(Line::raw(display_text(&format!(
                "\n{} — {heartbeat}\n{}",
                service.id, service.detail
            ))));
        }
        for queue in &health.queues {
            lines.push(Line::raw(display_text(&format!(
                "\n{} · {:?}\n{} pending, {} previous",
                queue.manifest.id, queue.manifest.mode, queue.pending, queue.settled
            ))));
        }
    }
    lines.push(Line::raw("\nOptional evidence sources:"));
    for source in &app.sources {
        lines.push(Line::raw(display_text(&format!(
            "{} — {}\n{}\nCoverage: {}",
            source.spec.id,
            if source.ready {
                "available"
            } else {
                "unavailable"
            },
            source.detail,
            source.spec.coverage
        ))));
    }
    frame.render_widget(
        Paragraph::new(Text::from(lines))
            .wrap(Wrap { trim: false })
            .scroll((app.scroll, 0))
            .block(panel(" Health ", app.focus == Focus::Details)),
        area,
    );
}
fn draw_input(frame: &mut Frame, area: Rect, input: &input::Input, title: String, focused: bool) {
    let block = panel(title, focused);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    let (row, _) = input.row_column();
    let column = Line::raw(
        input.text[..input.cursor]
            .rsplit('\n')
            .next()
            .unwrap_or_default(),
    )
    .width();
    let vertical = row
        .saturating_sub(inner.height.saturating_sub(1) as usize)
        .min(u16::MAX as usize) as u16;
    let horizontal = column
        .saturating_sub(inner.width.saturating_sub(1) as usize)
        .min(u16::MAX as usize) as u16;
    frame.render_widget(
        Paragraph::new(display_text(&input.text)).scroll((vertical, horizontal)),
        inner,
    );
    if focused {
        frame.set_cursor_position((
            inner.x
                + (column as u16)
                    .saturating_sub(horizontal)
                    .min(inner.width - 1),
            inner.y + (row as u16).saturating_sub(vertical).min(inner.height - 1),
        ));
    }
}
fn draw_chat(frame: &mut Frame, area: Rect, app: &App) {
    let Some(chat) = &app.chat else {
        return;
    };
    let composer_height = (chat.input.text.lines().count() as u16 + 2)
        .clamp(5, 8)
        .min(area.height.saturating_sub(4));
    let areas =
        Layout::vertical([Constraint::Min(4), Constraint::Length(composer_height)]).split(area);
    let mut lines = vec![
        Line::raw(display_text(&format!(
            "{} · {}",
            chat.conversation.queue, chat.conversation.status
        ))),
        Line::raw("Advisory investigation; decisions are separate."),
    ];
    for message in &chat.conversation.messages {
        lines.push(Line::styled(
            display_text(&format!("\n{} {}", message.role, message.model)),
            Style::default().fg(Color::Cyan),
        ));
        for line in display_text(&message.content).lines() {
            lines.push(Line::raw(line.to_owned()));
        }
    }
    frame.render_widget(
        Paragraph::new(Text::from(lines))
            .wrap(Wrap { trim: false })
            .scroll((app.scroll, 0))
            .block(panel(
                " Investigation ",
                chat.focus == ChatFocus::Transcript,
            )),
        areas[0],
    );
    draw_input(
        frame,
        areas[1],
        &chat.input,
        format!(
            " Message · {} ",
            chat.model.as_deref().unwrap_or("worker default")
        ),
        chat.focus == ChatFocus::Composer && app.menu.is_none(),
    );
}
fn hints(app: &App) -> &'static str {
    match app.menu {
        Some(Menu::Help) => return "Esc / ? / F1 Close help",
        Some(Menu::Models { .. }) => return "↑↓ Select model · Enter Choose · Esc Cancel · ? Help",
        Some(Menu::Actions(_)) => {
            return "↑↓ Select · Enter Apply action · Esc Cancel · PgUp/PgDn Scope · ? Help";
        }
        None => {}
    }
    if let Some(chat) = &app.chat {
        return if chat.focus == ChatFocus::Composer {
            "←→ Cursor · Enter Send · Alt+Enter Newline · Tab Transcript · F2 Model · Esc Back · Ctrl-C Cancel turn"
        } else {
            "↑↓ Scroll · →/Enter Write · m/F2 Model · Tab Composer · Esc Back · Ctrl-C Cancel turn · ? Help"
        };
    }
    match app.focus {
        Focus::Queues => {
            "↑↓ Choose queue · →/Enter Open · Tab Panes · 1/2/3 Views · ? Help · q Quit"
        }
        Focus::Requests => {
            "↑↓ Select · ← Queues · →/Enter Details · y/n Once · s Scope · c Chat · / Filter · ? Help · q Quit"
        }
        Focus::Details if app.tab == 2 => {
            "↑↓ Scroll · ← Queues · Tab Panes · 1/2/3 Views · r Refresh · ? Help · q Quit"
        }
        Focus::Details => {
            "↑↓ Scroll · ← Requests · Enter Actions · y/n Once · s Scope · c Chat · / Filter · ? Help · q Quit"
        }
    }
}
fn draw_footer(frame: &mut Frame, area: Rect, app: &App) {
    if app.searching {
        let rows = Layout::vertical([Constraint::Min(3), Constraint::Length(1)]).split(area);
        draw_input(
            frame,
            rows[0],
            &app.filter,
            " Filter requests ".into(),
            true,
        );
        frame.render_widget(
            Paragraph::new("Enter Apply · Esc Clear · ←→ Cursor · Ctrl-U Clear"),
            rows[1],
        );
        return;
    }
    let mut text = String::from(hints(app));
    if !app.filter.text.is_empty() {
        text.push_str(&format!("\nFilter: {}", display_text(&app.filter.text)));
    }
    if !app.notice.is_empty() {
        text.push_str(&format!("\n{}", display_text(&app.notice)));
    }
    frame.render_widget(
        Paragraph::new(text).wrap(Wrap { trim: false }).block(panel(
            if app.connected {
                " Connected "
            } else {
                " Disconnected: actions disabled "
            },
            false,
        )),
        area,
    );
}
fn draw_menu(frame: &mut Frame, app: &App) {
    let Some(menu) = &app.menu else {
        return;
    };
    let full = frame.area();
    let width = full.width.saturating_sub(8).min(90);
    let height = full.height.saturating_sub(4).min(28);
    let area = Rect::new(
        full.x + (full.width - width) / 2,
        full.y + (full.height - height) / 2,
        width,
        height,
    );
    frame.render_widget(Clear, area);
    let title = match menu {
        Menu::Help => " Keyboard help ",
        Menu::Models { .. } => " Model for the next message ",
        Menu::Actions(m) if m.scopes => " Choose an exact permission scope ",
        Menu::Actions(_) => " Choose an action ",
    };
    let block = panel(title, true);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    match menu {
        Menu::Help => {
            let help = "REVIEW\n←/→ or Tab/Shift+Tab: move between panes\n↑/↓ or j/k: navigate the focused pane\nEnter: open details, then actions\ny/n: allow once / deny once (a/d aliases)\ns: other permit scopes; Enter applies the displayed scope\nc or i: investigate the selected request\n/: filter requests · Esc: clear filter or go back\n1/2/3: Inbox / History / Health · r: refresh\nq or Ctrl-C: quit outside text editing\n\nTEXT INPUT\nArrow keys/Home/End edit text; Backspace/Delete remove text\nEnter sends chat; Alt+Enter inserts a newline\nTab switches transcript/composer; F2 opens model selection\nEsc leaves the composer, then returns to review\nCtrl-C cancels a turn; Ctrl-U clears a draft\nPasted text never triggers review shortcuts.\n\nEsc closes this help.";
            let compact = "←→ / Tab: focus panes · ↑↓ / j/k: navigate\nEnter: details/actions · y/n: once\ns: scopes · c/i: investigate · /: filter\n1/2/3: Inbox/History/Health · r: refresh\nTyping: arrows/Home/End edit · Ctrl-U clear\nChat: Enter send · Alt+Enter newline\nTab: transcript/composer · F2: model\nCtrl-C: cancel turn · Esc: back\nq: quit outside editors · Esc: close help";
            frame.render_widget(
                Paragraph::new(if inner.height < 24 { compact } else { help })
                    .wrap(Wrap { trim: false }),
                inner,
            );
        }
        Menu::Models { choices, index } => {
            let items = choices
                .iter()
                .map(|m| ListItem::new(display_text(m.as_deref().unwrap_or("Worker default"))))
                .collect::<Vec<_>>();
            frame.render_stateful_widget(
                List::new(items)
                    .highlight_style(selection(true))
                    .highlight_symbol("▶ "),
                inner,
                &mut ListState::default().with_selected(Some(*index)),
            );
        }
        Menu::Actions(menu) => {
            let parts = Layout::vertical([
                Constraint::Length((menu.actions.len() as u16).min(6) + 1),
                Constraint::Min(3),
                Constraint::Length(2),
            ])
            .split(inner);
            let items = menu
                .actions
                .iter()
                .map(|a| ListItem::new(display_text(&format!("{} · {:?}", a.label, a.effect))))
                .collect::<Vec<_>>();
            frame.render_stateful_widget(
                List::new(items)
                    .highlight_style(selection(true))
                    .highlight_symbol("▶ "),
                parts[0],
                &mut ListState::default().with_selected(Some(menu.index)),
            );
            let action = &menu.actions[menu.index];
            let scope = display_text(&format!(
                "{}\n\nExact scope:\n{}",
                action.label,
                serde_json::to_string_pretty(&action.scope).unwrap_or_default()
            ));
            frame.render_widget(
                Paragraph::new(scope)
                    .wrap(Wrap { trim: false })
                    .scroll((menu.scroll, 0)),
                parts[1],
            );
            frame.render_widget(
                Paragraph::new(if app.connected {
                    "Enter applies this action · Esc cancels"
                } else {
                    "Disconnected — cannot apply"
                })
                .wrap(Wrap { trim: false }),
                parts[2],
            );
        }
    }
}
fn draw(frame: &mut Frame, app: &App) {
    if frame.area().width < 70 || frame.area().height < 18 {
        frame.render_widget(
            Paragraph::new("Tachikoma needs 70 × 18. Resize, or q to quit."),
            frame.area(),
        );
        return;
    }
    let vertical = Layout::vertical([
        Constraint::Length(2),
        Constraint::Min(8),
        Constraint::Length(5),
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
    if app.chat.is_some() || app.tab == 2 {
        let columns =
            Layout::horizontal([Constraint::Length(24), Constraint::Min(30)]).split(vertical[1]);
        draw_queues(frame, columns[0], app);
        if app.chat.is_some() {
            draw_chat(frame, columns[1], app);
        } else {
            draw_health(frame, columns[1], app);
        }
    } else if frame.area().width >= 110 {
        let columns = Layout::horizontal([
            Constraint::Length(24),
            Constraint::Percentage(32),
            Constraint::Min(30),
        ])
        .split(vertical[1]);
        draw_queues(frame, columns[0], app);
        draw_requests(frame, columns[1], app);
        draw_details(frame, columns[2], app);
    } else {
        let columns =
            Layout::horizontal([Constraint::Length(22), Constraint::Min(30)]).split(vertical[1]);
        draw_queues(frame, columns[0], app);
        if app.focus == Focus::Details {
            draw_details(frame, columns[1], app);
        } else {
            draw_requests(frame, columns[1], app);
        }
    }
    draw_footer(frame, vertical[2], app);
    draw_menu(frame, app);
}
struct TerminalGuard;
impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = crossterm::execute!(std::io::stdout(), DisableBracketedPaste);
        ratatui::restore();
    }
}
pub async fn run(client: Client, queue: Option<String>) -> Result<()> {
    let mut app = App {
        queue: queue.clone(),
        queue_cursor: queue,
        ..Default::default()
    };
    app.refresh(&client).await;
    let mut terminal = ratatui::init();
    let _guard = TerminalGuard;
    crossterm::execute!(std::io::stdout(), EnableBracketedPaste)?;
    let mut keys = EventStream::new();
    let mut refresh = tokio::time::interval(Duration::from_secs(2));
    loop {
        app.sync();
        terminal.draw(|frame| draw(frame, &app))?;
        tokio::select! {
            _=refresh.tick()=>{app.refresh(&client).await;app.refresh_chat(&client).await;},
            event=keys.next()=>{
                let Some(event)=event else{break;};
                let command=match event? {Event::Key(key)=>app.key(key),Event::Paste(value)=>{app.paste(&value);None},_=>None};
                if let Some(command)=command && app.execute(&client,command).await {break;}
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn rendered(
        app: &App,
        width: u16,
        height: u16,
    ) -> ratatui::Terminal<ratatui::backend::TestBackend> {
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| draw(frame, app)).unwrap();
        terminal
    }
    fn text(terminal: &ratatui::Terminal<ratatui::backend::TestBackend>) -> String {
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect()
    }
    #[test]
    fn pane_focus_has_a_visible_border_and_contextual_controls() {
        let app = App {
            focus: Focus::Queues,
            ..Default::default()
        };
        let terminal = rendered(&app, 140, 40);
        assert_eq!(terminal.backend().buffer()[(0, 2)].fg, Color::Cyan);
        assert_eq!(terminal.backend().buffer()[(24, 2)].fg, Color::DarkGray);
        assert!(text(&terminal).contains("Choose queue"));
        assert!(text(&terminal).contains("Disconnected: actions disabled"));
        let app = App {
            focus: Focus::Details,
            ..Default::default()
        };
        let terminal = rendered(&app, 140, 40);
        assert!(text(&terminal).contains("Enter Actions"));
    }
    #[test]
    fn narrow_terminals_keep_the_focused_pane_and_help_is_discoverable() {
        let app = App {
            focus: Focus::Details,
            menu: Some(Menu::Help),
            ..Default::default()
        };
        let terminal = rendered(&app, 80, 30);
        let output = text(&terminal);
        assert!(output.contains("Keyboard help"));
        assert!(output.contains("Esc"));
        let terminal = rendered(&App::default(), 70, 18);
        assert!(text(&terminal).contains("Nothing waiting"));
    }
}

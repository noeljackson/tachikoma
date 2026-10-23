//! Interactive queue and investigation terminal UI.

use std::collections::BTreeSet;
use std::io;
use std::time::{Duration, Instant};

use anyhow::Result;
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use pi_tui::{EditorState, Selector, SelectorItem};
use ratatui::{
    Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap},
};

use crate::proto::tachikoma::v1::{AdapterStatus, Investigation, ModelInfo, Proposal};

pub struct App {
    pub proposals: Vec<Proposal>,
    pub investigation: Option<Investigation>,
    pub models: Vec<ModelInfo>,
    pub notice: Option<String>,
    adapters: Vec<AdapterStatus>,
    adapter: Option<String>,
    selected_proposal: usize,
    detail_scroll: u16,
    composer: EditorState,
    composing: bool,
    model_selector: Option<Selector>,
    show_tools: bool,
}

impl App {
    pub fn new(
        proposals: Vec<Proposal>,
        models: Vec<ModelInfo>,
        adapters: Vec<AdapterStatus>,
        adapter: Option<String>,
    ) -> Self {
        Self {
            proposals,
            investigation: None,
            models,
            notice: None,
            adapters,
            adapter,
            selected_proposal: 0,
            detail_scroll: 0,
            composer: EditorState::default(),
            composing: false,
            model_selector: None,
            show_tools: true,
        }
    }

    pub fn set_investigation(&mut self, investigation: Investigation) {
        self.notice = None;
        self.investigation = Some(investigation);
    }

    fn visible_proposals(&self) -> Vec<&Proposal> {
        self.proposals
            .iter()
            .filter(|proposal| {
                self.adapter
                    .as_ref()
                    .is_none_or(|adapter| proposal.adapter == *adapter)
            })
            .collect()
    }

    fn queue_adapters(&self) -> Vec<String> {
        self.adapters
            .iter()
            .filter(|adapter| adapter.name != "ai")
            .map(|adapter| adapter.name.clone())
            .chain(
                self.proposals
                    .iter()
                    .map(|proposal| proposal.adapter.clone()),
            )
            .chain(self.adapter.iter().cloned())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }

    fn cycle_queue(&mut self, backwards: bool) {
        let queues = std::iter::once(None)
            .chain(self.queue_adapters().into_iter().map(Some))
            .collect::<Vec<_>>();
        let current = queues
            .iter()
            .position(|adapter| *adapter == self.adapter)
            .unwrap_or(0);
        let next = if backwards {
            (current + queues.len() - 1) % queues.len()
        } else {
            (current + 1) % queues.len()
        };
        self.adapter = queues[next].clone();
        self.selected_proposal = 0;
        self.detail_scroll = 0;
        self.notice = None;
    }

    pub fn update_queue(&mut self, proposals: Vec<Proposal>, adapters: Vec<AdapterStatus>) {
        let selected_id = self
            .visible_proposals()
            .get(self.selected_proposal)
            .map(|proposal| proposal.id.clone());
        self.proposals = proposals;
        self.adapters = adapters;
        let selected = selected_id.and_then(|id| {
            self.visible_proposals()
                .iter()
                .position(|proposal| proposal.id == id)
        });
        if selected.is_none() {
            self.detail_scroll = 0;
        }
        self.selected_proposal = selected.unwrap_or(0);
    }

    fn ai_enabled(&self) -> bool {
        self.adapters
            .iter()
            .any(|adapter| adapter.name == "ai" && adapter.enabled)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Quit,
    RefreshQueue,
    OpenProposal(String),
    SendMessage {
        investigation_id: String,
        message: String,
    },
    SetModel {
        investigation_id: String,
        model: String,
    },
    CancelTurn(String),
    QueueRemediation(String),
    Refresh(String),
}

/// Owns terminal state across API calls, including periodic queue refreshes.
/// Dropping the session restores the terminal on both normal exit and errors.
pub struct Session {
    terminal: Terminal<CrosstermBackend<io::Stdout>>,
}

impl Session {
    pub fn new() -> Result<Self> {
        let mut session = Self {
            terminal: Terminal::new(CrosstermBackend::new(io::stdout()))?,
        };
        enable_raw_mode()?;
        execute!(session.terminal.backend_mut(), EnterAlternateScreen)?;
        session.terminal.clear()?;
        Ok(session)
    }

    /// Runs until an API action is requested; the CLI performs that action,
    /// updates the app, then resumes this same terminal session.
    pub fn run(&mut self, app: &mut App) -> Result<Action> {
        let terminal = &mut self.terminal;
        let opened = Instant::now();
        loop {
            terminal.draw(|frame| draw(frame, app))?;
            if app.investigation.is_none() && opened.elapsed() >= Duration::from_secs(2) {
                break Ok(Action::RefreshQueue);
            }
            if !event::poll(Duration::from_millis(100))? {
                if let Some(investigation) = &app.investigation
                    && investigation.status == "running"
                {
                    break Ok(Action::Refresh(investigation.id.clone()));
                }
                continue;
            }
            let Event::Key(key) = event::read()? else {
                continue;
            };
            if key.kind != KeyEventKind::Press {
                continue;
            }
            if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
                if let Some(investigation) = &app.investigation
                    && investigation.status == "running"
                {
                    break Ok(Action::CancelTurn(investigation.id.clone()));
                }
                break Ok(Action::Quit);
            }
            if key.code == KeyCode::Char('q') && !app.composing {
                break Ok(Action::Quit);
            }
            if let Some(selector) = &mut app.model_selector {
                match key.code {
                    KeyCode::Up => {
                        selector.selected = selector.selected.saturating_sub(1);
                    }
                    KeyCode::Down => {
                        selector.selected =
                            (selector.selected + 1).min(selector.items.len().saturating_sub(1));
                    }
                    KeyCode::Enter => {
                        let selected = selector.selected().map(|item| item.value.clone());
                        app.model_selector = None;
                        if let (Some(model), Some(investigation)) =
                            (selected, app.investigation.as_ref())
                        {
                            break Ok(Action::SetModel {
                                investigation_id: investigation.id.clone(),
                                model,
                            });
                        }
                    }
                    KeyCode::Esc => app.model_selector = None,
                    _ => {}
                }
                continue;
            }
            if app.investigation.is_none() {
                match key.code {
                    KeyCode::Tab => app.cycle_queue(false),
                    KeyCode::BackTab => app.cycle_queue(true),
                    KeyCode::Char('r') => break Ok(Action::RefreshQueue),
                    KeyCode::PageUp => app.detail_scroll = app.detail_scroll.saturating_sub(5),
                    KeyCode::PageDown => app.detail_scroll = app.detail_scroll.saturating_add(5),
                    KeyCode::Up => {
                        app.selected_proposal = app.selected_proposal.saturating_sub(1);
                        app.detail_scroll = 0;
                    }
                    KeyCode::Down => {
                        app.selected_proposal = (app.selected_proposal + 1)
                            .min(app.visible_proposals().len().saturating_sub(1));
                        app.detail_scroll = 0;
                    }
                    KeyCode::Enter => {
                        if !app.ai_enabled() {
                            app.notice = Some("AI investigation is disabled. Enable ai.enabled and configure a model in the daemon config; proposal details remain available here.".into());
                        } else if let Some(proposal) =
                            app.visible_proposals().get(app.selected_proposal)
                        {
                            break Ok(Action::OpenProposal(proposal.id.clone()));
                        }
                    }
                    _ => {}
                }
                continue;
            }
            let investigation = app.investigation.as_ref().expect("checked above");
            if app.composing {
                match key.code {
                    KeyCode::Esc => app.composing = false,
                    KeyCode::Backspace => {
                        let mut chars = app.composer.draft().chars().collect::<Vec<_>>();
                        chars.pop();
                        app.composer
                            .set_draft(chars.into_iter().collect::<String>());
                    }
                    KeyCode::Enter => {
                        let message = app.composer.draft().trim().to_string();
                        if !message.is_empty() {
                            app.composer.record_history(message.clone());
                            app.composer.set_draft("");
                            app.composing = false;
                            break Ok(Action::SendMessage {
                                investigation_id: investigation.id.clone(),
                                message,
                            });
                        }
                    }
                    KeyCode::Char(character)
                        if !key
                            .modifiers
                            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                    {
                        app.composer.insert(&character.to_string());
                    }
                    _ => {}
                }
                continue;
            }
            match key.code {
                KeyCode::Esc | KeyCode::Char('p') => app.investigation = None,
                KeyCode::Enter => app.composing = true,
                KeyCode::Char('m') => {
                    app.model_selector = Some(Selector::new(
                        "Switch model",
                        app.models
                            .iter()
                            .filter(|model| model.authenticated)
                            .map(|model| SelectorItem {
                                label: format!("{} — {}", model.name, model.credential_source),
                                value: format!("{}/{}", model.provider, model.id),
                                active: model.provider == investigation.active_provider
                                    && model.id == investigation.active_model,
                            })
                            .collect(),
                    ));
                }
                KeyCode::Char('t') => app.show_tools = !app.show_tools,
                KeyCode::Char('r') => {
                    break Ok(Action::QueueRemediation(investigation.id.clone()));
                }
                _ => {}
            }
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(self.terminal.backend_mut(), LeaveAlternateScreen);
        let _ = self.terminal.show_cursor();
    }
}

fn draw(frame: &mut ratatui::Frame<'_>, app: &App) {
    if let Some(selector) = &app.model_selector {
        draw_model_selector(frame, selector);
    } else if let Some(investigation) = &app.investigation {
        draw_investigation(frame, app, investigation);
    } else {
        draw_queue(frame, app);
    }
}

fn draw_queue(frame: &mut ratatui::Frame<'_>, app: &App) {
    let areas = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(3), Constraint::Length(3)])
        .split(frame.area());
    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(24), Constraint::Min(10)])
        .split(areas[0]);
    let details = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(40), Constraint::Percentage(60)])
        .split(columns[1]);
    let names = app.queue_adapters();
    let mut queues = vec![ListItem::new(format!("All ({})", app.proposals.len()))];
    queues.extend(names.iter().map(|name| {
        let count = app
            .proposals
            .iter()
            .filter(|proposal| proposal.adapter == *name)
            .count();
        ListItem::new(format!("{name} ({count})"))
    }));
    let selected_queue = app
        .adapter
        .as_ref()
        .and_then(|adapter| names.iter().position(|name| name == adapter))
        .map_or(0, |index| index + 1);
    frame.render_stateful_widget(
        List::new(queues)
            .block(
                Block::default()
                    .title(" Service queues ")
                    .borders(Borders::ALL),
            )
            .highlight_style(
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )
            .highlight_symbol("▶ "),
        columns[0],
        &mut ListState::default().with_selected(Some(selected_queue)),
    );
    let proposals = app.visible_proposals();
    let queue_block = Block::default()
        .title(format!(
            " {} — {} proposals ",
            app.adapter.as_deref().unwrap_or("All"),
            proposals.len()
        ))
        .borders(Borders::ALL);
    if proposals.is_empty() {
        frame.render_widget(
            Paragraph::new("No proposals in this queue.\nRequests appear when a producer submits them.\nTab switches service queues; r refreshes.")
                .wrap(Wrap { trim: false }).block(queue_block),
            details[0],
        );
    } else {
        let items = proposals
            .iter()
            .map(|proposal| {
                ListItem::new(format!(
                    "{}  {:?}  {}/{}  risk:{}",
                    proposal.id, proposal.state, proposal.adapter, proposal.action, proposal.risk
                ))
            })
            .collect::<Vec<_>>();
        frame.render_stateful_widget(
            List::new(items)
                .block(queue_block)
                .highlight_style(
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                )
                .highlight_symbol("▶ "),
            details[0],
            &mut ListState::default().with_selected(Some(app.selected_proposal)),
        );
    }
    let ai = if app.ai_enabled() {
        "enabled"
    } else {
        "disabled — enable ai.enabled and configure a model to investigate"
    };
    let body = if let Some(proposal) = proposals.get(app.selected_proposal) {
        let evidence = serde_json::from_str::<serde_json::Value>(&proposal.evidence_json)
            .ok()
            .and_then(|value| serde_json::to_string_pretty(&value).ok())
            .unwrap_or_else(|| proposal.evidence_json.clone());
        format!(
            "{}/{}   risk: {}\n{}\n\n{}\n\nEvidence:\n{}",
            proposal.adapter,
            proposal.action,
            proposal.risk,
            proposal.id,
            proposal.preview,
            evidence
        )
    } else {
        let adapters = app
            .adapters
            .iter()
            .filter(|adapter| {
                adapter.name != "ai"
                    && app
                        .adapter
                        .as_ref()
                        .is_none_or(|name| *name == adapter.name)
            })
            .map(|adapter| {
                format!(
                    "{}: {} — {}",
                    adapter.name,
                    if adapter.enabled {
                        "enabled"
                    } else {
                        "disabled"
                    },
                    adapter.detail
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        format!(
            "{adapters}\n\nServices submit requests to their adapter queue. Existing network traffic is not imported automatically.\nSelect a proposal to inspect it, then Enter to open an AI investigation.\n\nTo create a review-only example (does not run kubectl):\ntachikoma suggest-kubectl --context demo -- get pods"
        )
    };
    let notice = app
        .notice
        .as_ref()
        .map(|notice| format!("{notice}\n\n"))
        .unwrap_or_default();
    frame.render_widget(
        Paragraph::new(format!("{notice}AI: {ai}\n\n{body}"))
            .wrap(Wrap { trim: false })
            .scroll((app.detail_scroll, 0))
            .block(
                Block::default()
                    .title(" Proposal details / setup ")
                    .borders(Borders::ALL),
            ),
        details[1],
    );
    frame.render_widget(
        Paragraph::new(
            "Tab/Shift-Tab queue   ↑/↓ proposal   PgUp/PgDn details   Enter investigate   r refresh   q quit",
        )
        .wrap(Wrap { trim: false })
        .block(Block::default().borders(Borders::ALL)),
        areas[1],
    );
}

fn draw_investigation(frame: &mut ratatui::Frame<'_>, app: &App, investigation: &Investigation) {
    let areas = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(8),
            Constraint::Length(3),
            Constraint::Length(3),
        ])
        .split(frame.area());
    frame.render_widget(
        Paragraph::new(format!(
            "proposal {}   model {}/{}   status {}",
            investigation.proposal_id,
            investigation.active_provider,
            investigation.active_model,
            investigation.status
        ))
        .block(
            Block::default()
                .title(" Investigation ")
                .borders(Borders::ALL),
        ),
        areas[0],
    );
    frame.render_widget(
        Paragraph::new(transcript(investigation, app.show_tools))
            .wrap(Wrap { trim: false })
            .block(Block::default().title(" Transcript ").borders(Borders::ALL)),
        areas[1],
    );
    frame.render_widget(
        Paragraph::new(app.composer.draft()).block(
            Block::default()
                .title(if app.composing {
                    " Message (editing) "
                } else {
                    " Message "
                })
                .borders(Borders::ALL),
        ),
        areas[2],
    );
    frame.render_widget(
        Paragraph::new(
            "Enter compose/send   m model   p proposals   t tools   r queue draft   Ctrl-C cancel   q quit",
        )
        .block(Block::default().borders(Borders::ALL)),
        areas[3],
    );
}

fn draw_model_selector(frame: &mut ratatui::Frame<'_>, selector: &Selector) {
    let items = selector
        .items
        .iter()
        .map(|item| {
            ListItem::new(if item.active {
                format!("● {}", item.label)
            } else {
                format!("  {}", item.label)
            })
        })
        .collect::<Vec<_>>();
    let mut state =
        ListState::default().with_selected((!items.is_empty()).then_some(selector.selected));
    frame.render_stateful_widget(
        List::new(items)
            .block(
                Block::default()
                    .title(format!(" {} ", selector.title))
                    .borders(Borders::ALL),
            )
            .highlight_style(
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )
            .highlight_symbol("▶ "),
        frame.area(),
        &mut state,
    );
}

fn transcript(investigation: &Investigation, show_tools: bool) -> Text<'static> {
    let mut lines = Vec::new();
    for event in &investigation.events {
        let value: serde_json::Value = serde_json::from_str(&event.json).unwrap_or_default();
        match event.kind.as_str() {
            "message" => {
                let message = &value["message"];
                let role = message["role"].as_str().unwrap_or("message");
                let content = message["content"].as_str().unwrap_or_default();
                lines.push(Line::from(vec![
                    Span::styled(
                        format!("{role}> "),
                        Style::default().fg(match role {
                            "user" => Color::Green,
                            "assistant" => Color::Cyan,
                            "tool" => Color::Yellow,
                            _ => Color::Magenta,
                        }),
                    ),
                    Span::raw(content.to_string()),
                ]));
                lines.push(Line::raw(""));
            }
            "tool_started" | "tool_finished" if show_tools => {
                lines.push(Line::styled(
                    format!("[{}] {}", event.kind, compact_json(&value)),
                    Style::default().fg(Color::DarkGray),
                ));
            }
            "active_model_changed" | "compaction" | "remediation_drafted" => {
                lines.push(Line::styled(
                    format!("[{}] {}", event.kind, compact_json(&value)),
                    Style::default().fg(Color::Yellow),
                ));
            }
            "turn_failed" | "turn_cancelled" => {
                lines.push(Line::styled(
                    format!("[{}] {}", event.kind, compact_json(&value)),
                    Style::default().fg(Color::Red),
                ));
            }
            _ => {}
        }
    }
    if lines.is_empty() {
        lines.push(Line::raw(
            "No messages yet. Ask a question about this proposal.",
        ));
    }
    Text::from(lines)
}

fn compact_json(value: &serde_json::Value) -> String {
    let value = serde_json::to_string(value).unwrap_or_default();
    let mut chars = value.chars();
    let compact = chars.by_ref().take(240).collect::<String>();
    if chars.next().is_some() {
        format!("{compact}…")
    } else {
        compact
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;

    fn proposal(id: &str, adapter: &str) -> Proposal {
        Proposal {
            id: id.into(),
            adapter: adapter.into(),
            action: "review_request".into(),
            preview: format!("Inspect request {id}"),
            evidence_json: r#"{"package":"example"}"#.into(),
            ..Default::default()
        }
    }

    #[test]
    fn queues_discover_adapters_and_filter_exactly() {
        let mut app = App::new(
            vec![
                proposal("one", "registrygate"),
                proposal("two", "opensnitch"),
            ],
            vec![],
            vec![AdapterStatus {
                name: "ai".into(),
                ..Default::default()
            }],
            Some("registrygate".into()),
        );
        assert_eq!(app.queue_adapters(), vec!["opensnitch", "registrygate"]);
        assert_eq!(app.visible_proposals()[0].id, "one");
        assert_eq!(app.visible_proposals().len(), 1);
        app.cycle_queue(false);
        assert!(app.adapter.is_none());
        assert_eq!(app.visible_proposals().len(), 2);
        app.cycle_queue(true);
        assert_eq!(app.adapter.as_deref(), Some("registrygate"));
    }

    #[test]
    fn refresh_preserves_selected_identity_and_handles_removal() {
        let mut app = App::new(
            vec![
                proposal("one", "registrygate"),
                proposal("two", "registrygate"),
            ],
            vec![],
            vec![],
            Some("registrygate".into()),
        );
        app.selected_proposal = 1;
        app.update_queue(
            vec![
                proposal("other", "opensnitch"),
                proposal("new", "registrygate"),
                proposal("one", "registrygate"),
                proposal("two", "registrygate"),
            ],
            vec![],
        );
        assert_eq!(app.visible_proposals()[app.selected_proposal].id, "two");
        app.update_queue(vec![], vec![]);
        assert_eq!(app.selected_proposal, 0);
        assert!(app.visible_proposals().is_empty());
        assert_eq!(app.queue_adapters(), vec!["registrygate"]);
    }

    fn screen(app: &App) -> String {
        let mut terminal = Terminal::new(TestBackend::new(140, 40)).unwrap();
        terminal.draw(|frame| draw(frame, app)).unwrap();
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect()
    }

    #[test]
    fn empty_queue_explains_setup_and_selected_queue_remains_visible() {
        let app = App::new(vec![], vec![], vec![], Some("registrygate".into()));
        let rendered = screen(&app);
        assert!(rendered.contains("registrygate (0)"));
        assert!(rendered.contains("No proposals in this queue"));
        assert!(rendered.contains("AI: disabled"));
        assert!(rendered.contains("suggest-kubectl"));
    }

    #[test]
    fn filtered_details_only_show_the_selected_adapter() {
        let app = App::new(
            vec![
                proposal("one", "registrygate"),
                proposal("secret-other", "opensnitch"),
            ],
            vec![],
            vec![],
            Some("registrygate".into()),
        );
        let rendered = screen(&app);
        assert!(rendered.contains("registrygate (1)"));
        assert!(rendered.contains("opensnitch (1)"));
        assert!(rendered.contains("Inspect request one"));
        assert!(rendered.contains("example"));
        assert!(!rendered.contains("secret-other"));
    }
}

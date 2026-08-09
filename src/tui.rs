//! Interactive queue and investigation terminal UI.

use std::io;
use std::time::Duration;

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

use crate::proto::tachikoma::v1::{Investigation, ModelInfo, Proposal};

pub struct App {
    pub proposals: Vec<Proposal>,
    pub investigation: Option<Investigation>,
    pub models: Vec<ModelInfo>,
    selected_proposal: usize,
    composer: EditorState,
    composing: bool,
    model_selector: Option<Selector>,
    show_tools: bool,
}

impl App {
    pub fn new(proposals: Vec<Proposal>, models: Vec<ModelInfo>) -> Self {
        Self {
            proposals,
            investigation: None,
            models,
            selected_proposal: 0,
            composer: EditorState::default(),
            composing: false,
            model_selector: None,
            show_tools: true,
        }
    }

    pub fn set_investigation(&mut self, investigation: Investigation) {
        self.investigation = Some(investigation);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Quit,
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

/// Runs until an API action is requested. The CLI performs that action through
/// Connect, updates `App`, then calls this again.
pub fn run(app: &mut App) -> Result<Action> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    let result = (|| -> Result<Action> {
        loop {
            terminal.draw(|frame| draw(frame, app))?;
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
                    KeyCode::Up => app.selected_proposal = app.selected_proposal.saturating_sub(1),
                    KeyCode::Down => {
                        app.selected_proposal =
                            (app.selected_proposal + 1).min(app.proposals.len().saturating_sub(1))
                    }
                    KeyCode::Enter => {
                        if let Some(proposal) = app.proposals.get(app.selected_proposal) {
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
    })();
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    result
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
    let items = app
        .proposals
        .iter()
        .map(|proposal| {
            ListItem::new(format!(
                "{}  {:?}  {}/{}  risk:{}",
                proposal.id, proposal.state, proposal.adapter, proposal.action, proposal.risk
            ))
        })
        .collect::<Vec<_>>();
    let mut state =
        ListState::default().with_selected((!items.is_empty()).then_some(app.selected_proposal));
    frame.render_stateful_widget(
        List::new(items)
            .block(
                Block::default()
                    .title(" Proposal queue ")
                    .borders(Borders::ALL),
            )
            .highlight_style(
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )
            .highlight_symbol("▶ "),
        areas[0],
        &mut state,
    );
    frame.render_widget(
        Paragraph::new("↑/↓ select   Enter investigate   q quit")
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

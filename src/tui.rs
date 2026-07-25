//! Small local terminal UI for the proposal queue.

use std::io;
use std::time::Duration;

use anyhow::Result;
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Terminal,
    backend::CrosstermBackend,
    text::Text,
    widgets::{Block, Borders, Paragraph},
};

/// Render a snapshot obtained through the Connect API. `q` or Escape exits.
pub fn run(lines: &[String]) -> Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    let result = (|| -> Result<()> {
        loop {
            terminal.draw(|frame| {
                let text = if lines.is_empty() {
                    "No proposals.\n\nPress q or Escape to close.".to_owned()
                } else {
                    format!("{}\n\nPress q or Escape to close.", lines.join("\n"))
                };
                let widget = Paragraph::new(Text::raw(text)).block(
                    Block::default()
                        .title(" Tachikoma proposal queue ")
                        .borders(Borders::ALL),
                );
                frame.render_widget(widget, frame.area());
            })?;
            if event::poll(Duration::from_millis(250))?
                && let Event::Key(key) = event::read()?
                && key.kind == KeyEventKind::Press
                && matches!(key.code, KeyCode::Char('q') | KeyCode::Esc)
            {
                break;
            }
        }
        Ok(())
    })();
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    result
}

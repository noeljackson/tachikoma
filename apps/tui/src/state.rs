use crate::{input::Input, now};
use approval_protocol::*;
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use serde_json::json;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Default, Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Focus {
    Queues,
    #[default]
    Requests,
    Details,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Target {
    pub id: String,
    pub revision: i64,
}
impl Target {
    pub fn of(review: &Review) -> Self {
        Self {
            id: review.id.clone(),
            revision: review.revision,
        }
    }
}
pub(crate) enum Command {
    Quit,
    Refresh,
    OpenChat(String),
    SendChat {
        id: String,
        input: SendMessage,
    },
    CancelChat(String),
    Decide {
        target: Target,
        action: ReviewAction,
    },
}
pub(crate) struct Actions {
    pub target: Target,
    pub actions: Vec<ReviewAction>,
    pub index: usize,
    pub scroll: u16,
    pub scopes: bool,
    pub confirming: bool,
}
pub(crate) enum Menu {
    Actions(Actions),
    Models {
        choices: Vec<Option<String>>,
        index: usize,
    },
    Help,
}
#[derive(Default, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ChatFocus {
    Transcript,
    #[default]
    Composer,
}
pub(crate) struct Chat {
    pub conversation: Conversation,
    pub input: Input,
    pub model: Option<String>,
    pub focus: ChatFocus,
    pub pending_send: Option<SendMessage>,
}
#[derive(Default)]
pub(crate) struct App {
    pub reviews: Vec<Review>,
    pub history: Vec<Review>,
    pub health: Option<Health>,
    pub sources: Vec<SourceInfo>,
    pub preview: Option<PolicyPreview>,
    pub queue: Option<String>,
    pub queue_cursor: Option<String>,
    pub selected: Option<String>,
    pub tab: usize,
    pub focus: Focus,
    pub filter: Input,
    pub searching: bool,
    pub scroll: u16,
    pub menu: Option<Menu>,
    pub selection_lost: bool,
    pub notice: String,
    pub connected: bool,
    pub chat: Option<Chat>,
}
impl App {
    pub fn rows(&self) -> Vec<&Review> {
        let query = self.filter.text.to_lowercase();
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
    pub fn selected(&self) -> Option<&Review> {
        let id = self.selected.as_ref()?;
        self.rows().into_iter().find(|r| r.id == *id)
    }
    pub fn queues(&self) -> Vec<Option<String>> {
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
        if let Some(queue) = &self.queue_cursor {
            names.insert(queue.clone());
        }
        std::iter::once(None)
            .chain(names.into_iter().map(Some))
            .collect()
    }
    pub fn sync(&mut self) {
        let missing = self
            .selected
            .as_ref()
            .is_some_and(|id| !self.rows().iter().any(|r| r.id == *id));
        let changed = if let Some(Menu::Actions(menu)) = &self.menu {
            self.selected().is_none_or(|r| {
                Target::of(r) != menu.target || menu.actions.iter().any(|a| !r.actions.contains(a))
            })
        } else {
            false
        };
        if missing || changed {
            self.selected = None;
            self.selection_lost = true;
            if matches!(self.menu, Some(Menu::Actions(_))) {
                self.menu = None;
            }
            self.scroll = 0;
            self.notice = if self.chat.is_some() {
                "Request settled; the investigation can continue."
            } else {
                "Request settled or changed. Select another request before acting."
            }
            .into();
        } else if self.selected.is_none() && !self.selection_lost {
            self.selected = self.rows().first().map(|r| r.id.clone());
        }
    }
    fn navigate(&mut self, delta: isize) {
        let rows = self.rows();
        let current = self
            .selected
            .as_ref()
            .and_then(|id| rows.iter().position(|r| r.id == *id));
        let index = current
            .map(|i| {
                i.saturating_add_signed(delta)
                    .min(rows.len().saturating_sub(1))
            })
            .unwrap_or(0);
        self.selected = rows.get(index).map(|r| r.id.clone());
        self.selection_lost = false;
        self.scroll = 0;
    }
    fn move_queue(&mut self, delta: isize) {
        let queues = self.queues();
        let index = queues
            .iter()
            .position(|q| q == &self.queue_cursor)
            .unwrap_or(0);
        self.queue_cursor =
            queues[index.saturating_add_signed(delta).min(queues.len() - 1)].clone();
    }
    fn apply_queue(&mut self) {
        if self.queue != self.queue_cursor {
            self.queue = self.queue_cursor.clone();
            self.selected = None;
            self.selection_lost = false;
            self.scroll = 0;
            self.sync();
        }
    }
    fn move_focus(&mut self, delta: isize, wrap: bool) {
        let panes = if self.tab == 2 {
            vec![Focus::Queues, Focus::Details]
        } else {
            vec![Focus::Queues, Focus::Requests, Focus::Details]
        };
        let current = panes.iter().position(|p| *p == self.focus).unwrap_or(0);
        let index = if wrap {
            (current as isize + delta).rem_euclid(panes.len() as isize) as usize
        } else {
            current.saturating_add_signed(delta).min(panes.len() - 1)
        };
        if self.focus == Focus::Queues && panes[index] != Focus::Queues {
            self.apply_queue();
        }
        self.focus = panes[index];
    }
    fn switch_view(&mut self, tab: usize) {
        self.tab = tab;
        self.focus = if tab == 2 {
            Focus::Details
        } else {
            Focus::Requests
        };
        self.selected = None;
        self.selection_lost = false;
        self.scroll = 0;
        self.menu = None;
        self.sync();
    }
    fn editable_action(&self) -> bool {
        self.connected
            && self.tab == 0
            && self.focus != Focus::Queues
            && self.chat.is_none()
            && self
                .selected()
                .is_some_and(|r| r.state == ReviewState::Pending && r.expires_at_ms > now())
    }
    pub fn action_current(&self, target: &Target, action: &ReviewAction) -> bool {
        self.editable_action()
            && self
                .selected()
                .is_some_and(|r| Target::of(r) == *target && r.actions.contains(action))
    }
    pub fn once_action(review: &Review, effect: Effect) -> Option<&ReviewAction> {
        let mut matches = review.actions.iter().filter(|a| {
            a.effect == effect
                && (a.scope == json!({"duration":"once"}) || a.scope == json!({"uses":1}))
        });
        let first = matches.next()?;
        if matches.next().is_some() {
            None
        } else {
            Some(first)
        }
    }
    fn open_actions(&mut self, effect: Option<Effect>) {
        if !self.editable_action() {
            self.notice = "No live request selected, or disconnected. No decision sent.".into();
            return;
        }
        let review = self.selected().unwrap();
        let actions = review
            .actions
            .iter()
            .filter(|a| !a.selectable())
            .filter(|a| effect.as_ref().is_none_or(|e| *e == a.effect))
            .cloned()
            .collect::<Vec<_>>();
        if actions.is_empty() {
            self.notice = "This request does not offer that action.".into();
            return;
        }
        self.menu = Some(Menu::Actions(Actions {
            target: Target::of(review),
            actions,
            index: 0,
            scroll: 0,
            scopes: effect.is_some(),
            confirming: false,
        }));
    }
    fn quick(&mut self, effect: Effect) -> Option<Command> {
        if !self.editable_action() {
            self.notice = "No live request selected, or disconnected. No decision sent.".into();
            return None;
        }
        let review = self.selected().unwrap();
        if let Some(action) = Self::once_action(review, effect.clone()) {
            return Some(Command::Decide {
                target: Target::of(review),
                action: action.clone(),
            });
        }
        // Opaque or ambiguous scopes need an explicit menu selection.
        self.open_actions(Some(effect));
        None
    }
    fn model_menu(&mut self) {
        let Some(chat) = &self.chat else {
            return;
        };
        let models = self
            .health
            .as_ref()
            .and_then(|h| {
                h.queues
                    .iter()
                    .find(|q| q.manifest.id == chat.conversation.queue)
            })
            .and_then(|q| q.manifest.assessment.as_ref())
            .map(|p| p.models.clone())
            .unwrap_or_default();
        let choices = std::iter::once(None)
            .chain(models.into_iter().map(Some))
            .collect::<Vec<_>>();
        let index = choices
            .iter()
            .position(|model| model == &chat.model)
            .unwrap_or(0);
        self.menu = Some(Menu::Models { choices, index });
    }
    fn menu_key(&mut self, key: KeyEvent) -> Option<Command> {
        let menu = self.menu.take()?;
        if matches!(key.code, KeyCode::Esc | KeyCode::Char('q')) {
            return None;
        }
        match menu {
            Menu::Help => {
                if !matches!(
                    key.code,
                    KeyCode::Char('?') | KeyCode::Enter | KeyCode::F(1)
                ) {
                    self.menu = Some(Menu::Help);
                }
            }
            Menu::Actions(mut menu) => {
                match key.code {
                    KeyCode::Up | KeyCode::Char('k') => {
                        menu.confirming = false;
                        menu.index = menu.index.saturating_sub(1);
                        menu.scroll = 0;
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        menu.confirming = false;
                        menu.index = menu.index.saturating_add(1).min(menu.actions.len() - 1);
                        menu.scroll = 0;
                    }
                    KeyCode::PageUp => menu.scroll = menu.scroll.saturating_sub(5),
                    KeyCode::PageDown => menu.scroll = menu.scroll.saturating_add(5),
                    KeyCode::Enter => {
                        let action = menu.actions[menu.index].clone();
                        if self.action_current(&menu.target, &action) {
                            if !menu.confirming
                                && action.scope != json!({"duration":"once"})
                                && action.scope != json!({"uses":1})
                            {
                                menu.confirming = true;
                                self.menu = Some(Menu::Actions(menu));
                                return None;
                            }
                            return Some(Command::Decide {
                                target: menu.target,
                                action,
                            });
                        }
                        self.notice = "Request changed or expired. No decision sent.".into();
                        return None;
                    }
                    _ => {}
                }
                self.menu = Some(Menu::Actions(menu));
            }
            Menu::Models { choices, mut index } => {
                match key.code {
                    KeyCode::Up | KeyCode::Char('k') => index = index.saturating_sub(1),
                    KeyCode::Down | KeyCode::Char('j') => {
                        index = index.saturating_add(1).min(choices.len() - 1)
                    }
                    KeyCode::Enter => {
                        if let Some(chat) = &mut self.chat {
                            chat.model = choices[index].clone();
                        }
                        return None;
                    }
                    _ => {}
                }
                self.menu = Some(Menu::Models { choices, index });
            }
        }
        None
    }
    fn send_message(&mut self) -> Option<Command> {
        let chat = self.chat.as_mut()?;
        if chat.input.text.trim().is_empty() {
            return None;
        }
        let matches = chat
            .pending_send
            .as_ref()
            .is_some_and(|p| p.message == chat.input.text && p.model == chat.model);
        if chat.conversation.status == "running" && !matches {
            self.notice =
                "A turn is already running. Ctrl-C cancels it; your draft is kept.".into();
            return None;
        }
        if !matches {
            chat.pending_send = Some(SendMessage {
                message: chat.input.text.clone(),
                model: chat.model.clone(),
                idempotency_key: format!(
                    "tui-{}-{}",
                    std::process::id(),
                    SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_nanos()
                ),
            });
        }
        Some(Command::SendChat {
            id: chat.conversation.request_id.clone(),
            input: chat.pending_send.clone().unwrap(),
        })
    }
    fn chat_key(&mut self, key: KeyEvent) -> Option<Command> {
        if key.code == KeyCode::F(2) {
            self.model_menu();
            return None;
        }
        let chat = self.chat.as_mut()?;
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            return Some(Command::CancelChat(chat.conversation.request_id.clone()));
        }
        if matches!(key.code, KeyCode::Tab | KeyCode::BackTab) {
            chat.focus = if chat.focus == ChatFocus::Composer {
                ChatFocus::Transcript
            } else {
                ChatFocus::Composer
            };
            return None;
        }
        if chat.focus == ChatFocus::Composer {
            match key.code {
                KeyCode::Esc => chat.focus = ChatFocus::Transcript,
                KeyCode::Enter if key.modifiers.contains(KeyModifiers::ALT) => {
                    chat.input.insert("\n", 2048, true)
                }
                KeyCode::Enter => return self.send_message(),
                _ => chat.input.key(key, 2048, true),
            }
        } else {
            match key.code {
                KeyCode::Esc | KeyCode::Left | KeyCode::Char('q') => {
                    self.chat = None;
                    self.focus = if self.selected.is_some() {
                        Focus::Details
                    } else {
                        Focus::Requests
                    };
                    self.scroll = 0;
                }
                KeyCode::Enter | KeyCode::Right | KeyCode::Char('i') => {
                    chat.focus = ChatFocus::Composer
                }
                KeyCode::Up | KeyCode::Char('k') => self.scroll = self.scroll.saturating_sub(1),
                KeyCode::Down | KeyCode::Char('j') => self.scroll = self.scroll.saturating_add(1),
                KeyCode::PageUp => self.scroll = self.scroll.saturating_sub(5),
                KeyCode::PageDown => self.scroll = self.scroll.saturating_add(5),
                KeyCode::Char('m') => self.model_menu(),
                KeyCode::Char('?') => self.menu = Some(Menu::Help),
                _ => {}
            }
        }
        None
    }
    pub fn paste(&mut self, value: &str) {
        if self.menu.is_some() {
            return;
        }
        if self.searching {
            self.filter.insert(value, 256, false);
            self.selected = None;
            self.selection_lost = false;
            self.sync();
        } else if let Some(chat) = &mut self.chat
            && chat.focus == ChatFocus::Composer
        {
            chat.input.insert(value, 2048, true);
        }
    }
    pub fn key(&mut self, key: KeyEvent) -> Option<Command> {
        if key.kind == KeyEventKind::Release {
            return None;
        }
        if key.kind == KeyEventKind::Repeat
            && matches!(
                key.code,
                KeyCode::Enter | KeyCode::Char('y' | 'n' | 'a' | 'd' | 's' | 't' | 'c' | 'i' | 'q')
            )
            && !self.searching
            && self
                .chat
                .as_ref()
                .is_none_or(|c| c.focus != ChatFocus::Composer)
        {
            return None;
        }
        if key.kind == KeyEventKind::Repeat && key.code == KeyCode::Enter {
            return None;
        }
        if key.code == KeyCode::F(1)
            || (key.code == KeyCode::Char('?')
                && (self.menu.is_some()
                    || (!self.searching
                        && self
                            .chat
                            .as_ref()
                            .is_none_or(|c| c.focus != ChatFocus::Composer))))
        {
            self.menu = if matches!(self.menu, Some(Menu::Help)) {
                None
            } else {
                Some(Menu::Help)
            };
            return None;
        }
        if self.menu.is_some() {
            return self.menu_key(key);
        }
        if self.searching {
            match key.code {
                KeyCode::Enter => {
                    self.searching = false;
                    self.focus = Focus::Requests;
                }
                KeyCode::Esc => {
                    self.filter.clear();
                    self.searching = false;
                }
                _ => self.filter.key(key, 256, false),
            }
            self.selected = None;
            self.selection_lost = false;
            self.sync();
            return None;
        }
        if self.chat.is_some() {
            return self.chat_key(key);
        }
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            return Some(Command::Quit);
        }
        match key.code {
            KeyCode::Char('q') => return Some(Command::Quit),
            KeyCode::Char('?') => self.menu = Some(Menu::Help),
            KeyCode::Tab => self.move_focus(1, true),
            KeyCode::BackTab => self.move_focus(-1, true),
            KeyCode::Left => self.move_focus(-1, false),
            KeyCode::Right => self.move_focus(1, false),
            KeyCode::Up | KeyCode::Char('k') => match self.focus {
                Focus::Queues => self.move_queue(-1),
                Focus::Requests => self.navigate(-1),
                Focus::Details => self.scroll = self.scroll.saturating_sub(1),
            },
            KeyCode::Down | KeyCode::Char('j') => match self.focus {
                Focus::Queues => self.move_queue(1),
                Focus::Requests => self.navigate(1),
                Focus::Details => self.scroll = self.scroll.saturating_add(1),
            },
            KeyCode::PageUp => self.scroll = self.scroll.saturating_sub(5),
            KeyCode::PageDown => self.scroll = self.scroll.saturating_add(5),
            KeyCode::Enter => match self.focus {
                Focus::Queues => {
                    self.apply_queue();
                    self.focus = if self.tab == 2 {
                        Focus::Details
                    } else {
                        Focus::Requests
                    };
                }
                Focus::Requests => self.focus = Focus::Details,
                Focus::Details => self.open_actions(None),
            },
            KeyCode::Char(c @ '1'..='3') => self.switch_view((c as u8 - b'1') as usize),
            KeyCode::Char('y' | 'a') => return self.quick(Effect::Permit),
            KeyCode::Char('n' | 'd') => return self.quick(Effect::Block),
            KeyCode::Char('s' | 't') => self.open_actions(Some(Effect::Permit)),
            KeyCode::Char('c' | 'i') if self.focus != Focus::Queues && self.tab != 2 => {
                return self.selected().map(|r| Command::OpenChat(r.id.clone()));
            }
            KeyCode::Char('/') if self.tab != 2 => {
                self.searching = true;
                self.focus = Focus::Requests;
                self.filter.cursor = self.filter.text.len();
            }
            KeyCode::Char('r') => return Some(Command::Refresh),
            KeyCode::Esc => {
                if !self.filter.text.is_empty() {
                    self.filter.clear();
                    self.selected = None;
                    self.selection_lost = false;
                    self.sync();
                } else if self.focus == Focus::Queues {
                    self.queue_cursor = self.queue.clone();
                } else {
                    self.move_focus(-1, false);
                }
            }
            _ => {}
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }
    fn review(id: &str) -> Review {
        let actions = vec![
            ReviewAction {
                id: "allow-five-minutes".into(),
                label: "Allow for five minutes".into(),
                effect: Effect::Permit,
                scope: json!({"duration":"five-minutes"}),
            },
            ReviewAction {
                id: "deny-once".into(),
                label: "Deny once".into(),
                effect: Effect::Block,
                scope: json!({"duration":"once"}),
            },
            ReviewAction {
                id: "allow-once".into(),
                label: "Allow once".into(),
                effect: Effect::Permit,
                scope: json!({"duration":"once"}),
            },
        ];
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
            actions,
            request: SubmitRequest {
                queue: "fixture".into(),
                producer_request_id: id.into(),
                nonce: "fixture-nonce-123456".into(),
                idempotency_key: id.into(),
                schema_id: "fixture".into(),
                subject: json!({}),
                evidence: json!({}),
                action_ids: vec![
                    "allow-once".into(),
                    "deny-once".into(),
                    "allow-five-minutes".into(),
                ],
                expires_at_ms: now() + 60_000,
                summary: id.into(),
            },
            decision: None,
            assessment: None,
            receipt: None,
        }
    }
    fn app() -> App {
        let mut app = App {
            reviews: vec![review("first"), review("second")],
            connected: true,
            ..Default::default()
        };
        app.sync();
        app
    }
    fn chat() -> Chat {
        Chat {
            conversation: Conversation {
                request_id: "first".into(),
                queue: "fixture".into(),
                messages: vec![],
                status: "idle".into(),
                active_model: String::new(),
            },
            input: Input::default(),
            model: None,
            focus: ChatFocus::Composer,
            pending_send: None,
        }
    }
    #[test]
    fn arrows_navigate_focused_panes_and_queue_selection_is_explicit() {
        let mut app = app();
        app.key(key(KeyCode::Left));
        assert_eq!(app.focus, Focus::Queues);
        app.key(key(KeyCode::Down));
        assert_eq!(app.queue, None);
        assert_eq!(app.queue_cursor.as_deref(), Some("fixture"));
        assert!(app.key(key(KeyCode::Char('y'))).is_none());
        app.key(key(KeyCode::Right));
        assert_eq!(app.focus, Focus::Requests);
        assert_eq!(app.queue.as_deref(), Some("fixture"));
        app.key(key(KeyCode::Right));
        assert_eq!(app.focus, Focus::Details);
        app.key(key(KeyCode::Down));
        assert_eq!(app.scroll, 1);
        assert_eq!(app.selected.as_deref(), Some("first"));
        app.key(key(KeyCode::Left));
        app.key(key(KeyCode::Down));
        assert_eq!(app.selected.as_deref(), Some("second"));
        app.key(key(KeyCode::Tab));
        assert_eq!(app.focus, Focus::Details);
        app.key(key(KeyCode::BackTab));
        assert_eq!(app.focus, Focus::Requests);
        app.key(key(KeyCode::Char('3')));
        app.key(key(KeyCode::Left));
        assert_eq!(app.focus, Focus::Queues);
        app.key(key(KeyCode::Right));
        assert_eq!(app.focus, Focus::Details);
    }
    #[test]
    fn search_letters_and_arrows_never_trigger_review_or_chat_shortcuts() {
        let mut app = app();
        app.key(key(KeyCode::Char('/')));
        for c in "iynqm?".chars() {
            assert!(app.key(key(KeyCode::Char(c))).is_none());
        }
        assert!(app.chat.is_none());
        assert_eq!(app.filter.text, "iynqm?");
        assert!(app.menu.is_none());
        app.key(key(KeyCode::Left));
        app.key(key(KeyCode::Char('x')));
        assert_eq!(app.filter.text, "iynqmx?");
        app.key(key(KeyCode::Enter));
        assert!(!app.searching);
        app.key(key(KeyCode::Esc));
        assert!(app.filter.text.is_empty());
        assert!(
            matches!(app.key(key(KeyCode::Char('i'))),Some(Command::OpenChat(id)) if id=="first")
        );
        assert!(
            matches!(app.key(key(KeyCode::Char('c'))),Some(Command::OpenChat(id)) if id=="first")
        );
    }
    #[test]
    fn once_shortcuts_ignore_scope_order_and_other_scopes_require_a_menu() {
        let mut app = app();
        assert!(
            matches!(app.key(key(KeyCode::Char('y'))),Some(Command::Decide{action,..}) if action.id=="allow-once")
        );
        assert!(
            matches!(app.key(key(KeyCode::Char('n'))),Some(Command::Decide{action,..}) if action.id=="deny-once")
        );
        app.key(key(KeyCode::Char('s')));
        assert!(matches!(app.menu, Some(Menu::Actions(_))));
        assert!(app.key(key(KeyCode::Enter)).is_none());
        assert!(
            matches!(app.key(key(KeyCode::Enter)),Some(Command::Decide{target,action}) if target.id=="first" && action.id=="allow-five-minutes")
        );
        app.reviews[0].actions.retain(|a| a.id != "allow-once");
        assert!(app.key(key(KeyCode::Char('y'))).is_none());
        assert!(matches!(app.menu, Some(Menu::Actions(_))));
    }
    #[test]
    fn refresh_and_revision_changes_never_retarget_an_open_action() {
        let mut app = app();
        app.key(key(KeyCode::Char('s')));
        app.reviews.remove(0);
        app.sync();
        assert!(app.selected.is_none());
        assert!(app.menu.is_none());
        assert!(app.key(key(KeyCode::Char('y'))).is_none());
        app.sync();
        assert!(app.selected.is_none());
        app.key(key(KeyCode::Down));
        assert_eq!(app.selected.as_deref(), Some("second"));
        app.key(key(KeyCode::Char('s')));
        app.reviews[0].revision = 2;
        app.sync();
        assert!(app.selected.is_none());
        assert!(app.menu.is_none());
    }
    #[test]
    fn expired_disconnected_and_repeated_keys_cannot_apply_actions() {
        let mut app = app();
        let mut repeat = key(KeyCode::Char('y'));
        repeat.kind = KeyEventKind::Repeat;
        assert!(app.key(repeat).is_none());
        app.key(key(KeyCode::Char('s')));
        let mut repeat = key(KeyCode::Enter);
        repeat.kind = KeyEventKind::Repeat;
        assert!(app.key(repeat).is_none());
        assert!(app.menu.is_some());
        app.reviews[0].expires_at_ms = now() - 1;
        assert!(app.key(key(KeyCode::Enter)).is_none());
        app.reviews[0].expires_at_ms = now() + 60_000;
        app.connected = false;
        assert!(app.key(key(KeyCode::Char('y'))).is_none());
    }
    #[test]
    fn chat_arrows_edit_drafts_and_escape_returns_through_the_transcript() {
        let mut app = app();
        app.chat = Some(chat());
        for c in "iynqm?".chars() {
            assert!(app.key(key(KeyCode::Char(c))).is_none());
        }
        app.key(key(KeyCode::Left));
        app.key(key(KeyCode::Backspace));
        assert_eq!(app.chat.as_ref().unwrap().input.text, "iynq?");
        app.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT));
        assert!(app.chat.as_ref().unwrap().input.text.contains('\n'));
        app.key(key(KeyCode::Esc));
        assert!(app.chat.as_ref().unwrap().focus == ChatFocus::Transcript);
        assert!(app.key(key(KeyCode::Char('y'))).is_none());
        app.key(key(KeyCode::Esc));
        assert!(app.chat.is_none());
    }
    #[test]
    fn pasted_text_is_never_interpreted_as_commands() {
        let mut app = app();
        app.paste("ynq\n");
        assert_eq!(app.selected.as_deref(), Some("first"));
        assert!(app.filter.text.is_empty());
        app.chat = Some(chat());
        app.paste("ynq\nmore");
        assert_eq!(app.chat.as_ref().unwrap().input.text, "ynq\nmore");
        assert!(app.chat.as_ref().unwrap().pending_send.is_none());
    }
    #[test]
    fn uncertain_send_reuses_its_key_and_a_running_turn_preserves_new_drafts() {
        let mut app = app();
        app.chat = Some(chat());
        app.paste("Question");
        let Some(Command::SendChat { input: first, .. }) = app.key(key(KeyCode::Enter)) else {
            panic!("send")
        };
        app.chat.as_mut().unwrap().conversation.status = "running".into();
        let Some(Command::SendChat { input: retry, .. }) = app.key(key(KeyCode::Enter)) else {
            panic!("retry")
        };
        assert_eq!(first.idempotency_key, retry.idempotency_key);
        app.paste(" changed");
        assert!(app.key(key(KeyCode::Enter)).is_none());
        assert_eq!(app.chat.as_ref().unwrap().input.text, "Question changed");
    }
    #[test]
    fn settlement_preserves_chat_draft_and_model_picker() {
        let mut app = app();
        app.chat = Some(chat());
        app.paste("Draft");
        app.menu = Some(Menu::Models {
            choices: vec![None, Some("qwen".into())],
            index: 0,
        });
        app.reviews.remove(0);
        app.sync();
        assert!(matches!(app.menu, Some(Menu::Models { .. })));
        app.key(key(KeyCode::Down));
        app.key(key(KeyCode::Enter));
        let chat = app.chat.as_ref().unwrap();
        assert_eq!(chat.model.as_deref(), Some("qwen"));
        assert_eq!(chat.input.text, "Draft");
    }
}

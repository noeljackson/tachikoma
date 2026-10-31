use super::*;
use crate::permissions::{self, Field};

impl Desktop {
    fn open_rule_field(&mut self, field: Field, window: &mut Window, cx: &mut Context<Self>) {
        let Some(confirmation) = &self.state.confirmation else {
            return;
        };
        let options = field.options(&confirmation.review, &confirmation.action.scope);
        let cursor = options
            .iter()
            .position(|(value, _, _)| confirmation.action.scope[field.key()] == *value)
            .unwrap_or(0);
        self.rule_field = Some((field, cursor));
        self.rule_focus = Field::ALL
            .iter()
            .position(|value| *value == field)
            .unwrap_or(0);
        window.focus(&self.confirmation_focus, cx);
        cx.notify();
    }
    fn select_rule_value(
        &mut self,
        field: Field,
        value: &str,
        keyboard: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(confirmation) = &mut self.state.confirmation else {
            return;
        };
        if !field
            .options(&confirmation.review, &confirmation.action.scope)
            .iter()
            .any(|(option, _, reason)| *option == value && reason.is_none())
        {
            return;
        }
        confirmation.action.scope[field.key()] = serde_json::json!(value);
        self.rule_field = None;
        self.confirmation_requires_release = keyboard;
        cx.notify();
    }
    pub(super) fn rule_key(
        &mut self,
        event: &gpui::KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(confirmation) = &self.state.confirmation else {
            return false;
        };
        if confirmation.action.scope["kind"] != "network-rule" {
            return false;
        }
        if let Some((field, cursor)) = self.rule_field {
            let options = field.options(&confirmation.review, &confirmation.action.scope);
            match event.keystroke.key.as_str() {
                "up" | "down" => {
                    let next = if event.keystroke.key == "up" {
                        (0..cursor).rev().find(|index| options[*index].2.is_none())
                    } else {
                        (cursor + 1..options.len()).find(|index| options[*index].2.is_none())
                    };
                    if let Some(next) = next {
                        self.rule_field = Some((field, next));
                        cx.notify();
                    }
                    return true;
                }
                "enter" => {
                    if !event.is_held {
                        self.select_rule_value(field, options[cursor].0, true, cx);
                    }
                    return true;
                }
                "escape" => {
                    self.rule_field = None;
                    cx.notify();
                    return true;
                }
                _ => {}
            }
        }
        match event.keystroke.key.as_str() {
            "tab" => {
                self.rule_field = None;
                self.rule_focus = (self.rule_focus
                    + if event.keystroke.modifiers.shift {
                        4
                    } else {
                        1
                    })
                    % Field::ALL.len();
                cx.notify();
                true
            }
            "space" => {
                self.open_rule_field(Field::ALL[self.rule_focus], window, cx);
                true
            }
            _ => false,
        }
    }
    fn rule_field_control(
        &self,
        field: Field,
        confirmation: &Confirmation,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let colors = self.ui.colors;
        let metrics = self.ui.metrics;
        let options = field.options(&confirmation.review, &confirmation.action.scope);
        let label = options
            .iter()
            .find(|(value, _, _)| confirmation.action.scope[field.key()] == *value)
            .map(|(_, label, _)| label.clone())
            .unwrap_or_else(|| "Choose…".into());
        let focused = Field::ALL[self.rule_focus] == field;
        let mut control = div()
            .relative()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .gap(px(metrics.space[1]))
            .child(self.ui.caption(field.label()))
            .child(
                self.ui
                    .field_select(
                        SharedString::from(format!("rule-{}", field.key())),
                        label,
                        focused,
                    )
                    .debug_selector(move || format!("rule-{}", field.key()))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, window, cx| {
                            this.open_rule_field(field, window, cx)
                        }),
                    ),
            );
        let subject = &confirmation.review.request.subject;
        let detail = match field {
            Field::Source if confirmation.action.scope["source"] == "executable" => Some(text(
                subject["process"]
                    .as_str()
                    .filter(|s| !s.is_empty())
                    .unwrap_or("Executable unavailable"),
            )),
            Field::Source => Some(format!("All applications run by UID {}", subject["uid"])),
            Field::Destination => Some(match confirmation.action.scope["destination"].as_str() {
                Some("any") => "Any IPv4 or IPv6 destination".into(),
                Some("ip") => text(subject["ip"].as_str().unwrap_or("IP unavailable")),
                _ => permissions::hostname(&confirmation.review)
                    .unwrap_or_else(|| "Hostname unavailable".into()),
            }),
            _ => None,
        };
        if let Some(detail) = detail {
            control = control.child(self.ui.caption(&detail).truncate());
        }
        if let Some((open, cursor)) = self.rule_field
            && open == field
        {
            let mut rows = Vec::new();
            for (index, (value, label, reason)) in options.iter().enumerate() {
                let id = SharedString::from(format!("rule-{}-{value}", field.key()));
                let row = if reason.is_some() {
                    self.ui.unavailable_menu_option(id, label.clone())
                } else {
                    let value = (*value).to_owned();
                    self.ui
                        .menu_option(id, label.clone(), index == cursor)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.select_rule_value(field, &value, false, cx)
                        }))
                };
                rows.push(
                    row.debug_selector(move || format!("rule-choice-{index}"))
                        .into_any_element(),
                );
            }
            let reason = options.iter().find_map(|(_, _, reason)| *reason);
            control = control.child(deferred(
                anchored()
                    .position_mode(AnchoredPositionMode::Local)
                    .position(point(
                        px(0.),
                        px(metrics.control_height + metrics.space[1] + metrics.line_height),
                    ))
                    .snap_to_window()
                    .child(
                        div()
                            .id("rule-field-menu")
                            .debug_selector(|| "rule-field-menu".into())
                            .w(px(metrics.permission_menu_width))
                            .p(px(metrics.space[1]))
                            .rounded(px(metrics.panel_radius))
                            .bg(rgb(colors.panel))
                            .border_1()
                            .border_color(rgb(colors.control_border))
                            .shadow_lg()
                            .occlude()
                            .on_mouse_down_out(cx.listener(move |this, _, _, cx| {
                                if this.rule_field.is_some_and(|(open, _)| open == field) {
                                    this.rule_field = None;
                                    cx.notify();
                                }
                            }))
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap(px(metrics.space[0]))
                                    .children(rows),
                            )
                            .when_some(reason, |element, reason| {
                                element.child(
                                    self.ui
                                        .caption(reason)
                                        .whitespace_normal()
                                        .p(px(metrics.space[1])),
                                )
                            }),
                    ),
            ));
        }
        control.into_any_element()
    }
    pub(super) fn rule_fields(
        &self,
        confirmation: &Confirmation,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let metrics = self.ui.metrics;
        div()
            .id("rule-fields")
            .debug_selector(|| "rule-fields".into())
            .flex()
            .flex_col()
            .gap(px(metrics.space[3]))
            .child(
                div()
                    .flex()
                    .gap(px(metrics.space[2]))
                    .child(self.rule_field_control(Field::Source, confirmation, cx))
                    .child(self.rule_field_control(Field::Destination, confirmation, cx)),
            )
            .child(
                div()
                    .flex()
                    .gap(px(metrics.space[2]))
                    .child(self.rule_field_control(Field::Protocol, confirmation, cx))
                    .child(self.rule_field_control(Field::Port, confirmation, cx))
                    .child(self.rule_field_control(Field::Duration, confirmation, cx)),
            )
            .child(self.ui.caption(&format!(
                "Applies to UID {} · Tab moves between fields · Space opens choices",
                confirmation.review.request.subject["uid"]
            )))
            .when(
                confirmation.action.scope["source"] == "executable",
                |element| {
                    element.child(
                        self.ui
                            .caption("Includes future programs installed at this executable path.")
                            .whitespace_normal(),
                    )
                },
            )
            .when_some(
                permissions::error(&confirmation.review, &confirmation.action),
                |element, reason| {
                    element.child(
                        div()
                            .text_color(rgb(self.ui.colors.warning))
                            .text_size(px(metrics.body))
                            .whitespace_normal()
                            .child(reason),
                    )
                },
            )
            .into_any_element()
    }
}

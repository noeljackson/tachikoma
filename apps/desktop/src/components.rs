use crate::{
    design::{Colors, METRICS, Metrics, Mode},
    icons::Icon,
    state::text,
};
use gpui::{Div, ElementId, SharedString, Stateful, div, prelude::*, px, rgb};

#[derive(Clone, Copy)]
pub struct Ui {
    pub colors: Colors,
    pub metrics: Metrics,
}
impl Ui {
    pub fn new(mode: Mode) -> Self {
        Self {
            colors: Colors::new(mode),
            metrics: METRICS,
        }
    }
    pub fn caption(self, value: &str) -> Div {
        div()
            .text_size(px(self.metrics.caption))
            .text_color(rgb(self.colors.muted))
            .child(value.to_owned())
    }
    fn control(self, id: impl Into<ElementId>, color: u32) -> Stateful<Div> {
        div()
            .id(id)
            .px(px(self.metrics.space[2]))
            .py(px(self.metrics.space[1]))
            .min_h(px(self.metrics.control_height))
            .rounded(px(self.metrics.control_radius))
            .text_size(px(self.metrics.body))
            .cursor_pointer()
            .text_color(rgb(color))
            .bg(rgb(self.colors.control))
            .border_1()
            .border_color(rgb(self.colors.control_border))
            .hover_style(move |style| {
                style
                    .bg(rgb(self.colors.hover))
                    .border_color(rgb(self.colors.focus))
            })
            .active_style(move |style| style.bg(rgb(self.colors.selected)))
    }
    pub fn button(
        self,
        id: impl Into<ElementId>,
        label: impl Into<SharedString>,
        color: u32,
    ) -> Stateful<Div> {
        self.control(id, color).child(label.into())
    }
    pub fn segment(
        self,
        id: impl Into<ElementId>,
        label: impl Into<SharedString>,
        selected: bool,
    ) -> Stateful<Div> {
        self.segment_control(id, selected).child(label.into())
    }
    fn segment_control(self, id: impl Into<ElementId>, selected: bool) -> Stateful<Div> {
        self.control(
            id,
            if selected {
                self.colors.selected_text
            } else {
                self.colors.muted
            },
        )
        .bg(rgb(if selected {
            self.colors.selected
        } else {
            self.colors.control
        }))
        .border_color(rgb(if selected {
            self.colors.focus
        } else {
            self.colors.control_border
        }))
    }
    pub fn icon_button(
        self,
        id: impl Into<ElementId>,
        icon: Icon,
        label: &str,
        selected: bool,
    ) -> Stateful<Div> {
        let foreground = if selected {
            self.colors.selected_text
        } else {
            self.colors.muted
        };
        self.segment_control(id, selected)
            .flex()
            .items_center()
            .gap(px(self.metrics.space[1]))
            .min_w_0()
            .child(
                div()
                    .flex()
                    .items_center()
                    .min_w_0()
                    .gap(px(self.metrics.space[1]))
                    .child(icon.view(foreground, self.metrics.icon))
                    .child(div().min_w_0().truncate().child(label.to_owned())),
            )
    }
    pub fn section(self, title: &str, body: String) -> Div {
        div()
            .py(px(self.metrics.space[2]))
            .border_b_1()
            .border_color(rgb(self.colors.border))
            .child(self.caption(title))
            .child(
                div()
                    .mt(px(self.metrics.space[1]))
                    .text_size(px(self.metrics.body))
                    .whitespace_normal()
                    .child(text(&body)),
            )
    }
}

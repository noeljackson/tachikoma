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
        self.control_base(id, color)
            .cursor_pointer()
            .hover_style(move |style| {
                style
                    .bg(rgb(self.colors.hover))
                    .border_color(rgb(self.colors.focus))
            })
            .active_style(move |style| style.bg(rgb(self.colors.selected)))
    }
    fn control_base(self, id: impl Into<ElementId>, color: u32) -> Stateful<Div> {
        div()
            .id(id)
            .px(px(self.metrics.space[2]))
            .h(px(self.metrics.control_height))
            .flex()
            .items_center()
            .justify_center()
            .line_height(px(self.metrics.line_height))
            .rounded(px(self.metrics.control_radius))
            .text_size(px(self.metrics.body))
            .text_color(rgb(color))
            .bg(rgb(self.colors.control))
            .border_1()
            .border_color(rgb(self.colors.control_border))
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
    pub fn pill(self, id: impl Into<ElementId>, label: String, selected: bool) -> Stateful<Div> {
        self.segment(id, label, selected)
            .h(px(self.metrics.filter_height))
            .rounded_full()
    }
    pub fn queue_option(
        self,
        id: impl Into<ElementId>,
        label: String,
        checked: bool,
        focused: bool,
    ) -> Stateful<Div> {
        self.control(id, self.colors.text)
            .w_full()
            .justify_start()
            .gap(px(self.metrics.space[1]))
            .bg(rgb(if focused {
                self.colors.hover
            } else {
                self.colors.panel
            }))
            .border_color(rgb(if focused {
                self.colors.focus
            } else {
                self.colors.panel
            }))
            .child(
                div()
                    .size(px(self.metrics.icon))
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(self.metrics.control_radius / 2.))
                    .border_1()
                    .border_color(rgb(self.colors.control_border))
                    .when(checked, |element| {
                        element.child(Icon::Check.view(self.colors.accent, self.metrics.icon))
                    }),
            )
            .child(div().flex_1().min_w_0().truncate().child(label))
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
    pub fn split_primary(self, id: impl Into<ElementId>, label: &str, color: u32) -> Stateful<Div> {
        self.button(id, label.to_owned(), color)
            .rounded_r(px(0.))
            .border_r_0()
    }
    pub fn split_toggle(self, id: impl Into<ElementId>, color: u32) -> Stateful<Div> {
        self.control(id, color)
            .rounded_l(px(0.))
            .w(px(self.metrics.control_height))
            .px(px(0.))
            .child(Icon::Expand.view(color, self.metrics.icon))
    }
    pub fn menu_option(
        self,
        id: impl Into<ElementId>,
        label: String,
        focused: bool,
    ) -> Stateful<Div> {
        self.control(id, self.colors.text)
            .w_full()
            .justify_start()
            .bg(rgb(if focused {
                self.colors.hover
            } else {
                self.colors.panel
            }))
            .border_color(rgb(if focused {
                self.colors.focus
            } else {
                self.colors.panel
            }))
            .child(div().min_w_0().truncate().child(label))
    }
    pub fn field_select(
        self,
        id: impl Into<ElementId>,
        label: String,
        focused: bool,
    ) -> Stateful<Div> {
        self.control(id, self.colors.text)
            .w_full()
            .justify_between()
            .gap(px(self.metrics.space[1]))
            .when(focused, |element| {
                element.border_color(rgb(self.colors.focus))
            })
            .child(div().flex_1().min_w_0().truncate().child(label))
            .child(Icon::Expand.view(self.colors.muted, self.metrics.icon))
    }
    pub fn unavailable_menu_option(self, id: impl Into<ElementId>, label: String) -> Stateful<Div> {
        self.control_base(id, self.colors.muted)
            .w_full()
            .cursor_default()
            .bg(rgb(self.colors.panel))
            .border_color(rgb(self.colors.panel))
            .justify_between()
            .gap(px(self.metrics.space[1]))
            .child(div().min_w_0().truncate().child(label))
            .child(self.caption("Unavailable").flex_shrink_0())
    }
}

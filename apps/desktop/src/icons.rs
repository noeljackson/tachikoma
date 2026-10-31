use anyhow::{Context, Result};
use gpui::{AssetRegistry, Svg, prelude::*, px, rgb, svg};
use std::borrow::Cow;

#[derive(Clone, Copy)]
pub enum Icon {
    Inbox,
    History,
    Layers,
    Shield,
    Boxes,
    File,
    Assessment,
    Chat,
    Evidence,
    Check,
    Close,
    Clock,
    Chevron,
    Sun,
    Moon,
    System,
    Settings,
    Help,
    Search,
    Connected,
    Ready,
    Failure,
    Filter,
    Workflow,
    Server,
    Expand,
}
impl Icon {
    pub const ALL: [Self; 26] = [
        Self::Inbox,
        Self::History,
        Self::Layers,
        Self::Shield,
        Self::Boxes,
        Self::File,
        Self::Assessment,
        Self::Chat,
        Self::Evidence,
        Self::Check,
        Self::Close,
        Self::Clock,
        Self::Chevron,
        Self::Sun,
        Self::Moon,
        Self::System,
        Self::Settings,
        Self::Help,
        Self::Search,
        Self::Connected,
        Self::Ready,
        Self::Failure,
        Self::Filter,
        Self::Workflow,
        Self::Server,
        Self::Expand,
    ];
    pub fn path(self) -> &'static str {
        match self {
            Self::Inbox => "icons/inbox.svg",
            Self::History => "icons/clock-arrow-left.svg",
            Self::Layers => "icons/layers.svg",
            Self::Shield => "icons/shield-check.svg",
            Self::Boxes => "icons/boxes.svg",
            Self::File => "icons/file-text.svg",
            Self::Assessment => "icons/sparkles.svg",
            Self::Chat => "icons/message-circle.svg",
            Self::Evidence => "icons/braces.svg",
            Self::Check => "icons/check.svg",
            Self::Close => "icons/x.svg",
            Self::Clock => "icons/clock.svg",
            Self::Chevron => "icons/chevron-right.svg",
            Self::Sun => "icons/sun.svg",
            Self::Moon => "icons/moon.svg",
            Self::System => "icons/monitor.svg",
            Self::Settings => "icons/settings-2.svg",
            Self::Help => "icons/circle-question-mark.svg",
            Self::Search => "icons/search.svg",
            Self::Connected => "icons/circle-dot.svg",
            Self::Ready => "icons/circle-check.svg",
            Self::Failure => "icons/circle-x.svg",
            Self::Filter => "icons/funnel.svg",
            Self::Workflow => "icons/workflow.svg",
            Self::Server => "icons/server.svg",
            Self::Expand => "icons/chevron-down.svg",
        }
    }
    pub fn view(self, color: u32, size: f32) -> Svg {
        svg()
            .path(self.path())
            .size(px(size))
            .text_color(rgb(color))
            .flex_shrink_0()
    }
}
pub fn assets() -> Result<AssetRegistry> {
    let mut assets = AssetRegistry::default();
    debug_assert_eq!(Icon::ALL.len(), 26);
    assets
        .insert(
            "icons/inbox.svg",
            Cow::Borrowed(include_bytes!("../assets/icons/inbox.svg").as_slice()),
        )
        .context("register icon")?;
    assets
        .insert(
            "icons/clock-arrow-left.svg",
            Cow::Borrowed(include_bytes!("../assets/icons/clock-arrow-left.svg").as_slice()),
        )
        .context("register icon")?;
    assets
        .insert(
            "icons/layers.svg",
            Cow::Borrowed(include_bytes!("../assets/icons/layers.svg").as_slice()),
        )
        .context("register icon")?;
    assets
        .insert(
            "icons/shield-check.svg",
            Cow::Borrowed(include_bytes!("../assets/icons/shield-check.svg").as_slice()),
        )
        .context("register icon")?;
    assets
        .insert(
            "icons/boxes.svg",
            Cow::Borrowed(include_bytes!("../assets/icons/boxes.svg").as_slice()),
        )
        .context("register icon")?;
    assets
        .insert(
            "icons/file-text.svg",
            Cow::Borrowed(include_bytes!("../assets/icons/file-text.svg").as_slice()),
        )
        .context("register icon")?;
    assets
        .insert(
            "icons/sparkles.svg",
            Cow::Borrowed(include_bytes!("../assets/icons/sparkles.svg").as_slice()),
        )
        .context("register icon")?;
    assets
        .insert(
            "icons/message-circle.svg",
            Cow::Borrowed(include_bytes!("../assets/icons/message-circle.svg").as_slice()),
        )
        .context("register icon")?;
    assets
        .insert(
            "icons/braces.svg",
            Cow::Borrowed(include_bytes!("../assets/icons/braces.svg").as_slice()),
        )
        .context("register icon")?;
    assets
        .insert(
            "icons/check.svg",
            Cow::Borrowed(include_bytes!("../assets/icons/check.svg").as_slice()),
        )
        .context("register icon")?;
    assets
        .insert(
            "icons/x.svg",
            Cow::Borrowed(include_bytes!("../assets/icons/x.svg").as_slice()),
        )
        .context("register icon")?;
    assets
        .insert(
            "icons/clock.svg",
            Cow::Borrowed(include_bytes!("../assets/icons/clock.svg").as_slice()),
        )
        .context("register icon")?;
    assets
        .insert(
            "icons/chevron-right.svg",
            Cow::Borrowed(include_bytes!("../assets/icons/chevron-right.svg").as_slice()),
        )
        .context("register icon")?;
    assets
        .insert(
            "icons/sun.svg",
            Cow::Borrowed(include_bytes!("../assets/icons/sun.svg").as_slice()),
        )
        .context("register icon")?;
    assets
        .insert(
            "icons/moon.svg",
            Cow::Borrowed(include_bytes!("../assets/icons/moon.svg").as_slice()),
        )
        .context("register icon")?;
    assets
        .insert(
            "icons/monitor.svg",
            Cow::Borrowed(include_bytes!("../assets/icons/monitor.svg").as_slice()),
        )
        .context("register icon")?;
    assets
        .insert(
            "icons/settings-2.svg",
            Cow::Borrowed(include_bytes!("../assets/icons/settings-2.svg").as_slice()),
        )
        .context("register icon")?;
    assets
        .insert(
            "icons/circle-question-mark.svg",
            Cow::Borrowed(include_bytes!("../assets/icons/circle-question-mark.svg").as_slice()),
        )
        .context("register icon")?;
    assets
        .insert(
            "icons/search.svg",
            Cow::Borrowed(include_bytes!("../assets/icons/search.svg").as_slice()),
        )
        .context("register icon")?;
    assets
        .insert(
            "icons/circle-dot.svg",
            Cow::Borrowed(include_bytes!("../assets/icons/circle-dot.svg").as_slice()),
        )
        .context("register icon")?;
    assets
        .insert(
            "icons/circle-check.svg",
            Cow::Borrowed(include_bytes!("../assets/icons/circle-check.svg").as_slice()),
        )
        .context("register icon")?;
    assets
        .insert(
            "icons/circle-x.svg",
            Cow::Borrowed(include_bytes!("../assets/icons/circle-x.svg").as_slice()),
        )
        .context("register icon")?;
    assets
        .insert(
            "icons/funnel.svg",
            Cow::Borrowed(include_bytes!("../assets/icons/funnel.svg").as_slice()),
        )
        .context("register icon")?;
    assets
        .insert(
            "icons/workflow.svg",
            Cow::Borrowed(include_bytes!("../assets/icons/workflow.svg").as_slice()),
        )
        .context("register icon")?;
    assets
        .insert(
            "icons/server.svg",
            Cow::Borrowed(include_bytes!("../assets/icons/server.svg").as_slice()),
        )
        .context("register icon")?;
    assets
        .insert(
            "icons/chevron-down.svg",
            Cow::Borrowed(include_bytes!("../assets/icons/chevron-down.svg").as_slice()),
        )
        .context("register icon")?;
    assets
        .insert(
            "licenses/lucide-LICENSE",
            Cow::Borrowed(include_bytes!("../assets/licenses/lucide-LICENSE").as_slice()),
        )
        .context("register design license")?;
    assets
        .insert(
            "licenses/radix-colors-LICENSE",
            Cow::Borrowed(include_bytes!("../assets/licenses/radix-colors-LICENSE").as_slice()),
        )
        .context("register design license")?;
    Ok(assets)
}

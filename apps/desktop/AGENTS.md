# Native UI guidelines

Read `docs/desktop-design.md` before changing layout or components.

- Treat this as a decision system across services. Keep one chronological list;
  Pending and History are independent filters, not separate navigation pages.
- Keep settings at the bottom of the sidebar. Appearance and sources/services
  belong there. Avoid adding a global toolbar or app-name banner.
- Compose controls from `components.rs`. Every button has a fixed height,
  horizontal flex layout and `items_center`; icons and text share a vertical
  center. Set one text line height and avoid vertical padding that changes the
  intended hit area. Use the smaller pill component for list filters.
- Use semantic colors and metrics from `design.rs`. Avoid local hex colors or
  arbitrary dimensions. Keep both themes readable; pair status colors with text.
- Truncate compact row labels inside a constrained flex child. Keep full subject
  facts available in the inspector. Long labels must not push counts or times
  into another pane. Preserve investigation drafts and exact approval bindings.
- Resolve duplicate request snapshots by revision before filtering states. A
  settled request must not reappear as actionable pending data.
- Run runtime tests in Docker, never against live sockets or credentials. Native
  smoke checks must find a visible window, wait for its first rendered frame and
  verify real keyboard actions plus saved appearance in light and dark modes.
- Inspect the actual layout after component changes. Check icon/text alignment,
  long labels and the decision/settings views. Run `make check` before delivery.

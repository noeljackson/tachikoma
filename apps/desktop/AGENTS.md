# Native UI guidelines

Read `docs/desktop-design.md` before changing layout or components.

- Treat this as a decision system across services. Keep one chronological list;
  Pending and History are independent filters, not separate navigation pages.
- Keep the layout to a decision list and inspector, with no sidebar. Queues are
  a multi-select dropdown in the list header, with Settings beside it. Appearance
  and sources/services belong in Settings. Avoid a global toolbar or name banner.
- Queue filters must allow multiple selections and explicitly distinguish all
  from none. Keep the dropdown open while toggling choices; support keyboard
  navigation, Escape/Done and outside-click dismissal.
- Compose controls from `components.rs`. Every button has a fixed height,
  horizontal flex layout and `items_center`; icons and text share a vertical
  center. Set one text line height and avoid vertical padding that changes the
  intended hit area. Use the smaller pill component for list filters.
- Use semantic colors and metrics from `design.rs`. Avoid local hex colors or
  arbitrary dimensions. Keep both themes readable; pair status colors with text.
- Truncate compact row labels inside a constrained flex child. Keep full subject
  facts available in the inspector. Long labels must not push counts or times
  into another pane. Preserve investigation drafts and exact approval bindings.
- Network actions use split buttons whose primary action always stays once.
  Their arrows open a centered Source/Destination form for selected scopes;
  legacy requests retain their offered scopes. Keep captured values visible and
  show a complete permission sentence. The form requires explicit selections,
  with focused Return/Escape handling and no background keyboard navigation.
  Opening a modal with a held Return must never also confirm it. Cancel stale
  bindings and keep the next default narrow after cancelling or deciding.
- Resolve duplicate request snapshots by revision before filtering states. A
  settled request must not reappear as actionable pending data.
- Run runtime tests in Docker, never against live sockets or credentials. Native
  smoke checks must find a visible window, wait for its first rendered frame and
  verify real keyboard actions plus saved appearance in light and dark modes.
- Inspect the actual layout after component changes. Check icon/text alignment,
  long labels and the decision/settings views. Run `make check` before delivery.

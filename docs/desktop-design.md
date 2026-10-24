# Desktop design

The desktop presents decisions across queues and services. The operating system
window title and launcher use `tachikoma`; the interface has no brand banner.
Service-specific subject details and permission controls appear in the inspector.
One list combines pending and historical decisions with independent filter pills.
The sidebar ends with Settings, which contains appearance and source/service status.
There is no global navigation or appearance toolbar.

## Source and tokens

[Radix Colors](https://www.radix-ui.com/colors/docs/palette-composition/understanding-the-scale)
provides the light/dark slate, blue, green, red and amber scales.
`apps/desktop/src/design.rs` maps them into semantic roles: canvas, panel, sidebar,
control, hover, selection, border, text, muted text, focus and status colors.
Views use these roles rather than hard-coded palette values. Color never carries
a decision's meaning alone: labels and icons accompany status and actions.

The shared metrics define a four-pixel spacing grid, typography, control height,
radii, icon size and pane widths. `components.rs` owns captions, buttons,
segmented choices, icon buttons and inspector sections. `view.rs` composes these
components with queue and review data. This keeps appearance and geometry changes
out of application logic. Every button uses a fixed-height flex row with centered
content and a shared text line height; icon/text groups align on the same vertical
center. Filter pills use the same component foundation at a smaller height.

Appearance has three states: System, Light and Dark. System resolves GPUI's
current window appearance and observes changes; an explicit selection remains
fixed. The preference is saved atomically with mode 0600, outside the repository.
A missing file defaults to System. A malformed file falls back to System.

## Icons and assets

[Lucide](https://lucide.dev/guide/) supplies a consistent SVG icon family for
queues, navigation, evidence, policy, investigations, status and appearance.
Icons use the same semantic foreground colors as their text labels. Buttons
retain visible text. Assets are embedded through GPUI's asset registry and need
no network access or checkout at runtime.

`apps/desktop/assets/sources.json` records the exact source revisions. Both asset
licenses and Lucide's inherited attribution are kept beside the files and embedded
in the executable. Updating a source requires reviewing its license and pin.

## Validation

Unit tests check system versus explicit appearance, preference persistence and
normal-text contrast of at least 4.5:1 for the main semantic text pairs in both
modes. Native Docker smoke tests launch light and dark windows, exercise keyboard
navigation and appearance switching, then check the saved preference and mode.
They use offline fixtures without live service sockets or model credentials.

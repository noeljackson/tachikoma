# Desktop design

The desktop presents decisions across queues and services. The operating system
window title and launcher use `tachikoma`; the interface has no brand banner.
Service-specific subject details and permission controls appear in the inspector.
One list combines pending and historical decisions with independent filter pills.
The list header contains a multi-select queue dropdown and a compact Settings
button. Settings contains appearance and source/service status. There is no
sidebar or global navigation/appearance toolbar. The dropdown stays open while
selecting queues, supports all/none and closes with Done, Escape or an outside
click. Explicit selections exclude newly added queues until selected; All queues
includes them automatically.

Network permissions use two split buttons: Deny once and Allow once. Their
attached dropdowns list only the offered scopes, ordered from this destination
to any destination and from once to forever. Selecting a scope opens a centered
modal rather than expanding the inspector. It keeps the captured request/action
binding, owns keyboard focus, blocks background shortcuts and restores navigation
on dismissal. Return confirms and Escape cancels; a held Return used to choose
from the menu cannot also accept the modal. Primary buttons never remember a
broader scope. Disconnection, settlement, expiry or changed bindings invalidate
the modal. The same dialog handles other services' offered actions.

## Source and tokens

[Radix Colors](https://www.radix-ui.com/colors/docs/palette-composition/understanding-the-scale)
provides the light/dark slate, blue, green, red and amber scales.
`apps/desktop/src/design.rs` maps them into semantic roles: canvas, panel,
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

The desktop-test stage also runs the explicit `permission_layout` render test.
It captures the actual views with a software GPU offscreen, checks that the
scope menu sits below its button and the dialog centers in the window, and
saves light/dark PNGs under `/out/desktop-layout`. The native keyboard contract
uses a real approval core on private Docker sockets to check exact action scopes,
Enter/Escape, held Return, focus isolation, expiry and competing decisions.

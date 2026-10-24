# Native desktop review

The local interface is `tachikoma-gui`, built with GPUI CE. It connects to the
same private approval API as the TUI and Slack, so decisions and investigations
are shared across clients. It imports only the public protocol and client;
there is no database access, firewall writer or model credential in the GUI.

```sh
make install-desktop
tachikoma-gui            # live local queues; no sign-in form
tachikoma-gui --demo     # offline read-only example
```

Open **Tachikoma** from the desktop launcher after installation. Start or update
the approval daemon first (`make install`, then `systemctl --user restart
tachikoma.service`). The daemon creates a mode-0600 `approval/desktop.token`
beside its database. The default client reads it from the XDG state directory;
`--socket` and `--credential` support explicitly configured instances. The token
is a reserved local reviewer, cannot enroll principals, edit queues or claim
worker jobs, and is never put in arguments, logs or the environment.

The left pane filters queues; the middle pane shows Inbox or History. The
selected review has Overview, Evidence, Rules and Investigate tabs. Health shows
producer heartbeats and optional sensor readers with their coverage and stale
status. Missing observations do not establish safety. Rule previews are shadow
explanations and cannot approve requests.

Use Left/Right to change the active pane, Up/Down to navigate or scroll details,
Ctrl+F to search, Ctrl+1/2/3 for Inbox/History/Health and Ctrl+R to refresh. Click
Investigate to ask a question, optionally override the configured model, and send
with Ctrl+Enter. Text inputs use GPUI's editable-text elements with selection,
clipboard, undo and IME support. Drafts are kept separately per selected request
for the life of the window. Investigation messages have stable retry keys.

An action first opens its exact effect and scope for confirmation. Confirmation
is bound to the selected request, revision, immutable subject/evidence and action.
Expired, changed, disconnected, demo and busy reviews cannot be confirmed. The
API independently rejects stale revisions and conflicting decisions. Recording
a decision and confirming producer enforcement are displayed separately.

The daemon's loopback HTTP listener now serves health and a native-launch hint.
The old browser UI is available only with `tachikomad --legacy-web`; this flag is
used by compatibility browser tests. It is not enabled by the installed unit.

## GPUI fork reconciliation

The dependency is pinned to Noel's fork at
`26813dad6cbc4a5e1a2f56ef46e27d0f16ea30ff`, merging upstream
`94791b8882f88c126b3089759cb7829a9c1de1a2`. The merge keeps the fork's earlier
history and adopts upstream's complete workspace tree. The old fork-only
`Application::on_become_active` extension and graphics/font overrides are not
ported; consumers of that hook must migrate to upstream's window activation
observers. This desktop does not depend on the removed extension. No changes or
messages were submitted to the upstream project.

GPUI core, platform and editable-text crates all use the same pinned fork
revision. Linux needs a graphical session with Vulkan support and development
packages equivalent to `libxkbcommon`, `libx11`, `libxcb`, `wayland`, `libglvnd`,
`glib2` and `fontconfig`. Wayland and X11 are supported. A desktop launcher is
installed without starting the GUI at boot; the approval daemon and existing
workers continue running independently of any window.

## Validation and dependency maintenance

`make check` includes Rust tests and API contracts in Docker, browser compatibility
checks, a native window smoke test using Xvfb/software Vulkan, boundary checks,
formatting, clippy and the pinned strict source supply-chain scanner. Runtime
fixtures have no live approval socket, providers, Slack credentials or firewall.

Four GPUI transitive dependencies have maintenance notices with no patched
versions: [`paste`](https://rustsec.org/advisories/RUSTSEC-2024-0436.html),
[`zvariant`](https://rustsec.org/advisories/RUSTSEC-2026-0370.html),
[`zbus_names`](https://rustsec.org/advisories/RUSTSEC-2026-0371.html) and
[`zvariant_derive`](https://rustsec.org/advisories/RUSTSEC-2026-0372.html).
These advisories disclose discontinued standalone maintenance, not an exploit.
The D-Bus types/macros moved into zbus 6; GPUI's accessibility, portal and keyring
libraries still depend on zbus 5. `paste` is a proc-macro used in the shader/image
build paths, including Linux; it is not merely an unused macOS lock entry.

The source policy records each package/advisory individually, preserves all
other advisory and IOC checks, and expires on 2026-11-24. Revisit the GPUI/zbus
migration before then. The scanner pin was advanced to
`97df5883cf9a3de3d2c81693374f958badc02fc3` to support explicit reviewed exceptions
and its offline coverage diagnostics; all advisory severities remain enabled.

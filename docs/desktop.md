# tachikoma desktop

The local interface is `tachikoma-gui`, built with GPUI CE. It connects to the
same private approval API as the TUI and Slack, so decisions and investigations
are shared across clients. It imports only the public protocol and client;
there is no database access, firewall writer or model credential in the GUI.

```sh
make install-desktop
tachikoma-gui         # live local queues; no sign-in form
tachikoma-gui --demo  # offline read-only example
```

Open **tachikoma** from the desktop launcher after installation. Start or update
the approval daemon first (`make install`, then `systemctl --user restart
tachikoma.service`). The daemon creates a mode-0600 `approval/desktop.token`
beside its database. The default client reads it from the XDG state directory;
`--socket` and `--credential` support explicitly configured instances. The token
is a reserved local reviewer, cannot enroll principals, edit queues or claim
worker jobs, and is never put in arguments, logs or the environment.

The left pane selects queues across services. The middle pane has one
chronological list of decisions and subjects. Compact Pending and History pills
independently include or hide each state; both start enabled. A request keeps its
identity when it settles, without duplicate rows. The inspector has Overview,
Evidence, Policy and Investigate tabs.

Settings, at the bottom of the sidebar, contains appearance, producer heartbeats
and optional sensor readers with their coverage and stale status. Missing
observations do not establish safety. Policy previews explain recommendations
and cannot approve requests.

Use Left/Right to change the active pane, Up/Down to navigate or scroll details,
Ctrl+F to search, Ctrl+1/2 to toggle Pending/History, Ctrl+3 for Settings and Ctrl+R
to refresh. Search returns from Settings to the list. Escape closes Settings or
returns from an editor to navigation. Click Investigate to ask a question,
optionally override the configured model, and send with Ctrl+Enter. Text inputs
support selection, clipboard, undo and IME. Drafts are kept separately per selected
request for the life of the window. Investigation messages have stable retry keys.

Choose **System**, **Light** or **Dark** in Settings, or cycle them with
Ctrl+Shift+T. System follows desktop appearance changes while the window is open.
The choice is saved to `$XDG_STATE_HOME/tachikoma/desktop/appearance.json`
(default `~/.local/state/tachikoma/desktop/appearance.json`). `--appearance`
can override it for a launch. Changing it in the interface saves the new choice.
The interface uses shared [design tokens and components](desktop-design.md).

An action first opens its exact effect and scope for confirmation. Confirmation
is bound to the selected request, revision, immutable subject/evidence and action.
Expired, changed, disconnected, demo and busy reviews cannot be confirmed. The
API independently rejects stale revisions and conflicting decisions. Recording
a decision and confirming producer enforcement are displayed separately.

For network reviews, choose **This IP and port** or **Any destination**, then
**Once**, **5 minutes**, **Until restart** or **Forever**, and click **Allow** or
**Deny**. Once applies only to the current request. Reusable permissions always
bind the captured absolute executable path and UID; an any-destination permission
covers all IPv4/IPv6 addresses, ports and TCP/UDP for that path and user. The
confirmation spells out those bindings. Path matching also covers future programs
installed at that path, including all scripts using a selected interpreter.

Until restart means the OpenSnitch daemon restarting. Forever uses its persisted
rules and survives daemon/broker/UI restarts. The original request must still be
unexpired when approved. Classifiers and automatic policy cannot issue broad or
permanent rules; failures and timeouts still deny once. Unattributed requests
only offer once actions. The transport records a decision separately from
producer-reported enforcement; a returned rule is not a kernel acknowledgement.

Permanent rules are named `snitchgate-allow-<decision UUID>` or
`snitchgate-deny-<decision UUID>`. The producer outcome shows the rule name.
They are saved by the daemon in its configured rules directory; on Noelarchy,
UI-created rules are live policy until reviewed and promoted into the curated
policy repository. Disable or remove the exact generated rule through the normal
OpenSnitch policy maintenance workflow to revoke it. Do not replace a broad grant
with an unrelated permanent deny.

The daemon's loopback HTTP listener now serves health and a native-launch hint.
The old browser UI is available only with `tachikomad --legacy-web`; this flag is
used by compatibility browser tests. It is not enabled by the installed unit.

## GPUI dependency

GPUI core, platform and editable-text crates are pinned directly to
[upstream GPUI CE](https://github.com/gpui-ce/gpui-ce) at commit
`94791b8882f88c126b3089759cb7829a9c1de1a2`. This is the same source tree as
the previously tested fork reconciliation; no fork-specific API is required.
All three crates share this exact revision so their APIs stay compatible.
Upgrades are explicit rather than following a moving branch.

Linux needs a graphical session with Vulkan support and development
packages equivalent to `libxkbcommon`, `libx11`, `libxcb`, `wayland`, `libglvnd`,
`glib2` and `fontconfig`. Wayland and X11 are enabled explicitly in both GPUI core and platform crates.
A desktop launcher is
installed without starting the GUI at boot; the approval daemon and existing
workers continue running independently of any window.

## Validation and dependency maintenance

`make check` includes Rust tests and API contracts in Docker, browser compatibility
checks, light and dark native window smoke tests using Xvfb/software Vulkan, boundary checks,
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

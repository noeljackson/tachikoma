# Tachikoma

Tachikoma coordinates approval queues, Jev/LLM assessment, human review and
an audit trail. Independent services submit requests and validate scoped
decisions before acting. The daemon has no network-rule writer, Kubernetes
client, registry proxy, model credentials or command tools.

This Rust workspace includes:

| Component | Responsibility |
| --- | --- |
| `apps/daemon` | Generic durable approval API (`tachikomad`) |
| `apps/tui`, `apps/web` | Ratatui and local web review |
| `workers/assessment` | Bounded Jev classification and Ollama explanations/investigations |
| `workers/review-slack` | Socket Mode delivery and authorized replies |
| `services/snitchgate` | OpenSnitch transport, local deadline and temporary rules |
| `services/kubernetesgate` | Read-only command observations; no cluster access or execution |
| `crates/approval-protocol`, `crates/approval-client` | Optional public wire types and Rust client |

**Registrygate stays in its own repository and is implemented last.** It will
run independently with local automatic policy, periodic advisory refresh and
an optional Tachikoma workflow.

The [architecture plan](docs/queue-control-plane.md) explains boundaries and
migration order. The [adapter guide](docs/adapter-api.md) documents the actual
HTTP JSON contract and a standalone Python producer. The
[operations guide](docs/approval-operations.md) covers enrollment, workers,
Snitchgate integration and rollback.

## Try the approval platform

Rust 1.96+, Cargo and `protoc` are required for workspace builds. Runtime tests
run in Docker. The default build installs no services and contacts no providers.

```sh
make check             # strict supply-chain gate, Docker runtime/browser tests, fmt, clippy
make build-platform    # build all independently runnable services
make service-enable    # install/start the generic local approval daemon
```

The private administrator credential is generated at first startup in
`~/.local/state/tachikoma/approval/admin.token` with mode `0600`. Enrollment
writes scoped credentials to explicit private files; credentials are never
printed by the CLI. Do not put them in this repository.

```sh
tachikoma approval health
tachikoma tui
```

The TUI has Inbox, History and Health views (`1/2/3`). **Left/Right** or
`Tab`/`Shift+Tab` moves focus between Queues, Requests and Details; Up/Down or
`j/k` navigates the focused pane. Wide terminals show all three panes, while
narrow terminals show the focused request or detail view beside the queues.

`Enter` opens details, then the action menu. `y`/`n` allow or deny **once**
(`a`/`d` remain aliases); `s` opens the other permit scopes with their exact
values before applying one. `c` or `i` investigates; `/` filters; `?` or `F1`
shows help. Highlighted borders and the footer show the current focus/controls.

In chat, arrows/Home/End edit the draft, `Enter` sends, and `Alt+Enter` inserts
a newline. `Tab` switches transcript/composer; `F2` opens the model picker
(`m` also works in the transcript). `Esc` leaves the composer, then returns to
review. `Ctrl-C` cancels a turn and `Ctrl-U` clears the draft. Letters and pasted
text in editors never trigger review actions. Expired or settled selections are
not automatically replaced with another actionable request.

Use `make install-cli` to update the terminal client separately. Restart an
already open TUI to load the new keyboard controls.

Open <http://127.0.0.1:7447/approval/> directly; no web sign-in or credential entry
is required on this local workstation. The daemon keeps its own limited reviewer
capability server-side. Same-origin submissions and CSRF protection remain
enabled. History shows previous decisions; investigation replies refresh
automatically while a turn is running.

To try without installing binaries:

```sh
docker compose up --build --detach
# Open http://127.0.0.1:17447/approval/
docker compose exec tachikoma tachikoma --rpc-socket /tmp/tachikoma.sock \
  approval --credential /var/lib/tachikoma/approval/admin.token tui
```

Compose keeps its own state volume. `docker compose down` preserves it;
`--volumes` deliberately deletes that container state. `make check` uses a separate Compose project and deletes only its test volume.

## Authority and failure behavior

Manual mode is the default. Jev confidence and LLM text are advisory. Optional
policy mode uses administrator-defined exact fact matches, not model text.
A decision binds the producer, queue, schema, immutable subject/evidence hashes,
request nonce, revision, manifest revision, action scope and original deadline.
Shadow and observation decisions are never authoritative.

Producers keep their own deadline and validate every returned scope. Snitchgate
rejects broader durations and denies once on timeout even if Tachikoma is down.
Its receipt honestly reports the broker result as `unknown` because OpenSnitch
provides no kernel acknowledgement. Decision and enforcement outcome are separate.
Workers use durable jobs, bounded leases and fenced completions. Investigation
turns can continue after a request expires but cannot change its decision.

The new daemon never loads prototype configuration or AI environment files.
The [legacy prototype](docs/legacy-prototype.md) remains a separate compatibility
binary for existing SQLite/JSONL history. Import proposal metadata with:

```sh
tachikoma approval import-legacy /path/to/original/tachikoma.sqlite3
```

Imports are idempotent, read the source database with SQLite read-only flags,
archive metadata as non-authoritative observations, and leave original evidence,
analysis and transcript files in place. They do not revive historical approvals.

## Status

The platform and service adapters are implemented and tested with isolated
fixtures. Noelarchy now deploys the workstation network path through Tachikoma,
with separate system workers and authenticated health-based GUI fallback. See
`/home/noel/noelarchy/docs/snitchgate-live.md` for that machine’s deployment and
rollback workflow. No registry implementation or live Kubernetes executor is included.

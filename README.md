# Tachikoma

Tachikoma coordinates approval queues, Jev/LLM assessment, human review and
an audit trail. Independent services submit requests and validate scoped
decisions before acting. The daemon has no network-rule writer, Kubernetes
client, registry proxy, model credentials or command tools.

This Rust workspace includes:

| Component | Responsibility |
| --- | --- |
| `apps/daemon` | Generic durable approval API (`tachikomad`) |
| `apps/tui`, `apps/web` | Ratatui and authenticated local web review |
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
make check             # strict supply-chain gate, Docker runtime tests, fmt, clippy
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

The TUI has Inbox, History and Health tabs. `Tab` changes queue; `/` searches;
`Enter` shows exact actions; `a`/`d` allow or deny; `t` changes the permit scope.
Use `i` to investigate, `F2` to choose an enabled model, `Enter` to send,
`Ctrl-C` to cancel a turn and `Esc` to return. Decisions remain explicit actions.
When the selected request settles elsewhere, select another request before
acting again. Disconnected review controls are disabled.

Open <http://127.0.0.1:7447/approval/> and sign in with an enrolled reviewer
credential. The web UI requires a loopback Host, same-origin submissions and
CSRF tokens. It supports scoped actions and investigation chats without scripts.

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
fixtures. The existing workstation Snitchgate installation is unchanged until
an explicit cutover using the operations guide. No registry implementation or
live Kubernetes executor is included.

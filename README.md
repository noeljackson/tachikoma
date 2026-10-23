# Tachikoma

The proposed next direction is a **monorepo of independent services**, with
Tachikoma coordinating queues, policy, AI assessment, human review and audit.
Snitchgate, Registrygate and Kubernetesgate retain their own domain logic,
credentials, deadlines and enforcement. Services in external repositories use
the same versioned adapter API as services in the monorepo.

The [full architecture and implementation plan](docs/queue-control-plane.md)
covers service and crate boundaries, adapter registration, scoped decisions,
durable workers, TUI and Slack parity, external integrations, migration phases
and acceptance tests. This direction is not implemented by the prototype
described below; the existing repositories and live services remain in place.

Tachikoma is a local-first Rust control plane for turning security and
operational observations into typed, reviewable proposals. It starts with
OpenSnitch and Kubernetes command observations, but adapters do not execute
anything themselves.

It has three local interfaces:

- a loopback-only web queue at `127.0.0.1:7447`;
- Connect/gRPC on a user-owned Unix socket, mode `0600`;
- the `tachikoma` CLI and native terminal UI.

## Safety model

```text
local signal
    │
    ▼
typed proposal ──► awaiting_review ──► approved or rejected
    │
    └── exact low-risk automatic policy ──► queued
                                                │
                                                └── no executor in this release
```

- OpenSnitch history is read-only input. Tachikoma never writes OpenSnitch
  rules or creates automatic allow rules.
- Kubernetes observations never load kubeconfig, contact a cluster, or run
  `kubectl`.
- An approved proposal is recorded but does not execute a system change.
- An automatic policy must have a non-empty JSON scope, an exact evidence
  match, and a `low` risk ceiling. It can only queue a proposal; it grants no
  shell, network, or cluster access.

Tachikoma contains no React, Node, Bun, or browser package-manager toolchain.

## Requirements

Native builds require Rust/Cargo 1.96 or newer and `protoc`. Docker Compose is
optional, but is the easiest way to try the application without installing the
binaries on the host.

## Docker Compose quickstart

The Compose deployment is durable: its SQLite state lives in the named
`tachikoma-state` volume. The web UI is exposed only on the host loopback
address, and the Connect socket remains inside the container.

```sh
docker compose up --build --detach
curl --fail http://127.0.0.1:17447/health
```

Open <http://127.0.0.1:17447> for the web queue. Use the CLI through the same
container and private Unix socket:

```sh
docker compose exec --no-TTY tachikoma \
  tachikoma --rpc-socket /tmp/tachikoma.sock status

docker compose exec --no-TTY tachikoma \
  tachikoma --rpc-socket /tmp/tachikoma.sock tui
```

The terminal UI opens proposals as durable AI chats. Use `Enter` to investigate,
`m` to switch model, `p` to return to proposals, `t` to toggle tool details,
and `r` to queue a remediation draft for human review. Press `q` to close it.
Stop the service without deleting state with `docker compose down`. To deliberately
erase all Compose state, use `docker compose down --volumes`.

## Native installation

For a systemd-managed workstation, `make service-enable` performs the initial
locked release build, installs both binaries under `~/.local/bin`, installs the
user unit, reloads the user manager, and starts the service:

```sh
make service-enable
```

Override `INSTALL_PREFIX` or `SYSTEMD_USER_DIR` when your user layout differs.
For a non-systemd foreground build, use `make build-release` and run the two
binaries from `target/release/`.

The daemon defaults to:

| Item | Default |
| --- | --- |
| SQLite state | `$XDG_STATE_HOME/tachikoma/tachikoma.sqlite3` |
| Connect/gRPC socket | `$XDG_RUNTIME_DIR/tachikoma/tachikoma.sock` |
| Web UI | `http://127.0.0.1:7447` |

When an XDG variable is unavailable, Tachikoma falls back to
`~/.local/state/tachikoma` for durable state.

### JSON configuration

The daemon reads an optional JSON configuration file from
`$XDG_CONFIG_HOME/tachikoma/config.json` (normally
`~/.config/tachikoma/config.json`). A complete example is in
[`contrib/tachikoma/config.example.json`](contrib/tachikoma/config.example.json).
Copy it to the config directory and replace the OpenSnitch path as needed:

```sh
mkdir -p "${XDG_CONFIG_HOME:-$HOME/.config}/tachikoma"
cp contrib/tachikoma/config.example.json \
  "${XDG_CONFIG_HOME:-$HOME/.config}/tachikoma/config.json"
```

The top-level `database`, `rpc_socket`, and `listen` values configure the
durable store, local API socket, and web UI. They are resolved at startup.
Changes to those values are reported in the daemon log and require a service
restart. The `opensnitch` section supports `enabled`, a read-only `history` database
path, `poll_seconds`, and the maximum `limit` of recent rows read per scan.

The optional `ai` section defines a default model, ordered `model_preference`,
custom compatible endpoints, named read-only tools, and prompt-driven review
profiles. Tachikoma ships a compiled catalog for OpenAI, Codex login, Anthropic
and Claude login, GLM/Z.AI (general, coding-plan, and Anthropic-compatible), and
Kimi (general OpenAI-compatible and coding-plan OpenAI/Anthropic-compatible).
Run `tachikoma models` to see which entries are authenticated.

`default_model: "auto"` selects the first authenticated model in
`model_preference`. A qualified reference such as
`zai-coding/glm-5.1` or `kimi-coding/kimi-for-coding` disables fallback.
Custom entries in `models` include `provider`, `id`, `name`, `api`, `baseUrl`,
and optional `api_key_env`; this is also how to add an arbitrary
Anthropic-compatible endpoint. API keys remain in the daemon environment and
are never stored in JSONL or SQLite.

The legacy `providers`, `provider_preference`, and unqualified review-provider
aliases still load, but the daemon logs a migration warning. Tachikoma does not
read Pi settings, model caches, or Pi auth files.

The daemon checks the JSON file approximately once per second. OpenSnitch and
AI settings are applied without a restart, including provider preferences,
review prompts, and concurrency limits. Changes to the database, RPC socket,
or listen address still require a restart. If a reload contains invalid JSON,
the previous valid settings are kept. Command-line flags take precedence over
corresponding JSON values.

OpenSnitch remains a deterministic, read-only history adapter. When AI is
enabled, a matching review profile can ask a model to select only its named
tools, inspect their bounded outputs, and return an advisory finding. The
model cannot invent shell commands or approve/reject the proposal.

### Foreground Linux operation

For non-systemd systems, run the daemon in the foreground under the local
supervisor convention of your choice:

```sh
tachikomad
```

In another terminal, use the same Unix-socket API:

```sh
tachikoma status
tachikoma queue
tachikoma tui
```

Approve or reject a reviewable proposal by ID from `tachikoma queue`:

```sh
tachikoma approve p-example
tachikoma reject p-example --reason "destination is not expected"
```

### systemd user service

The service lifecycle targets are:

```sh
make service-status
make service-restart
make service-logs
```

The unit uses the XDG defaults above, restarts on failure, runs without new
privileges, and creates private state/socket files through its restrictive
umask. View logs with:

```sh
journalctl --user --unit=tachikoma.service --follow
```

To update a clean checkout safely, use:

```sh
make update
```

It refuses a dirty worktree, fast-forwards from Git, runs the Docker-only
application gate and systemd validation, reinstalls the locked release
binaries/unit, then restarts the service only if it is already active. To
reinstall local source changes without pulling, use `make install` followed by
`make service-restart`.

## OpenSnitch example

OpenSnitch is opt-in. Configure it in `config.json`; on the common Linux
desktop setup, its UI history is at
`$HOME/.local/share/opensnitch/ui.sqlite3`:

```json
{
  "opensnitch": {
    "enabled": true,
    "history": "/home/your-user/.local/share/opensnitch/ui.sqlite3",
    "poll_seconds": 60,
    "limit": 100
  }
}
```

The equivalent one-off foreground invocation remains available:

```sh
tachikomad --opensnitch-history "$HOME/.local/share/opensnitch/ui.sqlite3"
```

Only denied connections create proposals. Their preview explicitly records
that no OpenSnitch rule has been changed.

## AI-assisted investigations

After enabling AI, automatic matching reviews run in the background.
Investigations are advisory and durable:

```sh
tachikoma queue
tachikoma analysis <proposal-id>
```

With `provider: "auto"`, Tachikoma reads the existing Codex token from
`~/.codex/auth.json` or the Claude Code token from
`~/.claude/.credentials.json`, then talks to the provider API directly. It does
not spawn either CLI. API keys are also supported, for example:

```sh
export OPENAI_API_KEY='...'
export ANTHROPIC_API_KEY='...'
```

Those variables must be present in the daemon's environment, not only in the
interactive shell that invokes the CLI. The installed systemd user service
optionally reads `%h/.config/tachikoma/ai.env`; create that file with mode
`0600` when using API keys there.

Run or repeat the automatic structured review through the local Unix-socket API:

```sh
tachikoma analyze <proposal-id>
tachikoma analyze <proposal-id> --force
```

Automatic analysis uses the configured prompt plus proposal evidence and must
finish through a typed `submit_review` tool containing `finding`, `priority`,
`confidence`, `rationale`, and `next_step`. The same result seeds the proposal's
primary chat investigation:

```sh
tachikoma investigate <proposal-id>
tachikoma chat <investigation-id> "What evidence would distinguish noise from compromise?"
tachikoma model <investigation-id> anthropic/claude-sonnet-4-6
```

Every turn, message, tool call/result, model change, usage report, prompt hash,
compaction, failure, and cancellation is appended to a versioned JSONL journal
under `$XDG_STATE_HOME/tachikoma/investigations`. SQLite indexes the journal and
its proposal relationship; JSONL is the canonical transcript. Model switching
is allowed only while idle and compacts automatically before switching to a
smaller context window.

The analysis and chat runners execute configured absolute programs directly under the
Tachikoma service user. It does not invoke a shell, inherit the environment,
or allow model-supplied command arguments. Tool output is bounded, redacted,
and stored for local auditability. The internal `draft_remediation` tool only
saves JSON; `tachikoma remediate <investigation-id>` turns that draft into a
linked proposal, never an executed command. If the provider is unavailable or
returns invalid JSON, the proposal remains in its prior state and the analysis
run is marked failed.

## Kubernetes command observations

Turn an already-observed or planned `kubectl` command into a proposal without
executing it:

```sh
tachikoma suggest-kubectl --context development --namespace default -- get pods
tachikoma suggest-kubectl --context production -- apply -f deployment.yaml
tachikoma queue
```

Read-style verbs such as `get`, `describe`, `logs`, `events`, and `diff` are
low risk. Other verbs become high-risk review proposals. In both cases,
Tachikoma only records a suggestion; it does not invoke Kubernetes.

## Scoped automation policy example

Policies are explicit durable intent, not broad permission. This policy matches
only low-risk Kubernetes read observations whose evidence has exactly the
named `development` context:

```sh
tachikoma policy upsert kubectl-development-read \
  --adapter kubernetes \
  --action review_kubectl_observation \
  --scope '{"context":"development"}' \
  --mode automatic \
  --risk-ceiling low

tachikoma suggest-kubectl --context development -- get pods
tachikoma queue
```

The matching proposal moves to `queued`; it is still inert because Tachikoma
ships no executor. Use `--mode review` to retain an explicit human decision
for matching proposals. Inspect all configured policies with:

```sh
tachikoma policy list
```

## Connect/gRPC contract

The generated Connect and gRPC services are local-only and listen on the Unix
socket rather than a TCP API port. The stable request/response contract lives
in [`proto/tachikoma/v1/tachikoma.proto`](proto/tachikoma/v1/tachikoma.proto).
Use the CLI for local operation; external clients should use that protobuf
contract and a Unix-socket transport.

`InvestigationService` exposes model discovery, open/get/send, idle model
switching, cancellation, remediation queueing, and `WatchInvestigation`, a
server stream that replays persisted events and then follows live events.

## Verification

Application runtime tests run only in Docker:

```sh
make test-docker
```

That target builds the test image, runs the Rust unit suite, starts Compose,
checks the web UI and health endpoint, calls the Unix-socket CLI, creates a
scoped Kubernetes policy and proposal, restarts the service to prove state
persists, and checks the TUI interface.

Run the security and repository gates with:

```sh
make supplychain
make systemd-validate
make check
```

`make supplychain` uses the pinned `noeljackson/supplychain` scanner and audits
the GitHub workflow. `make check` includes the Docker application tests plus
formatting and Clippy checks.

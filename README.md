# Tachikoma

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

The terminal UI is a queue snapshot; press `q` or Escape to close it. Stop the
service without deleting state with `docker compose down`. To deliberately
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

OpenSnitch is opt-in. On the common Linux desktop setup, its UI history is at
`$HOME/.local/share/opensnitch/ui.sqlite3`; pass a different path when your
installation stores it elsewhere.

Foreground:

```sh
tachikomad \
  --opensnitch-history "$HOME/.local/share/opensnitch/ui.sqlite3" \
  --poll-seconds 60
```

For the systemd user service, create a drop-in with
`systemctl --user edit tachikoma.service`, then enter:

```ini
[Service]
ExecStart=
ExecStart=/usr/bin/env tachikomad --opensnitch-history %h/.local/share/opensnitch/ui.sqlite3 --poll-seconds 60
```

Reload and restart the service:

```sh
systemctl --user daemon-reload
systemctl --user restart tachikoma.service
```

Only denied connections create proposals. Their preview explicitly records
that no OpenSnitch rule has been changed.

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

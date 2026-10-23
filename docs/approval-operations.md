# Local approval platform operations

The workstation network path is deployed through Noelarchy’s system workers,
scoped encrypted credentials and authenticated readiness guard. Its canonical
workflow is `/home/noel/noelarchy/docs/snitchgate-live.md` and
`make -C /home/noel/noelarchy snitchgate-services-status`. The generic templates
below support other local installations; they do not replace Noelarchy’s
system units. Registrygate remains last.

## Build and start

```sh
make check
make service-enable
make install-workers
systemctl --user status tachikoma.service
tachikoma approval health
```

The generic daemon uses the existing state-directory anchor but stores its own
schema in `~/.local/state/tachikoma/approval/approval.sqlite3`. It does not open
or migrate the original proposal database. Its Unix socket defaults to
`$XDG_RUNTIME_DIR/tachikoma/tachikoma.sock`, mode `0600`. The generated
administrator key is in the same private approval state directory. A second
daemon cannot replace an active instance's socket.

User unit templates live in `contrib/systemd`. They launch each process with a
small explicit environment. The core receives no provider or Slack credentials;
Jev and Slack keys are delivered only to the worker that needs them, using
systemd credentials. OpenBao fetching/encryption belongs to the workstation
configuration or an external credential helper, never project startup scripts.
The templates deliberately do not start optional workers during installation.

For a standalone service image, select its package and binary:

```sh
docker build --target service-runtime \
  --build-arg SERVICE=kubernetesgate --build-arg BINARY=kubernetesgate \
  --tag kubernetesgate:local .
docker run --rm kubernetesgate:local manifest
```

The same target supports `snitchgate`, `tachikoma-assessment-worker` and
`tachikoma-slack`. Supply credentials as private mounted files, a private Unix
socket and explicit arguments. No container needs the Docker socket, privileges
or host networking.

The loopback web UI at `http://127.0.0.1:7447/approval/` opens directly without
credential entry. The daemon creates a private, process-local `local-web` reviewer
capability at startup; it is never sent to the browser. That reviewer can inspect
and decide queues but cannot administer identities/policy or claim jobs. Existing
service/API credentials remain scoped and protected. Keep the listener on
loopback; the container example publishes its port on host loopback only.

## Register queues and enroll processes

```sh
install -d -m700 ~/.config/tachikoma/credentials
tachikoma approval put-queue contrib/adapters/snitchgate.queue.json
```

Create enrollment JSON outside the repository. A network producer spec is:

```json
{"id":"workstation-network","role":"producer","queues":["network.egress"],"service_kind":"snitchgate","realm":"","actors":[]}
```

The network assessment worker spec is:

```json
{"id":"network-assessment","role":"worker","queues":["network.egress"],"service_kind":"","realm":"","actors":[]}
```

Enroll each using an administrator client:

```sh
tachikoma approval enroll /private/path/network-producer.json \
  --credential-out ~/.config/tachikoma/credentials/network-producer.token
tachikoma approval enroll /private/path/network-worker.json \
  --credential-out ~/.config/tachikoma/credentials/network-worker.token
```

The output file must not exist. Each process gets its own credential; no process
uses the administrator key as its runtime identity. For another human reviewer,
enroll role `reviewer` with the intended queues and launch:

```sh
tachikoma approval --credential /private/path/reviewer.token tui
```

Queues can be split by service instance or sensitivity. Producer credentials
are bound to service kind and queue allowlist; a producer can only read its own
requests. Reserve separate worker processes/identities for interactive network
queues and background services so background model calls cannot consume the
network worker's concurrency. Global admission reserves 512 pending slots for
interactive queues; each queue also has its own cap.

## Jev and local Qwen

The supplied network manifest enables Jev classification plus Ollama explanation.
Only its listed `subject_fields` and `evidence_fields` go to models. URL hints
and human resource details are excluded. High confidence still requires a human
in this manual queue.

Place an encrypted, user-scoped `TYPESAFE_API_KEY` credential at the path used
by `tachikoma-assessment.service`, or run the worker with an existing private
key file. The unit does not load the key from a general shell environment.
Ollama defaults to loopback `127.0.0.1:11434`, `qwen3.5:9b`, `think=false`.
Starting the worker does not download models.

```sh
systemctl --user start tachikoma-assessment.service
journalctl --user -u tachikoma-assessment.service
tachikoma approval health
```

Provider failures leave human review available. Worker logs contain no raw
provider responses, prompts, tokens or daemon configuration. Investigation model
selection requires both the queue's `models` allowlist and worker
`--allow-model` flags for alternatives to its default model. No worker runs shell
commands or queries arbitrary URLs supplied by a request.

## Slack

Enroll a transport spec with the real workspace ID and human actor IDs:

```json
{"id":"workstation-slack","role":"transport","queues":["network.egress"],"service_kind":"","realm":"slack:YOUR_WORKSPACE_ID","actors":["YOUR_SLACK_USER_ID"]}
```

Write its key as `~/.config/tachikoma/credentials/slack-transport.token` and put
user-scoped encrypted bot/app credentials at the paths in the Slack unit. The
existing Slack app needs Socket Mode, interactivity, app mentions, posting and
thread message events, as described in the imported Snitchgate app manifest.
No Slack key goes to Tachikoma or a producer.

**One Socket Mode owner per app.** Stop the old Slack worker before connecting
the new transport with the same app credentials; Slack distributes events among
simultaneous connections. A user-manager `Conflicts=` cannot stop a system-manager
unit, so Noelarchy's existing system service must be handled explicitly during
cutover. Do not start both and expect broadcasts.

Invite the app to the channel and say `@app approvals here` as an allowlisted
human. Alternatively use the worker's explicit `--channel` and
`--register-actor` bootstrap arguments. Merely inviting the app does not select
a route. Registration is persisted centrally. `Type=notify` readiness waits for
both the Slack connection and a registered route.

Cards include subject/evidence, classification, deadlines, exact scopes and
producer-reported outcomes. Reply yes/no in that card's thread or use its action
buttons. Other users, edited messages, wrong workspaces and late/replayed replies
cannot settle the request. The API arbitrates first settlement between Slack,
TUI and web; all three review the same central record.

A crash after posting but before binding the message leaves an ambiguous
reservation. It is not reposted automatically; review the central inbox instead
of risking duplicate cards. A later transport retry never extends the request.

## Snitchgate: shadow, then manual authority

The imported service can still run standalone. To submit to Tachikoma, use an
external config with the existing local settings plus:

```toml
[tachikoma]
socket = "/run/user/1000/tachikoma/tachikoma.sock"
producer_id = "workstation-network"
queue = "network.egress"
credential_env = "TACHIKOMA_API_TOKEN"
shadow = false
```

Deliver the enrolled producer credential as a systemd file and set
`TACHIKOMA_API_TOKEN_FILE` to it. The producer ID must match enrollment. Paths
above are workstation examples, not portable defaults.

For shadow evaluation, set the queue mode to `shadow` and the producer setting to
`shadow=true`. Shadow submits observations and never enforces a central decision;
its existing local reviewer remains in charge. For manual central enforcement,
set queue mode `manual` and producer `shadow=false`, with no local automatic
rules. Local Snitchgate TUI/old Slack decisions are then rejected; use central
review clients. This prevents two owners from acting on the same pending request.

The broker's original deadline and local deny-once fallback remain independent
of Tachikoma. It permits only the known allow-once, deny-once and process-bound
five-minute scopes, verifies all decision bindings and audits locally before
returning a rule. A central outage does not restart the timeout. No rule operator,
permanent permission or tool command is accepted from a model or generic API.

Do not point a demo/test at the real daemon socket. Docker conformance starts
isolated broker sockets and mock OpenSnitch clients and providers. For live
handover on this workstation, update Noelarchy's canonical units, credential
mapping, narrow executable egress rules and readiness watcher together.
Noelarchy’s watcher checks the new Slack/classifier units and authenticates to
queue health as a dedicated monitor with no request or decision authority.
Confirm central API/worker/route readiness, then use the reversible GUI handover. Never restart
OpenSnitch or rewrite `/etc` policy as a test shortcut.

## Kubernetesgate

```sh
kubernetesgate manifest > /private/path/kubernetes.queue.json
tachikoma approval put-queue /private/path/kubernetes.queue.json
```

Enroll a separate producer for `kubernetes.observe`, service kind `kubernetesgate`.
Then supply its socket and key:

```sh
kubernetesgate --socket "$XDG_RUNTIME_DIR/tachikoma/tachikoma.sock" \
  --credential /private/path/kubernetes-producer.token suggest \
  --context development --namespace default -- get pods
```

This records a read-only command observation. It does not load kubeconfig, run
kubectl or contact a cluster. Its only action is observation acknowledgement;
no Kubernetes execution grant exists. Secret resource observations are excluded
and credential-like arguments are redacted.

## Migration and rollback

Back up original SQLite plus WAL/SHM consistently and existing JSONL files before
a live migration. The new store is separate. `tachikoma approval import-legacy`
opens the source read-only and imports historical proposal metadata as Observe
records; it preserves original state labels as facts and grants no authority.
Original evidence, analysis and transcripts stay at their original paths. The
separate `tachikoma-legacyd` and `tachikoma tui --legacy` can inspect them using a
separate socket and port. Do not point both daemons at the same socket.

For a live network rollback, first restore native GUI routing with the existing
handover service, stop the new Slack owner, restore the previous root-owned
Snitchgate binary/configuration and old Slack worker, then re-enable the old
handover only after readiness checks pass. Restoring a binary does not revive
old decisions or require deleting the central audit database.

Check `tachikoma approval health` for queue counts, worker/producer/transport
heartbeats and leases. An old heartbeat is a warning, not an enforcement claim.
The database uses local SQLite WAL and a maximum page count; full disks or failed
audit writes cannot commit new authority. Use local storage, not network SQLite.

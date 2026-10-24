# Approval adapter API v1

The daemon accepts HTTP JSON at `/api/approval/v1` on its private Unix socket.
Every operation requires `Authorization: Bearer <enrolled credential>`.
There is no unauthenticated producer, discovery or decision endpoint.
An external service needs HTTP/JSON only; Rust workspace dependencies are optional.

`crates/approval-protocol/src/lib.rs` contains the complete serialized types.
`examples/external-adapter/service.py` is a stdlib-only producer with a durable
consumption journal. `conformance/adapter/test_contract.py` runs it as a separate
process, including crash/restart and duplicate-decision cases.

## Registration and roles

An administrator registers queue manifests before enrolling principals. A queue
has an immutable ID, service kind, schema ID, structural subject shape, bounded
allowed actions/scopes, pending/deadline limits, scheduling class and mode.
Changing its manifest increments the revision, cancels pending requests and
fences investigation work created under the previous revision.

Principals have a role and explicit queue allowlist:

| Role | Authority |
| --- | --- |
| `producer` | Submit/cancel/read its own requests and report its own outcomes |
| `reviewer` | Read and decide authorized queues; start/cancel advisory investigations |
| `worker` | Claim authorized jobs and complete only a live owned lease |
| `transport` | Route Slack deliveries and relay allowlisted actors through bound messages |
| `monitor` | Authorized queue/process health only; no request, job or decision access |
| `admin` | Queue/identity administration and review |

Enrollment returns a random 256-bit credential once. The daemon stores only its
hash. CLI enrollment writes it to a new mode-0600 file under a private directory.
The loopback web frontend uses a reserved `local-web` reviewer identity with a
private process-local capability, regenerated at daemon startup. Its browser UI
requires no credential entry; cookies/hidden tokens protect form submissions.
This identity cannot be enrolled or rotated through the public API, administer
queues/identities or claim worker jobs.

Rotation invalidates the old credential, pending producer consent and worker
leases. Updating a principal also rotates its key. Revoked identities cannot be
silently enrolled again with a different owner; use an explicit update/rotation.

## Endpoint map

All paths below are relative to `/api/approval/v1`.

| Method/path | Body or query | Result |
| --- | --- | --- |
| `GET /identity` | — | Current principal, no credential |
| `GET /queues` | — | Authorized manifests/counts |
| `POST /queues` | QueueManifest | Updated manifest revision (admin) |
| `POST /principals` | PrincipalSpec | Once-only credential (admin) |
| `POST /principals/update` | PrincipalSpec | Updated ACL and rotated credential (admin) |
| `POST /principals/{id}/revoke` | — | Revocation (admin) |
| `POST /principals/{id}/rotate` | — | Once-only replacement (admin) |
| `POST /requests` | SubmitRequest | Persisted review |
| `GET /requests` | `state=pending` or `history`; optional `before` | 16-row page and `next_before` |
| `GET /requests/{id}` | — | Current authorized review |
| `POST /requests/{id}/decision` | `revision`, `action_id` | Atomic decision or conflict |
| `POST /requests/{id}/cancel` | — | Owning producer cancels pending request |
| `POST /requests/{id}/receipt` | EnforcementReceipt | Producer-reported outcome |
| `GET /events` | `after`, `wait_ms` (max 30000) | Ordered cursor page, max 500 events |
| `POST /jobs/claim` | `kind=assessment` or `assessment_or_investigation` | Job/lease or null |
| `POST /jobs/{id}/complete` | `lease`, `assessment` | Fenced advisory completion |
| `POST /jobs/{id}/fail` | `lease`, `retryable` | Bounded retry or failure |
| `GET /requests/{id}/conversation` | — | Durable advisory transcript |
| `POST /requests/{id}/conversation` | `message`, optional `model`, `idempotency_key` | Queued turn |
| `POST /requests/{id}/conversation/cancel` | — | Cancel and fence active turn |
| `POST /heartbeat` | `detail` | Enrolled process heartbeat |
| `GET /health` | — | Authorized queues, services and job counts |
| `GET/POST /transport/route` | RouteRegistration for POST | Durable channel per realm |
| `GET/POST /transport/deliveries` | ReserveDelivery for POST | Durable delivery reservation/list |
| `POST /transport/deliveries/{id}/delivered` | `message` | Bind actual posted message |
| `POST /transport/deliveries/{id}/reply` | DeliveryReply | Actor/message/nonce-bound decision |

Requests are limited to 64 KiB. JSON duplicate keys and trailing documents are
rejected. Responses contain bounded pages. Errors return an HTTP status and
`{code,message}` without raw database/provider errors: 400 invalid input, 401
unknown/revoked key, 403 forbidden, 404 missing, 409 changed/expired/settled, 429
backpressure, 500/503 unavailable. A connection failure never means approval.
Persist event cursors and reconcile with request snapshots after reconnecting.
Events are currently retained in SQLite; no cursor-retention truncation exists.

## Submission and immutable facts

A SubmitRequest contains:

```json
{
  "queue": "example.review",
  "producer_request_id": "local-operation-123456",
  "nonce": "random-operation-nonce-123456",
  "idempotency_key": "local-operation-123456",
  "schema_id": "example.subject.v1",
  "subject": {"target": "example.invalid"},
  "evidence": {"source": "captured locally"},
  "action_ids": ["allow-once", "deny-once"],
  "expires_at_ms": 1790000000000,
  "summary": "Example request"
}
```

The timestamp is illustrative; submit the original operation's actual deadline.
Repeated producer/queue/idempotency keys with identical input return the same
review, including after settlement. Changed input conflicts. Deadlines are
clamped to the administrator's queue cap and never extended by retries.

Canonical immutable JSON uses recursively sorted object keys, UTF-8, no extra
whitespace and no ASCII escaping of Unicode. Numbers in immutable facts/scopes
must be integers in the inclusive range `[-9007199254740991,9007199254740991]`.
Represent decimal scores as strings. This avoids cross-language float ambiguity.
SHA-256 digests use lowercase hexadecimal:

- `subject_digest = SHA256(canonical([schema_id, subject]))`
- `evidence_digest = SHA256(canonical(evidence))`

The optional Rust client and the independent Python example implement this
same encoding. Mutable model confidence is a bounded floating-point opinion;
it is never an immutable fact or a permission.

## Receiving and consuming a decision

Persist the original request before submission. Keep a local monotonic deadline
independently of Tachikoma. Verify the echoed request, producer, queue, nonce,
schema-bound subject/evidence digests, request/manifest revisions, decision
lifetime and exact locally supported action/effect/scope. Require an
authoritative decision with the expected terminal state. Never translate free
text, model output, unknown actions or larger scopes into authority.

The channel is authenticated by the private socket and enrolled credential;
remote deployments require operator-managed HTTPS termination. Decisions are
not independently signed capability tokens. Never expose plain HTTP API access
on a public interface or treat an arbitrary copied decision as trustworthy.

Journal consumption before applying the service's effect. A crash in the gap
between consumption and an external side effect yields `unknown`, not a repeat.
A receipt includes `decision_id`, `state` (`enforced`, `not_enforced`, `unknown`),
a bounded `reason` and `observed_at_ms`. Identical receipts are idempotent;
conflicting receipts require investigation. The central service records a
producer claim, not proof of enforcement.

Manual and shadow reviews do not become automatic because confidence is high.
Explicit policy mode supports administrator-authored exact immutable-fact
matches. Shadow and Observe actions always have `authoritative=false`.

## Workers and transports

Assessment jobs carry the profile snapshot and request evidence digest. Worker
leases last at most 30 seconds and cannot outlive the original request. Fenced
or cancelled completions cannot attach a result. Retries are bounded to three
attempts. Profiles explicitly allow which subject/evidence fields reach a model.
Jev receives typed classification choices; Ollama supplies explanations. Neither
can create an action or invoke a tool.

Investigation jobs have their own 90-second deadline and can discuss historical
requests. A conversation permits one active turn, at most 64 messages and 2048
bytes per user message. Explicit models must be allowed by both the queue and
the worker. Cancellation fences late results. Chats never reopen consent.

Slack credentials belong to the transport process. A transport is bound to its
workspace realm and actor allowlist. Every reply binds the original channel,
message, delivery nonce and request revision. Only first settlement succeeds.
Unthreaded yes/no, edited messages and other workspaces are ignored. A reserved
but ambiguously posted card is not automatically reposted after a crash.

## Compatibility and packaging

This is a prerelease v1 contract. Additive optional fields may be introduced;
clients must reject unknown action/effect/state authority and tolerate unrelated
optional display fields. Breaking changes require a new URL version. Keep old
clients in the Docker conformance suite before changing binding semantics.

The protocol crate uses serde only; the optional client adds HTTP and hashing.
Use `cargo package -p approval-protocol --allow-dirty --no-verify` to inspect its
standalone source artifact. No public crate has been published by this change.
External services can use raw JSON today and need no private implementation.

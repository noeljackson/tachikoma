# Optional sources and shadow policies

Tachikoma works without Tetragon or Suricata. Sources are independently configured
publishers over the public API. Evidence policies are a separate **shadow-only**
layer: even an `action` recommendation never settles a request, changes an
OpenSnitch rule, or bypasses human review. Existing exact-match queue policies
retain their existing behavior; workstation network approvals stay manual.

## Configure only the sources you have

```sh
tachikoma approval put-source contrib/evidence/tetragon.source.json
tachikoma approval put-source contrib/evidence/suricata.source.json
tachikoma approval sources
tachikoma approval put-evidence-policy contrib/evidence/snitchgate.policy.json
tachikoma approval evidence-policy network.egress
tachikoma approval preview REQUEST_ID
```

These are JSON configuration files, with strict schemas and bounded predicates.
Omit a source entirely or set its `enabled` flag to false. Rules may reference
sources that are not installed. Their result is unknown, with a specific reason.
There is no dynamic code loading or command execution in a policy.

An administrator enrolls one `source` principal per publisher, with `realm` equal
to its source ID and only the queues it may publish into. For example:

```json
{"id":"host-observer","role":"source","queues":["network.egress"],"realm":"host.tetragon","service_kind":"tetragon","actors":[]}
```

Use `tachikoma approval enroll SPEC.json --credential-out PRIVATE.token` to
provision it. Publisher credentials cannot read requests, claim jobs, decide,
register sources or change policies. Keep the administrator key out of adapters.

## Adapters

`tachikoma-source` provides optional `tetragon`, `suricata` and `jsonl` formats.
It is a separate workspace service that depends only on the public protocol and
client. A custom service in another repository can publish the same HTTP records
without linking our code. No Tetragon/Suricata library is required by the core.

```sh
tachikoma-source --socket /path/to/tachikoma.sock --credential /private/source.token \
  --source host.tetragon --queue network.egress --format tetragon \
  --input /var/log/tetragon/events.json --tetragon-policy noelarchy-credentials
```

Run log readers with narrowly scoped access to their configured input and the
approval socket. Noelarchy uses root-owned systemd units and encrypted credentials.
The Tetragon adapter requires an explicit list of trusted tracing-policy names.
It exports process/parent identities and observation types, excluding arguments,
environment, watched file paths and contents. An open is an attempt, not proof of
a successful read. The Suricata adapter exports alert identifiers and flow keys,
excluding payloads, HTTP headers, URLs and file contents.

Followers start at EOF, retry failed delivery without advancing the record, and
follow rotations. Records larger than 64 KiB stop collection and mark it degraded.
`--once` imports at most 200 records from a 4 MiB tail, then marks the publisher
stopped. Unattributed Tetragon records are skipped without inventing an executable.
Original timestamps are preserved; stale records are never relabeled as
fresh. Restart gaps and upstream sensor health remain unknown: a heartbeat means
the adapter can watch its input, not that the upstream sensor captured everything.

The `jsonl` format accepts a normalized Observation per line. Define the data
schema in the SourceSpec; unknown fields are rejected. This format is a transport,
not a universal redactor: a custom publisher must omit secrets itself.

## Rule language

Each policy belongs to a queue and has a fallback plus up to 32 rules. Conditions
support `all`, `any`, `equals`, numeric `at_least` and `observed`. Request-context
JSON pointers start with `/subject/`, `/evidence/` or `/assessment/`. Jev judgments
are available through `/assessment/judgments/`; they remain model opinions.

An `observed` predicate names a source, capability, lookback window and explicit
join fields. For example, `/process/exec_id` can join a request's stable execution
identity. Multiple joins must all match; missing, null, blank or non-scalar identities are unknown.
Join fields must be nonblank strings or interoperable integers.
The evaluator never substitutes a PID or hostname for a missing join. The author
must choose suitable identities: a PID by itself is reusable, and PID/path joins
in the example detector are approximate context for review, not verified identity.

The window ends at the request's original creation time. Future Suricata alerts
cannot justify an initial connection that is still held. Late-ingested events
can match only when their original timestamp precedes the request. Query scans
are bounded to 129 recent candidates; no match is unknown, never proof of absence.

`requires` adds explicit source/capability requirements. Requirements are checked
when a rule might match; a definitively false condition does not require unrelated
sensors. `on_unavailable: ask` creates a review barrier. `continue` lets the other
rules and fallback determine the recommendation. Ask barriers override action
recommendations; conflicting action recommendations also resolve to ask.

Preview output includes each rule's matched/not_matched/unknown status, reason,
observation IDs/digests/source revisions, request and policy revisions, evaluation
time and a snapshot digest. Settled/expired requests cannot recommend execution.
The CLI, TUI Details pane and web review show previews; Health shows publishers.

## Public endpoints

All paths are relative to `/api/approval/v1`, with normal bearer authentication:

| Endpoint | Access |
| --- | --- |
| GET/POST `/evidence/sources` | Scoped status / administrator configuration |
| POST `/evidence/sources/{id}/heartbeat` | Matching source principal |
| POST `/evidence/sources/{id}/observations` | Matching source principal and queue |
| GET `/evidence/policies/{queue}` | Queue readers except source publishers |
| POST `/evidence/policies` | Administrator |
| GET `/requests/{id}/evidence-preview` | Authorized request readers |

Source configuration changes increment its revision and fence old observations
and health. Publisher revocation and key rotation also fence prior observations,
including records published before a first heartbeat. Revoked publishers become unavailable. Observation IDs are idempotent
within a source revision; changed payloads conflict. Data must match the declared
capability schema. Observations are bounded to one hour / 10,000 records globally,
so missing historical evidence must not be interpreted as a clean result.

The current release does not execute evidence-policy recommendations. Enabling
automatic enforcement later requires commit-time freshness/identity validation
and explicit action authorization; a preview is not an approval token.

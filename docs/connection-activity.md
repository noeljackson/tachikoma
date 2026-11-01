# Connection activity and approval history

OpenSnitch's original GUI owns its SQLite connection-history database. Snitchgate
does not open, migrate or write that file. Existing GUI history is preserved;
activity discarded before this collector was deployed cannot be reconstructed
from approval history.

The local broker now consumes the daemon's `PingRequest.stats.events` stream into
its own private `activity.sqlite3`, beside its separate approval `history.sqlite3`.
These are daemon-reported connection observations, including rule hits, and never
become human approvals, change policy or grant traffic. The central approval audit
remains in tachikoma's approval database. This adds no traffic-monitor page and
sends no new activity records to a model.

Recorded fields are the reported event timestamp, receipt time, executable path,
UID/PID, protocol, destination IP/hostname/port, reported rule action and a SHA-256
rule-name reference. Arguments, environment, cwd, checksums, source payloads and
raw rule names are omitted. Strings and batches are bounded. There is no HTTP
payload, file identity verification or complete packet capture.

The gRPC callback sanitizes up to 512 events per batch and uses a nonblocking queue
of eight batches. A dedicated SQLite writer applies transactions, deduplication,
seven-day retention and a 50,000-row cap. Database pages are capped at 128 MiB;
WAL checkpoints bound normal journal growth. Recording failures and overload are
counted and do not block approvals or change their deadlines. Storage failures
also produce a generic journal message without the incoming payload.

Deduplication covers the current broker session using timestamp and sanitized
connection fields. The daemon protocol has no durable delivery cursor or stable
connection ID. Restarted brokers may observe a repeated event; unreported events,
disabled logging, daemon buffers and overload can create gaps. Counts reset on
broker restart; the on-disk observations remain. These limitations prevent an
exactly-once or full-coverage claim.

Read counters through the existing owner-only control socket:

```sh
snitchgate status --socket /run/snitchgate/control.sock
snitchgate activity --socket /run/snitchgate/control.sock --limit 50
snitchgate activity --socket /run/snitchgate/control.sock --before SEQUENCE --limit 50
```

The activity command returns a page and an optional `next_before` sequence.
It can run while the desktop is closed. Keep the original GUI database as a
read-only historical source; it need not become the approval service's database.

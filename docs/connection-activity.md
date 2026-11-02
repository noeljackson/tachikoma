# Connection activity and approval history

OpenSnitch's original GUI normally writes its SQLite connection-history database.
Snitchgate can optionally keep an existing schema-v3 database current through a
separate compatibility writer, described below. It never migrates that file.
Existing GUI history is preserved;
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

Only valid TCP/UDP observations with a parsed destination IP, valid port,
positive timestamp and reported rule are retained. Unsupported or malformed
entries increment `dropped`, as do batch limits and queue overload. This stream
does not promise the same event coverage as the original GUI database.

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
It can run while the desktop is closed. The original GUI database remains
separate from the approval service's audit database.

## Keep existing OpenSnitch readers current

Opt in with an existing database and its local node key:

```toml
[opensnitch_database]
path = "/absolute/path/to/existing/ui.sqlite3"
node = "unix:/local"
```

Back up the file with SQLite's online backup API before enabling this writer.
The target must already exist, have `user_version=3`, match the supported table
and uniqueness layout, and be owned by the service user. Shared write permissions,
symlinks, unexpected triggers and unsupported versions are rejected. A private
adjacent flock lease prevents competing compatibility writers. Preserve the same
directory and basename when exposing the file inside a service namespace, so
existing readers and the writer share SQLite journal/WAL files. A file-only bind
under a different basename is unsuitable.

The writer appends connection observations using the GUI's `INSERT OR IGNORE`
uniqueness semantics, updates node status and the hosts/procs/addrs/ports/users
summary tables, and advances last-seen timestamps for already cached rules.
It never creates rule inventory, edits policy or mirrors alerts/socket dumps.
Reported protocols such as ICMP and TCP/UDP over IPv6 are retained. Timestamps
use the host's local time as the GUI does. Counter snapshots describe the daemon's
current statistics, not historical row totals.

New rows retain bounded process/UID/PID, source and destination, reported action
and rule identifier. Arguments and cwd are empty; environment and checksums are
never copied. Unlike the small activity store, this compatibility file keeps a
bounded rule name for existing cache lookups (oversized names become a hash).
Redacted arguments can collapse command variants under the upstream unique key.
Old rows, including their original fields, remain unchanged. This writer applies
no retention, schema migration or new tables/indexes to the GUI database; the
small activity store's seven-day/50,000-row limit does not apply to it. It resumes
new observations without backfilling the earlier gap. Plan disk retention for
the original file separately.

A dedicated worker receives up to 1,024 events and 1,024 entries per summary map
through a four-batch nonblocking queue. SQLite lock waits are capped at 100 ms;
failures pause recording and retry after a cooldown while approvals continue.
Schema/version or inode changes are checked on subsequent batches. Changes to
schema during a transaction cannot race validation because the writer holds an
immediate transaction. No full incoming statistics/config blob crosses the queue.

`snitchgate status` exposes `health.opensnitch_database`: `ready`,
`last_write_at`, event counters, `summary_dropped` and `errors`. Counters reset on
broker restart. Queue limits, unsupported metadata and failed batches can create
gaps; neither this mirror nor the daemon protocol promises complete capture.
Keep the old GUI retired as the live approval endpoint; existing database readers
can continue reading. Models receive no new data from either history writer.

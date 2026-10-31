#!/usr/bin/env python3
"""Export a bounded, private metadata sample. Firewall outcomes are never safety labels."""
import argparse
import ipaddress
import json
import os
from pathlib import Path
import sqlite3
import time


def export(database, limit):
    connection = sqlite3.connect(database.resolve().as_uri() + "?mode=ro", uri=True, timeout=0.2)
    deadline = time.monotonic() + 5
    connection.set_progress_handler(lambda: int(time.monotonic() > deadline), 10_000)
    rows = connection.execute("SELECT rowid, process, uid, protocol, dst_ip, dst_host, dst_port, CASE WHEN length(process_cwd)<=1024 THEN process_cwd END FROM connections ORDER BY rowid DESC LIMIT 2000")
    cases, seen = [], set()
    for row_id, process, uid, protocol, destination, host, port, cwd in rows:
        try:
            protocol = {"tcp6": "tcp", "udp6": "udp"}.get(protocol, protocol)
            if protocol not in {"tcp", "udp"} or not isinstance(process, str) or len(process) > 4096 or any(ord(c) < 32 for c in process):
                continue
            destination = str(ipaddress.ip_address(destination))
            port, uid = int(port), int(uid)
            if not 0 < port < 65536 or not 0 <= uid <= 4294967295:
                continue
            host = host or ""
            if len(host) > 253 or not all(c.isascii() and (c.isalnum() or c in ".-_") for c in host):
                continue
            cwd = cwd if isinstance(cwd, str) and cwd.startswith("/") and not any(ord(c) < 32 for c in cwd) else None
            identity = (process, uid, protocol, host or destination, port, cwd)
            if identity in seen:
                continue
            seen.add(identity)
            cases.append({"id": f"history-{row_id}", "tag": "unlabeled-history", "expected": None,
                "subject": {"process": process, "pid": 0, "uid": uid, "protocol": protocol, "ip": destination, "host": host, "port": port},
                "evidence": {"process_context": {"source_ip": None, "source_port": None, "working_directory": cwd, "ancestors": [], "checksums": {}, "argument_markers": [], "lifecycle_event": None},
                    "context_limits": "Historical metadata only. PID is a placeholder, not a live identity. No live /proc lookup was performed. Ancestry, checksums, argv markers, lifecycle metadata, file access, HTTP contents and listener ownership are unavailable. Previous allows or denies are not safety labels."}})
            if len(cases) >= limit:
                break
        except (TypeError, ValueError):
            continue
    connection.close()
    return {"name": "unlabeled-opensnitch-history", "cases": cases}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--database", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--limit", type=int, default=16)
    args = parser.parse_args()
    if not 1 <= args.limit <= 200:
        parser.error("limit must be 1..200")
    data = export(args.database, args.limit)
    descriptor = os.open(args.output, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    with os.fdopen(descriptor, "w") as file:
        json.dump(data, file, indent=2)
        file.write("\n")
    print(f"Exported {len(data['cases'])} unlabeled scopes to a private file. No argv, environment, or historical PIDs exported.")


if __name__ == "__main__":
    main()

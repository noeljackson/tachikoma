#!/usr/bin/env python3
"""Standalone Python producer using only the public HTTP JSON contract and stdlib.

The enforcer simulates its effect in a private journal. It never opens a firewall,
fetches a package, or executes a system command. Copy this directory into a
separate repository; no monorepo imports, Rust SDK or shared database are needed.
"""
import argparse
import hashlib
import http.client
import json
import os
from pathlib import Path
import socket
import stat
import time
import uuid

PREFIX = "/api/approval/v1"

class UnixHTTP(http.client.HTTPConnection):
    def __init__(self, path):
        super().__init__("localhost", timeout=3)
        self.path = str(path)
    def connect(self):
        self.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.sock.settimeout(self.timeout)
        self.sock.connect(self.path)

class API:
    def __init__(self, path, credential_file):
        metadata = credential_file.lstat()
        if not stat.S_ISREG(metadata.st_mode) or metadata.st_mode & 0o077 or metadata.st_uid != os.getuid():
            raise ValueError("credential must be a private, owned regular file")
        self.secret = credential_file.read_text().strip()
        if len(self.secret) != 64:
            raise ValueError("invalid credential format")
        self.path = path
    def call(self, method, path, data=None):
        connection = UnixHTTP(self.path)
        try:
            body = None if data is None else json.dumps(data, separators=(",", ":"), ensure_ascii=False).encode()
            connection.request(method, PREFIX + path, body, {"Authorization": "Bearer " + self.secret, "Content-Type": "application/json"})
            response = connection.getresponse()
            body = response.read(2_097_153)
            if response.status >= 400:
                raise RuntimeError(f"approval API rejected operation: HTTP {response.status}")
            if len(body) > 2_097_152:
                raise ValueError("response exceeds limit")
            return json.loads(body)
        finally:
            connection.close()

def save(path, state):
    path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
    temporary = path.with_name(path.name + "." + uuid.uuid4().hex)
    fd = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    with os.fdopen(fd, "w") as handle:
        json.dump(state, handle, ensure_ascii=False)
        handle.flush()
        os.fsync(handle.fileno())
    temporary.replace(path)
    directory = os.open(path.parent, os.O_RDONLY | os.O_DIRECTORY)
    try:
        os.fsync(directory)
    finally:
        os.close(directory)

def fingerprint(value):
    return hashlib.sha256(json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=False).encode()).hexdigest()

def boot():
    return Path("/proc/sys/kernel/random/boot_id").read_text().strip()

def valid(review, state):
    expected = state["submission"]
    decision = review.get("decision")
    if not decision or review["request"] != expected or review["producer"] != state["producer"]:
        return False
    if review["subject_digest"] != fingerprint([expected["schema_id"], expected["subject"]]) or review["evidence_digest"] != fingerprint(expected["evidence"]):
        return False
    return (
        decision["authoritative"]
        and decision["request_id"] == review["id"]
        and decision["producer"] == state["producer"]
        and decision["queue"] == expected["queue"]
        and decision["nonce"] == expected["nonce"]
        and decision["request_revision"] == review["revision"]
        and decision["manifest_revision"] == review["manifest_revision"]
        and decision["subject_digest"] == review["subject_digest"]
        and decision["evidence_digest"] == review["evidence_digest"]
        and decision["issued_at_ms"] <= int(time.time() * 1000) < decision["expires_at_ms"] <= review["expires_at_ms"] <= expected["expires_at_ms"]
        and decision["action"] in review["actions"]
        and decision["action"]["id"] in expected["action_ids"]
    )

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--socket", type=Path, required=True)
    parser.add_argument("--credential", type=Path, required=True)
    parser.add_argument("--state", type=Path, required=True)
    parser.add_argument("--target", default="example.invalid")
    parser.add_argument("--ttl", type=int, default=60)
    parser.add_argument("--crash-after-consume", action="store_true")
    args = parser.parse_args()
    api = API(args.socket, args.credential)
    if args.state.exists():
        state = json.loads(args.state.read_text())
    else:
        operation = str(uuid.uuid4())
        state = {"producer": api.call("GET", "/identity")["id"], "boot": boot(), "deadline_monotonic": time.monotonic() + args.ttl, "submission": {"queue": "example.review", "producer_request_id": operation, "nonce": operation, "idempotency_key": operation, "schema_id": "example.subject.v1", "subject": {"target": args.target}, "evidence": {"source": "standalone Python producer", "execution": "simulation only"}, "action_ids": ["allow-once", "deny-once"], "expires_at_ms": int(time.time() * 1000) + args.ttl * 1000, "summary": "Simulated external service request"}}
        save(args.state, state)
    if state.get("consumed"):
        # A crash after consumption cannot cause another effect. Reconcile/report unknown.
        if "receipt" not in state:
            state["receipt"] = {"decision_id": state["consumed"], "state": "unknown", "reason": "restart after consumption; effect was not repeated", "observed_at_ms": int(time.time() * 1000)}
            save(args.state, state)
        api.call("POST", f"/requests/{state['review_id']}/receipt", state["receipt"])
        return 0
    if state["boot"] != boot():
        raise RuntimeError("original operation did not survive reboot; refusing to revive it")
    review = api.call("POST", "/requests", state["submission"])
    state["review_id"] = review["id"]
    save(args.state, state)
    while time.monotonic() < state["deadline_monotonic"]:
        if review["state"] != "pending":
            if not valid(review, state):
                raise RuntimeError("decision binding or lifetime is invalid")
            action = review["decision"]["action"]
            if action["scope"] != {"uses": 1}:
                raise RuntimeError("service refuses unfamiliar scope")
            if action["id"] == "allow-once" and action["effect"] == "permit" and review["state"] == "allowed":
                state["consumed"] = review["decision"]["id"]
                save(args.state, state)
                if args.crash_after_consume:
                    return 17
                state["simulated_effect"] = {"target": state["submission"]["subject"]["target"], "decision": state["consumed"]}
                state["receipt"] = {"decision_id": state["consumed"], "state": "enforced", "reason": "simulated effect recorded once in local journal", "observed_at_ms": int(time.time() * 1000)}
                save(args.state, state)
                api.call("POST", f"/requests/{review['id']}/receipt", state["receipt"])
                return 0
            if action["id"] == "deny-once" and action["effect"] == "block":
                state["denied"] = True
                save(args.state, state)
                api.call("POST", f"/requests/{review['id']}/receipt", {"decision_id": review["decision"]["id"], "state": "enforced", "reason": "simulated action blocked", "observed_at_ms": int(time.time() * 1000)})
                return 0
            raise RuntimeError("unknown action; no effect performed")
        time.sleep(0.1)
        try:
            review = api.call("GET", f"/requests/{review['id']}")
        except (OSError, RuntimeError):
            continue
    try:
        api.call("POST", f"/requests/{review['id']}/cancel", {})
    except (OSError, RuntimeError):
        pass
    state["timed_out"] = True
    save(args.state, state)
    return 2

if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (OSError, ValueError, RuntimeError):
        # Do not print tokens, provider bodies or arbitrary request evidence.
        raise SystemExit("Producer stopped safely; no unverified action was performed")

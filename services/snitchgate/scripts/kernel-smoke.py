#!/usr/bin/python3 -I
"""Opt-in real-daemon enforcement check, confined to a fresh network namespace."""
import argparse
import http.server
import json
import os
from pathlib import Path
import signal
import shutil
import socket
import subprocess
import sys
import tempfile
import threading
import time

ROOT = Path(__file__).resolve().parents[1]
BIN = ROOT.parents[1] / "target/debug/snitchgate"


def wait_for(predicate, seconds=15):
    end = time.monotonic() + seconds
    while time.monotonic() < end:
        result = predicate()
        if result:
            return result
        time.sleep(0.1)
    raise RuntimeError("isolated smoke check deadline expired")


def control(path, message):
    with socket.socket(socket.AF_UNIX) as stream:
        stream.settimeout(3)
        stream.connect(str(path))
        stream.sendall(json.dumps(message).encode() + b"\n")
        response = b""
        while not response.endswith(b"\n"):
            chunk = stream.recv(65536)
            if not chunk:
                raise RuntimeError("isolated broker closed control connection")
            response += chunk
        result = json.loads(response)
        if result.get("error"):
            raise RuntimeError("isolated broker rejected operation")
        return result


def saved_rules(directory):
    # The isolated daemon saves files as root with a private umask.
    code = "import json,sys; from pathlib import Path; print(json.dumps([json.loads(p.read_text()) for p in Path(sys.argv[1]).glob('*.json')]))"
    return json.loads(subprocess.check_output(["sudo", "-n", sys.executable, "-I", "-c", code, str(directory)], text=True))


def namespace(root, uid, gid):
    if os.geteuid() != 0:
        raise RuntimeError("namespace worker needs root")
    os.umask(0o077)
    subprocess.run(["ip", "link", "set", "lo", "up"], check=True)
    rules = root / "rules"
    rules.mkdir(mode=0o700)
    abstract_name = f"snitchgate-kernel-{os.getpid()}"
    config = {
        "Server": {"Address": "unix:@snitchgate-unavailable-" + str(os.getpid()),
                   "Authentication": {"Type": "simple"}, "LogFile": str(root / "daemon.log")},
        "DefaultAction": "deny", "DefaultDuration": "once", "InterceptUnknown": True,
        "ProcMonitorMethod": "proc", "Firewall": "nftables", "LogLevel": 2,
        "FwOptions": {"ConfigPath": str(root / "system-fw.json"), "QueueNum": 11347,
                      "QueueBypass": False, "MonitorInterval": "15s"},
        "Rules": {"Path": str(rules), "EnableChecksums": False},
        "Internal": {"GCPercent": 100, "FlushConnsOnStart": False},
        "Stats": {"MaxEvents": 20, "MaxStats": 10, "Workers": 2},
    }
    path = root / "daemon-config.json"
    path.write_text(json.dumps(config))
    original_config = path.read_bytes()
    # System-wide rules are disabled; only the isolated daemon's queueing rules
    # are needed. Never open or copy the live host's rules/configuration.
    (root / "system-fw.json").write_text(json.dumps({"Enabled": False, "Version": 1, "SystemRules": []}))

    class Handler(http.server.BaseHTTPRequestHandler):
        def do_GET(self):
            self.send_response(200)
            self.end_headers()
            self.wfile.write(b"snitchgate-kernel-fixture")

        def log_message(self, *args):
            pass

    fixture = http.server.ThreadingHTTPServer(("127.0.0.1", 18081), Handler)
    threading.Thread(target=fixture.serve_forever, daemon=True).start()
    second_fixture = http.server.ThreadingHTTPServer(("127.0.0.2", 18082), Handler)
    threading.Thread(target=second_fixture.serve_forever, daemon=True).start()
    stopped = threading.Event()
    restart_requested = threading.Event()
    signal.signal(signal.SIGUSR1, lambda *_: restart_requested.set())
    signal.signal(signal.SIGTERM, lambda *_: stopped.set())
    signal.signal(signal.SIGINT, lambda *_: stopped.set())
    with (root / "daemon-output.log").open("w") as output:
        broker = subprocess.Popen([str(BIN), "serve", "--config", str(root / "broker.toml"),
                                   "--runtime-dir", str(root / "runtime"), "--state-dir", str(root / "state"),
                                   "--manual", "--abstract-name", abstract_name],
                                  user=uid, group=gid, stdin=subprocess.DEVNULL, stdout=output, stderr=output)
        wait_for((root / "runtime/control.sock").exists)
        daemon = subprocess.Popen(["/usr/bin/opensnitchd", "-config-file", str(path),
                                   "-rules-path", str(rules), "-workers", "2"],
                                  stdin=subprocess.DEVNULL, stdout=output, stderr=output)
        wait_for((root / "daemon.log").exists)
        time.sleep(2)
        # sudo keeps the invoking user's home in SUDO_USER, while root's HOME is
        # /root. Resolve the tested helper through the owner's passwd record.
        import pwd
        helper = Path(pwd.getpwuid(uid).pw_dir) / "noelarchy/bin/opensnitch-snitchgate-trial"
        handover = subprocess.Popen([sys.executable, "-I", str(helper), "trial", "--config-file", str(path),
                                    "--runtime-dir", str(root / "trial"), "--abstract-name", abstract_name,
                                    "--owner-uid", str(uid), "--seconds", "180"], stdout=output, stderr=output)
        try:
            (root / "namespace.json").write_text(json.dumps({"pid": os.getpid()}))
            os.chown(root / "namespace.json", uid, gid)
            generation = 0
            while not stopped.wait(0.25):
                if restart_requested.is_set():
                    restart_requested.clear()
                    daemon.terminate(); daemon.wait(timeout=10)
                    daemon = subprocess.Popen(["/usr/bin/opensnitchd", "-config-file", str(path), "-rules-path", str(rules), "-workers", "2"], stdin=subprocess.DEVNULL, stdout=output, stderr=output)
                    time.sleep(2)
                    generation += 1
                    temporary = root / "namespace.next"
                    temporary.write_text(json.dumps({"pid":os.getpid(),"generation":generation}))
                    os.chown(temporary,uid,gid)
                    temporary.replace(root / "namespace.json")
                if daemon.poll() is not None:
                    raise RuntimeError("isolated daemon exited; inspect private smoke logs")
        finally:
            try:
                handover.terminate()
                handover.wait(timeout=10)
                mounted = any(line.split()[4] == str(path) for line in Path('/proc/self/mountinfo').read_text().splitlines())
                assert path.read_bytes() == original_config and not mounted, "hot trial did not restore original config"
                assert daemon.poll() is None, "daemon exited during hot handover"
            finally:
                daemon.terminate()
                try:
                    daemon.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    daemon.kill()
                    daemon.wait()
                fixture.shutdown()
                second_fixture.shutdown()
                broker.terminate()
                broker.wait(timeout=5)
                for directory, folders, files in os.walk(root):
                    for name in [*folders, *files]:
                        os.chown(Path(directory) / name, uid, gid, follow_symlinks=False)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--namespace", type=Path)
    parser.add_argument("--uid", type=int)
    parser.add_argument("--gid", type=int)
    args = parser.parse_args()
    if args.namespace:
        namespace(args.namespace, args.uid, args.gid)
        return
    if os.geteuid() == 0:
        raise RuntimeError("run the controller as your ordinary user, with sudo available")
    os.umask(0o077)
    runtime = Path(os.environ["XDG_RUNTIME_DIR"])
    root = Path(tempfile.mkdtemp(prefix="snitchgate-kernel-", dir=runtime))
    (root / "broker.toml").write_text("request_timeout_seconds=3\n")
    with (root / "broker.log").open("w") as log:
        worker = None
        child = None
        try:
            path = root / "runtime/control.sock"
            worker = subprocess.Popen(["sudo", "-n", "unshare", "--net", "--mount", "--propagation", "private",
                                       sys.executable, "-I", str(Path(__file__).resolve()), "--namespace", str(root),
                                       "--uid", str(os.getuid()), "--gid", str(os.getgid())],
                                      stdin=subprocess.DEVNULL, stdout=log, stderr=log)
            wait_for((root / "namespace.json").exists)
            child = json.loads((root / "namespace.json").read_text())["pid"]
            wait_for(lambda: control(path, {"command": "snapshot"})["snapshot"]["health"]["daemon"])
            for action in ["allow", "deny", "timeout"]:
                curl = subprocess.Popen(["sudo", "-n", "nsenter", "-t", str(child), "-n", "/usr/bin/curl",
                                         "--silent", "--show-error", "--noproxy", "*", "--max-time", "8",
                                         "http://127.0.0.1:18081/"], stdout=subprocess.PIPE, stderr=subprocess.PIPE)
                entries = wait_for(lambda: control(path, {"command": "snapshot"})["snapshot"]["pending"])
                entry = entries[0]
                if action != "timeout":
                    control(path, {"command": "decide", "id": entry["id"], "action": action, "duration": "once"})
                stdout, _ = curl.communicate(timeout=12)
                if action == "allow":
                    assert curl.returncode == 0 and stdout == b"snitchgate-kernel-fixture", "allow did not reach fixture"
                else:
                    assert curl.returncode != 0 and not stdout, "denied traffic reached fixture"
                print(f"isolated kernel enforcement: {action} passed", flush=True)
            assert not saved_rules(root / "rules"), "temporary decisions persisted"
            def probe(address="127.0.0.1:18081", executable="/usr/bin/curl", different_user=False):
                command=["sudo","-n","nsenter","-t",str(child),"-n"]
                if different_user:
                    command += ["setpriv","--reuid",str(os.getuid()),"--regid",str(os.getgid()),"--clear-groups"]
                return subprocess.Popen(command+[executable,"--silent","--show-error","--noproxy","*","--max-time","8",f"http://{address}/"],stdout=subprocess.PIPE,stderr=subprocess.PIPE)
            def reached(process):
                stdout,_=process.communicate(timeout=12)
                assert process.returncode==0 and stdout==b"snitchgate-kernel-fixture", "expected scoped allow did not reach fixture"
            def restart_daemon(generation):
                subprocess.run(["sudo","-n","kill","-USR1",str(child)],check=True)
                wait_for(lambda:json.loads((root/"namespace.json").read_text()).get("generation",0)>=generation)
            process=probe()
            entry=wait_for(lambda:control(path,{"command":"snapshot"})["snapshot"]["pending"])[0]
            control(path,{"command":"decide_scope","id":entry["id"],"action":"allow","duration":"until_restart","match_scope":"executable"})
            reached(process)
            reached(probe("127.0.0.2:18082"))
            assert not control(path,{"command":"snapshot"})["snapshot"]["pending"], "executable scope prompted for another address/port"
            assert not saved_rules(root/"rules"), "until-restart scope persisted"
            restart_daemon(1)
            process=probe("127.0.0.2:18082")
            entry=wait_for(lambda:control(path,{"command":"snapshot"})["snapshot"]["pending"])[0]
            control(path,{"command":"decide_scope","id":entry["id"],"action":"allow","duration":"forever","match_scope":"executable"})
            reached(process)
            persisted=wait_for(lambda:saved_rules(root/"rules"))
            assert len(persisted)==1
            rule=persisted[0]
            assert rule["duration"]=="always"
            constraints={operator["operand"]:operator["data"] for operator in rule["operator"]["list"]}
            assert constraints=={"process.path":"/usr/bin/curl","user.id":str(entry["connection"]["uid"])}, constraints
            restart_daemon(2)
            reached(probe())
            assert not control(path,{"command":"snapshot"})["snapshot"]["pending"], "permanent rule was not reloaded"
            for executable,different_user in [("/usr/bin/curl",True),(str(root/"other-curl"),False)]:
                if not different_user:
                    shutil.copyfile("/usr/bin/curl",root/"other-curl");(root/"other-curl").chmod(0o700)
                process=probe(executable=executable,different_user=different_user)
                wait_for(lambda:control(path,{"command":"snapshot"})["snapshot"]["pending"])
                stdout,_=process.communicate(timeout=12)
                assert process.returncode!=0 and not stdout, "permanent executable rule crossed a user or path boundary"
            print("isolated kernel enforcement: executable any-destination, until-restart expiry, permanent reload, UID and path boundaries passed",flush=True)

        except BaseException:
            print(f"Private smoke evidence retained at {root}", file=sys.stderr)
            raise
        finally:
            if child:
                subprocess.run(["sudo", "-n", "kill", "-TERM", str(child)], check=False)
            if worker:
                try:
                    status = worker.wait(timeout=15)
                    if status:
                        raise RuntimeError("isolated namespace cleanup or rollback failed")
                except subprocess.TimeoutExpired:
                    worker.terminate()
    print("PASS: isolated daemon allow/deny/timeout, reusable scopes, permanent reload and UID/path boundaries; host policy unchanged.")


if __name__ == "__main__":
    main()

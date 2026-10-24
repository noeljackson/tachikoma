#!/usr/bin/env python3
"""Run inside the Docker test stage. All services, providers and credentials are fixtures."""
import http.client
import importlib.util
import json
import os
from pathlib import Path
import socket
import socketserver
import subprocess
import tempfile
import threading
import time
import unittest
import urllib.parse

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location("external", ROOT / "examples/external-adapter/service.py")
external = importlib.util.module_from_spec(spec)
spec.loader.exec_module(external)

class Fixture:
    def __init__(self, path, response):
        self.path = path
        self.requests = []
        fixture = self
        from http.server import BaseHTTPRequestHandler
        class Handler(BaseHTTPRequestHandler):
            def do_POST(self):
                data = self.rfile.read(int(self.headers.get("Content-Length", 0)))
                fixture.requests.append(json.loads(data))
                body = json.dumps(response(json.loads(data)) if callable(response) else response).encode()
                self.send_response(200)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)
            def log_message(self, *args):
                pass
        self.server = socketserver.UnixStreamServer(str(path), Handler)
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()
    def close(self):
        self.server.shutdown()
        self.server.server_close()
        self.thread.join()

def until(predicate, timeout=12):
    limit = time.monotonic() + timeout
    while time.monotonic() < limit:
        result = predicate()
        if result:
            return result
        time.sleep(0.05)
    raise AssertionError("fixture timed out")

class ContractTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.root = Path(self.directory.name)
        self.socket = self.root / "api.sock"
        self.children = []
        self.fixtures = []
        listener = socket.socket()
        listener.bind(("127.0.0.1", 0))
        self.port = listener.getsockname()[1]
        listener.close()
        self.start_daemon()
        self.admin = external.API(self.socket, self.root / "approval/admin.token")
    def start_daemon(self):
        self.daemon = self.child([os.environ.get("TACHIKOMA_TEST_DAEMON", str(ROOT / "target/release/tachikomad")), "--database", str(self.root / "legacy.sqlite3"), "--rpc-socket", str(self.socket), "--listen", f"127.0.0.1:{self.port}"] + (["--legacy-web"] if getattr(self,"legacy_web",False) else []))
        until(lambda: self.socket.exists() and (self.root / "approval/admin.token").exists())
    def child(self, command, env=None):
        child = subprocess.Popen(command, env=env, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        self.children.append(child)
        return child
    def tearDown(self):
        for child in reversed(self.children):
            if child.poll() is None:
                child.terminate()
                try:
                    child.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    child.kill()
                    child.wait()
            if child.stdout:
                child.stdout.close()
            if child.stderr:
                child.stderr.close()
        for fixture in self.fixtures:
            fixture.close()
        self.directory.cleanup()
    def enroll(self, name, role, queue, kind="example", realm=""):
        value = self.admin.call("POST", "/principals", {"id": name, "role": role, "queues": [queue], "service_kind": kind, "realm": realm, "actors": []})
        path = self.root / (name + ".token")
        descriptor = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
        with os.fdopen(descriptor, "w") as file:
            file.write(value["credential"])
        return path
    def example_queue(self):
        return {"id": "example.review", "title": "External fixture", "service_kind": "example", "schema_id": "example.subject.v1", "subject_shape": {"type": "object", "fields": {"target": {"type": "string", "max_len": 128}}, "required": ["target"]}, "actions": [{"id": "allow-once", "label": "Allow once", "effect": "permit", "scope": {"uses": 1}}, {"id": "deny-once", "label": "Deny once", "effect": "block", "scope": {"uses": 1}}], "max_pending": 16, "deadline_cap_ms": 60_000}
    def read_state(self, path):
        try:
            return json.loads(path.read_text())
        except (FileNotFoundError, json.JSONDecodeError):
            return {}
    def run_example(self, credential, state, extra=()):
        return self.child(["python3", str(ROOT / "examples/external-adapter/service.py"), "--socket", str(self.socket), "--credential", str(credential), "--state", str(state), *extra])
    def pending(self, queue):
        return until(lambda: next((r for r in self.admin.call("GET", "/requests")["reviews"] if r["request"]["queue"] == queue), None))
    def test_external_service_runs_without_workspace_imports_and_consumes_once(self):
        self.admin.call("POST", "/queues", self.example_queue())
        token = self.enroll("external", "producer", "example.review")
        state = self.root / "operation.json"
        child = self.run_example(token, state)
        review = self.pending("example.review")
        self.admin.call("POST", f"/requests/{review['id']}/decision", {"revision": 1, "action_id": "allow-once"})
        self.assertEqual(child.wait(timeout=8), 0)
        first = self.read_state(state)
        self.assertIn("simulated_effect", first)
        replay = self.run_example(token, state)
        self.assertEqual(replay.wait(timeout=8), 0)
        self.assertEqual(first, self.read_state(state))
        receipt = self.admin.call("GET", f"/requests/{review['id']}")["receipt"]
        self.assertEqual(receipt["state"], "enforced")
    def test_optional_jsonl_source_correlates_with_an_independent_service_without_deciding(self):
        self.admin.call('POST','/queues',self.example_queue())
        spec={'id':'external.sensor','title':'External fixture sensor','adapter':'jsonl','enabled':True,'queues':['example.review'],'max_age_ms':30000,'coverage':'Fixture records only','capabilities':{'example.scan':{'type':'object','fields':{'entity':{'type':'string','max_len':128}},'required':['entity']}}}
        self.admin.call('POST','/evidence/sources',spec)
        source=self.enroll('sensor','source','example.review',realm='external.sensor')
        producer=self.enroll('example-producer','producer','example.review')
        source_api=external.API(self.socket,source)
        path=self.root/'observations.jsonl';path.touch()
        collector=self.child([str(ROOT/'target/debug/tachikoma-source'),'--socket',str(self.socket),'--credential',str(source),'--source','external.sensor','--queue','example.review','--format','jsonl','--input',str(path)])
        until(lambda:self.admin.call('GET','/evidence/sources')[0]['ready'])
        event={'id':'fixture-scan','queue':'example.review','capability':'example.scan','observed_at_ms':int(time.time()*1000)-100,'data':{'entity':'example.invalid'}}
        with path.open('a') as file:file.write(json.dumps(event)+'\n')
        self.run_example(producer,self.root/'shadow.json')
        review=self.pending('example.review')
        policy={'queue':'example.review','default':{'kind':'ask'},'rules':[{'id':'scan-match','description':'Independent source and producer correlation','requires':[],'when':{'op':'observed','source':'external.sensor','capability':'example.scan','within_ms':30000,'joins':{'/entity':'/subject/target'},'equals':{}},'then':{'kind':'action','action_id':'allow-once'},'on_unavailable':'ask'}]}
        self.admin.call('POST','/evidence/policies',policy)
        preview=until(lambda:(p if p['outcome']['kind']=='action' else None) if (p:=self.admin.call('GET',f"/requests/{review['id']}/evidence-preview")) else None)
        self.assertFalse(preview['authoritative'])
        self.assertEqual(preview['traces'][0]['observations'][0]['id'],'fixture-scan')
        self.assertIsNone(self.admin.call('GET','/requests/'+review['id'])['decision'])
        with self.assertRaises(RuntimeError):source_api.call('GET','/requests')
        with self.assertRaises(RuntimeError):source_api.call('POST','/evidence/policies',policy)
        with self.assertRaises(RuntimeError):source_api.call('POST','/evidence/sources/other/observations',event)
        self.admin.call('POST','/principals/sensor/revoke',{})
        self.assertEqual(self.admin.call('GET',f"/requests/{review['id']}/evidence-preview")['outcome']['kind'],'ask')
        collector.terminate();collector.wait(timeout=5)
    def test_restart_after_consumption_reports_unknown_without_repeating_effect(self):
        self.admin.call("POST", "/queues", self.example_queue())
        token = self.enroll("external", "producer", "example.review")
        state = self.root / "operation.json"
        child = self.run_example(token, state, ["--crash-after-consume"])
        review = self.pending("example.review")
        self.admin.call("POST", f"/requests/{review['id']}/decision", {"revision": 1, "action_id": "allow-once"})
        self.assertEqual(child.wait(timeout=8), 17)
        self.assertNotIn("simulated_effect", self.read_state(state))
        replay = self.run_example(token, state)
        self.assertEqual(replay.wait(timeout=8), 0)
        self.assertNotIn("simulated_effect", self.read_state(state))
        self.assertEqual(self.admin.call("GET", f"/requests/{review['id']}")["receipt"]["state"], "unknown")
    def test_tui_pty_navigation_and_typing_never_settle_a_request(self):
        import fcntl
        import pty
        import select
        import struct
        import termios
        queue = self.example_queue()
        queue['assessment'] = {'instructions':'Explain fixture facts','labels':{'unknown':'Unknown'},'subject_fields':['target'],'evidence_fields':[],'jev':False,'llm':True,'models':[]}
        self.admin.call('POST','/queues',queue)
        producer = self.enroll('terminal-producer','producer','example.review')
        self.run_example(producer,self.root/'terminal-operation.json')
        review = self.pending('example.review')
        master,slave = pty.openpty()
        fcntl.ioctl(slave,termios.TIOCSWINSZ,struct.pack('HHHH',40,140,0,0))
        environment = dict(os.environ,TERM='xterm-256color')
        terminal = subprocess.Popen([str(ROOT/'target/debug/tachikoma'),'--rpc-socket',str(self.socket),'approval','--credential',str(self.root/'approval/admin.token'),'tui'],stdin=slave,stdout=slave,stderr=slave,env=environment,start_new_session=True)
        self.children.append(terminal)
        os.close(slave)
        output = bytearray()
        def drain(marker=None,timeout=3):
            limit=time.monotonic()+timeout
            start=len(output)
            while time.monotonic()<limit:
                if select.select([master],[],[],.05)[0]:
                    try:data=os.read(master,65536)
                    except OSError:break
                    if not data:break
                    output.extend(data)
                    if marker and marker in output[start:]:return
                elif marker is None:return
            if marker:raise AssertionError('TUI did not render expected state: '+marker.decode())
        try:
            drain(b'Requests')
            os.write(master,b'\x1b[C\x1b[D\x1b[D\x1b[B\x1b[C')
            drain()
            os.write(master,b'/');drain(b'Filter requests')
            os.write(master,b'iynq\x1b[Dx\r');drain()
            os.write(master,b'\x1b');time.sleep(.1);drain()
            os.write(master,b'i');drain(b'Investigation')
            os.write(master,b'ynq\x1b[Dx');drain()
            os.write(master,b'\x1b');time.sleep(.1);drain()
            os.write(master,b'\x1b');time.sleep(.1);drain()
            os.write(master,b'?');drain(b'Keyboard help')
            os.write(master,b'\x1b');time.sleep(.1);drain()
            os.write(master,b'q')
            self.assertEqual(terminal.wait(timeout=5),0)
            self.assertEqual(self.admin.call('GET','/requests/'+review['id'])['state'],'pending')
            self.assertEqual(self.admin.call('GET','/requests/'+review['id']+'/conversation')['messages'],[])
        finally:
            if terminal.poll() is None:terminal.terminate();terminal.wait(timeout=5)
            os.close(master)
    def test_native_is_default_and_local_desktop_credential_cannot_administer(self):
        connection=http.client.HTTPConnection("127.0.0.1",self.port)
        connection.request("GET","/")
        response=connection.getresponse(); body=response.read().decode(); connection.close()
        self.assertEqual(response.status,200); self.assertIn("tachikoma-gui",body)
        connection=http.client.HTTPConnection("127.0.0.1",self.port)
        connection.request("GET","/approval/")
        response=connection.getresponse(); response.read(); connection.close()
        self.assertEqual(response.status,404)
        path=self.root/"approval/desktop.token"
        self.assertEqual(path.stat().st_mode & 0o777,0o600)
        desktop=external.API(self.socket,path)
        self.assertEqual(desktop.call("GET","/identity")["role"],"reviewer")
        self.assertEqual(desktop.call("GET","/identity")["id"],"local-desktop")
        with self.assertRaisesRegex(RuntimeError,"HTTP 403"):
            desktop.call("POST","/principals",{"id":"desktop-escalation","role":"admin","queues":[],"service_kind":"","realm":"","actors":[]})
    def test_local_web_opens_without_credentials_and_requires_origin_and_csrf(self):
        self.daemon.terminate(); self.daemon.wait(timeout=5)
        self.legacy_web=True; self.start_daemon()
        connection = http.client.HTTPConnection("127.0.0.1", self.port)
        connection.request("GET", "/approval/", headers={"Host":"evil.example"})
        response=connection.getresponse()
        self.assertEqual(response.status,403)
        response.read();connection.close()
        connection = http.client.HTTPConnection("127.0.0.1", self.port)
        connection.request("GET", "/approval/")
        response=connection.getresponse()
        self.assertEqual(response.status,200)
        self.assertEqual(response.getheader("Referrer-Policy"),"same-origin")
        cookie=response.getheader("Set-Cookie")
        self.assertIn("HttpOnly",cookie);self.assertIn("SameSite=Strict",cookie)
        html=response.read().decode()
        self.assertIn("Local review",html)
        self.assertNotIn("type=password",html)
        self.assertNotIn((self.root/"approval/admin.token").read_text(),html)
        connection.close()
        for origin in [None,"null",f"http://127.0.0.1:{self.port}"]:
            connection=http.client.HTTPConnection("127.0.0.1",self.port)
            headers={"Cookie":cookie.split(";")[0],"Content-Type":"application/x-www-form-urlencoded"}
            if origin is not None:headers["Origin"]=origin
            connection.request("POST","/approval/requests/nonexistent/decide",urllib.parse.urlencode({"csrf":"wrong","revision":1,"action_id":"allow-once"}),headers)
            response=connection.getresponse();self.assertEqual(response.status,403)
            response.read();connection.close()
    def test_legacy_import_preserves_source_and_never_grants_authority(self):
        import sqlite3
        import hashlib
        source = self.root / "original.sqlite3"
        with sqlite3.connect(source) as db:
            db.execute("CREATE TABLE proposals(id,adapter,action,state,risk,created_at,updated_at)")
            db.execute("INSERT INTO proposals VALUES('old','opensnitch','allow','approved','low','1','2')")
        before = hashlib.sha256(source.read_bytes()).hexdigest()
        command = [str(ROOT / "target/debug/tachikoma"), "--rpc-socket", str(self.socket), "approval", "--credential", str(self.root / "approval/admin.token"), "import-legacy", str(source)]
        for _ in range(2):
            result = subprocess.run(command, capture_output=True, timeout=6)
            self.assertEqual(result.returncode, 0, result.stderr.decode())
        history = self.admin.call("GET", "/requests?state=history")["reviews"]
        self.assertEqual(len(history), 1)
        self.assertEqual(history[0]["state"], "observed")
        self.assertFalse(history[0]["decision"]["authoritative"])
        self.assertEqual(history[0]["request"]["subject"]["legacy_state"], "approved")
        self.assertEqual(hashlib.sha256(source.read_bytes()).hexdigest(), before)
    def test_historical_conversation_uses_worker_and_cannot_change_decision(self):
        queue = self.example_queue()
        queue["assessment"] = {"instructions":"Explain the facts", "labels":{"unknown":"Unknown"}, "subject_fields":["target"], "evidence_fields":[], "jev":False, "llm":True, "models":["fixture"]}
        self.admin.call("POST", "/queues", queue)
        token = self.enroll("external", "producer", "example.review")
        worker = self.enroll("assessment", "worker", "example.review", "")
        child = self.run_example(token, self.root / "operation.json")
        review = self.pending("example.review")
        self.admin.call("POST", f"/requests/{review['id']}/decision", {"revision":1,"action_id":"deny-once"})
        self.assertEqual(child.wait(timeout=8), 0)
        input = {"message":"What do we know?", "model":"fixture", "idempotency_key":"chat-first"}
        chat = self.admin.call("POST", f"/requests/{review['id']}/conversation", input)
        self.assertEqual(chat["status"], "running")
        self.assertEqual(len(self.admin.call("POST", f"/requests/{review['id']}/conversation", input)["messages"]), 1)
        llm = Fixture(self.root / "llm.sock", {"model":"fixture","done":True,"message":{"content":json.dumps({"explanation":"Only the supplied target is known."})}})
        self.fixtures.append(llm)
        completed = subprocess.run([str(ROOT / "target/debug/tachikoma-assessment-worker"), "--socket", str(self.socket), "--credential", str(worker), "--llm-endpoint", "http://fixture.invalid/chat", "--llm-socket", str(llm.path), "--allow-model", "fixture", "--once"], capture_output=True, timeout=6)
        self.assertEqual(completed.returncode, 0)
        chat = self.admin.call("GET", f"/requests/{review['id']}/conversation")
        self.assertEqual(chat["status"], "idle")
        self.assertEqual(chat["messages"][-1]["content"], "Only the supplied target is known.")
        self.assertEqual(self.admin.call("GET", f"/requests/{review['id']}")["state"], "denied")
        self.assertEqual(llm.requests[0]["model"], "fixture")
    def start_broker(self, token, timeout=5):
        config = self.root / "broker.toml"
        config.write_text(f"request_timeout_seconds={timeout}\n[tachikoma]\nsocket={json.dumps(str(self.socket))}\nproducer_id='network'\nqueue='network.egress'\ncredential_env='TACHIKOMA_API_TOKEN'\n")
        environment = os.environ.copy()
        environment["TACHIKOMA_API_TOKEN_FILE"] = str(token)
        runtime = self.root / "broker-runtime"
        child = self.child([str(ROOT / "target/debug/snitchgate"), "serve", "--runtime-dir", str(runtime), "--state-dir", str(self.root / "broker-state"), "--config", str(config)], env=environment)
        until(lambda: (runtime / "opensnitch.sock").exists())
        return child, runtime
    def test_invalid_first_stage_leaves_request_manual_without_calling_category(self):
        queue=self.example_queue()
        queue['assessment']={'instructions':'Classify fixture','labels':{'unknown':'Unknown'},'subject_fields':['target'],'jev':True,'llm':False,'questions':{'origin':{'instructions':'Identify origin','labels':{'unknown':'Unknown'}}}}
        self.admin.call('POST','/queues',queue)
        producer=self.enroll('producer','producer','example.review')
        worker=self.enroll('worker','worker','example.review','')
        self.run_example(producer,self.root/'unavailable.json')
        review=self.pending('example.review')
        fixture=Fixture(self.root/'invalid-stage.sock',{'model':'fixture','answers':{'origin':{'type':'choice','choice':'invented-allow','confidence':1.0}}})
        self.fixtures.append(fixture)
        key=self.root/'key'
        descriptor=os.open(key,os.O_WRONLY|os.O_CREAT|os.O_EXCL,0o600)
        with os.fdopen(descriptor,'w') as file:file.write('fixture-only')
        result=subprocess.run([str(ROOT/'target/debug/tachikoma-assessment-worker'),'--socket',str(self.socket),'--credential',str(worker),'--jev-endpoint','http://fixture.invalid/jev','--jev-socket',str(fixture.path),'--jev-key-file',str(key),'--once'],capture_output=True,timeout=6)
        self.assertEqual(result.returncode,0)
        current=self.admin.call('GET','/requests/'+review['id'])
        self.assertEqual(current['state'],'pending')
        self.assertIsNone(current['decision'])
        self.assertEqual(current['assessment']['state'],'unavailable')
        self.assertEqual(current['assessment']['judgments'],{})
        self.assertEqual(len(fixture.requests),1)
    def network_queue(self):
        queue = json.loads((ROOT / "contrib/adapters/snitchgate.queue.json").read_text())
        return queue
    def test_network_bridge_classifies_and_returns_a_scoped_decision_once(self):
        self.admin.call("POST", "/queues", self.network_queue())
        token = self.enroll("network", "producer", "network.egress", "snitchgate")
        worker = self.enroll("assessment", "worker", "network.egress", "")
        _, runtime = self.start_broker(token, timeout=8)
        mock = self.child([str(ROOT / "target/debug/snitchgate"), "mock", "--socket", str(runtime / "opensnitch.sock")])
        review = self.pending("network.egress")
        def jev_response(request):
            choices = {'category':'development','activity':'local_test','origin':'project_test'}
            return {'model':'jev-fixture','answers':{name:{'type':'choice','choice':choices[name],'confidence':0.97,'probabilities':{label:1.0 if label==choices[name] else 0.0 for label in question['criteria']}} for name,question in request['questions'].items()}}
        jev = Fixture(self.root / "jev.sock", jev_response)
        llm = Fixture(self.root / "llm.sock", {"model":"qwen-fixture","done":True,"done_reason":"stop","message":{"content":json.dumps({"explanation":"Fixture facts are insufficient to prove intent."})}})
        self.fixtures.extend([jev, llm])
        key = self.root / "provider.token"
        descriptor = os.open(key, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
        with os.fdopen(descriptor, "w") as file:
            file.write("fixture-provider-key")
        completed = subprocess.run([str(ROOT / "target/debug/tachikoma-assessment-worker"), "--socket", str(self.socket), "--credential", str(worker), "--jev-endpoint", "http://fixture.invalid/jev", "--jev-socket", str(jev.path), "--jev-key-file", str(key), "--llm-endpoint", "http://fixture.invalid/chat", "--llm-socket", str(llm.path), "--once"], capture_output=True, timeout=6)
        self.assertEqual(completed.returncode, 0)
        classified = self.admin.call("GET", f"/requests/{review['id']}")
        self.assertEqual(classified["assessment"]["category"], "development")
        self.assertEqual(classified["assessment"]["llm_model"], "qwen-fixture")
        self.assertEqual(classified["state"], "pending")
        self.assertEqual(len(jev.requests),2)
        self.assertEqual(classified['assessment']['judgments']['activity']['choice'],'local_test')
        self.assertIn('process_context',jev.requests[0]['state']['current_request']['evidence'])
        self.assertNotIn('preliminary_model_opinions',jev.requests[0]['state'])
        self.assertEqual(jev.requests[1]['state']['preliminary_model_opinions']['origin']['choice'],'project_test')
        self.assertEqual(set(jev.requests[1]['questions']),{'category'})
        self.assertGreater(len(jev.requests[1]['state']['reference_examples']),3)
        policy={'queue':'network.egress','default':{'kind':'ask'},'rules':[{'id':'decimal-score','description':'Decimal confidence shadow preview','requires':[],'when':{'op':'at_least','path':'/assessment/confidence','value':0.9},'then':{'kind':'action','action_id':'allow-once'},'on_unavailable':'ask'}]}
        first=self.admin.call('POST','/evidence/policies',policy)
        second=self.admin.call('POST','/evidence/policies',policy)
        self.assertEqual(first['revision'],second['revision'])
        preview=self.admin.call('GET',f"/requests/{review['id']}/evidence-preview")
        self.assertFalse(preview['authoritative'])
        self.assertEqual(preview['outcome']['kind'],'action')
        self.assertEqual(len(preview['snapshot_digest']),64)
        self.assertIsNone(self.admin.call('GET',f"/requests/{review['id']}")['decision'])
        self.assertNotIn("url_hints", json.dumps(jev.requests))
        self.assertNotIn("resource_details", json.dumps(llm.requests))
        self.admin.call("POST", f"/requests/{review['id']}/decision", {"revision":1,"action_id":"allow-once"})
        self.assertEqual(mock.wait(timeout=5), 0)
        self.assertIn("allow / once", mock.stdout.read().decode())
        settled = until(lambda: self.admin.call("GET", f"/requests/{review['id']}") if self.admin.call("GET", f"/requests/{review['id']}").get("receipt") else None)
        self.assertEqual(settled["receipt"]["state"], "unknown")
    def test_network_bridge_rejects_broadened_scope_and_times_out_denied(self):
        queue = self.network_queue()
        queue["assessment"] = None
        queue["actions"][0]["scope"] = {"duration":"always"}
        self.admin.call("POST", "/queues", queue)
        token = self.enroll("network", "producer", "network.egress", "snitchgate")
        _, runtime = self.start_broker(token, timeout=2)
        mock = self.child([str(ROOT / "target/debug/snitchgate"), "mock", "--socket", str(runtime / "opensnitch.sock")])
        review = self.pending("network.egress")
        self.admin.call("POST", f"/requests/{review['id']}/decision", {"revision":1,"action_id":"allow-once"})
        self.assertEqual(mock.wait(timeout=5), 0)
        self.assertIn("deny / once", mock.stdout.read().decode())
    def test_network_bridge_keeps_local_deadline_during_control_plane_outage(self):
        queue = self.network_queue()
        queue["assessment"] = None
        self.admin.call("POST", "/queues", queue)
        token = self.enroll("network", "producer", "network.egress", "snitchgate")
        _, runtime = self.start_broker(token, timeout=2)
        mock = self.child([str(ROOT / "target/debug/snitchgate"), "mock", "--socket", str(runtime / "opensnitch.sock")])
        self.pending("network.egress")
        self.daemon.terminate()
        self.daemon.wait(timeout=5)
        self.assertEqual(mock.wait(timeout=5), 0)
        self.assertIn("deny / once", mock.stdout.read().decode())

    def test_kubernetes_producer_is_observation_only(self):
        binary = ROOT / "target/debug/kubernetesgate"
        result = subprocess.run([str(binary), "manifest"], check=True, capture_output=True)
        self.admin.call("POST", "/queues", json.loads(result.stdout))
        token = self.enroll("cluster", "producer", "kubernetes.observe", "kubernetesgate")
        result = subprocess.run([str(binary), "--socket", str(self.socket), "--credential", str(token), "suggest", "--context", "fixture", "--", "get", "pods"], capture_output=True)
        self.assertEqual(result.returncode, 0)
        review = json.loads(result.stdout)
        self.assertTrue(review["request"]["subject"]["observation_only"])
        self.assertEqual(review["actions"][0]["effect"], "observe")
        self.assertFalse(any(a["effect"] == "permit" for a in review["actions"]))

if __name__ == "__main__":
    unittest.main()

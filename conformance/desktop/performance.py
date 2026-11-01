#!/usr/bin/python3
"""Large history + bounded API traffic and search input; private Docker fixtures."""
import importlib.util,json,os,sqlite3,socket,socketserver,http.client
from http.server import BaseHTTPRequestHandler
from pathlib import Path
import subprocess,threading,time,uuid,unittest,sys
ROOT=Path(__file__).resolve().parents[2]
spec=importlib.util.spec_from_file_location('notifications',Path(__file__).with_name('notifications.py'))
n=importlib.util.module_from_spec(spec);spec.loader.exec_module(n)
class HTTP(http.client.HTTPConnection):
 def __init__(self,path):super().__init__('local',timeout=33);self.path=path
 def connect(self):self.sock=socket.socket(socket.AF_UNIX);self.sock.connect(str(self.path))
class Proxy(socketserver.ThreadingUnixStreamServer):daemon_threads=True
class Performance(unittest.TestCase):
 def test_large_history_does_not_drive_idle_requests_and_search_is_on_demand(self):
  f=n.contract.ContractTests();self.addCleanup(f.tearDown);f.setUp()
  shell=n.Shell();self.addCleanup(shell.close)
  manifest=json.loads((ROOT/'contrib/adapters/snitchgate.queue.json').read_text());manifest['assessment']=None
  f.admin.call('POST','/queues',manifest)
  producer=n.contract.external.API(f.socket,f.enroll('performance-producer','producer',manifest['id'],'snitchgate'))
  identifier=uuid.uuid4().hex
  submitted={'queue':manifest['id'],'producer_request_id':identifier,'nonce':identifier,'idempotency_key':identifier,'schema_id':manifest['schema_id'],
   'subject':{'process':'/usr/bin/example','uid':1000,'pid':1,'ip':'192.0.2.1','host':'example.invalid','port':443,'protocol':'tcp'},
   'evidence':{},'action_ids':[a['id'] for a in manifest['actions']],'expires_at_ms':int(time.time()*1000)+90000,'summary':'Current pending review'}
  pending=producer.call('POST','/requests',submitted)
  db=sqlite3.connect(f.root/'approval/approval.sqlite3')
  with db:
   for age in range(10000,0,-1):
    old=json.loads(json.dumps(pending));old['id']='history-'+str(age);old['state']='expired';old['created_at_ms']-=age*1000;old['expires_at_ms']=0
    old['request']['summary']='old-search-needle' if age==10000 else 'History fixture'
    old['request']['idempotency_key']=old['id']
    db.execute("INSERT INTO requests(id,producer,queue,idempotency,fingerprint,state,expires,body) VALUES(?,?,?,?,'fixture','expired',0,?)",(old['id'],old['producer'],manifest['id'],old['id'],json.dumps(old)))
  db.close()
  records=[];lock=threading.Lock()
  class Handler(BaseHTTPRequestHandler):
   protocol_version='HTTP/1.1'
   def do_GET(self):self.forward()
   def do_POST(self):self.forward()
   def forward(self):
    start=time.monotonic();upstream=HTTP(f.socket)
    try:
     data=self.rfile.read(int(self.headers.get('Content-Length',0)))
     upstream.request(self.command,self.path,data,dict(self.headers))
     response=upstream.getresponse();body=response.read()
     record={'path':self.path,'at':start,'bytes':len(body)}
     if self.path.endswith('/inbox'):
      value=json.loads(body);record.update(history=len(value['history']['reviews']),pending=len(value['pending']))
     with lock:records.append(record)
     self.send_response(response.status);self.send_header('Content-Length',str(len(body)));self.send_header('Connection','close');self.end_headers();self.wfile.write(body)
    except (OSError,http.client.HTTPException):pass
    finally:upstream.close()
   def log_message(self,*args):pass
  proxy_path=f.root/'proxy.sock';proxy=Proxy(str(proxy_path),Handler);thread=threading.Thread(target=proxy.serve_forever,daemon=True);thread.start()
  self.addCleanup(lambda:(proxy.shutdown(),proxy.server_close(),thread.join(timeout=3)))
  desktop=f.child([os.environ['TACHIKOMA_TEST_GUI'],'--socket',str(proxy_path),'--credential',str(f.root/'approval/desktop.token'),'--request',pending['id']],env=dict(os.environ,XDG_STATE_HOME=str(f.root/'state')))
  window=n.contract.until(lambda:subprocess.run(['xdotool','search','--onlyvisible','--name','^tachikoma$'],capture_output=True,text=True).stdout.strip(),timeout=30)
  subprocess.run(['xdotool','windowfocus','--sync',window],check=True)
  n.contract.until(lambda:any(r['path'].endswith('/inbox') for r in records))
  time.sleep(3)
  first=next(r for r in records if r['path'].endswith('/inbox'));self.assertEqual(first['history'],100);self.assertEqual(first['pending'],1)
  def unused_details():return [r for r in records if '/conversation' in r['path'] or '/evidence-preview' in r['path']]
  self.assertEqual(unused_details(),[])
  before=len([r for r in records if '/requests?' in r['path'] or r['path'].endswith('/inbox')]);time.sleep(4)
  after=len([r for r in records if '/requests?' in r['path'] or r['path'].endswith('/inbox')]);self.assertEqual(before,after)
  subprocess.run(['xdotool','key','--clearmodifiers','ctrl+f'],check=True)
  start=time.monotonic();subprocess.run(['xdotool','type','--clearmodifiers','--delay','0','old-search-needle'],check=True)
  found=n.contract.until(lambda:next((r for r in records if 'search=old-search-needle' in r['path']),None))
  latency=found['at']-start;self.assertLess(latency,2.0)
  self.assertIsNone(desktop.poll());self.assertEqual(unused_details(),[])
  print(json.dumps({'fixture_history':10000,'initial_history':first['history'],'pending':first['pending'],'idle_history_reads':after-before,'search_dispatch_ms':round(latency*1000,1),'unused_detail_calls':len(unused_details())}),flush=True)
if __name__=='__main__':
 try:unittest.main(testRunner=unittest.TextTestRunner(stream=sys.stdout,verbosity=2))
 finally:n.stop_loop()

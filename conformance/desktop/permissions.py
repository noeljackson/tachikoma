#!/usr/bin/python3
"""Real GPUI input and exact approval bindings on a private Docker-only stack."""
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import time
import unittest
import uuid

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location('notification_fixture', Path(__file__).with_name('notifications.py'))
notifications = importlib.util.module_from_spec(spec)
spec.loader.exec_module(notifications)
contract = notifications.contract

class PermissionTests(unittest.TestCase):
    def test_split_menu_modal_focus_and_exact_confirmation(self):
        f = contract.ContractTests(); self.addCleanup(f.tearDown); f.setUp()
        shell = notifications.Shell(); self.addCleanup(shell.close)
        manifest = json.loads((ROOT/'contrib/adapters/snitchgate.queue.json').read_text())
        manifest['assessment'] = None
        f.admin.call('POST', '/queues', manifest)
        producer = contract.external.API(f.socket, f.enroll('permission-producer', 'producer', manifest['id'], 'snitchgate'))
        def submit(label, seconds=90):
            identifier = uuid.uuid4().hex
            return producer.call('POST','/requests', {'queue':manifest['id'],'producer_request_id':identifier,'nonce':identifier,'idempotency_key':identifier,
                'schema_id':manifest['schema_id'],'subject':{'process':'/usr/bin/curl','pid':44,'uid':1000,'protocol':'tcp','ip':'192.0.2.1','host':'registry.example.invalid','port':443},
                'evidence':{}, 'action_ids':[a['id'] for a in manifest['actions']], 'expires_at_ms':int(time.time()*1000)+seconds*1000, 'summary':label})
        def get(review): return f.admin.call('GET','/requests/'+review['id'])
        def key(*keys):
            subprocess.run(['xdotool','key','--clearmodifiers','--delay','80',*keys],check=True)
            time.sleep(.2)
        initial = submit('Review exact network permission')
        command = [os.environ.get('TACHIKOMA_TEST_GUI', str(ROOT/'target/debug/tachikoma-gui')),
            '--socket', str(f.socket), '--credential', str(f.root/'approval/desktop.token')]
        env = dict(os.environ, XDG_STATE_HOME=str(f.root/'state'))
        desktop = f.child(command+['--appearance','light','--request',initial['id']], env=env)
        window = contract.until(lambda: subprocess.run(['xdotool','search','--onlyvisible','--name','^tachikoma$'],capture_output=True,text=True).stdout.strip(), timeout=30)
        subprocess.run(['xdotool','windowfocus','--sync',window],check=True)
        contract.until(lambda: any(s['id']=='local-desktop' and s['detail']=='Desktop notifications available' for s in f.admin.call('GET','/health')['services']))
        time.sleep(2.2)
        key('ctrl+f')
        subprocess.run(['xdotool','type','--clearmodifiers','adAD'],check=True)
        key('ctrl+a','BackSpace','Escape')
        self.assertEqual(get(initial)['state'], 'pending')
        key('ctrl+3', 'a', 'Return')
        self.assertEqual(get(initial)['state'], 'pending')
        key('Escape')
        key('a'); key('Escape')
        self.assertEqual(get(initial)['state'], 'pending')
        key('shift+a', *(['Down']*6))
        # Enter chooses the widest scope, but a held/repeated Enter cannot also approve it.
        subprocess.run(['xdotool','keydown','Return'],check=True)
        time.sleep(1.0)
        self.assertEqual(get(initial)['state'], 'pending')
        subprocess.run(['xdotool','keyup','Return'],check=True)
        time.sleep(.2)
        # Background filtering/search shortcuts must remain inactive inside the modal.
        key('ctrl+1','ctrl+f','Down','Tab','Escape')
        self.assertEqual(get(initial)['state'], 'pending')
        key('ctrl+shift+t')
        key('shift+a', *(['Down']*6)); key('Return')
        key('Escape')
        # A normal once click/shortcut remains once after cancelling the widest option.
        key('a','Return')
        settled = contract.until(lambda: (r if (r:=get(initial))['state']=='allowed' else None))
        self.assertEqual(settled['decision']['action']['id'], 'allow-once')
        self.assertEqual(settled['decision']['actor'], 'local-desktop')
        def activate(review):
            subprocess.run(command+['--request',review['id']], env=env, check=True, timeout=10)
            subprocess.run(['xdotool','windowfocus','--sync',window],check=True)
            time.sleep(2.2)
        broad = submit('Explicit broad approval in isolated fixture')
        activate(broad)
        key('shift+a', *(['Down']*6), 'Return', 'Return')
        settled = contract.until(lambda: (r if (r:=get(broad))['state']=='allowed' else None))
        self.assertEqual(settled['decision']['action']['id'], 'allow-executable-forever')
        self.assertEqual(settled['decision']['action']['scope'], {'duration':'forever','match':'executable'})
        denied = submit('Deny once in isolated fixture')
        activate(denied); key('d','Return')
        settled = contract.until(lambda: (r if (r:=get(denied))['state']=='denied' else None))
        self.assertEqual(settled['decision']['action']['id'], 'deny-once')
        external = submit('Settle from another client')
        untouched = submit('Another request must remain pending')
        activate(external); key('a')
        f.admin.call('POST','/requests/'+external['id']+'/decision',{'revision':external['revision'],'action_id':'deny-once'})
        time.sleep(2.5); key('Return')
        self.assertEqual(get(external)['state'], 'denied')
        self.assertEqual(get(untouched)['state'], 'pending')
        expired = submit('Expired confirmation', seconds=7)
        activate(expired); key('a'); time.sleep(5); key('Return')
        self.assertEqual(get(expired)['state'], 'expired')
        self.assertEqual(get(untouched)['state'], 'pending')
        self.assertIsNone(desktop.poll())
        print('Split dropdown, Enter/Escape, focus trap, held Enter, exact scopes, safe defaults, external settlement and expiry verified')

if __name__ == '__main__':
    try: unittest.main(testRunner=unittest.TextTestRunner(stream=sys.stdout, verbosity=2))
    finally: notifications.stop_loop()

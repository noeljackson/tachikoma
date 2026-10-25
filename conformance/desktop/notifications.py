#!/usr/bin/python3
"""Real daemon, native window and private notification bus; Docker fixtures only."""
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import threading
import time
import unittest

import dbus
import dbus.service
from dbus.mainloop.glib import DBusGMainLoop
from gi.repository import GLib

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location('contract', ROOT/'conformance/adapter/test_contract.py')
contract = importlib.util.module_from_spec(spec)
spec.loader.exec_module(contract)
DBusGMainLoop(set_as_default=True)
loop = GLib.MainLoop()
threading.Thread(target=loop.run, daemon=True).start()
IFACE = 'org.freedesktop.Notifications'
PATH = '/org/freedesktop/Notifications'

class Shell(dbus.service.Object):
    def __init__(self):
        self.bus = dbus.SessionBus(private=True)
        self.bus.set_exit_on_disconnect(False)
        self.owner = dbus.service.BusName(IFACE, self.bus, do_not_queue=True)
        super().__init__(self.owner, PATH)
        self.notifications = {}
        self.calls = []
        self.closed = []
        self.next_id = 0
    @dbus.service.method(IFACE, out_signature='as')
    def GetCapabilities(self): return ['actions', 'body', 'body-markup']
    @dbus.service.method(IFACE, in_signature='susssasa{sv}i', out_signature='u')
    def Notify(self, app, replaces, icon, summary, body, actions, hints, expiry):
        if not replaces: self.next_id += 1
        identifier = int(replaces or self.next_id)
        self.notifications[identifier] = {'summary':str(summary), 'body':str(body), 'actions':list(actions), 'expiry':int(expiry)}
        self.calls.append((identifier, int(replaces)))
        return dbus.UInt32(identifier)
    @dbus.service.method(IFACE, in_signature='u')
    def CloseNotification(self, identifier):
        self.closed.append(int(identifier))
        self.NotificationClosed(identifier, 3)
    @dbus.service.signal(IFACE, signature='us')
    def ActionInvoked(self, identifier, action): pass
    @dbus.service.signal(IFACE, signature='uu')
    def NotificationClosed(self, identifier, reason): pass
    def close(self):
        self.remove_from_connection()
        self.bus.release_name(IFACE)
        self.owner = None

class NotificationTests(unittest.TestCase):
    def test_native_notifications_and_closed_window_keep_exact_manual_decisions(self):
        print('Starting isolated notification lifecycle test', flush=True)
        f = contract.ContractTests(); self.addCleanup(f.tearDown); f.setUp()
        print('Approval fixture ready', flush=True)
        shell = Shell(); self.addCleanup(lambda: shell.close())
        print('Notification fixture ready', flush=True)
        manifest = json.loads((ROOT/'contrib/adapters/snitchgate.queue.json').read_text())
        # No provider fixtures are needed for manual authorization.
        manifest['assessment'] = None
        f.admin.call('POST', '/queues', manifest)
        producer = contract.external.API(f.socket, f.enroll('notify-producer','producer',manifest['id'],'snitchgate'))
        env = dict(os.environ, XDG_STATE_HOME=str(f.root/'state'))
        command = [os.environ.get('TACHIKOMA_TEST_GUI', str(ROOT/'target/debug/tachikoma-gui')), '--socket', str(f.socket), '--credential', str(f.root/'approval/desktop.token')]
        desktop = f.child(command+['--background'], env=env)
        print('Desktop started', flush=True)
        def ready():
            return any(s['id']=='local-desktop' and s['detail']=='Desktop notifications available' for s in f.admin.call('GET','/health')['services'])
        try:
            contract.until(ready, timeout=30)
        except AssertionError:
            desktop.terminate(); desktop.wait(timeout=5)
            print(desktop.stderr.read().decode(), flush=True)
            raise
        def submit(label, seconds=40):
            return producer.call('POST','/requests', {'queue':manifest['id'],'producer_request_id':label,'nonce':label,'idempotency_key':label,
                'schema_id':manifest['schema_id'],'subject':{'process':'/usr/bin/test-fixture','pid':44,'uid':1000,'protocol':'tcp','ip':'192.0.2.1','host':label+'.example.invalid','port':443},
                'evidence':{},'action_ids':[a['id'] for a in manifest['actions']], 'expires_at_ms':int(time.time()*1000)+seconds*1000,'summary':label})
        def notification(label):
            return next((i for i,n in shell.notifications.items() if n['summary']==label and i not in shell.closed), None)
        def state(review): return f.admin.call('GET','/requests/'+review['id'])['state']
        first = submit('notification-allow')
        identifier = contract.until(lambda: notification('notification-allow'))
        self.assertEqual(shell.notifications[identifier]['actions'], ['default','Review','allow-once','Allow once','deny-once','Deny once'])
        # Another bus client cannot forge the daemon's action signal.
        rogue = dbus.SessionBus(private=True)
        rogue.set_exit_on_disconnect(False)
        signal = dbus.lowlevel.SignalMessage(PATH, IFACE, 'ActionInvoked')
        signal.append(dbus.UInt32(identifier), 'allow-once', signature='us')
        rogue.send_message(signal); rogue.flush(); time.sleep(1.2); rogue.close()
        self.assertEqual(state(first), 'pending')
        shell.ActionInvoked(identifier, 'allow-once')
        contract.until(lambda: state(first)=='allowed')
        decision = f.admin.call('GET','/requests/'+first['id'])['decision']
        self.assertEqual(decision['actor'], 'local-desktop')
        self.assertEqual(decision['action']['scope'], {'duration':'once'})
        contract.until(lambda: identifier in shell.closed)
        shell.ActionInvoked(identifier, 'deny-once')
        time.sleep(.2); self.assertEqual(state(first), 'allowed')
        second = submit('notification-dismiss')
        identifier = contract.until(lambda: notification('notification-dismiss'))
        shell.NotificationClosed(identifier, 2)
        calls = len(shell.calls); time.sleep(2.2)
        self.assertEqual(len(shell.calls), calls)
        self.assertEqual(state(second), 'pending')
        # A notification click opens a real native window; closing it leaves the reviewer alive.
        shell.ActionInvoked(identifier, 'default')
        window = contract.until(lambda: subprocess.run(['xdotool','search','--onlyvisible','--name','^tachikoma$'],capture_output=True,text=True).stdout.strip(), timeout=30)
        # Launcher activation uses the same process and same window.
        self.assertEqual(subprocess.run(command+['--request',second['id']],env=env,timeout=10).returncode, 0)
        subprocess.run(['xdotool','key','--window',window,'ctrl+w'],check=True)
        contract.until(lambda: not subprocess.run(['xdotool','search','--onlyvisible','--name','^tachikoma$'],capture_output=True,text=True).stdout.strip())
        self.assertIsNone(desktop.poll())
        third = submit('notification-deny')
        identifier = contract.until(lambda: notification('notification-deny'))
        shell.ActionInvoked(identifier, 'deny-once')
        contract.until(lambda: state(third)=='denied')
        fourth = submit('notification-other-reviewer')
        identifier = contract.until(lambda: notification('notification-other-reviewer'))
        f.admin.call('POST','/requests/'+fourth['id']+'/decision',{'revision':fourth['revision'],'action_id':'deny-once'})
        contract.until(lambda: identifier in shell.closed)
        expired = submit('notification-expiry', seconds=3)
        identifier = contract.until(lambda: notification('notification-expiry'))
        contract.until(lambda: state(expired)=='expired')
        contract.until(lambda: identifier in shell.closed)
        shell.ActionInvoked(identifier, 'allow-once'); time.sleep(.2)
        self.assertEqual(state(expired), 'expired')
        # A shell restart reconnects and presents still-pending reviews.
        shell.close(); shell = Shell()
        contract.until(lambda: notification('notification-dismiss'), timeout=12)
        self.assertTrue(ready())
        print('Notification actions, forged signals, dismissal, expiry, cross-client settlement, shell restart and closed-window operation verified')

if __name__ == '__main__':
    try: unittest.main(testRunner=unittest.TextTestRunner(stream=sys.stdout, verbosity=2))
    finally: loop.quit()

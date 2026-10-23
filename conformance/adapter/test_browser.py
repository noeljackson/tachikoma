#!/usr/bin/env python3
"""Real Chromium forms against isolated fixtures; run in the browser-test image."""
import http.client
import importlib.util
import json
from pathlib import Path
import socket
import time
import unittest

spec=importlib.util.spec_from_file_location('contract',Path(__file__).with_name('test_contract.py'))
contract=importlib.util.module_from_spec(spec)
spec.loader.exec_module(contract)

class Browser:
    def __init__(self,fixture):
        listener=socket.socket();listener.bind(('127.0.0.1',0));self.port=listener.getsockname()[1];listener.close()
        self.process=fixture.child(['/usr/bin/chromedriver',f'--port={self.port}','--allowed-ips=127.0.0.1'])
        self.id=None
        contract.until(self.available)
        capabilities={'capabilities':{'alwaysMatch':{'browserName':'chrome','goog:chromeOptions':{'binary':'/usr/bin/chromium','args':[
            '--headless=new','--no-sandbox','--disable-dev-shm-usage','--disable-gpu','--no-first-run',
            '--no-default-browser-check','--disable-background-networking','--disable-component-update',
            '--disable-sync','--disable-extensions','--disable-client-side-phishing-detection',
            '--metrics-recording-only','--password-store=basic','--window-size=1280,900',
            '--user-data-dir='+str(fixture.root/'browser-profile')
        ]}}}}
        self.id=self.call('POST','/session',capabilities)['sessionId']
    def available(self):
        try:return self.call('GET','/status')['ready']
        except OSError:return False
    def call(self,method,path,body=None):
        connection=http.client.HTTPConnection('127.0.0.1',self.port,timeout=15)
        try:
            connection.request(method,path,None if body is None else json.dumps(body),{'Content-Type':'application/json'})
            response=connection.getresponse();data=json.loads(response.read())
            if response.status>=400:raise AssertionError('Browser command failed: '+path)
            return data['value']
        finally:connection.close()
    def command(self,path,body=None):
        return self.call('POST','/session/'+self.id+path,body or {})
    def visit(self,url):self.command('/url',{'url':url})
    def elements(self,css):return self.command('/elements',{'using':'css selector','value':css})
    def element(self,css):return self.command('/element',{'using':'css selector','value':css})['element-6066-11e4-a52e-4f735466cecf']
    def click(self,css):self.command('/element/'+self.element(css)+'/click')
    def type(self,css,text):self.command('/element/'+self.element(css)+'/value',{'text':text})
    def text(self):return self.command('/execute/sync',{'script':'return document.body.innerText','args':[]})
    def close(self):
        if self.id:self.call('DELETE','/session/'+self.id)

class BrowserTests(unittest.TestCase):
    def setUp(self):
        self.fixture=contract.ContractTests()
        self.fixture.setUp()
        self.browser=None
        self.addCleanup(self.cleanup)
        self.browser=Browser(self.fixture)
    def cleanup(self):
        try:
            if self.browser:self.browser.close()
        finally:self.fixture.tearDown()
    def test_direct_entry_decision_history_and_async_investigation(self):
        f,b=self.fixture,self.browser
        queue=f.example_queue()
        queue['assessment']={'instructions':'Explain supplied fixture facts','labels':{'unknown':'Unknown'},'subject_fields':['target'],'evidence_fields':[],'llm':True,'jev':False,'models':['fixture']}
        f.admin.call('POST','/queues',queue)
        producer=f.enroll('browser-producer','producer','example.review')
        worker=contract.external.API(f.socket,f.enroll('browser-worker','worker','example.review',''))
        operation=f.run_example(producer,f.root/'operation.json')
        review=f.pending('example.review')
        b.visit(f'http://127.0.0.1:{f.port}/approval/')
        self.assertIn('Local review',b.text())
        self.assertEqual(b.elements('input[type=password]'),[])
        self.assertEqual(b.elements('[name=credential]'),[])
        self.assertTrue(b.elements('button[name=action_id][value=allow-once]'))
        b.click('button[name=action_id][value=deny-once]')
        self.assertIn('Local review',b.text())
        settled=contract.until(lambda: (value if (value:=f.admin.call('GET','/requests/'+review['id']))['state']!='pending' else None))
        self.assertEqual(settled['state'],'denied')
        self.assertEqual(settled['decision']['actor'],'local-web')
        self.assertEqual(operation.wait(timeout=5),0)
        b.click("a[href='?view=history']")
        self.assertIn('Deny once',b.text())
        b.click("a[href$='/conversation']")
        b.type('textarea[name=message]','What evidence is missing?')
        b.click('button')
        job=contract.until(lambda:worker.call('POST','/jobs/claim',{'kind':'assessment_or_investigation'}))
        self.assertIsNotNone(job['conversation'])
        worker.call('POST','/jobs/'+job['id']+'/complete',{'lease':job['lease'],'assessment':{
            'state':'complete','category':'','confidence':None,'rationale':'Fixture investigation response: intent is unknown.',
            'jev_model':'','llm_model':'fixture','evidence_digest':review['evidence_digest'],'completed_at_ms':int(time.time()*1000)}})
        contract.until(lambda:'Fixture investigation response' in b.text(),timeout=12)
        self.assertTrue(b.elements('textarea[name=message]'))
        self.assertEqual(f.admin.call('GET','/requests/'+review['id'])['state'],'denied')
        self.assertEqual(b.elements('input[type=password]'),[])

if __name__=='__main__':unittest.main()

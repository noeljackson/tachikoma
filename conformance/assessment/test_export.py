#!/usr/bin/env python3
"""Run in Docker: history export must not treat previous allows as safety labels."""
import importlib.util
import json
from pathlib import Path
import sqlite3
import tempfile
import unittest

ROOT=Path(__file__).resolve().parents[2]
spec=importlib.util.spec_from_file_location('exporter',ROOT/'bin/export-network-evaluation.py')
exporter=importlib.util.module_from_spec(spec)
spec.loader.exec_module(exporter)


class ExportTests(unittest.TestCase):
    def test_bounded_read_only_export_has_no_historical_identity_or_secrets(self):
        with tempfile.TemporaryDirectory() as directory:
            path=Path(directory)/'ui.sqlite3'
            db=sqlite3.connect(path)
            db.execute('CREATE TABLE connections(process, uid, protocol, dst_ip, dst_host, dst_port, process_cwd, process_args, action)')
            for action in ['allow','reject','deny']:
                db.execute('INSERT INTO connections VALUES(?,?,?,?,?,?,?,?,?)',('/usr/bin/node','1000','tcp6','::1','','18443','/workspace/test','--token=secret',action))
            db.commit();before=path.read_bytes();db.close()
            result=exporter.export(path,1)
            self.assertEqual(path.read_bytes(),before)
            self.assertEqual(len(result['cases']),1)
            case=result['cases'][0]
            self.assertIsNone(case['expected'])
            self.assertEqual(case['subject']['pid'],0)
            self.assertEqual(case['subject']['protocol'],'tcp')
            self.assertEqual(case['evidence']['process_context']['working_directory'],'/workspace/test')
            self.assertNotIn('secret',json.dumps(result))

    def test_regression_ids_do_not_overlap_reference_examples(self):
        cases=json.loads((ROOT/'conformance/assessment/network-cases.json').read_text())['cases']
        profile=json.loads((ROOT/'contrib/adapters/snitchgate.queue.json').read_text())['assessment']
        self.assertFalse({c['id'] for c in cases}&{c['id'] for c in profile['examples']})
        self.assertTrue(any(c['tag']=='shai-hulud-lookalike' for c in cases))
        self.assertTrue(any(c['expected']=='unknown' for c in cases))


if __name__=='__main__':
    unittest.main()

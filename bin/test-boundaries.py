#!/usr/bin/env python3
"""Exercise the architecture guard using allowed and forbidden workspace graphs."""
import importlib.util
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location("boundaries", Path(__file__).with_name("check-boundaries.py"))
boundaries = importlib.util.module_from_spec(spec)
spec.loader.exec_module(boundaries)


def package(name, role, dependencies=()):
    return {"name": name, "manifest_path": f"/workspace/{name}/Cargo.toml", "metadata": {"tachikoma": {"boundary": role}}, "dependencies": [{"path": f"/workspace/{target}"} for target in dependencies]}


class BoundaryTests(unittest.TestCase):
    def test_independent_services_can_use_public_client(self):
        graph = [package("wire", "protocol"), package("client", "client", ["wire"]), package("network", "domain:network", ["client"]), package("cluster", "domain:cluster", ["client"])]
        self.assertEqual(boundaries.check(graph), [])

    def test_service_cannot_import_other_service_or_control_store(self):
        for target_role in ["domain:cluster", "control"]:
            graph = [package("network", "domain:network", ["other"]), package("other", target_role)]
            self.assertTrue(boundaries.check(graph))

    def test_core_and_public_client_cannot_import_domain(self):
        for role in ["control", "client", "protocol", "app", "worker"]:
            graph = [package("core", role, ["network"]), package("network", "domain:network")]
            self.assertTrue(boundaries.check(graph))

    def test_core_cannot_load_provider_worker_and_protocol_cannot_load_database(self):
        self.assertTrue(boundaries.check([package("core", "control", ["worker"]), package("worker", "worker")]))
        wire = package("wire", "protocol")
        wire["dependencies"] = [{"name":"rusqlite"}]
        self.assertTrue(boundaries.check([wire]))

    def test_unknown_member_and_outside_path_fail_closed(self):
        self.assertTrue(boundaries.check([package("unknown", "")]))
        self.assertTrue(boundaries.check([package("network", "domain:network", ["outside"])]))


if __name__ == "__main__":
    unittest.main()

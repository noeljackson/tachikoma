#!/usr/bin/env python3
"""Reject workspace dependencies that cross service implementation boundaries."""
import json
from pathlib import Path
import subprocess
import sys


def check(packages):
    by_path = {Path(p["manifest_path"]).parent.resolve(): p for p in packages}
    errors = []
    for package in packages:
        role = package.get("metadata", {}).get("tachikoma", {}).get("boundary")
        if role not in {"control", "protocol", "client", "app", "worker", "assessment"} and not (isinstance(role, str) and role.startswith("domain:")):
            errors.append(f"{package['name']}: missing or invalid boundary metadata")
            continue
        for dependency in package["dependencies"]:
            if not dependency.get("path"):
                public_dependencies = {"protocol": {"serde", "serde_json"}, "client": {"anyhow", "reqwest", "serde", "serde_json", "sha2"}}
                if role in public_dependencies and dependency.get("name") not in public_dependencies[role]:
                    errors.append(f"{package['name']}: undeclared public-layer dependency {dependency.get('name')}")
                continue
            target = by_path.get(Path(dependency["path"]).resolve())
            if not target:
                errors.append(f"{package['name']}: local dependency outside declared workspace")
                continue
            target_role = target.get("metadata", {}).get("tachikoma", {}).get("boundary", "")
            allowed = True
            if role.startswith("domain:"):
                allowed = target_role in {role, "protocol", "client"}
            elif role == "protocol":
                allowed = False
            elif role == "client":
                allowed = target_role == "protocol"
            elif role in {"app", "worker"}:
                allowed = target_role in {"protocol", "client", "assessment"}
            elif role == "control":
                allowed = target_role in {"control", "protocol", "client", "app"}
            elif target_role.startswith("domain:"):
                allowed = False
            if not allowed:
                errors.append(f"{package['name']} ({role}) -> {target['name']} ({target_role}) is forbidden")
    return errors


def main():
    result = subprocess.run(["cargo", "metadata", "--locked", "--offline", "--no-deps", "--format-version", "1"], check=True, capture_output=True, text=True)
    metadata = json.loads(result.stdout)
    members = set(metadata["workspace_members"])
    packages = [p for p in metadata["packages"] if p["id"] in members]
    errors = check(packages)
    if errors:
        print("\n".join(errors), file=sys.stderr)
        return 1
    print(f"Dependency boundaries valid for {len(packages)} workspace packages")
    return 0


if __name__ == "__main__":
    sys.exit(main())

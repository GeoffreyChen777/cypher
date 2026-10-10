#!/usr/bin/env python3
"""Check the workspace crate dependency layering.

    python3 scripts/ci/check-crate-layers.py

Every workspace package sits in one layer of LAYERS below. A package may
depend (normal, build, target-specific or dev dependency) only on workspace
packages in strictly lower layers. Every workspace member must be listed, so
a new crate cannot join the workspace without choosing its place. The table is
mirrored in docs/architecture.md ("Crates and layering"); change both together.
"""
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]

# Lowest layer first.
LAYERS = [
    ("foundation", ["cypher-env", "cypher-proto", "cypher-syntax"]),
    ("model and transport", ["cypher-doc", "cypher-update", "cypher-net"]),
    ("clients", ["cypher-sync", "cypher-harness"]),
    ("relay", ["cypher-rpc"]),
    ("engine", ["cypher-engine"]),
    ("desktop UI", ["cypher-ui"]),
    ("application", ["cypher"]),
]

DEPENDENCY_TABLE = re.compile(
    r"^\[(?:target\.[^\]]+\.)?(dependencies|dev-dependencies|build-dependencies)\]$"
)
DEPENDENCY_KEY = re.compile(r"^([A-Za-z0-9_-]+)\s*(?:\.|=)")


def workspace_members():
    text = (ROOT / "Cargo.toml").read_text()
    block = re.search(r"^members\s*=\s*\[(.*?)\]", text, re.S | re.M)
    if not block:
        raise SystemExit("Cargo.toml: no [workspace] members list")
    return re.findall(r'"([^"]+)"', block.group(1))


def package_name(manifest):
    in_package = False
    for line in manifest.read_text().splitlines():
        line = line.strip()
        if line.startswith("["):
            in_package = line == "[package]"
        elif in_package:
            match = re.match(r'^name\s*=\s*"([^"]+)"', line)
            if match:
                return match.group(1)
    raise SystemExit(f"{manifest}: no [package] name")


def dependencies(manifest):
    """Yield (kind, dependency name) for every dependency table entry."""
    kind = None
    for line in manifest.read_text().splitlines():
        line = line.split("#", 1)[0].strip()
        if not line:
            continue
        if line.startswith("["):
            match = DEPENDENCY_TABLE.match(line)
            kind = match.group(1) if match else None
            continue
        if kind:
            match = DEPENDENCY_KEY.match(line)
            if match:
                yield kind, match.group(1)


def main():
    layer_of = {}
    for index, (_, packages) in enumerate(LAYERS):
        for package in packages:
            if package in layer_of:
                raise SystemExit(f"{package} is listed in two layers")
            layer_of[package] = index

    manifests = {}
    for member in workspace_members():
        manifest = ROOT / member / "Cargo.toml"
        manifests[package_name(manifest)] = manifest

    errors = []
    for package, manifest in sorted(manifests.items()):
        if package not in layer_of:
            errors.append(f"{package}: not assigned to a layer in {Path(__file__).name}")
            continue
        for kind, dependency in dependencies(manifest):
            if dependency not in manifests and dependency not in layer_of:
                continue  # an external crate
            if dependency not in layer_of:
                errors.append(f"{package}: depends on unlisted workspace crate {dependency}")
            elif layer_of[dependency] >= layer_of[package]:
                errors.append(
                    f"{package} ({LAYERS[layer_of[package]][0]}) must not depend on "
                    f"{dependency} ({LAYERS[layer_of[dependency]][0]}) in [{kind}]"
                )

    for error in errors:
        print(error)
    if errors:
        return 1
    print(f"crate layering verified for {len(manifests)} packages")
    return 0


if __name__ == "__main__":
    sys.exit(main())

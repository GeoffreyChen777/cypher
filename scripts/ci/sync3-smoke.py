#!/usr/bin/env python3
"""Local-only Rust <-> actual workerd <-> Swift protocol/journal smoke.

Does not use Cloudflare credentials, deployed Workers, production data, or
existing simulators. Owns a temporary server process group and deletes its
temporary databases after shutdown. Run from any working directory.
"""
import os
from pathlib import Path
import signal
import shutil
import socket
import subprocess
import tempfile
import time
import urllib.request
import uuid

ROOT = Path(__file__).resolve().parents[2]
ENV = {**os.environ, "WRANGLER_SEND_METRICS": "false",
       "DEVELOPER_DIR": os.environ.get("DEVELOPER_DIR", "/Applications/Xcode.app/Contents/Developer")}


def run(args):
    subprocess.run([str(arg) for arg in args], cwd=ROOT, env=ENV, check=True)


def main():
    run(["cargo", "build", "-p", "cypher-sync", "--example", "sync3_smoke"])
    # Let the OS choose a free local port; Wrangler remains loopback-only.
    with socket.socket() as probe:
        probe.bind(("127.0.0.1", 0))
        port = probe.getsockname()[1]
    base = f"http://127.0.0.1:{port}"
    room = str(uuid.uuid4())
    with tempfile.TemporaryDirectory(prefix="cypher-sync3-smoke-") as tmp:
        swift = Path(tmp) / "swift-smoke"
        shutil.copyfile(ROOT / "apps/ios/Cypher/Sync/Sync3CommandSchema.json", Path(tmp) / "Sync3CommandSchema.json")
        shutil.copyfile(ROOT / "apps/ios/Cypher/Sync/Sync3PartSchema.json", Path(tmp) / "Sync3PartSchema.json")
        run(["xcrun", "swiftc", "-parse-as-library",
             "apps/ios/Cypher/Sync/RegistryCore.swift",
             "apps/ios/Cypher/Sync/Sync3Protocol.swift",
             "apps/ios/Cypher/Sync/Sync3CommandSchema.swift",
             "apps/ios/Cypher/Sync/Sync3Journal.swift",
             "apps/ios/Cypher/Sync/Sync3Client.swift",
             "apps/ios/Cypher/Sync/Sync3HTTPTransport.swift",
             "scripts/ci/sync3-swift-main.swift", "-o", swift])
        shared = Path(tmp) / "shared.sqlite"
        run([ROOT / "target/debug/examples/sync3_smoke", base, room, "--write-journal", shared])
        run([swift, ROOT / "fixtures/sync3/golden.json", "--shared-journal", shared])
        run([ROOT / "target/debug/examples/sync3_smoke", base, room, "--verify-journal", shared])
        if shared.stat().st_mode & 0o777 != 0o600:
            raise RuntimeError("journal is not private")
        log_path = Path(tmp) / "workerd.log"
        with log_path.open("w") as log:
            server = subprocess.Popen(
                ["./node_modules/.bin/wrangler", "dev", "--local", "--config", "wrangler.sync3test.jsonc",
                 "--port", str(port), "--persist-to", str(Path(tmp) / "state")],
                cwd=ROOT / "edge", env=ENV, stdout=log, stderr=subprocess.STDOUT, start_new_session=True)
            try:
                for _ in range(90):
                    if server.poll() is not None:
                        raise RuntimeError("workerd exited before readiness")
                    try:
                        with urllib.request.urlopen(base + "/health", timeout=1) as response:
                            if response.status == 200:
                                break
                    except Exception:
                        time.sleep(0.5)
                else:
                    raise RuntimeError("workerd startup timeout")
                run([ROOT / "target/debug/examples/sync3_smoke", base, room])
                run([swift, ROOT / "fixtures/sync3/golden.json", base, room])
                run([ROOT / "target/debug/examples/sync3_smoke", base, room, "--verify-swift"])
            except BaseException:
                print(log_path.read_text()[-8000:])
                raise
            finally:
                if server.poll() is None:
                    os.killpg(server.pid, signal.SIGTERM)
                    try:
                        server.wait(timeout=10)
                    except subprocess.TimeoutExpired:
                        os.killpg(server.pid, signal.SIGKILL)
                        server.wait()
    print("PASS: Sync v3 three-language live convergence and restart")


if __name__ == "__main__":
    main()

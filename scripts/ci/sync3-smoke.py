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
import urllib.error
import uuid

ROOT = Path(__file__).resolve().parents[2]
ENV = {**os.environ, "WRANGLER_SEND_METRICS": "false",
       "DEVELOPER_DIR": os.environ.get("DEVELOPER_DIR", "/Applications/Xcode.app/Contents/Developer")}


def run(args):
    subprocess.run([str(arg) for arg in args], cwd=ROOT, env=ENV, check=True)


def main():
    run(["cargo", "build", "-p", "cypher-sync", "--example", "sync3_smoke"])
    run(["cargo", "build", "-p", "cypher-engine", "--example", "v3_execution_smoke"])
    run(["cargo", "build", "-p", "cypher-engine", "--example", "normal_v3_smoke"])
    run(["cargo", "build", "-p", "cypher-sync", "--example", "workspace3_smoke"])
    run(["cargo", "build", "-p", "cypher-rpc", "--example", "workspace3_codec"])
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
             "apps/ios/Cypher/Models/JSONValue.swift",
             "apps/ios/Cypher/Sync/Sync3Protocol.swift",
             "apps/ios/Cypher/Sync/Sync3CommandSchema.swift",
             "apps/ios/Cypher/Sync/Sync3Journal.swift",
             "apps/ios/Cypher/Sync/Sync3Client.swift",
             "apps/ios/Cypher/Sync/V3NoRedirect.swift",
             "apps/ios/Cypher/Sync/Sync3HTTPTransport.swift",
             "scripts/ci/sync3-swift-main.swift", "-o", swift])
        shared = Path(tmp) / "shared.sqlite"
        workspace_swift = Path(tmp) / "workspace-swift"
        run(["xcrun", "swiftc", "-parse-as-library",
             "apps/ios/Cypher/Models/JSONValue.swift",
             "apps/ios/Cypher/Sync/Sync3Protocol.swift",
             "apps/ios/Cypher/Sync/Sync3CommandSchema.swift",
             "apps/ios/Cypher/Sync/Workspace3Wire.swift",
             "apps/ios/Cypher/Sync/Workspace3Metadata.swift",
             "apps/ios/Cypher/Sync/Workspace3Journal.swift",
             "apps/ios/Cypher/Sync/Workspace3RPCCodec.swift",
             "apps/ios/Cypher/Sync/Workspace3Client.swift",
             "apps/ios/Cypher/Sync/V3NoRedirect.swift",
             "apps/ios/Cypher/Sync/RemoteError.swift",
             "apps/ios/Cypher/Sync/Workspace3RPC.swift",
             "scripts/ci/workspace3-swift-main.swift", "-o", workspace_swift])
        shared_workspace = Path(tmp) / "workspace-shared.sqlite"
        run([ROOT / "target/debug/examples/workspace3_smoke", base, "--write-journal", shared_workspace])
        run([workspace_swift, "--journal", shared_workspace])
        run([ROOT / "target/debug/examples/workspace3_smoke", base, "--verify-journal", shared_workspace])
        swift_created = Path(tmp) / "workspace-swift-created.sqlite"
        run([workspace_swift, "--create-journal", swift_created])
        run([ROOT / "target/debug/examples/workspace3_smoke", base, "--verify-swift-created", swift_created])
        rpc_corpus = Path(tmp) / "rpc-codec.json"
        run([ROOT / "target/debug/examples/workspace3_codec", "--write", rpc_corpus])
        run([workspace_swift, "--codec", rpc_corpus])
        run([ROOT / "target/debug/examples/workspace3_codec", "--verify", rpc_corpus])
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
                # A swapped token must be rejected before /init can create a
                # conversation in the unintended account.
                fence_room = str(uuid.uuid4())
                wrong = urllib.request.Request(f"{base}/sync3/sync3-org/chats/{fence_room}/init",
                    data=b'{"owner":"host"}', headers={"Authorization": "Bearer other-user@sync3-org",
                        "x-cypher-expected-user": "sync3-user", "Content-Type": "application/json"})
                try:
                    urllib.request.urlopen(wrong, timeout=5)
                    raise RuntimeError("cross-account initialization accepted")
                except urllib.error.HTTPError as error:
                    if error.code != 403 or b"account_mismatch" not in error.read():
                        raise
                probe = urllib.request.Request(f"{base}/sync3/sync3-org/chats/{fence_room}/exchange",
                    data=b'{"version":3,"type":"hello","actor":"host","epoch":0,"after":0}',
                    headers={"Authorization": "Bearer other-user@sync3-org", "x-cypher-expected-user": "other-user",
                        "Content-Type": "application/json"})
                try:
                    urllib.request.urlopen(probe, timeout=5)
                    raise RuntimeError("wrong-account initialization left a room behind")
                except urllib.error.HTTPError as error:
                    if error.code != 409 or b"not_initialized" not in error.read():
                        raise
                print("PASS: real workerd rejects swapped-account conversation init before any room state is created")
                run([ROOT / "target/debug/examples/sync3_smoke", base, room])
                run([swift, ROOT / "fixtures/sync3/golden.json", base, room])
                run([ROOT / "target/debug/examples/sync3_smoke", base, room, "--verify-swift"])
                writer_room = str(uuid.uuid4())
                writer_report = Path(tmp) / "writer-report.json"
                run([ROOT / "target/debug/examples/sync3_smoke", base, writer_room, "--writer", writer_report])
                run([swift, ROOT / "fixtures/sync3/golden.json", base, writer_room, "--writer", writer_report])
                engine_room = str(uuid.uuid4())
                engine_report = Path(tmp) / "engine-report.json"
                run([ROOT / "target/debug/examples/v3_execution_smoke", base, engine_room, engine_report])
                run([swift, ROOT / "fixtures/sync3/golden.json", base, engine_room, "--writer", engine_report])
                normal_room = str(uuid.uuid4())
                run([ROOT / "target/debug/examples/workspace3_smoke", base])
                run([workspace_swift, base])
                normal_report = Path(tmp) / "normal-report.json"
                run([ROOT / "target/debug/examples/normal_v3_smoke", base, normal_room, normal_report, workspace_swift])
                run([swift, ROOT / "fixtures/sync3/golden.json", base, normal_room, "--writer", normal_report])
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

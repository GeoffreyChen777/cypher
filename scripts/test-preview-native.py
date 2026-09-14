#!/usr/bin/env python3
"""Local-only native Engine x2 ↔ iOS Simulator ↔ real workerd preview test.

No production credentials, Cloudflare deployment, data copying or quota changes.
Requires the dedicated Cypher Profile Tests simulator already installed.
"""
import json
import os
from pathlib import Path
import signal
import socket
import subprocess
import tempfile
import time
import urllib.request

ROOT = Path(__file__).resolve().parent.parent
CONTROL = Path("/tmp/cypher-preview-native.json")
RECEIPTS = [CONTROL, CONTROL.with_suffix(".ios.json"), CONTROL.with_suffix(".desktop.json")]
if any(p.exists() for p in RECEIPTS):
    raise SystemExit("Existing preview fixture/receipts found; inspect them before rerunning")
env = {k: v for k, v in os.environ.items() if not k.startswith(("CYPHER_", "CLOUDFLARE_"))}
env.update(DEVELOPER_DIR="/Applications/Xcode.app/Contents/Developer", CARGO_PROFILE_DEV_DEBUG="0",
           CARGO_PROFILE_TEST_DEBUG="0", CARGO_INCREMENTAL="0", CYPHER_MOCK_DELAY_MS="150")
env["PATH"] = f"{Path.home()}/.rustup/toolchains/stable-aarch64-apple-darwin/bin:{Path.home()}/.local/node-v24.19.0/bin:" + env["PATH"]
env["DYLD_FALLBACK_LIBRARY_PATH"] = str(Path.home() / ".rustup/toolchains/stable-aarch64-apple-darwin/lib")
output = Path(tempfile.mkdtemp(prefix="cypher-preview-native-"))
processes = []
logs = []

def start(args, name):
    log = (output / name).open("wb")
    logs.append(log)
    p = subprocess.Popen(args, cwd=ROOT, env=env, stdin=subprocess.DEVNULL, stdout=log, stderr=log, start_new_session=True)
    processes.append(p)
    return p

try:
    subprocess.run(["cargo", "build", "--locked", "-p", "cypher-engine", "--example", "preview_native_fixture"],
                   cwd=ROOT, env=env, check=True)
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        port = sock.getsockname()[1]
    env["CYPHER_PREVIEW_TEST_EDGE"] = f"http://127.0.0.1:{port}"
    env["CYPHER_PREVIEW_TEST_CONTROL"] = str(CONTROL)
    worker = start(["node", "edge/node_modules/wrangler/bin/wrangler.js", "dev", "--local", "--ip", "127.0.0.1",
                    "--port", str(port), "--config", "edge/wrangler.preview-test.jsonc", "--persist-to", str(output / "workerd")], "worker.log")
    for _ in range(200):
        if worker.poll() is not None:
            raise RuntimeError("Local workerd exited; inspect worker.log")
        try:
            with urllib.request.urlopen(env["CYPHER_PREVIEW_TEST_EDGE"] + "/health", timeout=1) as response:
                if json.load(response).get("localPreviewFixture"):
                    break
        except Exception:
            pass
        time.sleep(.1)
    else:
        raise RuntimeError("Local workerd did not become ready")
    engine = start(["target/debug/examples/preview_native_fixture"], "engines.log")
    for _ in range(300):
        if engine.poll() is not None:
            raise RuntimeError("Native fixture exited during startup")
        if CONTROL.exists():
            break
        time.sleep(.1)
    else:
        raise RuntimeError("Native fixture did not become ready")
    ios = start(["xcodebuild", "test", "-project", "apps/ios/Cypher.xcodeproj", "-scheme", "CypherDev",
                 "-destination", "platform=iOS Simulator,id=46B0FBF5-7026-40E8-8C2F-72D1486A135B",
                 "-derivedDataPath", "/tmp/cypher-p1-ios-tests", "-only-testing:CypherTests/PreviewNativeInteropTests",
                 "ENABLE_TESTABILITY=YES", "CODE_SIGNING_ALLOWED=YES", "CODE_SIGN_IDENTITY=-"], "ios.log")
    if ios.wait() != 0:
        raise RuntimeError("iOS native interop failed; inspect ios.log")
    if engine.wait(timeout=180) != 0:
        raise RuntimeError("Desktop native interop failed; inspect engines.log")
    for path in RECEIPTS[1:]:
        receipt = json.loads(path.read_text())
        assert receipt["passed"], receipt
        (output / path.name).write_text(json.dumps(receipt, indent=2))
    print("PASS: native desktop and iOS both displayed previews; final docs converged, one durable command")
finally:
    for p in reversed(processes):
        if p.poll() is None:
            os.killpg(p.pid, signal.SIGTERM)
            try:
                p.wait(timeout=5)
            except subprocess.TimeoutExpired:
                os.killpg(p.pid, signal.SIGKILL)
                p.wait()
    for log in logs:
        log.close()
    for path in RECEIPTS:
        path.unlink(missing_ok=True)
    print("Local test evidence:", output)

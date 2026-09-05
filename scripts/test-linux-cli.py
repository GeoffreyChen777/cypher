#!/usr/bin/env python3
"""Isolated installer/CLI regressions. No production downloads or real services.

python3 scripts/test-linux-cli.py [--binary /path/to/headless/cypher]
On macOS only uname, GNU mv -T, sha256sum and service commands are simulated.
On Linux GNU file tools are real; systemctl/loginctl are always test doubles.
"""
import argparse
import hashlib
import http.server
import io
import json
import os
from pathlib import Path
import platform
import pty
import shutil
import signal
import socket
import subprocess
import sys
import tarfile
import tempfile
import threading
import time
import unittest

ROOT = Path(__file__).resolve().parent.parent
PARSER = argparse.ArgumentParser()
PARSER.add_argument("--binary", type=lambda p: str(Path(p).resolve()))
ARGS, TEST_ARGS = PARSER.parse_known_args()
BINARY = ARGS.binary


def executable(path, text):
    path.write_text(text)
    path.chmod(0o755)


class Fixture(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="cypher-cli-test-")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.home = self.root / "home with spaces % $"
        self.home.mkdir()
        self.bin = self.root / "bin"
        self.bin.mkdir()
        self.env = {k: v for k, v in os.environ.items()
                    if not k.startswith(("CYPHER_", "ZERON_", "XDG_"))}
        self.env.update(
            HOME=str(self.home), PATH=f"{self.bin}:{os.environ['PATH']}",
            TEST_ACTIONS=str(self.root / "actions"), TEST_BUS="0", TEST_ARCH="x86_64",
            CYPHER_DATA_DIR=str(self.home / ".cypher"), CYPHER_IPC_PORT="1",
            CYPHER_EDGE_URL="http://127.0.0.1:1", CYPHER_WORKOS_CLIENT_ID="client_fixture",
            CYPHER_HARNESS="mock", RUST_LOG="error",
        )
        executable(self.bin / "systemctl", """#!/bin/sh
if [ "$*" = "--user show-environment" ]; then [ "$TEST_BUS" = 1 ]; exit $?; fi
if [ "${TEST_SYSTEMCTL_FAIL:-}" = "$*" ]; then exit 1; fi
printf '%s\\n' "$*" >> "$TEST_ACTIONS"
""")
        executable(self.bin / "loginctl", "#!/bin/sh\nexit 1\n")

    def run_command(self, command, **kwargs):
        return subprocess.run(command, env=self.env, text=True,
                              stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                              timeout=kwargs.pop("timeout", 15), **kwargs)


class Installer(Fixture):
    def setUp(self):
        super().setUp()
        executable(self.bin / "uname", """#!/bin/sh
case "$1" in -s) echo Linux;; -m) echo "$TEST_ARCH";; *) exit 1;; esac
""")
        if platform.system() != "Linux":
            real_mv = shutil.which("mv")
            executable(self.bin / "mv", f"""#!{sys.executable}
import os, sys
if sys.argv[1] == '-Tf':
    os.replace(sys.argv[2], sys.argv[3])
else:
    os.execv({real_mv!r}, [{real_mv!r}] + sys.argv[1:])
""")
        if not shutil.which("sha256sum"):
            executable(self.bin / "sha256sum", f"""#!{sys.executable}
import hashlib, sys
print(hashlib.sha256(open(sys.argv[1], 'rb').read()).hexdigest() + '  ' + sys.argv[1])
""")
        if not shutil.which("timeout"):
            executable(self.bin / "timeout", f"""#!{sys.executable}
import subprocess, sys
try:
    sys.exit(subprocess.run(sys.argv[2:], timeout=float(sys.argv[1])).returncode)
except subprocess.TimeoutExpired:
    sys.exit(124)
""")
        self.routes = {}
        routes = self.routes

        class Handler(http.server.BaseHTTPRequestHandler):
            def do_GET(handler):
                code, body, advertised = routes.get(handler.path, (404, b"", None))
                handler.send_response(code)
                handler.send_header("Content-Length", str(advertised or len(body)))
                handler.end_headers()
                handler.wfile.write(body)

            def log_message(handler, *_):
                pass

        self.http = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        thread = threading.Thread(target=self.http.serve_forever, daemon=True)
        thread.start()
        self.addCleanup(self.http.server_close)
        self.addCleanup(self.http.shutdown)
        self.env["CYPHER_BASE_URL"] = f"http://127.0.0.1:{self.http.server_port}"
        self.app = self.home / ".cypher/app"
        (self.app / "0.1.0").mkdir(parents=True)
        (self.app / "0.1.0/cypher").write_text("old binary: must not change")
        (self.app / "current").symlink_to(self.app / "0.1.0")
        self.sentinel = self.home / ".cypher/session.json"
        self.sentinel.write_text("saved fixture: must not change")
        # Detect accidental interaction with a system Pi configuration.
        (self.home / ".pi").mkdir()
        (self.home / ".pi/settings.json").write_text("do not touch")

    def release(self, *, version="1.2.3", arch="x86_64", member=None,
                kind=tarfile.REGTYPE, mode=0o755, body=None):
        name = f"cypher-{version}-linux-{arch}"
        binary = body if body is not None else b"""#!/bin/sh
case "$*" in
  --help) exit 0;;
  "daemon install") printf '%s\\n' "$*" >> "$TEST_ACTIONS"; exit 0;;
  *) exit 1;;
esac
"""
        buffer = io.BytesIO()
        with tarfile.open(fileobj=buffer, mode="w:gz") as archive:
            entry = tarfile.TarInfo(member or f"{name}/cypher")
            entry.type, entry.mode = kind, mode
            if kind in (tarfile.SYMTYPE, tarfile.LNKTYPE):
                entry.linkname = "/bin/sh"
            else:
                entry.size = len(binary)
            archive.addfile(entry, io.BytesIO(binary))
        data = buffer.getvalue()
        self.archive_url = f"/releases/{name}.tar.gz"
        self.routes["/releases/latest.txt"] = (200, version.encode(), None)
        self.routes[self.archive_url] = (200, data, None)
        self.routes[self.archive_url + ".sha256"] = (
            200, hashlib.sha256(data).hexdigest().encode() + b"\n", None)
        return data

    def install(self, success=True):
        result = self.run_command(["sh", str(ROOT / "edge/src/install.sh")])
        if success:
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        else:
            self.assertNotEqual(result.returncode, 0, result.stdout)
            self.assertEqual(os.readlink(self.app / "current"), str(self.app / "0.1.0"))
        self.assertEqual((self.app / "0.1.0/cypher").read_text(), "old binary: must not change")
        self.assertEqual(self.sentinel.read_text(), "saved fixture: must not change")
        self.assertEqual((self.home / ".pi/settings.json").read_text(), "do not touch")
        self.assertFalse(list(self.app.glob(".install-*")))
        return result

    def test_install_and_reinstall_keep_current_a_link(self):
        self.release()
        for _ in range(2):
            self.install()
            self.assertEqual(os.readlink(self.app / "current"), str(self.app / "1.2.3"))
            self.assertTrue((self.home / ".local/bin/cypher").is_symlink())
            self.assertFalse((self.app / "0.1.0/current").exists())

    def test_arm64_artifact_and_service_delegation(self):
        self.env.update(TEST_ARCH="aarch64", TEST_BUS="1")
        self.release(arch="aarch64")
        self.install()
        self.assertEqual((self.root / "actions").read_text(), "daemon install\n")

    def test_no_user_bus_falls_back_even_with_runtime_dir(self):
        self.env["XDG_RUNTIME_DIR"] = str(self.root)
        self.release()
        self.assertIn("no systemd user bus", self.install().stdout)
        self.assertFalse((self.root / "actions").exists())

    def test_missing_checksum_fails_closed(self):
        self.release()
        del self.routes[self.archive_url + ".sha256"]
        self.install(False)

    def test_invalid_checksum_fails_closed(self):
        self.release()
        for hash_value in [b"0" * 64, b"not a digest", b"f" * 64 + b"  arbitrary-file"]:
            with self.subTest(hash=hash_value):
                self.routes[self.archive_url + ".sha256"] = (200, hash_value, None)
                self.install(False)

    def test_download_interruption_preserves_old_install(self):
        data = self.release()
        self.routes[self.archive_url] = (200, data[:20], len(data))
        self.install(False)

    def test_invalid_version_never_becomes_a_path(self):
        self.release()
        for version in [b"../../escape", b"/tmp/escape", b"1..2", b"", b"1.2\n3", b"1.2?x"]:
            with self.subTest(version=version):
                self.routes["/releases/latest.txt"] = (200, version, None)
                self.install(False)

    def test_archive_paths_links_permissions_and_loader_failures(self):
        cases = [
            dict(member="../escape"),
            dict(member="/tmp/escape"),
            dict(member="cypher-1.2.3-linux-x86_64/unexpected"),
            dict(kind=tarfile.SYMTYPE),
            dict(kind=tarfile.LNKTYPE),
            dict(mode=0o644),
            dict(body=b"#!/bin/sh\nexit 1\n"),
        ]
        for case in cases:
            with self.subTest(case=case):
                self.release(**case)
                self.install(False)
                self.assertFalse((self.app / "1.2.3").exists())

    def test_conflicting_version_is_not_overwritten(self):
        self.release()
        (self.app / "1.2.3").mkdir()
        (self.app / "1.2.3/cypher").write_text("incomplete")
        self.install(False)
        self.assertEqual((self.app / "1.2.3/cypher").read_text(), "incomplete")

    def test_manual_package_installer_does_not_follow_managed_command_link(self):
        source = (ROOT / "scripts/package-linux.sh").read_text()
        script = source.split("<<'INSTALL'\n", 1)[1].split("\nINSTALL\n", 1)[0]
        package = self.root / "package"
        package.mkdir()
        executable(package / "install.sh", script)
        executable(package / "cypher", "#!/bin/sh\necho new\n")
        command_dir = self.home / ".local/bin"
        command_dir.mkdir(parents=True)
        (command_dir / "cypher").symlink_to(self.app / "current/cypher")
        result = self.run_command(["bash", str(package / "install.sh")])
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertFalse((command_dir / "cypher").is_symlink())
        self.assertEqual((self.app / "0.1.0/cypher").read_text(), "old binary: must not change")
        self.assertFalse((self.home / ".local/share/applications/cypher.desktop").exists())

    def test_runtime_metadata_size_is_portable_and_validated(self):
        node = shutil.which("node")
        if not node:
            self.skipTest("Node required for Runtime packaging metadata test")
        source = (ROOT / "scripts/package-pi-runtime.sh").read_text()
        archive = self.root / "archive"
        archive.write_bytes(b"12345")
        self.env["ARCHIVE"] = str(archive)
        size_line = next(line for line in source.splitlines() if line.startswith("SIZE="))
        result = self.run_command(["sh", "-c", size_line + '\nprintf "%s" "$SIZE"'])
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, "5")
        script = source.rsplit("<<'NODE'\n", 1)[1].split("\nNODE", 1)[0]
        runtime = self.root / "runtime.json"
        runtime.write_text('{"version":"0.85.0.4","piVersion":"0.85.0","plugins":{}}')
        metadata = self.root / "metadata.json"
        command = [node, "-", str(runtime), str(metadata), "linux-x86_64", "runtime.tar.gz"]
        result = self.run_command(command + ["5", "a" * 64, "0.2.2"], input=script)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(json.loads(metadata.read_text())["files"]["linux-x86_64"]["size"], 5)
        metadata.unlink()
        for size in ["filesystem information\n5", "NaN", "0", "-1"]:
            result = self.run_command(command + [size, "a" * 64, "0.2.2"], input=script)
            self.assertNotEqual(result.returncode, 0)
            self.assertFalse(metadata.exists())


@unittest.skipUnless(BINARY, "pass --binary to run real headless CLI tests")
class Cli(Fixture):
    def cli(self, *args, **kwargs):
        return self.run_command([BINARY, *args], **kwargs)

    def test_help_version_and_empty_invocation_do_not_create_data(self):
        self.assertEqual(self.cli("--help").returncode, 0)
        self.assertRegex(self.cli("--version").stdout, r"cypher \d+\.\d+\.\d+")
        self.assertNotEqual(self.cli().returncode, 0)
        self.assertFalse((self.home / ".cypher").exists())

    def test_invalid_explicit_port_never_falls_back_to_default(self):
        for port in ["invalid", "-1", "65536", "0"]:
            with self.subTest(port=port):
                self.env["CYPHER_IPC_PORT"] = port
                result = self.cli("status")
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("CYPHER_IPC_PORT", result.stderr)

    def test_login_eof_exits_instead_of_hanging(self):
        master, slave = pty.openpty()
        try:
            with tempfile.TemporaryFile() as output:
                process = subprocess.Popen([BINARY, "login"], stdin=slave, stdout=output,
                                           stderr=output, env=self.env)
                try:
                    os.write(master, b"\x04")
                    self.assertNotEqual(process.wait(timeout=6), 0)
                    output.seek(0)
                    self.assertIn(b"terminal input closed", output.read())
                finally:
                    if process.poll() is None:
                        process.kill()
                        process.wait()
        finally:
            os.close(master)
            os.close(slave)

    def test_non_websocket_listener_cannot_hang_diagnostics(self):
        with socket.socket() as listener:
            listener.bind(("127.0.0.1", 0))
            listener.listen(8)
            self.env["CYPHER_IPC_PORT"] = str(listener.getsockname()[1])
            self.assertEqual(self.cli("status", timeout=5).returncode, 0)
            result = self.cli("sync", timeout=8)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("timed out", result.stderr)

    def test_headless_lock_status_and_graceful_sigterm(self):
        with socket.socket() as reservation:
            reservation.bind(("127.0.0.1", 0))
            port = reservation.getsockname()[1]
        self.env["CYPHER_IPC_PORT"] = str(port)
        with tempfile.TemporaryFile() as output:
            process = subprocess.Popen([BINARY, "headless"], env=self.env,
                                       stdin=subprocess.DEVNULL, stdout=output, stderr=output)
            try:
                for _ in range(100):
                    if process.poll() is not None:
                        output.seek(0)
                        self.fail(output.read().decode(errors="replace"))
                    try:
                        with socket.create_connection(("127.0.0.1", port), timeout=0.1):
                            break
                    except OSError:
                        time.sleep(0.1)
                else:
                    self.fail("engine did not bind IPC")
                status = self.cli("status")
                self.assertEqual(status.returncode, 0, status.stderr)
                self.assertIn("local only", status.stdout)
                self.assertIn("Engine:   running", status.stdout)
                for command in ("login", "logout", "headless"):
                    result = self.cli(command, timeout=6)
                    self.assertNotEqual(result.returncode, 0)
                    self.assertIn("already running", result.stderr)
                self.assertEqual(self.cli("sync").returncode, 0)
                process.send_signal(signal.SIGTERM)
                self.assertEqual(process.wait(timeout=10), 0)
            finally:
                if process.poll() is None:
                    process.kill()
                    process.wait()

    @unittest.skipUnless(platform.system() == "Linux", "requires the Linux CLI branch")
    def test_systemd_install_captures_private_escaped_environment_and_restarts(self):
        config_home = self.home / "custom config"
        self.env.update(XDG_CONFIG_HOME=str(config_home), CYPHER_DEVICE_NAME='name % " \nline',
                        CYPHER_AUTO_UPDATE="1")
        executable_dir = self.home / "bin with space %"
        executable_dir.mkdir()
        executable_path = executable_dir / "cypher$fixture"
        shutil.copy2(BINARY, executable_path)
        result = self.run_command([str(executable_path), "daemon", "install"])
        self.assertEqual(result.returncode, 0, result.stderr)
        unit = config_home / "systemd/user/cypher.service"
        text = unit.read_text()
        self.assertEqual(unit.stat().st_mode & 0o777, 0o600)
        self.assertIn('Environment="CYPHER_DEVICE_NAME=name %% \\" \\nline"', text)
        self.assertIn('Environment="CYPHER_AUTO_UPDATE=1"', text)
        self.assertIn('ExecStart=:"', text)
        self.assertIn('/cypher$fixture" headless', text)
        if shutil.which("systemd-analyze"):
            verification = self.run_command(["systemd-analyze", "verify", str(unit)])
            self.assertEqual(verification.returncode, 0, verification.stderr)
        self.assertEqual((self.root / "actions").read_text(),
                         "--user daemon-reload\n--user enable cypher.service\n--user restart cypher.service\n")
        self.env["TEST_SYSTEMCTL_FAIL"] = "--user disable --now cypher.service"
        self.assertNotEqual(self.cli("daemon", "uninstall").returncode, 0)
        self.assertTrue(unit.exists(), "failed stop must preserve the service unit")
        del self.env["TEST_SYSTEMCTL_FAIL"]
        self.assertEqual(self.cli("daemon", "uninstall").returncode, 0)
        self.assertFalse(unit.exists())


if __name__ == "__main__":
    unittest.main(argv=[sys.argv[0], *TEST_ARGS], verbosity=2)

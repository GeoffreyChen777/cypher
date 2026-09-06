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
import select
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


def ipc_path(env):
    data = Path(env["CYPHER_DATA_DIR"]).resolve()
    key = hashlib.sha256(os.fsencode(str(data))).hexdigest()[:32]
    return Path(f"/tmp/cypher-ipc-{os.geteuid()}/{key}/engine.sock")


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
            CYPHER_DATA_DIR=str(self.home / ".cypher"),
            CYPHER_EDGE_URL="http://127.0.0.1:1", CYPHER_WORKOS_CLIENT_ID="client_fixture",
            CYPHER_HARNESS="mock", RUST_LOG="error",
            SHELL="/bin/bash",
        )
        executable(self.bin / "systemctl", """#!/bin/sh
if [ "$*" = "--user show-environment" ]; then [ "$TEST_BUS" = 1 ]; exit $?; fi
if [ "${TEST_SYSTEMCTL_FAIL:-}" = "$*" ]; then exit 1; fi
printf '%s\\n' "$*" >> "$TEST_ACTIONS"
""")
        executable(self.bin / "loginctl", "#!/bin/sh\nexit 1\n")

    def run_command(self, command, **kwargs):
        return subprocess.run(command, env=kwargs.pop("env",self.env), text=True,
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
  "setup --help") exit 0;;
  setup) printf '%s\\n' "$*" >> "$TEST_ACTIONS"; exit 0;;
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
        result = self.run_command(["sh", str(ROOT / "edge/src/install.sh"), "--no-setup"])
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
        self.assertEqual((self.home/".bashrc").read_text().count("# Cypher command path"),1)
        self.assertEqual((self.home/".profile").read_text().count("# Cypher command path"),1)

    def test_pipe_install_uses_the_controlling_terminal_for_setup(self):
        self.release(body=b'''#!/bin/sh
case "$*" in
  --help|"setup --help") exit 0;;
  setup) echo setup-ready; read -r answer; printf 'setup:%s\\n' "$answer" >> "$TEST_ACTIONS"; exit 0;;
  *) exit 1;;
esac
''')
        read_fd, write_fd = os.pipe()
        pid, master = pty.fork()
        if pid == 0:
            os.dup2(read_fd, 0)
            os.close(read_fd)
            os.close(write_fd)
            os.execvpe("sh", ["sh"], self.env)
        os.close(read_fd)
        output = b""
        answered = False
        reaped = False
        try:
            with os.fdopen(write_fd,"wb") as script:
                script.write((ROOT/"edge/src/install.sh").read_bytes())
            deadline = time.monotonic()+15
            while time.monotonic()<deadline:
                if select.select([master],[],[],0.1)[0]:
                    try: output += os.read(master,4096)
                    except OSError: pass
                if b"setup-ready" in output and not answered:
                    os.write(master,b"confirm-fixture\n")
                    answered=True
                got,status=os.waitpid(pid,os.WNOHANG)
                if got:
                    reaped=True
                    self.assertEqual(os.waitstatus_to_exitcode(status),0,output.decode(errors="replace"))
                    break
            self.assertTrue(reaped,output.decode(errors="replace"))
            self.assertEqual((self.root/"actions").read_text(),"setup:confirm-fixture\n")
            self.assertFalse(list(self.app.glob(".install-*")))
        finally:
            os.close(master)
            if not reaped:
                os.kill(pid,signal.SIGKILL)
                os.waitpid(pid,0)

    def test_old_release_is_not_activated_by_the_new_installer(self):
        self.release(version="0.3.2")
        self.install(False)

    def test_path_setup_preserves_existing_managed_files_and_handles_fish(self):
        original=self.home/".cypher/shell-env.sh"
        original.write_text("unrelated content")
        self.release()
        self.install()
        self.assertEqual(original.read_text(),"unrelated content")
        self.assertFalse((self.home/".bashrc").exists())
        original.unlink()
        self.env["SHELL"]="/usr/bin/fish"
        self.install()
        fish=self.home/".config/fish/conf.d/cypher.fish"
        self.assertIn("set -gx PATH",fish.read_text())
        self.assertFalse((self.home/".bashrc").exists())

    def test_arm64_noninteractive_install_does_not_restart_a_service(self):
        self.env.update(TEST_ARCH="aarch64", TEST_BUS="1")
        self.release(arch="aarch64")
        self.install()
        self.assertFalse((self.root / "actions").exists())

    def test_no_user_bus_falls_back_even_with_runtime_dir(self):
        self.env["XDG_RUNTIME_DIR"] = str(self.root)
        self.release()
        self.assertIn("~/.local/bin/cypher setup", self.install().stdout)
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

    def test_removed_tcp_configuration_is_rejected(self):
        for port in ["invalid", "-1", "65536", "0", "27654"]:
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
        path = ipc_path(self.env)
        path.parent.parent.mkdir(mode=0o700, exist_ok=True)
        self.assertEqual(path.parent.parent.stat().st_uid,os.geteuid())
        self.assertEqual(path.parent.parent.stat().st_mode & 0o077,0)
        path.parent.mkdir(mode=0o700, exist_ok=True)
        with socket.socket(socket.AF_UNIX) as listener:
            listener.bind(str(path));os.chmod(path,0o600);listener.listen(8)
            self.assertEqual(self.cli("status", timeout=5).returncode, 1 if platform.system()=="Linux" else 0)
            result = self.cli("sync", timeout=8)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("timed out", result.stderr)
        path.unlink()

    def test_headless_lock_status_and_graceful_sigterm(self):
        path = ipc_path(self.env)
        with tempfile.TemporaryFile() as output:
            process = subprocess.Popen([BINARY, "headless"], env=self.env,
                                       stdin=subprocess.DEVNULL, stdout=output, stderr=output)
            try:
                for _ in range(100):
                    if process.poll() is not None:
                        output.seek(0)
                        self.fail(output.read().decode(errors="replace"))
                    try:
                        with socket.socket(socket.AF_UNIX) as connection:
                            connection.settimeout(.1);connection.connect(str(path))
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

    def test_two_real_engines_need_no_ports_and_stop_independently(self):
        processes=[]
        environments=[]
        try:
            for name in ["device-a","device-b"]:
                env={**self.env,"CYPHER_DATA_DIR":str(self.home/name)}
                environments.append(env)
                log=(self.root/(name+".log")).open("wb")
                try:
                    process=subprocess.Popen([BINARY,"headless"],env=env,
                        stdin=subprocess.DEVNULL,stdout=log,stderr=log)
                finally:log.close()
                processes.append(process)
            for env,process in zip(environments,processes):
                deadline=time.monotonic()+10
                while time.monotonic()<deadline:
                    self.assertIsNone(process.poll())
                    result=self.run_command([BINARY,"sync"],env=env)
                    if result.returncode==0:break
                    time.sleep(.05)
                self.assertEqual(result.returncode,0,result.stderr)
                self.assertTrue(ipc_path(env).is_socket())
            ids=[(Path(env["CYPHER_DATA_DIR"])/"device-id").read_text() for env in environments]
            self.assertNotEqual(ids[0],ids[1])
            self.assertNotEqual(ipc_path(environments[0]),ipc_path(environments[1]))
            processes[0].send_signal(signal.SIGTERM)
            self.assertEqual(processes[0].wait(timeout=10),0)
            self.assertFalse(ipc_path(environments[0]).exists())
            self.assertEqual(self.run_command([BINARY,"sync"],env=environments[1]).returncode,0)
        finally:
            for process in processes:
                if process.poll() is None:
                    process.send_signal(signal.SIGTERM)
                    try:process.wait(timeout=10)
                    except subprocess.TimeoutExpired:process.kill();process.wait(timeout=5)

    @unittest.skipUnless(platform.system() == "Linux", "requires the Linux CLI branch")
    def test_systemd_units_are_distinct_for_two_data_directories(self):
        units=[]
        for name in ["instance-a","instance-b"]:
            self.env["CYPHER_DATA_DIR"]=str(self.home/name)
            result=self.cli("daemon","install")
            self.assertEqual(result.returncode,0,result.stderr)
            unit="cypher@"+ipc_path(self.env).parent.name+".service"
            units.append(unit)
            path=self.home/".config/systemd/user"/unit
            self.assertTrue(path.is_file())
            escaped_data_dir=str((self.home/name).resolve()).replace("%","%%")
            self.assertIn('Environment="CYPHER_DATA_DIR='+escaped_data_dir+'"',path.read_text())
        self.assertNotEqual(units[0],units[1])
        actions=(self.root/"actions").read_text()
        for unit in units:self.assertIn("--user restart "+unit,actions)

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

@unittest.skipUnless(BINARY and platform.system()=="Linux", "requires a Linux headless binary")
class GuidedSetup(Fixture):
    def setUp(self):
        super().setUp()
        self.env.update(TEST_BUS="1",
                        TEST_ENGINE_BINARY=BINARY,TEST_PID=str(self.root/"engine.pid"),
                        TEST_ENGINE_LOG=str(self.root/"engine.log"),
                        TEST_UNIT=str(self.home/".config/systemd/user/cypher.service"))
        executable(self.bin/"systemctl",f'''#!{sys.executable}
import os, pathlib, signal, subprocess, sys, time
args=sys.argv[1:]
if " ".join(args)==os.environ.get("TEST_SYSTEMCTL_FAIL"): sys.exit(1)
pidfile=pathlib.Path(os.environ["TEST_PID"])
pid=int(pidfile.read_text()) if pidfile.exists() else 0
def alive():
    if not pid: return False
    try: return pathlib.Path("/proc/"+str(pid)+"/stat").read_text().split(") ",1)[1][0]!="Z"
    except FileNotFoundError: return False
if args==["--user","show-environment"]: sys.exit(0 if os.environ["TEST_BUS"]=="1" else 1)
if "--property=MainPID" in args: print(pid if alive() else 0); sys.exit(0)
if "--property=FragmentPath" in args:
    path=os.environ["TEST_UNIT"]; print(path if pathlib.Path(path).exists() else ""); sys.exit(0)
with open(os.environ["TEST_ACTIONS"],"a") as out: out.write(" ".join(args)+"\\n")
if len(args)>1 and args[1]=="stop":
    if alive():
        os.kill(pid,signal.SIGTERM)
        for _ in range(100):
            if not alive(): break
            time.sleep(.05)
        else: sys.exit(1)
    pidfile.write_text("0")
if len(args)>1 and args[1] in ("start","restart") and not alive():
    with open(os.environ["TEST_ENGINE_LOG"],"ab") as out:
        child=subprocess.Popen([os.environ["TEST_ENGINE_BINARY"],"headless"],
            stdin=subprocess.DEVNULL,stdout=out,stderr=out,start_new_session=True)
    pidfile.write_text(str(child.pid))
''')
        executable(self.bin/"loginctl","#!/bin/sh\ncase \"$*\" in *--property=Linger*) echo yes;; esac\nexit 0\n")
        self.routes={}
        routes=self.routes
        class Handler(http.server.BaseHTTPRequestHandler):
            def do_GET(handler):
                status,body=routes.get(handler.path,(404,b""))
                handler.send_response(status);handler.send_header("Content-Length",str(len(body)))
                handler.end_headers();handler.wfile.write(body)
            def log_message(handler,*_): pass
        self.server=http.server.ThreadingHTTPServer(("127.0.0.1",0),Handler)
        threading.Thread(target=self.server.serve_forever,daemon=True).start()
        self.addCleanup(self.server.server_close);self.addCleanup(self.server.shutdown)
        self.env["CYPHER_PI_RUNTIME_BASE_URL"]=f"http://127.0.0.1:{self.server.server_port}"
        inner={"version":"1","piVersion":"fixture","plugins":{}}
        contents={
            "runtime.json":json.dumps(inner).encode(),
            "bin/pi":b"#!/bin/sh\necho fixture\n",
            "bin/node":b"#!/bin/sh\nexit 0\n",
            "pi/package.json":b'{"name":"fixture","version":"1"}',
            "npm/package.json":b'{"private":true}',
            "extensions/cypher-provider-auth.ts":b"export default function() {}",
            "provider-service.mjs":b"",
        }
        archive=io.BytesIO()
        with tarfile.open(fileobj=archive,mode="w:gz") as tar:
            for name,body in contents.items():
                info=tarfile.TarInfo("fixture/"+name);info.mode=0o755;info.size=len(body)
                tar.addfile(info,io.BytesIO(body))
        body=archive.getvalue()
        self.manifest=dict(inner,files={"linux-"+platform.machine():{
            "url":"runtime.tar.gz","size":len(body),"sha256":hashlib.sha256(body).hexdigest()}})
        self.routes["/manifest.json"]=(200,json.dumps(self.manifest).encode())
        self.routes["/runtime.tar.gz"]=(200,body)
        self.addCleanup(self.stop_engine)

    def stop_engine(self):
        path=self.root/"engine.pid"
        if path.exists() and path.read_text().strip()!="0":
            self.run_command(["systemctl","--user","stop","cypher.service"])

    def setup(self,success=True):
        result=self.run_command([BINARY,"setup","--local","--non-interactive"],timeout=45)
        if success:self.assertEqual(result.returncode,0,result.stdout+result.stderr)
        else:self.assertNotEqual(result.returncode,0,result.stdout)
        return result

    def test_local_setup_installs_runtime_starts_service_and_is_idempotent(self):
        result=self.setup()
        self.assertIn("Local device ready",result.stdout)
        self.assertTrue((self.home/".cypher/pi-runtime/current/bin/pi").is_file())
        first=(self.root/"engine.pid").read_text()
        self.setup()
        self.assertEqual((self.root/"engine.pid").read_text(),first,"idempotent setup restarted the engine")
        result=self.run_command([BINARY])
        self.assertEqual(result.returncode,0,result.stderr)
        self.assertIn("Engine:   running",result.stdout)

    def test_unattended_remote_setup_does_not_start_a_service_or_request_login(self):
        result=self.run_command([BINARY,"setup","--non-interactive"])
        self.assertNotEqual(result.returncode,0)
        self.assertIn("needs a terminal",result.stderr)
        self.assertFalse((self.home/".cypher").exists())
        self.assertFalse((self.root/"actions").exists())

    def test_failed_runtime_download_does_not_install_a_service(self):
        self.manifest["files"]["linux-"+platform.machine()]["sha256"]="0"*64
        self.routes["/manifest.json"]=(200,json.dumps(self.manifest).encode())
        result=self.setup(False)
        self.assertIn("Runtime installation failed",result.stderr)
        self.assertFalse((self.home/".config/systemd/user/cypher.service").exists())
        self.assertFalse((self.home/".cypher/setup-completed.json").exists())

    def test_runtime_repair_failure_restores_the_previous_service(self):
        self.setup()
        (self.home/".cypher/pi-runtime/current").unlink()
        self.manifest["files"]["linux-"+platform.machine()]["url"]="missing.tar.gz"
        # A new revision forces a download, instead of reusing the known version.
        self.manifest["version"]="2"
        self.routes["/manifest.json"]=(200,json.dumps(self.manifest).encode())
        result=self.setup(False)
        self.assertIn("Previous background service restarted",result.stderr)
        deadline=time.monotonic()+8
        while time.monotonic()<deadline:
            result=self.run_command([BINARY,"status"])
            if "Engine:   running" in result.stdout: break
            time.sleep(.1)
        self.assertIn("Engine:   running",result.stdout)

    def test_missing_user_bus_is_not_reported_as_ready(self):
        self.env["TEST_BUS"]="0"
        result=self.setup(False)
        self.assertIn("--foreground",result.stderr)
        self.assertFalse((self.root/"engine.pid").exists())
        self.assertFalse((self.home/".cypher/setup-completed.json").exists())

    def test_cancel_during_sign_in_restores_service_and_does_not_hang_on_stdin(self):
        self.setup()
        master,slave=pty.openpty()
        log=self.root/"setup-output.log"
        with log.open("wb") as output:
            process=subprocess.Popen([BINARY,"setup"],env=self.env,stdin=slave,
                                     stdout=output,stderr=output)
        try:
            def wait_text(text):
                deadline=time.monotonic()+10
                while time.monotonic()<deadline:
                    if text in log.read_text(): return
                    if process.poll() is not None: break
                    time.sleep(.05)
                self.fail("Setup did not reach expected fixture prompt: "+text)
            wait_text("Connect to your Cypher account?")
            os.write(master,b"y\n")
            wait_text("Then paste the code")
            process.send_signal(signal.SIGINT)
            self.assertNotEqual(process.wait(timeout=8),0)
            self.assertIn("Previous background service restarted",log.read_text())
            self.assertFalse((self.home/".cypher/session.json").exists())
        finally:
            os.close(master);os.close(slave)
            if process.poll() is None:process.kill();process.wait(timeout=5)

    def test_foreign_unit_is_not_overwritten(self):
        unit=Path(self.env["TEST_UNIT"]);unit.parent.mkdir(parents=True)
        original="[Service]\nExecStart=/bin/unrelated\n"
        unit.write_text(original)
        self.setup(False)
        self.assertEqual(unit.read_text(),original)
        self.assertFalse((self.root/"engine.pid").exists())

    def test_failed_service_start_stays_incomplete_and_can_be_retried(self):
        self.env["TEST_SYSTEMCTL_FAIL"]="--user restart cypher.service"
        self.setup(False)
        self.assertFalse((self.home/".cypher/setup-completed.json").exists())
        self.assertFalse((self.root/"engine.pid").exists())
        del self.env["TEST_SYSTEMCTL_FAIL"]
        self.setup()

    def test_foreground_mode_does_not_install_service_and_forwards_termination(self):
        self.env["TEST_BUS"]="0"
        log=self.root/"foreground.log"
        with log.open("wb") as output:
            process=subprocess.Popen([BINARY,"setup","--local","--foreground","--non-interactive"],
                env=self.env,stdin=subprocess.DEVNULL,stdout=output,stderr=output)
        try:
            deadline=time.monotonic()+20
            while time.monotonic()<deadline:
                if process.poll() is not None: self.fail(log.read_text())
                status=self.run_command([BINARY,"status"])
                if "Engine:   running" in status.stdout:break
                time.sleep(.1)
            self.assertIn("Engine:   running",status.stdout)
            self.assertFalse(Path(self.env["TEST_UNIT"]).exists())
            self.assertIn("not a persistent background service",log.read_text())
            process.send_signal(signal.SIGTERM)
            self.assertEqual(process.wait(timeout=12),0,log.read_text())
            self.assertIn("Engine:   stopped",self.run_command([BINARY,"status"]).stdout)
        finally:
            if process.poll() is None:
                process.send_signal(signal.SIGTERM)
                try:process.wait(timeout=12)
                except subprocess.TimeoutExpired:process.kill();process.wait(timeout=5)


if __name__ == "__main__":
    unittest.main(argv=[sys.argv[0], *TEST_ARGS], verbosity=2)

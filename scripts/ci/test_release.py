import copy
import hashlib
import http.server
import importlib.util
import io
import json
import os
from pathlib import Path
import subprocess
import sys
import tarfile
import tempfile
import threading
import unittest
from unittest.mock import patch
import urllib.error
import urllib.parse

import release
import workflow_policy


class Fixture(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="cypher-release-tests-")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.dist = self.root / "dist"
        self.dist.mkdir()
        self.out = self.root / "plan"
        self.v = "1.2.3"
        self.rv = "0.85.0.4"
        self.spec = release.ROOT / "dist/pi-runtime/package.json"
        dependencies = json.loads(self.spec.read_text())["dependencies"]
        self.plugins = {k: v for k, v in dependencies.items() if not k.startswith("@earendil-works/")}
        self.inner = {"version": self.rv, "piVersion": dependencies["@earendil-works/pi-coding-agent"],
                      "plugins": self.plugins}
        for name in release.app_files(self.v):
            if name.endswith(".dmg"):
                (self.dist / name).write_bytes(b"fixture dmg")
            else:
                mac = name.endswith("-app.tar.gz")
                root = "Cypher.app" if mac else name[:-7]
                binary = "Contents/MacOS/cypher" if mac else "cypher"
                self.tar(name, {root + "/" + binary: b"fixture binary"})
        for platform in release.PLATFORMS:
            name = "cypher-pi-runtime-{}-{}".format(self.rv, platform)
            members = {name + "/" + p: b"fixture" for p in [
                "bin/node", "bin/pi", "bin/npm", "provider-service.mjs",
                "extensions/cypher-provider-auth.ts"]}
            members[name + "/runtime.json"] = release.json_bytes(self.inner)
            self.tar(name + ".tar.gz", members)
            h, size = release.digest(self.dist / (name + ".tar.gz"))
            meta = dict(self.inner, minimumCypherVersion="0.2.2", files={
                platform: {"url": name + ".tar.gz", "size": size, "sha256": h},
            })
            (self.dist / (name + ".json")).write_bytes(release.json_bytes(meta))

    def tar(self, name, members):
        with tarfile.open(self.dist / name, "w:gz") as archive:
            for name, data in members.items():
                info = tarfile.TarInfo(name)
                info.size = len(data)
                info.mode = 0o755
                archive.addfile(info, io.BytesIO(data))

    def metadata(self, platform="linux-x86_64"):
        return self.dist / "cypher-pi-runtime-{}-{}.json".format(self.rv, platform)

    def change_metadata(self, modify, platform="linux-x86_64"):
        path = self.metadata(platform)
        value = json.loads(path.read_bytes())
        modify(value)
        path.write_bytes(release.json_bytes(value))

    def plan(self):
        return release.validate(self.dist, self.v, self.out, self.spec)


class Validation(Fixture):
    def test_complete_plan_has_all_platforms_and_checksums(self):
        plan = self.plan()
        self.assertEqual(set(plan["runtime"]["files"]), set(release.PLATFORMS))
        for name, meta in plan["app"]["files"].items():
            self.assertEqual((self.out / (name + ".sha256")).read_text().strip(), meta["sha256"])
        self.assertEqual(len(plan["assets"]), 16)

    def test_duplicate_platforms_cannot_be_merged_away(self):
        self.change_metadata(lambda m: m.update(files={"linux-x86_64": next(iter(m["files"].values()))}),
                             "macos-arm64")
        with self.assertRaisesRegex(release.ReleaseError, "platform"):
            self.plan()
        self.assertFalse(self.out.exists(), "validation failure must generate nothing")

    def test_invalid_size_checksum_and_url_fail(self):
        original = self.metadata().read_bytes()
        for field, value in [("size", 1), ("size", True), ("sha256", "0" * 64),
                             ("url", "../outside"), ("url", "https://example.com/archive")]:
            with self.subTest(field=field, value=value):
                self.metadata().write_bytes(original)
                self.change_metadata(lambda m: m["files"]["linux-x86_64"].update({field: value}))
                with self.assertRaises(release.ReleaseError):
                    self.plan()

    def test_missing_or_extra_artifacts_fail(self):
        extra = self.dist / "unexpected"
        extra.write_text("x")
        with self.assertRaisesRegex(release.ReleaseError, "Unexpected"):
            self.plan()
        extra.unlink()
        self.metadata().unlink()
        with self.assertRaises(release.ReleaseError):
            self.plan()

    def test_wrong_minimum_or_plugin_spec_fails(self):
        original = self.metadata().read_bytes()
        for field, value in [("minimumCypherVersion", "9.9.9"), ("plugins", {}),
                             ("piVersion", "0.1.0"), ("version", "../escape")]:
            self.metadata().write_bytes(original)
            self.change_metadata(lambda m: m.update({field: value}))
            with self.assertRaises(release.ReleaseError):
                self.plan()

    def test_tar_metadata_and_paths_are_checked_not_just_checksums(self):
        path = self.metadata()
        meta = json.loads(path.read_bytes())
        name = meta["files"]["linux-x86_64"]["url"]
        self.tar(name, {"../outside": b"x"})
        h, size = release.digest(self.dist / name)
        meta["files"]["linux-x86_64"].update(sha256=h, size=size)
        path.write_bytes(release.json_bytes(meta))
        with self.assertRaisesRegex(release.ReleaseError, "path"):
            self.plan()

    def test_inner_runtime_metadata_must_agree(self):
        original = release.archive_metadata
        def read(*args, **kwargs):
            value = original(*args, **kwargs)
            if value is not None:
                value["version"] = "0.1.0"
            return value
        with patch.object(release, "archive_metadata", side_effect=read):
            with self.assertRaisesRegex(release.ReleaseError, "disagrees"):
                self.plan()

    def test_archive_links_cannot_escape_root(self):
        path = self.dist / "links.tar.gz"
        with tarfile.open(path, "w:gz") as archive:
            entry = tarfile.TarInfo("root/link")
            entry.type = tarfile.SYMTYPE
            entry.linkname = "../../outside"
            archive.addfile(entry)
        with self.assertRaisesRegex(release.ReleaseError, "link"):
            release.archive_metadata(path, "root", [])


class Store:
    def __init__(self, operations):
        self.objects = {}
        self.operations = operations
        self.fail = None
        self.after_put = None

    def get(self, key):
        return self.objects.get(key)

    def digest(self, key):
        data = self.get(key)
        return None if data is None else (release.sha256(data), len(data))

    def put(self, key, value):
        if key == self.fail:
            raise release.ReleaseError("fixture upload failure")
        self.operations.append(("put", key))
        self.objects[key] = value.read_bytes() if isinstance(value, Path) else value
        if self.after_put:
            self.after_put(self, key)


class GitHub:
    def __init__(self, operations):
        self.operations = operations

    def preflight(self, plan):
        pass

    def stage(self, plan):
        self.operations.append(("draft", None))

    def promote(self):
        self.operations.append(("public", None))


class Publication(Fixture):
    def setUp(self):
        super().setUp()
        self.operations = []
        self.store = Store(self.operations)
        self.github = GitHub(self.operations)
        self.release = self.plan()

    def publish(self):
        release.publish(self.release, self.store, self.github)

    def test_publication_order_and_idempotent_retry(self):
        self.publish()
        self.assertEqual(self.operations[0], ("draft", None))
        self.assertEqual(self.operations[-4:], [
            ("put", "runtimes/pi/manifest.json"), ("put", "manifest.json"),
            ("put", "latest.txt"), ("public", None),
        ])
        self.operations.clear()
        self.publish()
        self.assertFalse(any(kind == "put" for kind, _ in self.operations))

    def test_older_app_or_runtime_never_writes(self):
        for key, data in [
            ("latest.txt", b"9.0.0"),
            ("manifest.json", b'{"version":"9.0.0"}'),
            ("runtimes/pi/manifest.json", b'{"version":"99.0.0"}'),
        ]:
            with self.subTest(key=key):
                self.store.objects = {key: data}
                with self.assertRaisesRegex(release.ReleaseError, "roll back"):
                    self.publish()
                self.assertEqual(self.operations, [])

    def test_same_version_metadata_cannot_be_rewritten(self):
        old = copy.deepcopy(self.release["app"])
        old["files"] = {}
        self.store.objects["manifest.json"] = release.json_bytes(old)
        with self.assertRaisesRegex(release.ReleaseError, "bump"):
            self.publish()
        self.assertEqual(self.operations, [])

    def test_conflict_is_found_before_any_draft_or_object_write(self):
        key = next(reversed(self.release["objects"]))
        self.store.objects[key] = b"different bytes under same version"
        with self.assertRaisesRegex(release.ReleaseError, "immutable"):
            self.publish()
        self.assertEqual(self.operations, [])

    def test_local_changes_after_validation_are_rejected(self):
        self.release["objects"][next(iter(self.release["objects"]))].write_bytes(b"changed")
        with self.assertRaisesRegex(release.ReleaseError, "changed"):
            self.publish()
        self.assertEqual(self.operations, [])

    def test_failed_upload_never_promotes_release_or_channel(self):
        self.store.fail = next(iter(self.release["objects"]))
        with self.assertRaises(release.ReleaseError):
            self.publish()
        self.assertEqual(self.operations, [("draft", None)])
        self.assertIsNone(self.store.get("latest.txt"))

    def test_out_of_band_channel_change_stops_promotion(self):
        def change(store, key):
            store.objects["latest.txt"] = b"9.9.9"
        self.store.after_put = change
        with self.assertRaisesRegex(release.ReleaseError, "changed"):
            self.publish()
        self.assertNotIn(("put", "manifest.json"), self.operations)
        self.assertNotIn(("public", None), self.operations)
        self.assertEqual(self.store.get("latest.txt"), b"9.9.9")

    def test_partial_pointer_failure_can_resume_without_overwrite(self):
        self.store.fail = "manifest.json"
        with self.assertRaises(release.ReleaseError):
            self.publish()
        self.assertIsNone(self.store.get("latest.txt"))
        self.assertNotIn(("public", None), self.operations)
        self.store.fail = None
        self.operations.clear()
        self.publish()
        self.assertEqual([key for kind, key in self.operations if kind == "put"],
                         ["manifest.json", "latest.txt"])


class HttpTests(unittest.TestCase):
    def setUp(self):
        self.routes = {}
        self.received = []
        parent = self
        class Handler(http.server.BaseHTTPRequestHandler):
            def do_GET(self):
                status, body, length = parent.routes.get(self.path, (404, b"", None))
                self.send_response(status)
                self.send_header("Content-Length", str(len(body) if length is None else length))
                self.end_headers()
                if self.command != "HEAD":
                    self.wfile.write(body)

            do_HEAD = do_GET

            def do_PUT(self):
                data = self.rfile.read(int(self.headers["Content-Length"]))
                parent.received.append((self.path, data, self.headers.get("Authorization")))
                self.send_response(200)
                self.end_headers()
                self.wfile.write(b'{"success":true}')

            def log_message(self, *_):
                pass
        self.server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        threading.Thread(target=self.server.serve_forever, daemon=True).start()
        self.addCleanup(self.server.server_close)
        self.addCleanup(self.server.shutdown)
        self.url = "http://127.0.0.1:{}".format(self.server.server_port)

    def test_installer_gate_requires_both_platforms_and_matching_pointers(self):
        v = "1.2.3"
        files = {name: {"sha256": "a" * 64} for name in release.app_files(v)}
        self.routes["/releases/manifest.json"] = (200, release.json_bytes({"version": v, "files": files}), None)
        self.routes["/releases/latest.txt"] = (200, v.encode(), None)
        with self.assertRaises(release.ReleaseError):
            release.check_deploy(self.url)
        for name in release.app_files(v)[:2]:
            self.routes["/releases/" + name + ".sha256"] = (200, b"a" * 64 + b"\n", None)
            self.routes["/releases/" + name] = (200, b"", 20 * 1024 * 1024)
        self.assertEqual(release.check_deploy(self.url), v)
        self.routes["/releases/latest.txt"] = (200, b"0.1.0", None)
        with self.assertRaisesRegex(release.ReleaseError, "pointers"):
            release.check_deploy(self.url)

    def test_authenticated_put_streams_file_and_read_errors_are_not_missing(self):
        api = release.Api(self.url, "fixture-token")
        with tempfile.TemporaryDirectory() as root:
            path = Path(root) / "file"
            path.write_bytes(b"binary fixture")
            api.request("/object", "PUT", path)
        self.assertEqual(self.received, [("/object", b"binary fixture", "Bearer fixture-token")])
        self.routes["/object"] = (403, b"secret-response-never-echo", None)
        with self.assertRaises(release.ReleaseError) as error:
            api.request("/object", missing=True)
        self.assertNotIn("secret-response", str(error.exception))
        self.assertNotIn("fixture-token", str(error.exception))
        self.routes["/object"] = (404, b"", None)
        self.assertIsNone(api.request("/object", missing=True))

    def test_metadata_limit_and_invalid_credentials(self):
        self.routes["/large"] = (200, b"x" * 32, None)
        api = release.Api(self.url, "fixture")
        with self.assertRaisesRegex(release.ReleaseError, "limit"):
            api.request("/large", limit=8)
        with self.assertRaisesRegex(release.ReleaseError, "credential"):
            release.Api(self.url, "bad\nsecret")


class GitHubApi:
    def __init__(self, tag, commit):
        self.tag, self.commit = tag, commit
        self.release = None
        self.latest = None
        self.interrupt = False
        self.deleted = []
        self.promoted = 0

    def request(self, path, method="GET", data=None, **_):
        path = path.split("/repos/fixture/repo", 1)[1]
        if path == "/releases/latest":
            return release.json_bytes(self.latest) if self.latest else None
        if path.startswith("/git/ref/tags/"):
            return release.json_bytes({"object": {"type": "commit", "sha": self.commit}})
        if path.startswith("/releases/tags/"):
            return release.json_bytes(self.release) if self.release else None
        if path == "/releases/generate-notes":
            return release.json_bytes({"body": "fixture release notes"})
        if path == "/releases" and method == "POST":
            self.release = dict(data, id=1, assets=[])
            return release.json_bytes(self.release)
        if path.startswith("/releases/1/assets?") and method == "POST":
            name = urllib.parse.parse_qs(urllib.parse.urlsplit(path).query)["name"][0]
            asset = {"id": len(self.release["assets"]) + 1, "name": name,
                     "size": 0, "state": "starter", "digest": None}
            self.release["assets"].append(asset)
            if self.interrupt:
                self.interrupt = False
                raise release.ReleaseError("interrupted asset upload")
            h, size = release.digest(data)
            asset.update(size=size, state="uploaded", digest="sha256:" + h)
            return release.json_bytes(asset)
        if path.startswith("/releases/assets/") and method == "DELETE":
            asset_id = int(path.rsplit("/", 1)[1])
            self.deleted.append(asset_id)
            self.release["assets"] = [a for a in self.release["assets"] if a["id"] != asset_id]
            return b""
        if path == "/releases/1" and method == "PATCH":
            self.promoted += 1
            self.release.update(data)
            return release.json_bytes(self.release)
        raise AssertionError((method, path))


class GitHubPublication(Fixture):
    def setUp(self):
        super().setUp()
        self.plan_value = self.plan()
        self.api = GitHubApi("cypher-v" + self.v, "a" * 40)
        self.github = release.GitHubRelease("fixture/repo", "fixture-token", self.api.tag, self.api.commit)
        self.github.api = self.github.upload = self.api
        self.store = Store([])

    def test_real_github_adapter_stages_checksums_then_promotes(self):
        release.publish(self.plan_value, self.store, self.github)
        self.assertFalse(self.api.release["draft"])
        self.assertTrue(self.api.release["body"].startswith("<!-- cypher-release-plan:"))
        self.assertEqual(len(self.api.release["assets"]), len(self.plan_value["assets"]))
        release.publish(self.plan_value, self.store, self.github)
        self.assertEqual(self.api.promoted, 1)

    def test_interrupted_owned_draft_upload_can_resume(self):
        self.api.interrupt = True
        with self.assertRaisesRegex(release.ReleaseError, "interrupted"):
            release.publish(self.plan_value, self.store, self.github)
        self.assertTrue(self.api.release["draft"])
        self.assertEqual(self.store.objects, {})
        release.publish(self.plan_value, self.store, self.github)
        self.assertEqual(self.api.deleted, [1])
        self.assertFalse(self.api.release["draft"])

    def test_unowned_draft_and_moved_tag_are_preserved(self):
        self.api.release = {"id": 1, "draft": True, "body": "user-authored draft", "assets": []}
        with self.assertRaisesRegex(release.ReleaseError, "not owned"):
            release.publish(self.plan_value, self.store, self.github)
        self.assertEqual(self.store.objects, {})
        self.assertEqual(self.api.deleted, [])
        self.api.release = None
        self.api.commit = "b" * 40
        with self.assertRaisesRegex(release.ReleaseError, "tag moved"):
            release.publish(self.plan_value, self.store, self.github)

    def test_github_latest_cannot_regress_even_if_r2_is_older(self):
        self.api.latest = {"tag_name": "cypher-v9.9.9"}
        with self.assertRaisesRegex(release.ReleaseError, "regress"):
            release.publish(self.plan_value, self.store, self.github)
        self.assertEqual(self.store.objects, {})


class Policies(unittest.TestCase):
    def test_manual_tag_context_is_never_a_publish(self):
        with tempfile.TemporaryDirectory() as root:
            output = Path(root) / "outputs"
            env = {k: v for k, v in os.environ.items() if not k.startswith(("GITHUB_", "CLOUDFLARE_"))}
            env.update(GITHUB_OUTPUT=str(output), GITHUB_EVENT_NAME="workflow_dispatch",
                       GITHUB_REF="refs/tags/cypher-v0.2.2")
            command = [sys.executable, str(Path(release.__file__)), "context"]
            p = subprocess.run(command, env=env, capture_output=True, text=True)
            self.assertEqual(p.returncode, 0, p.stderr)
            self.assertIn("publish=false", output.read_text())
            env["GITHUB_EVENT_NAME"] = "push"
            p = subprocess.run(command, env=env, capture_output=True, text=True)
            self.assertNotEqual(p.returncode, 0)
            self.assertIn("NOT PUBLISHED", p.stderr)

    def test_production_queue_policy_and_no_delta_deployment(self):
        text = "concurrency:\n  group: cypher-production\n  cancel-in-progress: false\n  queue: max\n"
        self.assertEqual(workflow_policy.check_queue(text), 1)
        for invalid in [text.replace("max", "typo"), text.replace("false", "true"),
                        text.replace("cypher-production", "per-tag")]:
            with self.assertRaises(ValueError):
                workflow_policy.check_queue(invalid)
        workflow_policy.main()
        deploy = (release.ROOT / ".github/workflows/deploy.yml").read_text()
        code = "\n".join(line for line in deploy.splitlines() if not line.lstrip().startswith("#"))
        self.assertNotIn("git diff", code)
        self.assertNotIn("grep -q", code)
        self.assertIn("ref: main", code)
        self.assertIn("needs.prepare.outputs.commit", code)

    def test_runtime_archives_ignore_mtime_and_owner(self):
        source = Path(__file__).with_name("deterministic-tar.py")
        spec = importlib.util.spec_from_file_location("deterministic_tar", source)
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        with tempfile.TemporaryDirectory() as root:
            root = Path(root)
            directory = root / "runtime"
            directory.mkdir()
            file = directory / "node"
            file.write_bytes(b"same bytes")
            first, second = root / "first.tar.gz", root / "second.tar.gz"
            module.pack(directory, first)
            os.utime(file, (100000, 100000))
            module.pack(directory, second)
            self.assertEqual(hashlib.sha256(first.read_bytes()).digest(),
                             hashlib.sha256(second.read_bytes()).digest())


if __name__ == "__main__":
    unittest.main()

import copy
import hashlib
import http.server
import importlib.util
import io
import json
import os
from pathlib import Path
import re
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
        self.spec = release.ROOT / "dist/pi-runtime/package.json"
        self.rv = json.loads(self.spec.with_name("release.json").read_text())["version"]
        dependencies = json.loads(self.spec.read_text())["dependencies"]
        self.plugins = {k: v for k, v in dependencies.items() if not k.startswith("@earendil-works/")}
        self.inner = {"version": self.rv, "piVersion": dependencies["@earendil-works/pi-coding-agent"],
                      "plugins": self.plugins}
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

    def tar(self, name, members, into=None):
        with tarfile.open((into or self.dist) / name, "w:gz") as archive:
            for name, data in members.items():
                info = tarfile.TarInfo(name)
                info.size = len(data)
                info.mode = 0o755
                archive.addfile(info, io.BytesIO(data))

    def app_dist(self, platform, build=1, v=None, body=b"fixture binary"):
        """One desktop platform's artifacts, alone — decoupled releases never
        carry another platform's build or the Runtime."""
        v = v or self.v
        target = self.root / "dist-{}-{}-{}".format(platform, v, build)
        target.mkdir(exist_ok=True)
        for name in release.platform_files(v, build, platform).values():
            if name.endswith(".dmg"):
                (target / name).write_bytes(b"fixture dmg" + body)
            else:
                mac = name.endswith("-app.tar.gz")
                root = "Cypher.app" if mac else name[:-7]
                binary = "Contents/MacOS/cypher" if mac else "cypher"
                self.tar(name, {root + "/" + binary: body}, into=target)
        return target

    def platform_plan(self, platform="linux", build=1, v=None, body=b"fixture binary"):
        return release.validate_platform(
            self.app_dist(platform, build, v, body), v or self.v, build, platform,
            self.root / "plan-{}-{}-{}".format(platform, v or self.v, build))

    def published(self, platform, v=None, build=1):
        """The channel state a completed publication of that platform leaves."""
        return release.json_bytes(self.platform_plan(platform, build, v)["app"])

    def metadata(self, platform="linux-x86_64"):
        return self.dist / "cypher-pi-runtime-{}-{}.json".format(self.rv, platform)

    def change_metadata(self, modify, platform="linux-x86_64"):
        path = self.metadata(platform)
        value = json.loads(path.read_bytes())
        modify(value)
        path.write_bytes(release.json_bytes(value))

    def plan(self):
        return release.validate_runtime(self.dist, self.rv, self.out, self.spec)


class Validation(Fixture):
    def test_complete_plan_has_all_platforms_and_checksums(self):
        plan = self.plan()
        self.assertEqual(set(plan["runtime"]["files"]), set(release.PLATFORMS))

    def test_platform_plan_covers_only_its_own_artifacts(self):
        plan = self.platform_plan("macos")
        out = self.root / "plan-macos-{}-1".format(self.v)
        for name, meta in plan["app"]["files"].items():
            self.assertEqual((out / (name + ".sha256")).read_text().strip(), meta["sha256"])
        self.assertEqual(set(plan["app"]["roles"]), {"dmg-arm64", "app-arm64"})
        self.assertNotIn("runtime", plan)
        self.assertTrue(all("linux" not in name for name in plan["app"]["files"]))

    def test_build_one_keeps_the_legacy_name_and_rebuilds_do_not(self):
        self.assertEqual(release.platform_files(self.v, 1, "linux"),
                         {"headless-x86_64": "cypher-1.2.3-linux-x86_64.tar.gz",
                          "headless-aarch64": "cypher-1.2.3-linux-aarch64.tar.gz"})
        second = release.platform_files(self.v, 2, "linux")
        self.assertEqual(second["headless-x86_64"], "cypher-1.2.3-b2-linux-x86_64.tar.gz")
        # A re-cut must not collide with the immutable build-1 object.
        self.assertFalse(set(second.values()) & set(release.platform_files(self.v, 1, "linux").values()))

    def test_another_platforms_artifact_is_rejected(self):
        dist = self.app_dist("linux")
        (dist / release.platform_files(self.v, 1, "macos")["dmg-arm64"]).write_bytes(b"x")
        with self.assertRaisesRegex(release.ReleaseError, "Unexpected"):
            release.validate_platform(dist, self.v, 1, "linux", self.out)
        self.assertFalse(self.out.exists(), "validation failure must generate nothing")

    def test_runtime_artifact_is_not_accepted_in_a_platform_release(self):
        dist = self.app_dist("linux")
        (dist / "cypher-pi-runtime-{}-linux-x86_64.json".format(self.rv)).write_bytes(b"{}")
        with self.assertRaisesRegex(release.ReleaseError, "Unexpected"):
            release.validate_platform(dist, self.v, 1, "linux", self.out)

    def test_build_number_must_be_a_positive_integer(self):
        for bad in ("0", "-1", "01", "x", "1.0", ""):
            with self.subTest(build=bad), self.assertRaises(release.ReleaseError):
                release.build_number(bad)

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

    def list(self):
        return {key: len(value) for key, value in self.objects.items()}

    def delete(self, key):
        self.operations.append(("delete", key))
        self.objects.pop(key, None)


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
        self.release = self.platform_plan("linux")

    def publish(self, plan=None):
        release.publish_platform(plan or self.release, self.store, self.github)

    def puts(self):
        return [key for kind, key in self.operations if kind == "put"]

    def test_publication_order_and_idempotent_retry(self):
        self.publish()
        self.assertEqual(self.operations[0], ("draft", None))
        self.assertEqual(self.operations[-4:], [
            ("put", "linux/manifest.json"), ("put", "linux/latest.txt"),
            ("put", "linux/stem.txt"), ("public", None),
        ])
        self.operations.clear()
        self.publish()
        self.assertFalse(any(kind == "put" for kind, _ in self.operations))

    def test_one_platform_never_touches_another_or_the_runtime(self):
        self.publish()
        self.assertNotIn("macos/manifest.json", self.puts())
        self.assertNotIn("runtimes/pi/manifest.json", self.puts())
        self.assertTrue(all("macos" not in key for key in self.puts()))

    def test_legacy_pointers_wait_for_full_coverage_then_move(self):
        # Linux alone cannot move the shared pointers: a macOS client reading
        # them would rebuild a name that does not exist.
        self.publish()
        self.assertNotIn("manifest.json", self.puts())
        self.assertNotIn("latest.txt", self.puts())
        self.operations.clear()
        self.publish(self.platform_plan("macos"))
        self.assertIn("manifest.json", self.puts())
        legacy = json.loads(self.store.get("manifest.json"))
        self.assertEqual(legacy["version"], self.v)
        self.assertEqual(set(legacy["files"]), set(release.app_files(self.v)))
        self.assertEqual(self.store.get("latest.txt"), self.v.encode())

    def test_a_rebuild_holds_the_legacy_channel_still(self):
        self.publish()
        self.publish(self.platform_plan("macos"))
        self.operations.clear()
        # Build 2 exists only under its own name, so pre-decoupling clients stay
        # on the fully covered build rather than a superseded one.
        self.publish(self.platform_plan("linux", build=2, body=b"rebuilt"))
        self.assertIn("linux/manifest.json", self.puts())
        self.assertNotIn("manifest.json", self.puts())
        self.assertEqual(json.loads(self.store.get("manifest.json"))["version"], self.v)
        self.assertEqual(json.loads(self.store.get("linux/manifest.json"))["build"], 2)

    def test_platforms_may_sit_on_different_versions(self):
        self.publish(self.platform_plan("linux", v="1.2.3"))
        self.publish(self.platform_plan("macos", v="1.3.0"))
        self.assertEqual(json.loads(self.store.get("linux/manifest.json"))["version"], "1.2.3")
        self.assertEqual(json.loads(self.store.get("macos/manifest.json"))["version"], "1.3.0")
        # Neither version is fully covered, so the shared channel stays unset.
        self.assertIsNone(self.store.get("manifest.json"))

    def test_older_version_or_build_never_writes(self):
        for key, data in [
            ("linux/manifest.json", b'{"version":"9.0.0","build":1}'),
            ("linux/manifest.json", b'{"version":"1.2.3","build":7}'),
        ]:
            with self.subTest(data=data):
                self.operations.clear()
                self.store.objects = {key: data}
                with self.assertRaisesRegex(release.ReleaseError, "roll back"):
                    self.publish()
                self.assertEqual(self.operations, [])

    def test_same_version_metadata_cannot_be_rewritten(self):
        old = copy.deepcopy(self.release["app"])
        old["files"] = {}
        self.store.objects["linux/manifest.json"] = release.json_bytes(old)
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
            store.objects["linux/latest.txt"] = b"9.9.9"
        self.store.after_put = change
        with self.assertRaisesRegex(release.ReleaseError, "changed"):
            self.publish()
        self.assertNotIn(("put", "linux/manifest.json"), self.operations)
        self.assertNotIn(("public", None), self.operations)
        self.assertEqual(self.store.get("linux/latest.txt"), b"9.9.9")

    def test_partial_pointer_failure_can_resume_without_overwrite(self):
        self.store.fail = "linux/manifest.json"
        with self.assertRaises(release.ReleaseError):
            self.publish()
        self.assertIsNone(self.store.get("linux/latest.txt"))
        self.assertNotIn(("public", None), self.operations)
        self.store.fail = None
        self.operations.clear()
        self.publish()
        self.assertEqual(self.puts(),
                         ["linux/manifest.json", "linux/latest.txt", "linux/stem.txt"])


class RuntimeOnly(Fixture):
    """The decoupled Runtime release: one pointer moves, the application channel does not."""

    def setUp(self):
        super().setUp()
        self.operations = []
        self.store = Store(self.operations)
        self.published_app = release.json_bytes({"version": self.v, "files": {}})
        self.store.objects["manifest.json"] = self.published_app
        self.store.objects["latest.txt"] = self.v.encode()
        for platform in release.DESKTOP_PLATFORMS:
            self.store.objects[platform + "/manifest.json"] = self.published(platform)
            self.store.objects[platform + "/latest.txt"] = self.v.encode()

    def plan(self):
        return release.validate_runtime(self.dist, self.rv, self.out, self.spec)

    def publish(self):
        release.publish_runtime(self.plan(), self.store)

    def puts(self):
        return [key for kind, key in self.operations if kind == "put"]

    def test_plan_covers_every_platform_and_no_application_object(self):
        plan = self.plan()
        self.assertEqual(set(plan["runtime"]["files"]), set(release.PLATFORMS))
        self.assertEqual(sorted(plan["objects"]), sorted(
            ["runtimes/pi/cypher-pi-runtime-{}-{}.tar.gz".format(self.rv, p) for p in release.PLATFORMS]
            + ["runtimes/pi/manifests/{}.json".format(self.rv)]))
        self.assertFalse(list(self.out.glob("*.sha256")), "application checksums are not a Runtime concern")

    def test_application_artifacts_are_not_accepted_here(self):
        (self.dist / release.app_files(self.v)[0]).write_bytes(b"stale application build")
        with self.assertRaisesRegex(release.ReleaseError, "Unexpected or missing Runtime artifacts"):
            self.plan()
        self.assertFalse(self.out.exists(), "validation failure must generate nothing")

    def test_requested_version_must_match_the_pinned_definition(self):
        with self.assertRaisesRegex(release.ReleaseError, "do not match the requested version"):
            release.validate_runtime(self.dist, "9.9.9", self.out, self.spec)

    def test_publication_moves_only_the_runtime_pointer(self):
        self.publish()
        self.assertEqual(self.puts()[-1], "runtimes/pi/manifest.json")
        self.assertNotIn("manifest.json", self.puts())
        self.assertNotIn("latest.txt", self.puts())
        for platform in release.DESKTOP_PLATFORMS:
            self.assertNotIn(platform + "/manifest.json", self.puts())
            self.assertNotIn(platform + "/latest.txt", self.puts())
        self.assertEqual(self.store.objects["manifest.json"], self.published_app)
        self.assertEqual(self.store.objects["latest.txt"], self.v.encode())
        self.assertEqual(json.loads(self.store.objects["runtimes/pi/manifest.json"])["version"], self.rv)
        self.operations.clear()
        self.publish()
        self.assertEqual(self.puts(), [], "a retry of the same Runtime writes nothing")

    def test_older_runtime_never_writes(self):
        self.store.objects["runtimes/pi/manifest.json"] = b'{"version":"99.0.0"}'
        with self.assertRaisesRegex(release.ReleaseError, "roll back"):
            self.publish()
        self.assertEqual(self.operations, [])

    def test_same_version_metadata_cannot_be_rewritten(self):
        stale = dict(self.plan()["runtime"], piVersion="0.0.1")
        self.store.objects["runtimes/pi/manifest.json"] = release.json_bytes(stale)
        with self.assertRaisesRegex(release.ReleaseError, "bump its version"):
            self.publish()
        self.assertEqual(self.operations, [])

    def test_runtime_needs_a_published_application_it_can_run_on(self):
        for key in list(self.store.objects):
            if key.endswith("manifest.json") and not key.startswith("runtimes/"):
                del self.store.objects[key]
        with self.assertRaisesRegex(release.ReleaseError, "No published application release"):
            self.publish()
        # A Runtime whose floor is above the published client would be fetched
        # by nobody and refused by every engine that tried.
        self.store.objects["manifest.json"] = release.json_bytes({"version": "0.1.0"})
        with self.assertRaisesRegex(release.ReleaseError, "newer application"):
            self.publish()
        self.assertEqual(self.operations, [])

    def test_the_oldest_desktop_channel_binds_not_the_newest(self):
        # Decoupled platforms drift apart. A Runtime that only the newer channel
        # could load must not reach the platform still sitting behind it.
        self.store.objects["linux/manifest.json"] = release.json_bytes(
            {"version": "0.1.0", "build": 1, "files": {}})
        self.store.objects["macos/manifest.json"] = release.json_bytes(
            {"version": "9.9.9", "build": 1, "files": {}})
        with self.assertRaisesRegex(release.ReleaseError, "newer application"):
            self.publish()
        self.assertEqual(self.operations, [])

    def test_immutable_artifacts_are_never_overwritten(self):
        key = "runtimes/pi/cypher-pi-runtime-{}-{}.tar.gz".format(self.rv, release.PLATFORMS[0])
        self.store.objects[key] = b"a different build under the same name"
        with self.assertRaisesRegex(release.ReleaseError, "immutable"):
            self.publish()
        self.assertEqual(self.operations, [])

    def test_out_of_band_application_release_stops_promotion(self):
        def interfere(store, key):
            store.objects["runtimes/pi/manifest.json"] = b'{"version":"0.0.1"}'
        self.store.after_put = interfere
        with self.assertRaisesRegex(release.ReleaseError, "Release channel changed during upload"):
            self.publish()
        self.assertNotIn("runtimes/pi/manifest.json", self.puts())


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

    def channel(self, v, build=1, files=None):
        """Publish the Linux channel the installer actually resolves."""
        names = release.platform_files(v, build, "linux").values()
        if files is None:
            files = {name: {"sha256": "a" * 64} for name in names}
        stem = release.version_stem(v, build)
        self.routes["/releases/linux/manifest.json"] = (
            200, release.json_bytes({"version": v, "build": build, "files": files}), None)
        self.routes["/releases/linux/latest.txt"] = (200, v.encode(), None)
        self.routes["/releases/linux/stem.txt"] = (200, stem.encode(), None)
        return names

    def test_installer_gate_requires_both_archives_and_matching_pointers(self):
        v = "1.2.3"
        names = self.channel(v)
        with self.assertRaises(release.ReleaseError):
            release.check_deploy(self.url)
        for name in names:
            self.routes["/releases/" + name + ".sha256"] = (200, b"a" * 64 + b"\n", None)
            self.routes["/releases/" + name] = (200, b"", 20 * 1024 * 1024)
        self.assertEqual(release.check_deploy(self.url), v)
        self.routes["/releases/linux/latest.txt"] = (200, b"0.1.0", None)
        with self.assertRaisesRegex(release.ReleaseError, "pointers"):
            release.check_deploy(self.url)

    def test_installer_gate_checks_the_recut_build_not_the_historic_name(self):
        v = "1.2.3"
        names = self.channel(v, build=2)
        for name in names:
            self.assertIn("-b2-", name)
            self.routes["/releases/" + name + ".sha256"] = (200, b"a" * 64 + b"\n", None)
            self.routes["/releases/" + name] = (200, b"", 20 * 1024 * 1024)
        self.assertEqual(release.check_deploy(self.url), v)
        # A stem that disagrees with the manifest would send the installer to an
        # artifact the channel never validated.
        self.routes["/releases/linux/stem.txt"] = (200, v.encode(), None)
        with self.assertRaisesRegex(release.ReleaseError, "stem"):
            release.check_deploy(self.url)

    def test_installer_gate_blocks_clients_without_guided_setup(self):
        self.channel("0.3.2", files={})
        with self.assertRaisesRegex(release.ReleaseError, "guided setup"):
            release.check_deploy(self.url)

    def test_gate_follows_the_installer_onto_the_shared_channel(self):
        # Before the first per-platform release the shared pointers are still
        # the real channel, and install.sh falls back to them. Deployment must
        # not be blocked by a Linux channel that does not exist yet.
        v = "1.2.3"
        names = release.platform_files(v, 1, "linux").values()
        files = {name: {"sha256": "a" * 64} for name in release.app_files(v)}
        self.routes["/releases/manifest.json"] = (
            200, release.json_bytes({"version": v, "files": files}), None)
        self.routes["/releases/latest.txt"] = (200, v.encode(), None)
        for name in names:
            self.routes["/releases/" + name + ".sha256"] = (200, b"a" * 64 + b"\n", None)
            self.routes["/releases/" + name] = (200, b"", 20 * 1024 * 1024)
        self.assertEqual(release.check_deploy(self.url), v)
        # Once the Linux channel exists it takes precedence, as in the installer.
        self.channel("9.9.9")
        with self.assertRaises(release.ReleaseError):
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
        self.plan_value = self.platform_plan("linux")
        self.api = GitHubApi("cypher-linux-v{}-b1".format(self.v), "a" * 40)
        self.github = release.GitHubRelease("fixture/repo", "fixture-token", self.api.tag, self.api.commit)
        self.github.api = self.github.upload = self.api
        self.store = Store([])

    def test_real_github_adapter_stages_checksums_then_promotes(self):
        release.publish_platform(self.plan_value, self.store, self.github)
        self.assertFalse(self.api.release["draft"])
        self.assertTrue(self.api.release["body"].startswith("<!-- cypher-release-plan:"))
        self.assertEqual(len(self.api.release["assets"]), len(self.plan_value["assets"]))
        release.publish_platform(self.plan_value, self.store, self.github)
        self.assertEqual(self.api.promoted, 1)

    def test_interrupted_owned_draft_upload_can_resume(self):
        self.api.interrupt = True
        with self.assertRaisesRegex(release.ReleaseError, "interrupted"):
            release.publish_platform(self.plan_value, self.store, self.github)
        self.assertTrue(self.api.release["draft"])
        self.assertEqual(self.store.objects, {})
        release.publish_platform(self.plan_value, self.store, self.github)
        self.assertEqual(self.api.deleted, [1])
        self.assertFalse(self.api.release["draft"])

    def test_unowned_draft_and_moved_tag_are_preserved(self):
        self.api.release = {"id": 1, "draft": True, "body": "user-authored draft", "assets": []}
        with self.assertRaisesRegex(release.ReleaseError, "not owned"):
            release.publish_platform(self.plan_value, self.store, self.github)
        self.assertEqual(self.store.objects, {})
        self.assertEqual(self.api.deleted, [])
        self.api.release = None
        self.api.commit = "b" * 40
        with self.assertRaisesRegex(release.ReleaseError, "tag moved"):
            release.publish_platform(self.plan_value, self.store, self.github)

    def test_first_per_platform_release_accepts_the_old_coupled_latest(self):
        # The bootstrap release runs while GitHub's latest is still a coupled
        # `cypher-v<version>` tag. That must not be read as an unexpected
        # release, and it cannot be version-compared against a platform tag.
        self.api.latest = {"tag_name": "cypher-v9.9.9"}
        release.publish_platform(self.plan_value, self.store, self.github)
        self.assertFalse(self.api.release["draft"])

    def test_another_platforms_release_does_not_order_against_this_one(self):
        # Platforms move independently, so GitHub's single "latest" is often a
        # different platform. Ordering against it would block valid releases.
        self.api.latest = {"tag_name": "cypher-macos-v9.9.9-b1"}
        release.publish_platform(self.plan_value, self.store, self.github)
        self.assertFalse(self.api.release["draft"])

    def test_github_latest_cannot_regress_even_if_r2_is_older(self):
        self.api.latest = {"tag_name": "cypher-linux-v9.9.9-b1"}
        with self.assertRaisesRegex(release.ReleaseError, "regress"):
            release.publish_platform(self.plan_value, self.store, self.github)
        self.assertEqual(self.store.objects, {})


class Retention(unittest.TestCase):
    """`prune_plan` deletes production artifacts, so its safety rules are
    pinned here rather than left to the caller."""

    def bucket(self):
        objects = {}
        for v in ["0.3.18", "0.3.19", "0.3.20", "0.3.21", "0.3.22"]:
            for name in ["linux-x86_64.tar.gz", "macos-arm64.dmg"]:
                objects["cypher-{}-{}".format(v, name)] = 10
                objects["cypher-{}-{}.sha256".format(v, name)] = 1
            objects["linux/manifests/{}.json".format(v)] = 1
            objects["macos/manifests/{}.json".format(v)] = 1
        for rv in ["0.85.1.9", "0.85.1.11", "0.86.0.1", "0.86.0.2"]:
            objects["runtimes/pi/cypher-pi-runtime-{}-linux-x86_64.tar.gz".format(rv)] = 100
            objects["runtimes/pi/manifests/{}.json".format(rv)] = 1
        for pointer in ["manifest.json", "latest.txt", "runtimes/pi/manifest.json",
                        "linux/manifest.json", "linux/latest.txt", "linux/stem.txt",
                        "macos/manifest.json", "macos/latest.txt", "macos/stem.txt"]:
            objects[pointer] = 1
        return objects

    def pointers(self, app="0.3.22", runtime="0.86.0.2"):
        return {
            "linux/manifest.json": release.json_bytes(
                {"version": app, "files": {"cypher-{}-linux-x86_64.tar.gz".format(app): {}}}),
            "macos/manifest.json": release.json_bytes(
                {"version": app, "files": {"cypher-{}-macos-arm64.dmg".format(app): {}}}),
            "runtimes/pi/manifest.json": release.json_bytes(
                {"files": {"linux-x86_64": {
                    "url": "cypher-pi-runtime-{}-linux-x86_64.tar.gz".format(runtime)}}}),
        }

    def test_keeps_the_window_and_every_pointer(self):
        objects = self.bucket()
        doomed = set(release.prune_plan(objects, self.pointers()))
        kept = set(objects) - doomed
        for pointer in ["manifest.json", "latest.txt", "runtimes/pi/manifest.json",
                        "linux/manifest.json", "macos/stem.txt"]:
            self.assertIn(pointer, kept, "pointers are never candidates")
        for v in ["0.3.22", "0.3.21", "0.3.20"]:
            self.assertIn("cypher-{}-macos-arm64.dmg".format(v), kept)
            self.assertIn("cypher-{}-macos-arm64.dmg.sha256".format(v), kept)
        for v in ["0.3.19", "0.3.18"]:
            self.assertIn("cypher-{}-macos-arm64.dmg".format(v), doomed)
            self.assertIn("cypher-{}-macos-arm64.dmg.sha256".format(v), doomed)
        for rv in ["0.86.0.2", "0.86.0.1"]:
            self.assertIn("runtimes/pi/cypher-pi-runtime-{}-linux-x86_64.tar.gz".format(rv), kept)
        for rv in ["0.85.1.11", "0.85.1.9"]:
            self.assertIn("runtimes/pi/cypher-pi-runtime-{}-linux-x86_64.tar.gz".format(rv), doomed)

    def test_never_deletes_what_a_live_pointer_names(self):
        """Even a version outside the window survives while a pointer names it,
        which is what makes a rollback (republishing an older pointer) safe."""
        objects = self.bucket()
        doomed = set(release.prune_plan(objects, self.pointers(app="0.3.18", runtime="0.85.1.9")))
        self.assertNotIn("cypher-0.3.18-linux-x86_64.tar.gz", doomed)
        self.assertNotIn("cypher-0.3.18-macos-arm64.dmg", doomed)
        self.assertNotIn(
            "runtimes/pi/cypher-pi-runtime-0.85.1.9-linux-x86_64.tar.gz", doomed)

    def test_unrecognised_keys_are_never_candidates(self):
        objects = self.bucket()
        objects["install.sh"] = 1
        objects["some/unexpected/object"] = 1
        doomed = set(release.prune_plan(objects, self.pointers()))
        self.assertNotIn("install.sh", doomed)
        self.assertNotIn("some/unexpected/object", doomed)

    def test_recut_builds_sort_after_their_base_version(self):
        objects = self.bucket()
        objects["cypher-0.3.22-b2-linux-x86_64.tar.gz"] = 10
        objects["linux/manifests/0.3.22-b2.json"] = 1
        doomed = set(release.prune_plan(objects, self.pointers()))
        self.assertNotIn("cypher-0.3.22-b2-linux-x86_64.tar.gz", doomed)
        # The re-cut takes a slot, so the oldest kept version rolls off.
        self.assertIn("cypher-0.3.20-macos-arm64.dmg", doomed)

    def test_is_idempotent(self):
        objects = self.bucket()
        pointers = self.pointers()
        for key in release.prune_plan(objects, pointers):
            objects.pop(key)
        self.assertEqual(release.prune_plan(objects, pointers), [],
                         "a second pass must find nothing")

    def test_bounds_the_bucket_across_many_releases(self):
        """The property this exists for: publishing forever must not grow R2."""
        objects, pointers = {}, None
        for minor in range(40):
            version = "0.3.{}".format(minor)
            objects["cypher-{}-linux-x86_64.tar.gz".format(version)] = 10
            objects["linux/manifests/{}.json".format(version)] = 1
            objects["linux/manifest.json"] = 1
            pointers = {"linux/manifest.json": release.json_bytes(
                            {"version": version,
                             "files": {"cypher-{}-linux-x86_64.tar.gz".format(version): {}}}),
                        "runtimes/pi/manifest.json": release.json_bytes({"files": {}})}
            for key in release.prune_plan(objects, pointers):
                objects.pop(key)
        artifacts = [k for k in objects if k.endswith(".tar.gz")]
        self.assertEqual(len(artifacts), release.KEEP_APP_VERSIONS,
                         "40 releases converge on the keep window, not 40 artifacts")


class Policies(unittest.TestCase):
    def test_release_smoke_does_not_reintroduce_removed_tcp_ipc(self):
        workflow = (release.ROOT / ".github/workflows/linux.yml").read_text()
        self.assertNotIn("CYPHER_IPC_PORT=", workflow)
        self.assertIn('CYPHER_DATA_DIR="$stage/data" timeout 8', workflow)

    def context(self, action, ref, extra=(), event="workflow_dispatch"):
        root = tempfile.TemporaryDirectory()
        self.addCleanup(root.cleanup)
        output = Path(root.name) / "outputs"
        env = {k: v for k, v in os.environ.items()
               if not k.startswith(("GITHUB_", "CLOUDFLARE_", "AC_API_"))}
        env.update(GITHUB_OUTPUT=str(output), GITHUB_EVENT_NAME=event, GITHUB_REF=ref)
        command = [sys.executable, str(Path(release.__file__)), action, *extra]
        return subprocess.run(command, env=env, capture_output=True, text=True), output

    def test_manual_tag_context_is_never_a_publish(self):
        for platform in sorted(release.DESKTOP_PLATFORMS):
            with self.subTest(platform=platform):
                extra = ["--platform", platform]
                tag = "refs/tags/cypher-{}-v0.2.2-b1".format(platform)
                p, output = self.context("platform-context", tag, extra)
                self.assertEqual(p.returncode, 0, p.stderr)
                self.assertIn("publish=false", output.read_text())
                values = dict(line.split("=", 1) for line in output.read_text().splitlines())
                real = "refs/tags/cypher-{}-v{}-b1".format(platform, values["version"])
                p, _ = self.context("platform-context", real, extra, event="push")
                self.assertNotEqual(p.returncode, 0)
                self.assertIn("NOT PUBLISHED", p.stderr)

    def test_a_platform_tag_must_carry_a_build_and_the_right_version(self):
        version = release.workspace_version()
        extra = ["--platform", "linux"]
        for ref in ["refs/tags/cypher-linux-v" + version,          # no build
                    "refs/tags/cypher-linux-v9.9.9-b1",            # wrong version
                    "refs/tags/cypher-linux-v{}-b0".format(version)]:  # invalid build
            with self.subTest(ref=ref):
                p, _ = self.context("platform-context", ref, extra, event="push")
                self.assertNotEqual(p.returncode, 0, ref)

    def test_ios_tag_must_match_the_xcode_project(self):
        v, build = release.ios_version()
        p, output = self.context("ios-context", "refs/tags/cypher-ios-v{}-b{}".format(v, build))
        self.assertEqual(p.returncode, 0, p.stderr)
        self.assertIn("version={}".format(v), output.read_text())
        self.assertIn("build={}".format(build), output.read_text())
        # A tag whose build disagrees with CURRENT_PROJECT_VERSION would upload
        # a package Apple rejects as a duplicate build number.
        p, _ = self.context("ios-context", "refs/tags/cypher-ios-v{}-b{}".format(v, build + 1),
                            event="push")
        self.assertNotEqual(p.returncode, 0)

    def test_ios_workflow_stops_at_upload_and_touches_no_release_channel(self):
        text = (release.ROOT / ".github/workflows/ios.yml").read_text()
        self.assertIn("altool --upload-app", text)
        self.assertIn("ios-verify.py", text)
        # Prose may discuss what the workflow does not do; the configuration is
        # what actually grants access, so assert against that alone.
        config = "\n".join(line for line in text.splitlines()
                           if not line.lstrip().startswith("#"))
        for forbidden in ("CLOUDFLARE_API_TOKEN", "contents: write", "publish-platform",
                          "publish-runtime", "latest.txt", "manifest.json"):
            self.assertNotIn(forbidden, config)
        # TestFlight upload is not a channel write, so it takes no production lock.
        self.assertEqual(workflow_policy.check_queue(text), 0)

    def test_every_platform_publishes_from_its_own_workflow(self):
        workflows = release.ROOT / ".github/workflows"
        self.assertFalse((workflows / "release.yml").exists(),
                         "the coupled release workflow must not come back")
        for name, platform in (("linux.yml", "linux"), ("macos.yml", "macos")):
            text = (workflows / name).read_text()
            self.assertIn("publish-platform --platform " + platform, text)
            self.assertIn('tags: ["cypher-{}-v*"]'.format(platform), text)
            other = "macos" if platform == "linux" else "linux"
            self.assertNotIn("--platform " + other, text)
            self.assertNotIn("publish-runtime", text)

    def test_only_the_runtime_workflow_builds_the_runtime(self):
        # The Runtime now ships from exactly one place, so there is no second
        # copy of the build to drift out of step with it.
        for name in ("linux.yml", "macos.yml", "ios.yml"):
            text = (release.ROOT / ".github/workflows" / name).read_text()
            self.assertNotIn("package-pi-runtime.sh", text)
        runtime = (release.ROOT / ".github/workflows/pi-runtime.yml").read_text()
        self.assertIn("package-pi-runtime.sh", runtime)

    def test_runtime_release_cannot_write_the_application_channel(self):
        workflow = (release.ROOT / ".github/workflows/pi-runtime.yml").read_text()
        self.assertIn("release.py publish-runtime", workflow)
        self.assertNotIn("contents: write", workflow)
        self.assertNotIn("GITHUB_TOKEN", workflow)

    def test_manual_runtime_tag_context_is_never_a_publish(self):
        rv = json.loads((release.ROOT / "dist/pi-runtime/release.json").read_text())["version"]
        with tempfile.TemporaryDirectory() as root:
            output = Path(root) / "outputs"
            env = {k: v for k, v in os.environ.items() if not k.startswith(("GITHUB_", "CLOUDFLARE_"))}
            env.update(GITHUB_OUTPUT=str(output), GITHUB_EVENT_NAME="workflow_dispatch",
                       GITHUB_REF="refs/tags/pi-runtime-v" + rv)
            command = [sys.executable, str(Path(release.__file__)), "runtime-context"]
            p = subprocess.run(command, env=env, capture_output=True, text=True)
            self.assertEqual(p.returncode, 0, p.stderr)
            self.assertIn("version=" + rv, output.read_text())
            self.assertIn("publish=false", output.read_text())
            env["GITHUB_EVENT_NAME"] = "push"
            env["GITHUB_REF"] = "refs/tags/pi-runtime-v0.0.1"
            p = subprocess.run(command, env=env, capture_output=True, text=True)
            self.assertNotEqual(p.returncode, 0)
            self.assertIn("must equal the pinned Runtime version", p.stderr)
            env["GITHUB_REF"] = "refs/tags/pi-runtime-v" + rv
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

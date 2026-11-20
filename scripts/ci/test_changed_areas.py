import contextlib
import io
import os
from pathlib import Path
import re
import subprocess
import tempfile
import unittest

import changed_areas

ROOT = Path(__file__).resolve().parents[2]
ALL = {"rust": True, "ios": True}
NONE = {"rust": False, "ios": False}


class Areas(unittest.TestCase):
    def test_unknown_changes_run_everything(self):
        self.assertEqual(changed_areas.areas(None), ALL)

    def test_changes_to_ci_itself_run_everything(self):
        for path in (".github/workflows/ci.yml", ".github/actions/setup-rust/action.yml",
                     "scripts/ci/changed_areas.py"):
            self.assertEqual(changed_areas.areas(["docs/README.md", path]), ALL, path)

    def test_documentation_and_web_changes_skip_both(self):
        for path in ("docs/operations/ci-cd.md", "README.md", "AGENTS.md", "LICENSE",
                     ".agents/skills/cypher-dev-app/SKILL.md", "apps/landing/public/index.html",
                     "apps/www-redirect/wrangler.jsonc", "apps/edge/src/index.ts",
                     "apps/edge/package-lock.json"):
            self.assertEqual(changed_areas.areas([path]), NONE, path)

    def test_ios_changes_run_ios_alone(self):
        for path in ("apps/ios/Cypher/Home/HomeView.swift", "apps/ios/CypherTests/TestSupport.swift",
                     "apps/ios/Cypher.xcodeproj/project.pbxproj", "apps/ios/README.md",
                     "apps/ios/Cypher/Sync/RegistryClient.swift"):
            self.assertEqual(changed_areas.areas([path]), {"rust": False, "ios": True}, path)

    def test_rust_changes_run_rust_alone(self):
        for path in ("crates/engine/src/lib.rs", "apps/cypher/src/main.rs", "Cargo.lock",
                     "rust-toolchain.toml", "pi-runtime/provider-service.mjs",
                     "packaging/cypher.png", "scripts/demo/tasks-app/README.md",
                     "scripts/tests/test-linux-cli.py", "apps/edge/src/install.sh",
                     "crates/ui/assets/sounds/README.md"):
            self.assertEqual(changed_areas.areas([path]), {"rust": True, "ios": False}, path)

    def test_shared_inputs_run_both(self):
        for path in ("protocol/vectors/chat-frames-v1.json", "scripts/check.sh",
                     "apps/ios/Cypher/Sync/StreamPreview.swift"):
            self.assertEqual(changed_areas.areas([path]), ALL, path)

    def test_a_path_nobody_listed_runs_rust(self):
        self.assertEqual(changed_areas.areas(["a-new-directory/file"]),
                         {"rust": True, "ios": False})

    def test_one_relevant_file_among_many_is_enough(self):
        files = ["docs/README.md", "apps/landing/public/index.html", "crates/doc/src/lib.rs"]
        self.assertTrue(changed_areas.areas(files)["rust"])
        self.assertEqual(changed_areas.areas([]), NONE)


class Inputs(unittest.TestCase):
    def test_the_swift_sources_the_macos_stage_compiles_count_as_rust(self):
        check = (ROOT / "scripts/check.sh").read_text()
        stage = check[check.index("stage_macos_test() {"):check.index("stage_macos() {")]
        compiled = set(re.findall(r"apps/\S+\.swift", stage))
        self.assertTrue(compiled)
        self.assertEqual(compiled, {path for path in changed_areas.RUST_READS
                                    if path.endswith(".swift")})

    def test_files_rust_sources_include_from_outside_count_as_rust(self):
        # include_str!/include_bytes! of a path that leaves crates/ or apps/cypher/.
        include = re.compile(r'include_(?:str|bytes)!\(\s*"((?:\.\./)+[^"]+)"')
        for source in list(ROOT.glob("crates/**/*.rs")) + list(ROOT.glob("apps/cypher/**/*.rs")):
            for target in include.findall(source.read_text()):
                path = os.path.relpath(os.path.normpath(source.parent / target), ROOT)
                with self.subTest(source=str(source.relative_to(ROOT)), path=path):
                    self.assertTrue(changed_areas.areas([path])["rust"])

    def test_every_listed_file_exists(self):
        for path in changed_areas.RUST_READS + tuple(
                path for path in changed_areas.EVERYTHING + changed_areas.IOS if "*" not in path):
            self.assertTrue((ROOT / path).is_file(), path)


class ChangedInCi(unittest.TestCase):
    def setUp(self):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.addCleanup(os.chdir, os.getcwd())
        os.chdir(directory.name)
        self.git("init", "--quiet")
        self.git("symbolic-ref", "HEAD", "refs/heads/main")
        self.commit("base", {"docs/a.txt": "a", "crates/x/src/lib.rs": "x"})

    def git(self, *arguments):
        environment = dict(os.environ, GIT_AUTHOR_NAME="t", GIT_AUTHOR_EMAIL="t@example.invalid",
                           GIT_COMMITTER_NAME="t", GIT_COMMITTER_EMAIL="t@example.invalid",
                           GIT_CONFIG_GLOBAL=os.devnull, GIT_CONFIG_SYSTEM=os.devnull)
        return subprocess.run(("git",) + arguments, check=True, capture_output=True, text=True,
                              env=environment).stdout.strip()

    def commit(self, message, files):
        for path, text in files.items():
            Path(path).parent.mkdir(parents=True, exist_ok=True)
            Path(path).write_text(text)
        self.git("add", "--all")
        self.git("commit", "--quiet", "--message", message)
        return self.git("rev-parse", "HEAD")

    def test_a_pull_request_is_its_merge_commit_against_the_base(self):
        self.git("checkout", "--quiet", "-b", "feature")
        self.commit("one", {"docs/b.txt": "b"})
        self.commit("two", {"apps/ios/App.swift": "s"})
        self.git("checkout", "--quiet", "main")
        self.commit("main moved", {"crates/x/src/lib.rs": "changed on main"})
        self.git("merge", "--quiet", "--no-ff", "--message", "merge", "feature")
        self.assertEqual(changed_areas.changed_in_ci("pull_request", ""),
                         ["apps/ios/App.swift", "docs/b.txt"])

    def test_a_pull_request_checkout_that_is_no_merge_is_unknown(self):
        self.assertIsNone(changed_areas.changed_in_ci("pull_request", ""))

    def test_a_push_is_everything_since_before(self):
        before = self.git("rev-parse", "HEAD")
        self.commit("one", {"docs/b.txt": "b"})
        self.commit("two", {"crates/x/src/lib.rs": "changed"})
        self.assertEqual(changed_areas.changed_in_ci("push", before),
                         ["crates/x/src/lib.rs", "docs/b.txt"])

    def test_a_moved_file_counts_at_both_paths(self):
        before = self.git("rev-parse", "HEAD")
        self.git("mv", "crates/x/src/lib.rs", "docs/lib.rs")
        self.git("commit", "--quiet", "--message", "move")
        self.assertEqual(changed_areas.changed_in_ci("push", before),
                         ["crates/x/src/lib.rs", "docs/lib.rs"])

    def test_a_push_without_a_usable_before_and_a_manual_run_are_unknown(self):
        self.assertIsNone(changed_areas.changed_in_ci("push", "0" * 40))
        self.assertIsNone(changed_areas.changed_in_ci("push", ""))
        with contextlib.redirect_stderr(io.StringIO()) as stderr:
            self.assertIsNone(changed_areas.changed_in_ci("push", "1" * 40))
        self.assertIn("could not list the changed files", stderr.getvalue())
        self.assertIsNone(changed_areas.changed_in_ci("workflow_dispatch", ""))


if __name__ == "__main__":
    unittest.main()

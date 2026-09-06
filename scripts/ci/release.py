#!/usr/bin/env python3
"""Release validation, deployment compatibility gate and guarded publication.

Only `publish` writes remotely. It requires a tag-push Actions context and the
shared `cypher-production` concurrency lock in release.yml. No local credentials
are read. Object GET/PUT uses the same R2 API as the lockfile-pinned Wrangler.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import posixpath
import re
import subprocess
import sys
import tarfile
import urllib.error
import urllib.parse
import urllib.request

ROOT = Path(__file__).resolve().parents[2]
PLATFORMS = ("macos-arm64", "linux-x86_64", "linux-aarch64")
MAX_METADATA = 2 * 1024 * 1024
MAX_ARTIFACT = 512 * 1024 * 1024


class ReleaseError(Exception):
    pass


def require(condition, message):
    if not condition:
        raise ReleaseError(message)


def version(value):
    require(isinstance(value, str) and len(value) <= 64
            and re.fullmatch(r"[0-9]+(?:\.[0-9]+)*", value), "Invalid release version")
    return tuple(int(part) for part in value.split("."))


def sha256(value):
    return hashlib.sha256(value).hexdigest()


def digest(path):
    h = hashlib.sha256()
    size = 0
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            size += len(chunk)
            require(size <= MAX_ARTIFACT, "Artifact exceeds size limit")
            h.update(chunk)
    return h.hexdigest(), size


def json_bytes(value):
    return (json.dumps(value, sort_keys=True, separators=(",", ":")) + "\n").encode()


def read_json(raw):
    require(len(raw) <= MAX_METADATA, "Metadata exceeds size limit")
    try:
        result = json.loads(raw)
    except (ValueError, UnicodeError):
        raise ReleaseError("Invalid release JSON") from None
    require(isinstance(result, dict), "Release metadata must be an object")
    return result


def app_files(v):
    version(v)
    return [
        "cypher-{}-linux-x86_64.tar.gz".format(v),
        "cypher-{}-linux-aarch64.tar.gz".format(v),
        "cypher-{}-macos-arm64.dmg".format(v),
        "cypher-{}-macos-arm64-app.tar.gz".format(v),
    ]


def archive_metadata(path, root, required, json_member=None):
    """Validate before publication without extracting or executing anything."""
    seen = {}
    total = 0
    metadata = None
    try:
        with tarfile.open(path, "r:gz") as archive:
            for member in archive:
                name = member.name.rstrip("/")
                parts = PurePosixPath(name).parts
                require(parts and parts[0] == root and ".." not in parts
                        and not name.startswith("/") and "\\" not in name,
                        "Archive path escapes its expected root")
                require(name not in seen, "Duplicate archive member")
                require(member.isfile() or member.isdir() or member.issym() or member.islnk(),
                        "Archive contains a special file")
                seen[name] = member
                total += member.size
                require(total <= 2 * 1024 * 1024 * 1024, "Expanded archive exceeds size limit")
                if member.issym() or member.islnk():
                    require(not member.linkname.startswith("/") and "\\" not in member.linkname,
                            "Archive has an absolute link")
                    target = posixpath.normpath(
                        posixpath.join(posixpath.dirname(name), member.linkname)
                        if member.issym() else member.linkname)
                    require(target == root or target.startswith(root + "/"), "Archive link escapes root")
                if json_member and name == root + "/" + json_member:
                    require(member.isfile(), "Runtime metadata must be a regular file")
                    metadata = read_json(archive.extractfile(member).read(MAX_METADATA + 1))
    except (tarfile.TarError, EOFError, OSError):
        raise ReleaseError("Invalid archive: " + path.name) from None
    for relative in required:
        member = seen.get(root + "/" + relative)
        require(member is not None and member.isfile(), "Archive is missing " + relative)
        if relative in ("cypher", "Contents/MacOS/cypher", "bin/node", "bin/pi", "bin/npm"):
            require(member.mode & 0o111, "Archive binary is not executable: " + relative)
    return metadata


def validate(dist, v, output, spec):
    """Exact artifact set, platform coverage, bytes and archive/manifest agreement."""
    version(v)
    definition = read_json(spec.with_name("release.json").read_bytes())
    spec = read_json(spec.read_bytes())["dependencies"]
    plugins = {k: val for k, val in spec.items() if not k.startswith("@earendil-works/")}
    required_app = app_files(v)
    runtime_files = {}
    runtime = None
    expected = set(required_app)
    for platform in PLATFORMS:
        candidates = list(dist.glob("cypher-pi-runtime-*-{}.json".format(platform)))
        require(len(candidates) == 1, "Expected one Runtime metadata file for " + platform)
        meta_path = candidates[0]
        require(meta_path.is_file() and not meta_path.is_symlink(), "Runtime metadata must be a regular file")
        meta = read_json(meta_path.read_bytes())
        rv = meta.get("version")
        version(rv)
        require(rv == definition["version"] and
                meta.get("minimumCypherVersion") == definition["minimumCypherVersion"],
                "Runtime release definition mismatch")
        name = "cypher-pi-runtime-{}-{}".format(rv, platform)
        require(meta_path.name == name + ".json", "Runtime metadata filename/version mismatch")
        require(meta.get("piVersion") == spec["@earendil-works/pi-coding-agent"]
                and meta.get("plugins") == plugins, "Runtime does not match the pinned package spec")
        require(version(meta.get("minimumCypherVersion")) <= version(v),
                "Runtime requires a newer application")
        require(isinstance(meta.get("files"), dict) and set(meta["files"]) == {platform},
                "Runtime platform must match its filename")
        entry = meta["files"][platform]
        require(isinstance(entry, dict) and entry.get("url") == name + ".tar.gz",
                "Runtime archive URL must match its platform and version")
        archive = dist / entry["url"]
        require(archive.is_file() and not archive.is_symlink(), "Missing Runtime archive")
        actual_hash, actual_size = digest(archive)
        require(type(entry.get("size")) is int and entry["size"] == actual_size and actual_size > 0,
                "Runtime size does not match archive bytes")
        require(entry.get("sha256") == actual_hash, "Runtime checksum mismatch")
        inner = archive_metadata(archive, name, [
            "runtime.json", "bin/node", "bin/pi", "bin/npm", "provider-service.mjs",
            "extensions/cypher-provider-auth.ts",
        ], "runtime.json")
        require(inner == {"version": rv, "piVersion": meta["piVersion"], "plugins": plugins},
                "Runtime archive metadata disagrees with its manifest")
        common = {key: meta[key] for key in ("version", "piVersion", "plugins", "minimumCypherVersion")}
        require(runtime is None or runtime == common, "Runtime metadata differs across platforms")
        runtime = common
        runtime_files[platform] = entry
        expected.update((meta_path.name, archive.name))
    require({p.name for p in dist.iterdir()} == expected, "Unexpected or missing release artifacts")
    sources = {}
    app = {"version": v, "files": {}}
    for name in required_app:
        path = dist / name
        require(path.is_file() and not path.is_symlink(), "Missing application artifact")
        h, size = digest(path)
        require(size > 0, "Empty application artifact")
        if name.endswith(".tar.gz"):
            is_mac = name.endswith("-app.tar.gz")
            archive_metadata(path, "Cypher.app" if is_mac else name[:-7],
                             ["Contents/MacOS/cypher"] if is_mac else ["cypher"])
        app["files"][name] = {"sha256": h, "size": size}
        sources[name] = path
    # Nothing is generated until the ENTIRE input set passes.
    output.mkdir(parents=True, exist_ok=True)
    for name, entry in app["files"].items():
        checksum = output / (name + ".sha256")
        checksum.write_text(entry["sha256"] + "\n")
        sources[checksum.name] = checksum
    runtime["files"] = runtime_files
    for entry in runtime_files.values():
        sources["runtimes/pi/" + entry["url"]] = dist / entry["url"]
    for name, value, key in [
        ("manifest.json", app, "manifests/{}.json".format(v)),
        ("pi-runtime-manifest.json", runtime, "runtimes/pi/manifests/{}.json".format(runtime["version"])),
    ]:
        path = output / name
        path.write_bytes(json_bytes(value))
        sources[key] = path
    assets = dict((p.name, p) for p in dist.iterdir())
    assets.update((p.name, p) for p in sources.values())
    return {"app": app, "runtime": runtime, "objects": sources, "assets": assets,
            "digests": {key: digest(path) for key, path in sources.items()},
            "asset_digests": {name: digest(path) for name, path in assets.items()}}


def check_deploy(base):
    """Use the same unauthenticated curl transport as the public installer."""
    parsed = urllib.parse.urlsplit(base)
    require(parsed.scheme == "https" or
            (parsed.scheme == "http" and parsed.hostname in ("127.0.0.1", "localhost", "::1")),
            "Release URL must use HTTPS except loopback")
    require(not parsed.username and not parsed.password and not parsed.query and not parsed.fragment,
            "Invalid release base URL")

    def get(key, head=False):
        command = ["curl", "--fail", "--silent", "--show-error", "--connect-timeout", "10",
                   "--max-time", "30"]
        if head:
            command += ["--head"]
        else:
            command += ["--max-filesize", str(MAX_METADATA)]
        command += [base.rstrip("/") + "/releases/" + key]
        result = subprocess.run(command, stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=35)
        require(result.returncode == 0, "Deployment blocked: release endpoint unavailable: " + key)
        require(len(result.stdout) <= MAX_METADATA, "Release metadata exceeds limit")
        return result.stdout

    manifest = read_json(get("manifest.json"))
    v = manifest.get("version")
    version(v)
    require(get("latest.txt").decode().strip() == v, "Deployment blocked: release pointers disagree")
    installer = (ROOT / "edge/src/install.sh").read_text()
    floor = re.search(r"^MINIMUM_SETUP_VERSION=([0-9.]+)$", installer, re.M)
    if floor:
        require(version(v) >= version(floor.group(1)),
                "Deployment blocked: publish a client release supporting guided setup first (>= " + floor.group(1) + ")")
    for name in app_files(v)[:2]:
        expected = manifest.get("files", {}).get(name, {}).get("sha256")
        require(isinstance(expected, str) and re.fullmatch("[0-9a-fA-F]{64}", expected),
                "Deployment blocked: missing Linux checksum in manifest")
        require(get(name + ".sha256").decode().strip().lower() == expected.lower(),
                "Deployment blocked: missing or inconsistent Linux .sha256")
        # A HEAD checks existence, not artifact size against the metadata limit.
        get(name, head=True)
    return v


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, *args, **kwargs):
        return None


class Api:
    def __init__(self, base, token):
        self.base = base.rstrip("/")
        self.token = token.strip()
        require(self.token and all(33 <= ord(ch) <= 126 for ch in self.token), "Invalid CI credential")
        self.opener = urllib.request.build_opener(NoRedirect())

    def request(self, path, method="GET", data=None, missing=False, limit=MAX_METADATA):
        headers = {"Authorization": "Bearer " + self.token, "User-Agent": "cypher-release-ci",
                   "Accept": "application/json", "Content-Type": "application/octet-stream"}
        stream = None
        if isinstance(data, Path):
            headers["Content-Type"] = "application/octet-stream"
            headers["Content-Length"] = str(data.stat().st_size)
            stream = data.open("rb")
            data = stream
        elif isinstance(data, dict):
            data = json_bytes(data)
            headers["Content-Type"] = "application/json"
        try:
            req = urllib.request.Request(self.base + path, headers=headers, method=method, data=data)
            try:
                response = self.opener.open(req, timeout=120)
            except urllib.error.HTTPError as err:
                code = err.code
                err.close()
                if missing and code == 404:
                    return None
                raise ReleaseError("API request failed (HTTP {})".format(code)) from None
            with response:
                if limit is None:
                    h = hashlib.sha256()
                    size = 0
                    for chunk in iter(lambda: response.read(1024 * 1024), b""):
                        h.update(chunk)
                        size += len(chunk)
                        require(size <= MAX_ARTIFACT, "Remote artifact exceeds limit")
                    return h.hexdigest(), size
                raw = response.read(limit + 1)
                require(len(raw) <= limit, "API response exceeds limit")
                return raw
        except (urllib.error.URLError, TimeoutError, OSError):
            raise ReleaseError("API transport failed") from None
        finally:
            if stream:
                stream.close()


class R2:
    def __init__(self, account, token):
        require(re.fullmatch("[0-9a-f]{32}", account), "Invalid Cloudflare account ID")
        self.api = Api("https://api.cloudflare.com/client/v4", token)
        self.prefix = "/accounts/{}/r2/buckets/cypher-releases/objects/".format(account)

    def get(self, key):
        return self.api.request(self.prefix + urllib.parse.quote(key, safe="/"), missing=True)

    def digest(self, key):
        return self.api.request(self.prefix + urllib.parse.quote(key, safe="/"), missing=True, limit=None)

    def put(self, key, value):
        raw = self.api.request(self.prefix + urllib.parse.quote(key, safe="/"), "PUT", value)
        if raw:
            require(read_json(raw).get("success", True), "R2 rejected upload")


class GitHubRelease:
    def __init__(self, repo, token, tag, commit):
        require(re.fullmatch(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+", repo), "Invalid GitHub repository")
        self.api = Api("https://api.github.com", token)
        self.upload = Api("https://uploads.github.com", token)
        self.prefix = "/repos/" + repo
        self.tag = tag
        self.commit = commit
        self.release = None
        self.starters = []

    @staticmethod
    def marker(plan):
        return "<!-- cypher-release-plan:{} -->".format(sha256(json_bytes({
            "app": plan["app"], "runtime": plan["runtime"], "assets": plan["asset_digests"],
        })))

    def preflight(self, plan):
        latest = self.api.request(self.prefix + "/releases/latest", missing=True)
        if latest:
            latest_tag = read_json(latest)["tag_name"]
            require(latest_tag.startswith("cypher-v"), "Unexpected GitHub latest release")
            require(version(plan["app"]["version"]) >= version(latest_tag[len("cypher-v"):]),
                    "Refusing to regress GitHub latest release")
        # Never let create-release implicitly create or reuse a moved tag.
        ref = read_json(self.api.request(self.prefix + "/git/ref/tags/" + self.tag))["object"]
        for _ in range(8):
            if ref["type"] != "tag":
                break
            ref = read_json(self.api.request(self.prefix + "/git/tags/" + ref["sha"]))["object"]
        require(ref["type"] == "commit" and ref["sha"] == self.commit, "Release tag moved or mismatches build")
        raw = self.api.request(self.prefix + "/releases/tags/" + self.tag, missing=True)
        self.release = read_json(raw) if raw else None
        self.starters = []
        if self.release:
            if self.release["draft"]:
                require(self.marker(plan) in (self.release.get("body") or ""),
                        "Existing draft is not owned by this release plan; preserve it")
            assets = {a["name"]: a for a in self.release["assets"]}
            for name, path in plan["assets"].items():
                existing = assets.get(name)
                if existing:
                    if (self.release["draft"] and existing.get("state") == "starter"
                            and not existing.get("digest") and existing.get("size") == 0):
                        self.starters.append(existing["id"])
                        continue
                    require(existing.get("digest") == "sha256:" + plan["asset_digests"][name][0],
                            "Existing GitHub asset differs; do not overwrite published versions")
                else:
                    require(self.release["draft"], "Public release is missing an asset")
            require(set(assets) <= set(plan["assets"]), "GitHub release contains unexpected assets")

    def stage(self, plan):
        if not self.release:
            notes = read_json(self.api.request(self.prefix + "/releases/generate-notes", "POST",
                                              {"tag_name": self.tag}))
            self.release = read_json(self.api.request(self.prefix + "/releases", "POST", {
                "tag_name": self.tag, "name": "Cypher " + plan["app"]["version"],
                "draft": True, "body": self.marker(plan) + "\n\n" + notes.get("body", ""),
            }))
        # GitHub can leave zero-byte "starter" assets after an interrupted
        # upload. Only clean these in a private draft owned by this exact plan.
        for asset_id in self.starters:
            self.api.request(self.prefix + "/releases/assets/" + str(asset_id), "DELETE")
        assets = {a["name"] for a in self.release["assets"] if a["id"] not in self.starters}
        for name, path in sorted(plan["assets"].items()):
            if name not in assets:
                require(digest(path) == plan["asset_digests"][name], "Local asset changed after validation")
                endpoint = self.prefix + "/releases/{}/assets?name={}".format(
                    self.release["id"], urllib.parse.quote(name, safe=""))
                result = read_json(self.upload.request(endpoint, "POST", path))
                require(result.get("digest") == "sha256:" + plan["asset_digests"][name][0],
                        "GitHub asset checksum mismatch")

    def promote(self):
        if self.release["draft"]:
            result = read_json(self.api.request(self.prefix + "/releases/" + str(self.release["id"]), "PATCH",
                                               {"draft": False, "make_latest": "true"}))
            require(result.get("draft") is False and result.get("tag_name") == self.tag,
                    "GitHub release promotion was not confirmed")


def remote_preflight(plan, store):
    pointers = {key: store.get(key) for key in
                ("manifest.json", "latest.txt", "runtimes/pi/manifest.json")}
    for key, value in pointers.items():
        if value is None:
            continue
        desired = plan["runtime"] if key.startswith("runtimes/") else plan["app"]
        old = {"version": value.decode().strip()} if key == "latest.txt" else read_json(value)
        require(version(desired["version"]) >= version(old.get("version")),
                "Refusing to roll back " + key)
        if key != "latest.txt" and old["version"] == desired["version"]:
            require(old == desired, "Existing version metadata differs; bump its version")
    missing = []
    # Complete the conflict check for ALL objects before performing any write.
    for key, path in plan["objects"].items():
        require(digest(path) == plan["digests"][key], "Local artifact changed after validation")
        existing = store.digest(key)
        if existing is None:
            missing.append(key)
        else:
            require(existing == plan["digests"][key], "Refusing to overwrite immutable artifact: " + key)
    return pointers, missing


def publish(plan, store, github):
    pointers, missing = remote_preflight(plan, store)
    github.preflight(plan)
    github.stage(plan)  # Private draft only; no public release before validation.
    for key in missing:
        require(digest(plan["objects"][key]) == plan["digests"][key], "Local artifact changed during publication")
        store.put(key, plan["objects"][key])
        require(store.digest(key) == plan["digests"][key], "R2 upload verification failed")
    # Detect out-of-band changes before promotion. All supported CI publishers
    # also hold the shared workflow lock. Manual concurrent R2 writes are unsupported.
    require(all(store.get(k) == v for k, v in pointers.items()), "Release channel changed during upload")
    values = {
        "runtimes/pi/manifest.json": json_bytes(plan["runtime"]),
        "manifest.json": json_bytes(plan["app"]),
        "latest.txt": plan["app"]["version"].encode(),
    }
    for key, value in values.items():
        if pointers[key] != value:
            store.put(key, value)
        require(store.get(key) == value, "Release pointer verification failed")
    github.promote()  # Public GitHub release is the final step.


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("action", choices=["context", "validate", "publish", "check-deploy"])
    parser.add_argument("--dist", type=Path, default=Path("dist"))
    parser.add_argument("--version")
    parser.add_argument("--out", type=Path, default=Path("target/release-plan"))
    parser.add_argument("--base-url", default="https://edge.letscypher.app")
    args = parser.parse_args()
    if args.action == "context":
        workspace = (ROOT / "Cargo.toml").read_text().split("[workspace.package]", 1)[1].split("\n[", 1)[0]
        v = re.search(r'^version\s*=\s*"([^"]+)"', workspace, re.M).group(1)
        version(v)
        event = os.environ.get("GITHUB_EVENT_NAME")
        require(event in ("push", "workflow_dispatch"), "Unsupported release event")
        publishing = event == "push"
        if publishing:
            require(os.environ.get("GITHUB_REF") == "refs/tags/cypher-v" + v,
                    "Release tag must equal the Cargo workspace version")
            require(os.environ.get("CLOUDFLARE_API_TOKEN"),
                    "NOT PUBLISHED: configure CLOUDFLARE_API_TOKEN in repository Actions secrets")
        with open(os.environ["GITHUB_OUTPUT"], "a") as output:
            output.write("version={}\npublish={}\n".format(v, str(publishing).lower()))
        return
    if args.action == "check-deploy":
        print("Installer prerequisites verified for " + check_deploy(args.base_url))
        return
    plan = validate(args.dist, args.version, args.out, ROOT / "dist/pi-runtime/package.json")
    if args.action == "publish":
        require(os.environ.get("GITHUB_ACTIONS") == "true"
                and os.environ.get("GITHUB_EVENT_NAME") == "push"
                and os.environ.get("GITHUB_REF") == "refs/tags/cypher-v" + args.version
                and os.environ.get("CYPHER_PRODUCTION_LOCK") == "held",
                "Publication requires a tag-push workflow holding cypher-production")
        for key in ("CLOUDFLARE_API_TOKEN", "GITHUB_TOKEN", "CLOUDFLARE_ACCOUNT_ID",
                    "GITHUB_REPOSITORY", "GITHUB_SHA"):
            require(os.environ.get(key), "Missing required CI credential/context: " + key)
        commit = subprocess.check_output(["git", "rev-parse", "HEAD"], text=True).strip()
        event_commit = subprocess.check_output(
            ["git", "rev-parse", os.environ["GITHUB_SHA"] + "^{commit}"], text=True).strip()
        require(commit == event_commit, "Checkout does not match the workflow event")
        publish(plan, R2(os.environ["CLOUDFLARE_ACCOUNT_ID"], os.environ["CLOUDFLARE_API_TOKEN"]),
                GitHubRelease(os.environ["GITHUB_REPOSITORY"], os.environ["GITHUB_TOKEN"],
                              "cypher-v" + args.version, commit))
    print("{} complete: application {}, Runtime {}".format(
        args.action, args.version, plan["runtime"]["version"]))


if __name__ == "__main__":
    try:
        main()
    except (ReleaseError, OSError, ValueError, KeyError, TypeError, subprocess.TimeoutExpired) as error:
        print("Release check failed: " + str(error), file=sys.stderr)
        sys.exit(1)

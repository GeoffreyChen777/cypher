#!/usr/bin/env python3
"""Release validation, deployment compatibility gate and guarded publication.

Only the `publish-*` actions write remotely. Each requires a tag-push Actions
context and the shared `cypher-production` concurrency lock held by linux.yml,
macos.yml, pi-runtime.yml and deploy.yml. Every platform publishes on its own
channel and may move only its own pointers. No local credentials are read.
Object GET/PUT uses the same R2 API as the lockfile-pinned Wrangler.
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


def build_number(value):
    """Per-platform build counter. Desktop platforms share one version but
    publish independently, so the same version may be re-cut for one platform
    alone. Build 1 is the historic, legacy-compatible naming."""
    require(isinstance(value, (int, str)), "Build must be an integer")
    text = str(value)
    require(re.fullmatch("[1-9][0-9]{0,4}", text), "Build must be a positive integer")
    return int(text)


# Desktop artifact roles. New clients resolve their download from the manifest
# by role; only pre-decoupling clients reconstruct the file name themselves.
DESKTOP_PLATFORMS = {
    "linux": (("headless-x86_64", "cypher-{stem}-linux-x86_64.tar.gz"),
              ("headless-aarch64", "cypher-{stem}-linux-aarch64.tar.gz")),
    "macos": (("dmg-arm64", "cypher-{stem}-macos-arm64.dmg"),
              ("app-arm64", "cypher-{stem}-macos-arm64-app.tar.gz")),
}


def version_stem(v, build):
    """Build 1 keeps the historic name so already-installed clients, which build
    the name from the version alone, still resolve it. A re-cut of the same
    version changes bytes, so it must also change the immutable object name."""
    version(v)
    return v if build_number(build) == 1 else "{}-b{}".format(v, build_number(build))


def platform_files(v, build, platform):
    require(platform in DESKTOP_PLATFORMS, "Unknown desktop platform: " + str(platform))
    stem = version_stem(v, build)
    return {role: name.format(stem=stem) for role, name in DESKTOP_PLATFORMS[platform]}


def app_files(v):
    """Legacy coupled artifact set: every desktop artifact at build 1."""
    version(v)
    return [name for platform in ("linux", "macos")
            for name in platform_files(v, 1, platform).values()]


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


def runtime_plan(dist, spec):
    """Platform coverage, bytes and archive/manifest agreement for the Runtime set.

    Shared by the application release and the standalone Runtime release, so a
    decoupled Runtime is held to exactly the same checks. The only thing it
    cannot judge here is which application versions may load the result:
    a coupled release compares against the version being published, a
    standalone one against the published application channel.
    """
    definition = read_json(spec.with_name("release.json").read_bytes())
    spec = read_json(spec.read_bytes())["dependencies"]
    plugins = {k: val for k, val in spec.items() if not k.startswith("@earendil-works/")}
    runtime_files = {}
    runtime = None
    names = set()
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
        version(meta.get("minimumCypherVersion"))
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
        names.update((meta_path.name, archive.name))
    return runtime, runtime_files, names


def validate_platform(dist, v, build, platform, output):
    """One desktop platform's artifact set — no other platform, no Runtime.

    Decoupled publication means the absent artifacts are genuinely absent
    rather than stale, so an artifact belonging to another platform is an
    error here, not extra. The Runtime ships on its own channel and is never
    republished by an application release.
    """
    version(v)
    build = build_number(build)
    roles = platform_files(v, build, platform)
    require({p.name for p in dist.iterdir()} == set(roles.values()),
            "Unexpected or missing {} release artifacts".format(platform))
    app = {"version": v, "build": build, "platform": platform, "files": {}, "roles": {}}
    sources = {}
    for role, name in roles.items():
        path = dist / name
        require(path.is_file() and not path.is_symlink(), "Missing application artifact")
        h, size = digest(path)
        require(size > 0, "Empty application artifact")
        if name.endswith(".tar.gz"):
            is_mac = name.endswith("-app.tar.gz")
            archive_metadata(path, "Cypher.app" if is_mac else name[:-7],
                             ["Contents/MacOS/cypher"] if is_mac else ["cypher"])
        app["files"][name] = {"sha256": h, "size": size}
        app["roles"][role] = name
        sources[name] = path
    # Nothing is generated until the ENTIRE input set passes.
    output.mkdir(parents=True, exist_ok=True)
    for name, entry in app["files"].items():
        checksum = output / (name + ".sha256")
        checksum.write_text(entry["sha256"] + "\n")
        sources[checksum.name] = checksum
    path = output / "{}-manifest.json".format(platform)
    path.write_bytes(json_bytes(app))
    sources["{}/manifests/{}.json".format(platform, version_stem(v, build))] = path
    assets = dict((p.name, p) for p in dist.iterdir())
    assets.update((p.name, p) for p in sources.values())
    return {"app": app, "platform": platform, "objects": sources, "assets": assets,
            "digests": {key: digest(p) for key, p in sources.items()},
            "asset_digests": {name: digest(p) for name, p in assets.items()}}


def validate_runtime(dist, rv, output, spec):
    """Runtime-only release: exactly the Runtime artifact set, no application.

    Devices poll `runtimes/pi/manifest.json` independently of the application
    channel, so a Runtime revision does not need an application release to
    reach them. The application artifacts are therefore absent here rather than
    stale, and an application artifact in this dist is an error, not extra.
    """
    version(rv)
    runtime, runtime_files, runtime_names = runtime_plan(dist, spec)
    require(runtime["version"] == rv, "Runtime artifacts do not match the requested version")
    require({p.name for p in dist.iterdir()} == runtime_names, "Unexpected or missing Runtime artifacts")
    # Nothing is generated until the ENTIRE input set passes.
    output.mkdir(parents=True, exist_ok=True)
    runtime["files"] = runtime_files
    sources = {"runtimes/pi/" + entry["url"]: dist / entry["url"] for entry in runtime_files.values()}
    path = output / "pi-runtime-manifest.json"
    path.write_bytes(json_bytes(runtime))
    sources["runtimes/pi/manifests/{}.json".format(rv)] = path
    return {"runtime": runtime, "objects": sources,
            "digests": {key: digest(value) for key, value in sources.items()}}


def check_deploy(base):
    """Use the same unauthenticated curl transport as the public installer."""
    parsed = urllib.parse.urlsplit(base)
    require(parsed.scheme == "https" or
            (parsed.scheme == "http" and parsed.hostname in ("127.0.0.1", "localhost", "::1")),
            "Release URL must use HTTPS except loopback")
    require(not parsed.username and not parsed.password and not parsed.query and not parsed.fragment,
            "Invalid release base URL")

    def get(key, head=False, missing=False):
        command = ["curl", "--fail", "--silent", "--show-error", "--connect-timeout", "10",
                   "--max-time", "30"]
        if head:
            command += ["--head"]
        else:
            command += ["--max-filesize", str(MAX_METADATA)]
        command += [base.rstrip("/") + "/releases/" + key]
        result = subprocess.run(command, stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=35)
        if missing and result.returncode != 0:
            return None
        require(result.returncode == 0, "Deployment blocked: release endpoint unavailable: " + key)
        require(len(result.stdout) <= MAX_METADATA, "Release metadata exceeds limit")
        return result.stdout

    # Gate on exactly what the installer resolves. It prefers the Linux channel
    # and falls back to the shared pointers, so this does too: before the first
    # per-platform release the shared channel is still the real one, and after
    # it the Linux channel is.
    raw = get("linux/manifest.json", missing=True)
    channel = "linux/" if raw is not None else ""
    manifest = read_json(raw if raw is not None else get("manifest.json"))
    v = manifest.get("version")
    version(v)
    require(get(channel + "latest.txt").decode().strip() == v,
            "Deployment blocked: release pointers disagree")
    build = manifest.get("build", 1)
    if channel:
        stem = get(channel + "stem.txt").decode().strip()
        require(stem == version_stem(v, build),
                "Deployment blocked: release stem disagrees with the manifest")
    installer = (ROOT / "edge/src/install.sh").read_text()
    floor = re.search(r"^MINIMUM_SETUP_VERSION=([0-9.]+)$", installer, re.M)
    if floor:
        require(version(v) >= version(floor.group(1)),
                "Deployment blocked: publish a client release supporting guided setup first (>= " + floor.group(1) + ")")
    for name in platform_files(v, build, "linux").values():
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
            "app": plan["app"], "assets": plan["asset_digests"],
        })))

    def preflight(self, plan):
        latest = self.api.request(self.prefix + "/releases/latest", missing=True)
        if latest:
            latest_tag = read_json(latest)["tag_name"]
            prefix = "cypher-{}-v".format(plan["platform"])
            match = re.fullmatch(re.escape(prefix) + r"([0-9.]+)-b([1-9][0-9]{0,4})", latest_tag)
            # Platforms release independently, so GitHub's single "latest" may
            # belong to another platform. Only a same-platform release orders
            # against this one; the channel check in R2 remains authoritative.
            require(match is not None or re.fullmatch(r"cypher-[a-z]+-v[0-9.]+-b[0-9]+", latest_tag)
                    or latest_tag.startswith("cypher-v"),
                    "Unexpected GitHub latest release")
            if match:
                require((version(plan["app"]["version"]), plan["app"]["build"])
                        >= (version(match.group(1)), build_number(match.group(2))),
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


def runtime_remote_preflight(plan, store):
    """A Runtime release may move ONE pointer; the application channel is read-only here."""
    keys = ["manifest.json", "latest.txt", "runtimes/pi/manifest.json"]
    keys += [p + "/manifest.json" for p in sorted(DESKTOP_PLATFORMS)]
    keys += [p + "/latest.txt" for p in sorted(DESKTOP_PLATFORMS)]
    pointers = {key: store.get(key) for key in keys}
    desired = plan["runtime"]
    current = pointers["runtimes/pi/manifest.json"]
    if current is not None:
        old = read_json(current)
        require(version(desired["version"]) >= version(old.get("version")),
                "Refusing to roll back runtimes/pi/manifest.json")
        if old["version"] == desired["version"]:
            require(old == desired, "Existing version metadata differs; bump its version")
    # Desktop platforms publish independently, so the binding constraint is the
    # OLDEST published desktop channel: a client on either platform must be able
    # to load this Runtime. Fall back to the legacy channel before decoupling.
    published = [read_json(pointers[p + "/manifest.json"]).get("version")
                 for p in sorted(DESKTOP_PLATFORMS)
                 if pointers[p + "/manifest.json"] is not None]
    if not published and pointers["manifest.json"] is not None:
        published = [read_json(pointers["manifest.json"]).get("version")]
    # No application release means no installed client that could load this.
    require(published, "No published application release")
    require(version(desired["minimumCypherVersion"]) <= min(version(p) for p in published),
            "Runtime requires a newer application than the published release")
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


def legacy_value(store, platform, app):
    """Pointers for clients predating decoupled channels.

    Such a client reads one global manifest and rebuilds the artifact name from
    the version alone, so it can only be sent to a version that is fully
    covered on BOTH desktop platforms at build 1, where the historic names
    exist. The moment either platform re-cuts a version, those clients hold
    still rather than being pointed at a superseded build; they resume at the
    next fully covered version. This channel is never rolled back.
    """
    merged = {}
    for other in sorted(DESKTOP_PLATFORMS):
        current = app if other == platform else None
        if current is None:
            raw = store.get(other + "/manifest.json")
            if raw is None:
                return None
            current = read_json(raw)
        if current.get("version") != app["version"] or current.get("build") != 1:
            return None
        files = current.get("files")
        require(isinstance(files, dict), "Invalid platform manifest")
        merged.update(files)
    if set(merged) != set(app_files(app["version"])):
        return None
    return {"version": app["version"], "files": merged}


def platform_remote_preflight(plan, store):
    """A desktop platform release may move only its OWN pointers, plus the
    shared legacy pointers when a version becomes fully covered."""
    platform = plan["platform"]
    desired = plan["app"]
    keys = (platform + "/manifest.json", platform + "/latest.txt",
            platform + "/stem.txt", "manifest.json", "latest.txt")
    pointers = {key: store.get(key) for key in keys}
    current = pointers[platform + "/manifest.json"]
    if current is not None:
        old = read_json(current)
        old_pair = (version(old.get("version")), build_number(old.get("build", 1)))
        new_pair = (version(desired["version"]), desired["build"])
        require(new_pair >= old_pair, "Refusing to roll back " + platform + "/manifest.json")
        if old_pair == new_pair:
            require(old == desired, "Existing version metadata differs; bump its build")
    missing = []
    # Complete the conflict check for ALL objects before performing any write.
    for key, path in plan["objects"].items():
        require(digest(path) == plan["digests"][key], "Local artifact changed after validation")
        existing = store.digest(key)
        if existing is None:
            missing.append(key)
        else:
            require(existing == plan["digests"][key],
                    "Refusing to overwrite immutable artifact: " + key)
    return pointers, missing


def publish_platform(plan, store, github):
    platform = plan["platform"]
    pointers, missing = platform_remote_preflight(plan, store)
    github.preflight(plan)
    github.stage(plan)  # Private draft only; no public release before validation.
    for key in missing:
        require(digest(plan["objects"][key]) == plan["digests"][key],
                "Local artifact changed during publication")
        store.put(key, plan["objects"][key])
        require(store.digest(key) == plan["digests"][key], "R2 upload verification failed")
    # Detect out-of-band changes before promotion, including the other desktop
    # platform publishing while this one was uploading.
    require(all(store.get(k) == v for k, v in pointers.items()),
            "Release channel changed during upload")
    values = {
        platform + "/manifest.json": json_bytes(plan["app"]),
        platform + "/latest.txt": plan["app"]["version"].encode(),
        # The artifact stem the shell installer needs. `latest.txt` stays a bare
        # version so it can still be compared numerically; a re-cut build is not
        # reachable from the version alone, so it is published separately rather
        # than parsed out of JSON in POSIX sh.
        platform + "/stem.txt": version_stem(plan["app"]["version"],
                                             plan["app"]["build"]).encode(),
    }
    legacy = legacy_value(store, platform, plan["app"])
    if legacy is not None:
        old = pointers["manifest.json"]
        if old is None or version(legacy["version"]) >= version(read_json(old).get("version")):
            values["manifest.json"] = json_bytes(legacy)
            values["latest.txt"] = legacy["version"].encode()
    for key, value in values.items():
        if pointers.get(key) != value:
            store.put(key, value)
        require(store.get(key) == value, "Release pointer verification failed")
    github.promote()  # Public GitHub release is the final step.


def publish_runtime(plan, store):
    pointers, missing = runtime_remote_preflight(plan, store)
    for key in missing:
        require(digest(plan["objects"][key]) == plan["digests"][key], "Local artifact changed during publication")
        store.put(key, plan["objects"][key])
        require(store.digest(key) == plan["digests"][key], "R2 upload verification failed")
    # Detect out-of-band changes before promotion, including an application
    # release that promoted its own Runtime while this one was uploading.
    require(all(store.get(k) == v for k, v in pointers.items()), "Release channel changed during upload")
    key, value = "runtimes/pi/manifest.json", json_bytes(plan["runtime"])
    if pointers[key] != value:
        store.put(key, value)
    require(store.get(key) == value, "Release pointer verification failed")


def require_ci_context(tag, credentials):
    """Every remote write happens in a tag-push job holding the production lock."""
    require(os.environ.get("GITHUB_ACTIONS") == "true"
            and os.environ.get("GITHUB_EVENT_NAME") == "push"
            and os.environ.get("GITHUB_REF") == "refs/tags/" + tag
            and os.environ.get("CYPHER_PRODUCTION_LOCK") == "held",
            "Publication requires a tag-push workflow holding cypher-production")
    for key in credentials:
        require(os.environ.get(key), "Missing required CI credential/context: " + key)
    commit = subprocess.check_output(["git", "rev-parse", "HEAD"], text=True).strip()
    event_commit = subprocess.check_output(
        ["git", "rev-parse", os.environ["GITHUB_SHA"] + "^{commit}"], text=True).strip()
    require(commit == event_commit, "Checkout does not match the workflow event")
    return commit


def workspace_version():
    workspace = (ROOT / "Cargo.toml").read_text().split("[workspace.package]", 1)[1].split("\n[", 1)[0]
    return re.search(r'^version\s*=\s*"([^"]+)"', workspace, re.M).group(1)


def ios_version():
    """Marketing version and build from the Xcode project, which must agree
    across every build configuration."""
    project = (ROOT / "apps/ios/Cypher.xcodeproj/project.pbxproj").read_text()
    found = {}
    for key in ("MARKETING_VERSION", "CURRENT_PROJECT_VERSION"):
        values = set(re.findall(r"^\s*" + key + r" = ([^;]+);", project, re.M))
        require(len(values) == 1, "iOS " + key + " disagrees across build configurations")
        found[key] = values.pop().strip()
    return version(found["MARKETING_VERSION"]) and found["MARKETING_VERSION"], \
        build_number(found["CURRENT_PROJECT_VERSION"])


def emit_context(**values):
    with open(os.environ["GITHUB_OUTPUT"], "a") as output:
        for key, value in values.items():
            output.write("{}={}\n".format(key, value))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("action", choices=["platform-context", "validate-platform",
                                           "publish-platform", "check-deploy",
                                           "runtime-context", "validate-runtime", "publish-runtime",
                                           "ios-context"])
    parser.add_argument("--dist", type=Path, default=Path("dist"))
    parser.add_argument("--version")
    parser.add_argument("--build", default="1")
    parser.add_argument("--platform", choices=sorted(DESKTOP_PLATFORMS))
    parser.add_argument("--out", type=Path, default=Path("target/release-plan"))
    parser.add_argument("--base-url", default="https://edge.letscypher.app")
    args = parser.parse_args()
    if args.action in ("platform-context", "runtime-context", "ios-context"):
        event = os.environ.get("GITHUB_EVENT_NAME")
        require(event in ("push", "workflow_dispatch"), "Unsupported release event")
        publishing = event == "push"
        build = 1
        if args.action == "platform-context":
            require(args.platform, "--platform is required")
            v = workspace_version()
            prefix = "refs/tags/cypher-{}-v".format(args.platform)
            source = "the Cargo workspace version"
            credential = "CLOUDFLARE_API_TOKEN"
        elif args.action == "ios-context":
            v, build = ios_version()
            prefix, source = "refs/tags/cypher-ios-v", "the Xcode project version"
            credential = "AC_API_KEY_P8"
        else:
            v = read_json((ROOT / "dist/pi-runtime/release.json").read_bytes()).get("version")
            prefix, source = "refs/tags/pi-runtime-v", "the pinned Runtime version"
            credential = "CLOUDFLARE_API_TOKEN"
        version(v)
        if publishing:
            ref = os.environ.get("GITHUB_REF", "")
            if args.action == "runtime-context":
                require(ref == prefix + v, "Release tag must equal " + source)
            else:
                # Desktop platforms share a version but carry their own build.
                match = re.fullmatch(re.escape(prefix) + r"([0-9.]+)-b([1-9][0-9]{0,4})", ref)
                require(match is not None,
                        "Release tag must be {}<version>-b<build>".format(prefix.split("/")[-1]))
                require(match.group(1) == v, "Release tag must equal " + source)
                if args.action == "ios-context":
                    require(build_number(match.group(2)) == build,
                            "Release tag build must equal the Xcode build number")
                build = build_number(match.group(2))
            require(os.environ.get(credential),
                    "NOT PUBLISHED: configure {} in repository Actions secrets".format(credential))
        emit_context(version=v, build=build, publish=str(publishing).lower())
        return
    if args.action == "check-deploy":
        print("Installer prerequisites verified for " + check_deploy(args.base_url))
        return
    spec = ROOT / "dist/pi-runtime/package.json"
    if args.action in ("validate-runtime", "publish-runtime"):
        plan = validate_runtime(args.dist, args.version, args.out, spec)
        if args.action == "publish-runtime":
            require_ci_context("pi-runtime-v" + args.version,
                               ("CLOUDFLARE_API_TOKEN", "CLOUDFLARE_ACCOUNT_ID", "GITHUB_SHA"))
            publish_runtime(plan, R2(os.environ["CLOUDFLARE_ACCOUNT_ID"],
                                     os.environ["CLOUDFLARE_API_TOKEN"]))
        print("{} complete: Runtime {} (application channel untouched)".format(
            args.action, plan["runtime"]["version"]))
        return
    require(args.platform, "--platform is required")
    build = build_number(args.build)
    plan = validate_platform(args.dist, args.version, build, args.platform, args.out)
    if args.action == "publish-platform":
        tag = "cypher-{}-v{}-b{}".format(args.platform, args.version, build)
        commit = require_ci_context(tag,
                                    ("CLOUDFLARE_API_TOKEN", "GITHUB_TOKEN", "CLOUDFLARE_ACCOUNT_ID",
                                     "GITHUB_REPOSITORY", "GITHUB_SHA"))
        publish_platform(plan, R2(os.environ["CLOUDFLARE_ACCOUNT_ID"],
                                  os.environ["CLOUDFLARE_API_TOKEN"]),
                         GitHubRelease(os.environ["GITHUB_REPOSITORY"], os.environ["GITHUB_TOKEN"],
                                       tag, commit))
    print("{} complete: {} {} build {} (other platforms and Runtime untouched)".format(
        args.action, args.platform, args.version, build))


if __name__ == "__main__":
    try:
        main()
    except (ReleaseError, OSError, ValueError, KeyError, TypeError, subprocess.TimeoutExpired) as error:
        print("Release check failed: " + str(error), file=sys.stderr)
        sys.exit(1)

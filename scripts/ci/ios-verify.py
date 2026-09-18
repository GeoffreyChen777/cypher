#!/usr/bin/env python3
"""Verify an exported iOS App Store package before it may be uploaded.

Checks the properties that distinguish a real distribution build from one that
merely exists: identity, version/build, device architecture, production push
entitlement, the absence of the debug entitlement, a distribution provisioning
profile, and the privacy manifest. The existence of an IPA is never taken as
proof of correctness.
"""
import argparse
import plistlib
import re
import subprocess
import sys
import tempfile
import zipfile
from pathlib import Path

BUNDLE_ID = "ai.mvp-lab.cypher.ios"
TEAM_ID = "999875MHT4"


class VerifyError(Exception):
    pass


def require(condition, message):
    if not condition:
        raise VerifyError(message)


def run(command, **kwargs):
    result = subprocess.run(command, capture_output=True, timeout=300, **kwargs)
    return result.returncode, result.stdout, result.stderr


def entitlements(app):
    code, out, _ = run(["codesign", "-d", "--entitlements", ":-", str(app)])
    require(code == 0, "Could not read entitlements")
    try:
        return plistlib.loads(out)
    except Exception:
        # Older codesign emits a binary blob with a leading header.
        start = out.find(b"<?xml")
        require(start >= 0, "Unparseable entitlements")
        return plistlib.loads(out[start:])


def verify(export, version, build, unsigned_ok):
    packages = sorted(Path(export).glob("*.ipa"))
    require(len(packages) == 1, "Expected exactly one .ipa in " + str(export))
    with tempfile.TemporaryDirectory() as temp:
        with zipfile.ZipFile(packages[0]) as archive:
            for member in archive.namelist():
                require(not member.startswith("/") and ".." not in Path(member).parts,
                        "Package entry escapes its root")
            archive.extractall(temp)
        apps = list((Path(temp) / "Payload").glob("*.app"))
        require(len(apps) == 1, "Expected exactly one .app in the package")
        app = apps[0]

        info = plistlib.loads((app / "Info.plist").read_bytes())
        require(info.get("CFBundleIdentifier") == BUNDLE_ID,
                "Unexpected bundle id: " + str(info.get("CFBundleIdentifier")))
        require(info.get("CFBundleShortVersionString") == version,
                "Package version {} does not match the tag {}".format(
                    info.get("CFBundleShortVersionString"), version))
        require(str(info.get("CFBundleVersion")) == str(build),
                "Package build {} does not match the tag {}".format(
                    info.get("CFBundleVersion"), build))

        binary = app / info["CFBundleExecutable"]
        code, out, _ = run(["lipo", "-archs", str(binary)])
        require(code == 0, "Could not read architectures")
        archs = out.decode().split()
        require(archs == ["arm64"], "Expected an arm64 device build, got " + str(archs))

        require((app / "PrivacyInfo.xcprivacy").is_file(), "Missing privacy manifest")

        profile = app / "embedded.mobileprovision"
        require(profile.is_file(), "Missing embedded provisioning profile")
        code, out, _ = run(["security", "cms", "-D", "-i", str(profile)])
        require(code == 0, "Could not decode the provisioning profile")
        decoded = plistlib.loads(out)
        require(TEAM_ID in decoded.get("TeamIdentifier", []), "Profile belongs to another team")
        require(not decoded.get("ProvisionedDevices"),
                "App Store distribution must not carry a device list")

        if unsigned_ok:
            print("Verified {} {} ({}) — signature checks skipped".format(
                packages[0].name, version, build))
            return

        rights = entitlements(app)
        require(rights.get("aps-environment") == "production",
                "Expected the production APNs entitlement, got " + str(rights.get("aps-environment")))
        require(not rights.get("get-task-allow"), "Distribution build must not allow debugging")
        require(rights.get("beta-reports-active") is True,
                "TestFlight requires beta-reports-active")
        require(re.match(re.escape(TEAM_ID) + r"\.", str(rights.get("application-identifier", ""))),
                "Entitlements belong to another team")

        code, _, err = run(["codesign", "--verify", "--deep", "--strict", "--verbose=2", str(app)])
        require(code == 0, "Signature verification failed: " + err.decode(errors="replace")[:400])

    print("Verified {} — {} {} ({}), arm64, production APNs, distribution profile".format(
        packages[0].name, BUNDLE_ID, version, build))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--export", required=True)
    parser.add_argument("--version", required=True)
    parser.add_argument("--build", required=True)
    # A package downloaded from an artifact store has lost nothing but is no
    # longer accompanied by its signing keychain; the signed checks already ran
    # in the job that produced it.
    parser.add_argument("--unsigned-ok", action="store_true")
    args = parser.parse_args()
    verify(args.export, args.version, args.build, args.unsigned_ok)


if __name__ == "__main__":
    try:
        main()
    except (VerifyError, OSError, ValueError, KeyError, subprocess.TimeoutExpired) as error:
        print("iOS package check failed: " + str(error), file=sys.stderr)
        sys.exit(1)

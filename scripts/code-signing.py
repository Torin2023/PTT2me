#!/usr/bin/env python3
"""Explicit app-signing policy; never imports keys or falls back to ad-hoc.

Verification uses the caller's expected identity, not the artifact's metadata,
and does not search a keychain or require access to a signing private key.
"""

import argparse
import os
from pathlib import Path
import plistlib
import re
import shutil
import subprocess
import sys


CODE_PATHS = (
    "Contents/Frameworks/libonnxruntime.1.17.1.dylib",
    "Contents/Frameworks/libsherpa-onnx-c-api.dylib",
    "Contents/MacOS/PTT2me",
)
APP_IDENTIFIER = "com.ptt2me.app"
APP_ENTITLEMENTS = {"com.apple.security.device.audio-input": True}
ENTITLEMENTS_PATH = Path(__file__).with_name("developer-id-entitlements.plist")


class PolicyError(Exception):
    """Safe static diagnosis, without certificate, keychain or user details."""


class Once(argparse.Action):
    def __call__(self, parser, namespace, values, option_string=None):
        if getattr(namespace, self.dest, None) is not None:
            raise PolicyError("duplicate signing argument")
        setattr(namespace, self.dest, values)


def validate_policy(mode, identity, team_id):
    if mode == "ad-hoc":
        if identity is not None or team_id is not None:
            raise PolicyError("ad-hoc mode does not accept an identity or Team ID")
    elif mode == "developer-id":
        if not identity or not re.fullmatch(r"[A-Fa-f0-9]{40}", identity):
            raise PolicyError("Developer ID requires an exact 40-hex certificate fingerprint")
        if not team_id or not re.fullmatch(r"[A-Z0-9]{10}", team_id):
            raise PolicyError("Developer ID requires an exact 10-character Team ID")
    else:
        raise PolicyError("explicit --signing-mode ad-hoc|developer-id is required")


def run_tool(arguments, diagnosis):
    try:
        # Do not forward tool stderr: it can include certificate/keychain paths.
        result = subprocess.run(arguments, capture_output=True, check=False, timeout=300)
    except (OSError, subprocess.TimeoutExpired) as error:
        raise PolicyError(diagnosis) from error
    if result.returncode != 0:
        raise PolicyError(diagnosis)
    return result


def preflight(mode, identity, team_id):
    if shutil.which("codesign") is None:
        raise PolicyError("codesign is unavailable")
    if mode == "ad-hoc":
        return
    result = run_tool(
        ["security", "find-identity", "-v", "-p", "codesigning"],
        "could not inspect existing code-signing identities",
    )
    # This is an availability check, not proof of Apple's certificate policy.
    # The cryptographic requirement below is enforced after every signing pass.
    identities = re.findall(
        rb'^\s*\d+\) ([A-Fa-f0-9]{40}) "([^"\r\n]+)"\s*$',
        result.stdout,
        re.MULTILINE,
    )
    expected_hash = identity.upper().encode("ascii")
    expected_suffix = (" (" + team_id + ")").encode("ascii")
    if not any(
        fingerprint.upper() == expected_hash
        and name.startswith(b"Developer ID Application: ")
        and name.endswith(expected_suffix)
        for fingerprint, name in identities
    ):
        raise PolicyError("requested Developer ID Application identity is unavailable for the expected Team ID")


def code_paths(app):
    app = Path(os.path.abspath(app))
    if app.is_symlink() or not app.is_dir():
        raise PolicyError("app must be a real directory")
    paths = []
    for relative in CODE_PATHS:
        current = app
        for component in Path(relative).parts:
            current = current / component
            if current.is_symlink():
                raise PolicyError("app code paths must not contain symlinks")
        if not current.is_file():
            raise PolicyError("required app code file is missing")
        paths.append(current)
    return paths + [app]


def requirement(identity, team_id):
    # '=' makes this literal source rather than a filename (codesign -R).
    return (
        '=anchor apple generic'
        ' and certificate 1[field.1.2.840.113635.100.6.2.6] exists'
        ' and certificate leaf[field.1.2.840.113635.100.6.1.13] exists'
        ' and certificate leaf = H"' + identity.upper() + '"'
        ' and certificate leaf[subject.OU] = "' + team_id + '"'
    )


def metadata_value(metadata, key):
    values = re.findall(r"^" + re.escape(key) + r"=(.*)$", metadata, re.MULTILINE)
    if len(values) != 1 or not values[0].strip():
        raise PolicyError("signature metadata is missing or ambiguous")
    return values[0].strip()


def verify_metadata(metadata, mode, team_id, main_code):
    if mode == "ad-hoc":
        if metadata_value(metadata, "Signature") != "adhoc":
            raise PolicyError("app signature does not match expected ad-hoc mode")
        return
    if metadata_value(metadata, "TeamIdentifier") != team_id:
        raise PolicyError("app signature does not match the expected Team ID")
    flags = re.findall(r"^CodeDirectory .* flags=(0x[0-9a-fA-F]+)\(", metadata, re.MULTILINE)
    if len(flags) != 1 or not int(flags[0], 16) & 0x10000:
        raise PolicyError("Developer ID code must enable Hardened Runtime")
    if metadata_value(metadata, "Timestamp").lower() == "none":
        raise PolicyError("Developer ID code must have a secure timestamp")
    if main_code and metadata_value(metadata, "Identifier") != APP_IDENTIFIER:
        raise PolicyError("app code-signing identifier does not match PTT2me")


def verify_entitlements(raw, main_code):
    try:
        actual = plistlib.loads(raw) if raw.strip() else {}
    except (ValueError, plistlib.InvalidFileException) as error:
        raise PolicyError("could not decode signed entitlements") from error
    expected = APP_ENTITLEMENTS if main_code else {}
    # bool and int compare equal in Python; require the actual Boolean type.
    if actual != expected or any(type(value) is not bool for value in actual.values()):
        raise PolicyError("signed entitlements differ from the minimal app policy")


def verify(app, mode, identity, team_id):
    paths = code_paths(app)
    for index, path in enumerate(paths):
        main_code = index >= 2
        arguments = ["codesign", "--verify", "--strict"]
        if path == paths[-1]:
            arguments.append("--deep")
        if mode == "developer-id":
            arguments += ["--test-requirement", requirement(identity, team_id)]
        run_tool(arguments + [str(path)], "app signature integrity or expected identity verification failed")
        displayed = run_tool(
            ["codesign", "--display", "--verbose=4", str(path)],
            "could not inspect signed code metadata",
        )
        try:
            metadata = (displayed.stdout + displayed.stderr).decode("utf-8")
        except UnicodeDecodeError as error:
            raise PolicyError("could not decode signed code metadata") from error
        verify_metadata(metadata, mode, team_id, main_code)
        if mode == "developer-id":
            entitlements = run_tool(
                ["codesign", "--display", "--entitlements", "-", "--xml", str(path)],
                "could not inspect signed entitlements",
            )
            verify_entitlements(entitlements.stdout, main_code)


def sign(app, mode, identity, team_id):
    preflight(mode, identity, team_id)
    paths = code_paths(app)
    if mode == "developer-id":
        if ENTITLEMENTS_PATH.is_symlink() or not ENTITLEMENTS_PATH.is_file():
            raise PolicyError("minimal signing entitlements file is unavailable")
        verify_entitlements(ENTITLEMENTS_PATH.read_bytes(), True)
    for index, path in enumerate(paths):
        arguments = ["codesign", "--force", "--sign", identity if mode == "developer-id" else "-"]
        if mode == "developer-id":
            arguments += ["--options", "runtime", "--timestamp"]
            if index >= 2:
                arguments += ["--identifier", APP_IDENTIFIER, "--entitlements", str(ENTITLEMENTS_PATH)]
        run_tool(arguments + [str(path)], "app signing failed; no fallback was attempted")
    verify(app, mode, identity, team_id)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("operation", choices=("preflight", "sign", "verify"))
    parser.add_argument("--signing-mode", action=Once)
    parser.add_argument("--signing-identity", action=Once)
    parser.add_argument("--team-id", action=Once)
    parser.add_argument("--app", action=Once)
    try:
        args = parser.parse_args()
        validate_policy(args.signing_mode, args.signing_identity, args.team_id)
        if args.operation == "preflight":
            if args.app is not None:
                raise PolicyError("preflight does not accept an app")
            preflight(args.signing_mode, args.signing_identity, args.team_id)
        else:
            if not args.app:
                raise PolicyError("signing and verification require an app")
            operation = sign if args.operation == "sign" else verify
            operation(args.app, args.signing_mode, args.signing_identity, args.team_id)
    except (PolicyError, OSError) as error:
        message = str(error) if isinstance(error, PolicyError) else "could not read signing policy inputs"
        print("PTT2me code signing failed [signing]: " + message, file=sys.stderr)
        return 1
    print("PTT2me code-signing policy passed (" + args.operation + ", " + args.signing_mode + ")")
    return 0


if __name__ == "__main__":
    sys.exit(main())

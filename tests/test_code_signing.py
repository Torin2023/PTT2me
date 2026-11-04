"""Policy/command contracts, not evidence of a real Developer ID signature."""

import importlib.util
import json
import os
from pathlib import Path
import plistlib
import re
import shutil
import subprocess
import sys
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location("code_signing", ROOT / "scripts/code-signing.py")
POLICY = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(POLICY)
IDENTITY = "0123456789ABCDEF0123456789ABCDEF01234567"
TEAM_ID = "ABCD123456"
DEV_ARGS = ["--signing-mode", "developer-id", "--signing-identity", IDENTITY, "--team-id", TEAM_ID]

# Both tools are fake. Their output deliberately contains private-looking
# details to ensure production errors do not echo raw tool output.
FAKE_TOOL = r'''
import json, os, pathlib, plistlib, sys
args = sys.argv[1:]
tool = pathlib.Path(sys.argv[0]).name
with open(os.environ["CALL_LOG"], "a") as log:
    log.write(json.dumps([tool] + args) + "\n")
fault = os.environ.get("FAULT", "")
identity = "0123456789ABCDEF0123456789ABCDEF01234567"
team = "ABCD123456"
private_detail = "PRIVATE_CERTIFICATE_DETAIL /Users/fixture/private.keychain-db"
if tool == "security":
    if fault == "keychain-failure":
        print(private_detail, file=sys.stderr)
        sys.exit(9)
    if fault in ("missing-identity", "verify-no-keychain"):
        print("0 valid identities found")
    else:
        name = "Developer ID Application: Fixture"
        if fault == "wrong-type": name = "Apple Development: Fixture"
        if fault == "wrong-identity": identity = "F" * 40
        if fault == "wrong-team": team = "ZZZZ999999"
        print('  1) ' + identity + ' "' + name + ' (' + team + ')"')
    sys.exit(0)
if "--sign" in args:
    if fault == "signing-failure":
        print(private_detail, file=sys.stderr)
        sys.exit(8)
    sys.exit(0)
if "--verify" in args:
    if fault == "requirement-rejected":
        print(private_detail, file=sys.stderr)
        sys.exit(7)
    sys.exit(0)
main_code = args[-1].endswith(".app") or args[-1].endswith("/MacOS/PTT2me")
if "--entitlements" in args:
    entitlements = {"com.apple.security.device.audio-input": True} if main_code else {}
    if fault == "missing-audio" and main_code: entitlements = {}
    if fault == "library-entitlements" and not main_code: entitlements = {"com.apple.security.cs.disable-library-validation": True}
    if fault == "extra-entitlement": entitlements["com.apple.security.cs.allow-unsigned-executable-memory"] = True
    if fault == "integer-entitlement" and main_code: entitlements["com.apple.security.device.audio-input"] = 1
    if fault == "invalid-entitlements":
        sys.stdout.buffer.write(b"invalid plist")
    elif entitlements:
        sys.stdout.buffer.write(plistlib.dumps(entitlements))
    sys.exit(0)
mode = os.environ.get("DISPLAY_MODE", "developer-id")
if mode == "ad-hoc":
    print("Signature=adhoc", file=sys.stderr)
else:
    flags = "0x0(none)" if fault == "missing-runtime" else "0x10000(runtime)"
    print("CodeDirectory v=20500 size=800 flags=" + flags + " hashes=1+1", file=sys.stderr)
    print("TeamIdentifier=" + ("ZZZZ999999" if fault == "metadata-team" else team), file=sys.stderr)
    if fault != "missing-timestamp":
        print("Timestamp=" + ("none" if fault == "disabled-timestamp" else "Nov 4, 2026 at 00:00:00"), file=sys.stderr)
    if main_code:
        print("Identifier=" + ("com.fixture.wrong" if fault == "wrong-app-id" else "com.ptt2me.app"), file=sys.stderr)
'''


class SigningPolicyTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="ptt2me-signing-contracts-")
        self.addCleanup(self.temp.cleanup)
        self.base = Path(self.temp.name)
        self.app = self.base / "PTT2me.app"
        for relative in POLICY.CODE_PATHS:
            code = self.app / relative
            code.parent.mkdir(parents=True, exist_ok=True)
            code.write_bytes(b"fixture, not a Mach-O or signed app")
        self.bin = self.base / "bin"
        self.bin.mkdir()
        for name in ("codesign", "security"):
            path = self.bin / name
            path.write_text("#!" + sys.executable + "\n" + FAKE_TOOL)
            path.chmod(0o755)
        self.log = self.base / "calls.jsonl"
        self.env = dict(os.environ, PATH=str(self.bin) + os.pathsep + os.environ["PATH"], CALL_LOG=str(self.log))

    def invoke(self, operation="verify", policy_args=None, **environment):
        self.env.update(environment)
        args = [sys.executable, "-B", str(ROOT / "scripts/code-signing.py"), operation]
        args += DEV_ARGS if policy_args is None else policy_args
        if operation != "preflight":
            args += ["--app", str(self.app)]
        return subprocess.run(args, capture_output=True, text=True, env=self.env)

    def calls(self):
        return [json.loads(line) for line in self.log.read_text().splitlines()] if self.log.exists() else []

    def assert_failure(self, result):
        self.assertNotEqual(result.returncode, 0)
        self.assertNotIn("PRIVATE_CERTIFICATE_DETAIL", result.stdout + result.stderr)
        self.assertNotIn("private.keychain-db", result.stdout + result.stderr)
        self.assertNotIn("policy passed", result.stdout)

    def test_invalid_or_ambiguous_policy_never_calls_a_tool(self):
        cases = [
            [], ["--signing-mode", "automatic"],
            ["--signing-mode", "developer-id"],
            ["--signing-mode", "developer-id", "--signing-identity", "Developer ID Application"],
            ["--signing-mode", "developer-id", "--signing-identity", "-", "--team-id", TEAM_ID],
            DEV_ARGS[:-1] + ["invalid"],
            DEV_ARGS + ["--signing-mode", "ad-hoc"],
            ["--signing-mode", "ad-hoc", "--signing-identity", IDENTITY],
            ["--signing-mode", "ad-hoc", "--team-id", TEAM_ID],
        ]
        for args in cases:
            with self.subTest(args=args):
                self.assert_failure(self.invoke("preflight", args))
                self.assertEqual(self.calls(), [])

    def test_unavailable_identity_stops_before_any_signing_and_hides_details(self):
        for fault in ("missing-identity", "wrong-identity", "wrong-team", "wrong-type", "keychain-failure"):
            with self.subTest(fault=fault):
                self.log.unlink(missing_ok=True)
                self.assert_failure(self.invoke("sign", FAULT=fault))
                self.assertEqual([call[0] for call in self.calls()], ["security"])

    def test_signing_error_never_falls_back_or_continues(self):
        result = self.invoke("sign", FAULT="signing-failure")
        self.assert_failure(result)
        calls = self.calls()
        self.assertEqual(len(calls), 2)
        self.assertEqual(calls[1][calls[1].index("--sign") + 1], IDENTITY)
        self.assertIn("no fallback", result.stderr)

    def test_inside_out_signing_explicit_runtime_timestamp_and_minimal_entitlements(self):
        result = self.invoke("sign")
        self.assertEqual(result.returncode, 0, result.stderr)
        signatures = [call for call in self.calls() if "--sign" in call]
        self.assertEqual([call[-1] for call in signatures], [str(self.app / p) for p in POLICY.CODE_PATHS] + [str(self.app)])
        for index, call in enumerate(signatures):
            self.assertEqual(call[call.index("--sign") + 1], IDENTITY)
            self.assertEqual(call[call.index("--options") + 1], "runtime")
            self.assertIn("--timestamp", call)
            self.assertNotIn("--deep", call)
            self.assertEqual("--entitlements" in call, index >= 2)
        self.assertEqual(plistlib.loads(POLICY.ENTITLEMENTS_PATH.read_bytes()), POLICY.APP_ENTITLEMENTS)

    def test_independent_verifier_checks_every_code_without_keychain_access(self):
        result = self.invoke(FAULT="verify-no-keychain")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue(all(call[0] == "codesign" for call in self.calls()))
        verifies = [call for call in self.calls() if "--verify" in call]
        self.assertEqual(len(verifies), 4)
        for call in verifies:
            self.assertIn("--strict", call)
            self.assertEqual(call[call.index("--test-requirement") + 1], POLICY.requirement(IDENTITY, TEAM_ID))
        self.assertIn("--deep", verifies[-1])

    def test_verification_failures_do_not_accept_signed_metadata_alone(self):
        for fault in (
            "requirement-rejected", "metadata-team", "missing-runtime", "missing-timestamp",
            "disabled-timestamp", "wrong-app-id", "missing-audio", "extra-entitlement",
            "integer-entitlement", "library-entitlements", "invalid-entitlements",
        ):
            with self.subTest(fault=fault):
                self.assert_failure(self.invoke(FAULT=fault))
        self.assert_failure(self.invoke(DISPLAY_MODE="ad-hoc", FAULT=""))

    def test_explicit_ad_hoc_stays_available_and_rejects_other_signature_mode(self):
        args = ["--signing-mode", "ad-hoc"]
        result = self.invoke("sign", args, DISPLAY_MODE="ad-hoc")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue(all(call[0] == "codesign" for call in self.calls()))
        for call in self.calls():
            if "--sign" in call:
                self.assertEqual(call[call.index("--sign") + 1], "-")
        self.assert_failure(self.invoke("verify", args, DISPLAY_MODE="developer-id"))

    def test_symlinked_code_is_rejected_before_any_signature(self):
        code = self.app / POLICY.CODE_PATHS[-1]
        code.unlink()
        code.symlink_to(self.app / POLICY.CODE_PATHS[0])
        self.assert_failure(self.invoke("sign"))
        self.assertFalse(any("--sign" in call for call in self.calls()))

    @unittest.skipUnless(Path("/usr/bin/csreq").is_file(), "Apple requirement compiler is macOS-only")
    def test_apple_compiler_accepts_exact_certificate_requirement_without_an_identity(self):
        result = subprocess.run(
            ["/usr/bin/csreq", "-r", POLICY.requirement(IDENTITY, TEAM_ID), "-t"],
            capture_output=True, text=True,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        for expected in ("anchor apple generic", "1.2.840.113635.100.6.1.13", "1.2.840.113635.100.6.2.6", IDENTITY.lower(), TEAM_ID):
            self.assertIn(expected, result.stdout)

    def test_builder_forwards_policy_to_preflight_before_any_build_side_effect(self):
        fixture = self.base / "repo"
        scripts = fixture / "scripts"
        scripts.mkdir(parents=True)
        for name in ("build-release-artifacts.sh", "code-signing.sh"):
            shutil.copy2(ROOT / "scripts" / name, scripts / name)
        preflight = scripts / "release-preflight.sh"
        preflight.write_text('#!/bin/bash\nprintf "%s\\n" "$@" >"$CALL_LOG"\nexit 73\n')
        preflight.chmod(0o755)
        for name in ("model.json", "public.txt", "private.txt"):
            (self.base / name).touch()
        (self.base / "model").mkdir()
        (self.base / "output").mkdir()
        args = [str(scripts / "build-release-artifacts.sh")] + DEV_ARGS + [
            "--version", "1.3.2", "--build", "202611040000", "--source-commit", "1" * 40,
            "--model-manifest", str(self.base / "model.json"), "--model-source", str(self.base / "model"),
            "--public-key", str(self.base / "public.txt"), "--private-key", str(self.base / "private.txt"),
            "--published-at", "2026-11-04T00:00:00Z", "--output-dir", str(self.base / "output"),
        ]
        result = subprocess.run(args, capture_output=True, text=True, env=self.env)
        self.assertEqual(result.returncode, 73, result.stderr)
        self.assertEqual(self.log.read_text().splitlines()[:len(DEV_ARGS)], DEV_ARGS)
        self.assertEqual(list((self.base / "output").iterdir()), [])

    def test_all_packaging_and_independent_verifier_calls_forward_expected_policy(self):
        expected_counts = {
            "build-app.sh": 1, "build-release-artifacts.sh": 6,
            "build-dmg.sh": 2, "verify-release-artifacts.sh": 2,
        }
        for filename, count in expected_counts.items():
            text = (ROOT / "scripts" / filename).read_text()
            calls = re.findall(r'^(?:"\$SCRIPT_DIR/|scripts/)(?:build-app|build-dmg|check-bundle|release-preflight)\.sh"?[^\n]*', text, re.MULTILINE)
            self.assertEqual(len(calls), count, filename)
            for call in calls:
                self.assertIn('"${SIGNING_ARGS[@]}"', call, filename)
        builder = (ROOT / "scripts/build-release-artifacts.sh").read_text()
        mutation = builder.index('"Set :PTT2meDistributionVariant update"')
        reseal = builder.index('signing_sign_app "$UPDATE_APP"')
        parity = builder.index('"$SCRIPT_DIR/compare-macho-payload.sh"')
        package = builder.index('"$SCRIPT_DIR/build-dmg.sh"')
        self.assertLess(builder.index('rm -rf "$UPDATE_APP/Contents/Resources/models"'), mutation)
        self.assertLess(mutation, reseal)
        self.assertLess(reseal, parity)
        self.assertLess(parity, package)
        for filename in ("build-app.sh", "build-release-artifacts.sh"):
            self.assertNotIn("codesign --force --sign -", (ROOT / "scripts" / filename).read_text())


if __name__ == "__main__":
    unittest.main()

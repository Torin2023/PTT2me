import contextlib
import copy
import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch
import zipfile
from datetime import datetime, timezone

SPEC = importlib.util.spec_from_file_location(
    "ci_validation", Path(__file__).resolve().parents[1] / "scripts/ci-validation.py")
ci = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(ci)


class ScopeTests(unittest.TestCase):
    def test_main_site_checks_are_delegated_only_when_pages_will_trigger(self):
        cases = ((["site/app/page.tsx"], set()),
                 ([".github/workflows/ci.yml"], {"rust", "site"}),
                 (["scripts/ci-validation.py"], {"rust", "site"}),
                 (["unknown-input"], {"rust", "site"}),
                 (["site/app/page.tsx", ".github/workflows/ci.yml"], {"rust"}),
                 (["README.md"], set()),
                 ([f"site/{i}.tsx" for i in range(3000)], set()),
                 ([f"site/{i}.tsx" for i in range(3001)], {"site"}),
                 (None, {"rust", "site"}))
        for paths, expected in cases:
            with self.subTest(paths=paths), patch.object(ci, "changed_paths", return_value=paths):
                self.assertEqual(ci.changed_scopes("a" * 40, "HEAD", event_name="push"), expected)
        with patch.object(ci, "changed_paths", return_value=["site/app/page.tsx"]):
            self.assertEqual(ci.changed_scopes("a" * 40, "HEAD", event_name="pull_request"), {"site"})

    def test_pages_routing_matches_the_actual_workflow_filter(self):
        workflow = (Path(__file__).resolve().parents[1] / ".github/workflows/pages.yml").read_text()
        block = workflow.split("    paths:\n", 1)[1].split("  workflow_dispatch:", 1)[0]
        actual = tuple(line.strip()[2:] for line in block.splitlines() if line.strip().startswith("- "))
        self.assertEqual(actual, ci.PAGES_PATHS)

    def test_documentation_and_release_metadata_do_not_need_rust(self):
        for path in ("README.md", "AGENTS.md", "docs/release/AUTOMATED_RELEASE.md",
                     ".github/pull_request_template.md", ".github/ISSUE_TEMPLATE/task.yml",
                     "updates/channels/stable.json", "updates/releases/1.3.2.json"):
            with self.subTest(path=path):
                self.assertEqual(ci.scopes(path), set())

    def test_runtime_security_policy_and_unknown_files_are_not_exempt(self):
        for path in ("Cargo.toml", "Cargo.lock", "rust-toolchain.toml", ".cargo/config.toml",
                     "src/main.rs", "tests/pasteboard_main.rs", "scripts/cloud-setup.sh",
                     "updates/public-key.txt", "models/manifests/gigaam-v3-rnnt-v1.json",
                     ".github/workflows/ci.yml", "scripts/ci-validation.py", "new/input.bin",
                     "docs/helper.py", "updates/schema.json"):
            with self.subTest(path=path):
                self.assertIn("rust", ci.scopes(path))

    def test_site_does_not_run_rust_but_ci_policy_checks_both(self):
        self.assertEqual(ci.scopes("site/package-lock.json"), {"site"})
        self.assertEqual(ci.scopes(".github/workflows/ci.yml"), {"rust", "site"})

    def test_fingerprint_includes_mode_name_contents_and_unknown_inputs(self):
        entry = ("src/main.rs", "100644", "blob", "a" * 40)
        baseline = ci.fingerprint([entry])
        for changed in (("src/renamed.rs", *entry[1:]),
                        (entry[0], "100755", *entry[2:]),
                        (*entry[:3], "b" * 40)):
            self.assertNotEqual(baseline, ci.fingerprint([changed]))
        self.assertNotEqual(baseline, ci.fingerprint([entry, ("unknown", *entry[1:])]))
        self.assertEqual(baseline, ci.fingerprint([entry, ("README.md", *entry[1:])]))

    def test_missing_base_and_force_dispatch_fail_closed(self):
        for base in (None, "", "0" * 40, "bad-ref"):
            self.assertEqual(ci.changed_scopes(base, "HEAD"), {"rust", "site"})
        self.assertEqual(ci.changed_scopes("a" * 40, "HEAD", force=True), {"rust", "site"})
        with patch.object(ci, "command", side_effect=subprocess.CalledProcessError(1, "git")):
            self.assertEqual(ci.changed_scopes("a" * 40, "HEAD"), {"rust", "site"})

    def test_real_git_metadata_followup_rename_delete_and_base_change(self):
        previous = Path.cwd()
        with tempfile.TemporaryDirectory() as directory:
            try:
                os.chdir(directory)
                ci.command("git", "init", "-q")

                def commit():
                    ci.command("git", "add", ".")
                    ci.command("git", "-c", "user.name=CI Test", "-c", "user.email=ci@example.invalid",
                               "commit", "-qm", "fixture")
                    return ci.command("git", "rev-parse", "HEAD").decode().strip()

                Path("Cargo.toml").write_text('version = "1.3.1"\n')
                Path("README.md").write_text("old\n")
                base = commit()
                Path("Cargo.toml").write_text('version = "1.3.2"\n')
                version = commit()
                original = ci.tree_fingerprint()
                Path("README.md").write_text("release notes\n")
                docs = commit()
                self.assertIn("rust", ci.changed_scopes(base, docs))
                self.assertEqual(ci.changed_scopes(version, docs), set())
                self.assertEqual(ci.tree_fingerprint(), original)
                Path("Cargo.toml").rename("docs.md")
                renamed = commit()
                self.assertIn("rust", ci.changed_scopes(docs, renamed))
                self.assertNotEqual(ci.tree_fingerprint(), original)
                Path("docs.md").unlink()
                deleted = commit()
                self.assertIn("rust", ci.changed_scopes(renamed, deleted))
                Path("src").mkdir()
                Path("src/base_change.rs").write_text("// merged base input\n")
                commit()
                self.assertNotEqual(ci.tree_fingerprint(), original)
            finally:
                os.chdir(previous)


class EvidenceTests(unittest.TestCase):
    def setUp(self):
        self.now = datetime(2026, 11, 3, 12, tzinfo=timezone.utc)
        self.repo = "owner/project"
        self.runner = {"ImageOS": "macos15", "ImageVersion": "20261101", "RUNNER_ARCH": "ARM64"}
        self.entries = [("src/main.rs", "100644", "blob", "a" * 40),
                        (".github/workflows/ci.yml", "100644", "blob", "b" * 40)]
        self.digest = ci.fingerprint(self.entries)
        self.run = {"id": 7, "run_attempt": 1, "path": ci.WORKFLOW, "event": "pull_request",
                    "status": "completed", "conclusion": "success", "head_sha": "c" * 40,
                    "repository": {"full_name": self.repo}, "head_repository": {"full_name": self.repo},
                    "updated_at": "2026-11-03T11:00:00Z"}
        self.proof = {"schema": 1, "repository": self.repo, "run_id": 7, "run_attempt": 1,
                      "head_sha": "c" * 40, "tested_sha": "d" * 40, "input_sha256": self.digest,
                      "runner": self.runner, "advisory_revision": "e" * 40}
        self.artifact = {"id": 8, "name": "rust-validation-1", "expired": False,
                         "size_in_bytes": 1000, "created_at": "2026-11-03T10:59:59Z"}
        self.job = {"name": "Format, test, lint, and audit", "conclusion": "success",
                    "started_at": "2026-11-03T10:00:00Z", "completed_at": "2026-11-03T11:00:00Z",
                    "steps": [{"name": name, "conclusion": "success"} for name in ci.GATES]}
        self.tree = {"truncated": False,
                     "tree": [dict(zip(("path", "mode", "type", "sha"), e)) for e in self.entries]}
        self.tested_tree = copy.deepcopy(self.tree)

    def archive(self, proof=None, name="evidence.json", extra=None):
        stream = io.BytesIO()
        with zipfile.ZipFile(stream, "w", compression=zipfile.ZIP_DEFLATED) as archive:
            archive.writestr(name, json.dumps(self.proof if proof is None else proof))
            if extra:
                archive.writestr(extra, "extra")
        data = stream.getvalue()
        self.artifact["digest"] = "sha256:" + hashlib.sha256(data).hexdigest()
        return data

    def find(self):
        data = self.archive()

        def request(endpoint, binary=False):
            if endpoint.endswith("runs?status=success&per_page=20"):
                return {"workflow_runs": [self.run]}
            if endpoint.endswith("/artifacts"):
                return {"artifacts": [self.artifact]}
            if endpoint.endswith("/zip"):
                self.assertTrue(binary)
                return data
            if endpoint.endswith("/jobs"):
                return {"jobs": [self.job]}
            if "/git/trees/" in endpoint:
                return self.tested_tree if self.proof["tested_sha"] in endpoint else self.tree
            self.fail(endpoint)

        return ci.find_evidence(self.repo, self.digest, self.runner, "e" * 40, self.now, request)

    def test_complete_evidence_is_reused(self):
        self.assertEqual(self.find(), "https://github.com/owner/project/actions/runs/7")

    def test_every_proof_identity_field_must_match(self):
        for key in self.proof:
            with self.subTest(key=key):
                changed = copy.deepcopy(self.proof)
                changed[key] = "wrong"
                self.assertFalse(ci.matching_evidence(changed, self.run, self.repo,
                                                      self.digest, self.runner, "e" * 40))

    def test_failed_cancelled_fork_wrong_workflow_old_and_future_runs(self):
        changes = ({"conclusion": "failure"}, {"status": "in_progress"}, {"conclusion": "cancelled"},
                   {"path": ".github/workflows/other.yml"}, {"event": "pull_request_target"},
                   {"head_repository": {"full_name": "fork/project"}},
                   {"repository": {"full_name": "other/project"}},
                   {"updated_at": "2026-11-01T12:00:00Z"}, {"updated_at": "2026-11-04T12:00:00Z"})
        for change in changes:
            with self.subTest(change=change):
                self.assertFalse(ci.eligible_run(self.run | change, self.repo, self.now))

    def test_all_full_steps_must_have_executed(self):
        for status in ("skipped", "failure", "cancelled"):
            for index in range(len(ci.GATES)):
                job = copy.deepcopy(self.job)
                job["steps"][index]["conclusion"] = status
                self.assertFalse(ci.completed_gates([job], self.artifact, self.now))

    def test_producer_and_artifact_times_are_verified(self):
        for key, value in (("completed_at", "2026-11-01T11:00:00Z"),
                           ("completed_at", "2026-11-04T11:00:00Z")):
            self.assertFalse(ci.completed_gates([self.job | {key: value}], self.artifact, self.now))
        for created in ("2026-11-03T09:00:00Z", "2026-11-03T11:01:00Z"):
            self.assertFalse(ci.completed_gates([self.job], self.artifact | {"created_at": created}, self.now))

    def test_wrong_attempt_or_expired_artifact_is_not_reused(self):
        self.run["run_attempt"] = 2
        self.assertIsNone(self.find())
        self.run["run_attempt"] = 1
        self.artifact["expired"] = True
        self.assertIsNone(self.find())

    def test_independent_producer_tree_blocks_forged_claims(self):
        self.tree["tree"][1]["sha"] = "f" * 40
        self.assertIsNone(self.find())

    def test_tested_merge_tree_is_checked_independently(self):
        self.tested_tree["tree"][0]["sha"] = "f" * 40
        self.assertIsNone(self.find())

    def test_truncated_producer_or_tested_tree_is_not_reused(self):
        for tree in (self.tree, self.tested_tree):
            tree["truncated"] = True
            self.assertIsNone(self.find())
            tree["truncated"] = False

    def test_archive_shape_size_and_digest_fail_closed(self):
        data = self.archive()
        self.assertEqual(ci.archive_evidence(data, self.artifact), self.proof)
        with self.assertRaises(ValueError):
            ci.archive_evidence(data, self.artifact | {"digest": "sha256:wrong"})
        for name, extra in (("../evidence.json", None), ("evidence.json", "extra")):
            data = self.archive(name=name, extra=extra)
            with self.assertRaises(ValueError):
                ci.archive_evidence(data, self.artifact)
        data = self.archive(proof={"oversize": "a" * (ci.MAX_ARCHIVE_BYTES + 1)})
        with self.assertRaises(ValueError):
            ci.archive_evidence(data, self.artifact)
        with self.assertRaises(ValueError):
            ci.archive_evidence(b"a" * (ci.MAX_ARCHIVE_BYTES + 1), self.artifact)
        with self.assertRaises(zipfile.BadZipFile):
            ci.archive_evidence(b"invalid", {"digest": "sha256:" + hashlib.sha256(b"invalid").hexdigest()})
        data = self.archive(proof=[])
        with self.assertRaises(ValueError):
            ci.archive_evidence(data, self.artifact)

    def test_empty_advisory_response_is_unavailable(self):
        with patch.object(ci, "command", return_value=b""):
            with self.assertRaises(ValueError):
                ci.current_advisory_revision()

    def test_api_failure_forces_full_gate_without_leaking_error_output(self):
        with patch.dict(os.environ, {"GITHUB_EVENT_NAME": "pull_request", "GITHUB_REPOSITORY": self.repo}, clear=True), \
                patch.object(ci.sys, "argv", ["ci-validation.py", "reuse"]), \
                patch.object(ci, "tree_fingerprint", return_value=self.digest), \
                patch.object(ci, "runner_identity", return_value=self.runner), \
                patch.object(ci, "current_advisory_revision", side_effect=OSError("private error detail")), \
                contextlib.redirect_stdout(io.StringIO()) as output, \
                contextlib.redirect_stderr(io.StringIO()) as error:
            ci.main()
            self.assertIn("reused=false", output.getvalue())
            self.assertNotIn("private error detail", error.getvalue())

    def test_dispatch_does_not_attempt_reuse(self):
        with patch.dict(os.environ, {"GITHUB_EVENT_NAME": "workflow_dispatch"}, clear=True), \
                patch.object(ci.sys, "argv", ["ci-validation.py", "reuse"]), \
                patch.object(ci, "find_evidence") as find, contextlib.redirect_stdout(io.StringIO()) as output:
            ci.main()
            find.assert_not_called()
            self.assertIn("reused=false", output.getvalue())


class RequiredGateTests(unittest.TestCase):
    def test_failed_cancelled_or_incomplete_scope_never_passes_required_gate(self):
        workflow = (Path(__file__).resolve().parents[1] / ci.WORKFLOW).read_text()
        step = workflow.split("      - name: Require a successful scope decision\n", 1)[1]
        step = step.split("      - name:", 1)[0]
        script = "\n".join(line[10:] for line in step.split("        run: |\n", 1)[1].splitlines()
                           if line.startswith("          "))
        self.assertTrue(script)
        for result, rust, site in (("success", "true", "false"), ("success", "false", "true"),
                                   ("success", "false", "false"), ("success", "true", "true"),
                                   ("failure", "true", "true"), ("cancelled", "true", "true"),
                                   ("skipped", "false", "false"), ("success", "", "false"),
                                   ("success", "false", ""), ("success", "true", "unknown")):
            with self.subTest(result=result, rust=rust, site=site):
                process = subprocess.run(["bash", "-eo", "pipefail", "-c", script],
                                         env=os.environ | {"SCOPE_RESULT": result, "RUST_AFFECTED": rust,
                                                           "SITE_SELECTED": site}, capture_output=True)
                expected = result == "success" and rust in {"true", "false"} and site in {"true", "false"}
                self.assertEqual(process.returncode == 0, expected)


if __name__ == "__main__":
    unittest.main()

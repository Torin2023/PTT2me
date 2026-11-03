#!/usr/bin/env python3
"""Select CI work and reuse only recent, independently identified full runs."""

import argparse
import fnmatch
import hashlib
import io
import json
import os
import re
import subprocess
import sys
import zipfile
from datetime import datetime, timezone
from pathlib import Path

WORKFLOW = ".github/workflows/ci.yml"
GATES = (
    "Prepare repository toolchain and audit dependencies",
    "Check formatting",
    "Run all tests",
    "Run shell contract tests",
    "Build AppKit and WebKit insertion fixture",
    "Deny Clippy warnings",
)
MAX_AGE_SECONDS = 24 * 60 * 60
MAX_ARCHIVE_BYTES = 64 * 1024
PAGES_PATHS = (
    "site/**", "updates/**", "models/manifests/**",
    "scripts/assemble-pages-artifact.sh", "scripts/validate-update-manifest.sh",
    ".github/workflows/pages.yml",
)


def command(*args):
    return subprocess.run(args, check=True, stdout=subprocess.PIPE,
                          stderr=subprocess.PIPE, timeout=45).stdout


def scopes(path):
    # Only known non-runtime inputs are exempt. New/unknown paths fail closed.
    if path.startswith("site/"):
        return {"site"}
    if (path in {"README.md", "AGENTS.md", ".github/pull_request_template.md"}
            or (path.startswith("docs/") and path.endswith(".md"))
            or path.startswith(".github/ISSUE_TEMPLATE/")):
        return set()
    if re.fullmatch(r"updates/(?:releases/[0-9]+\.[0-9]+\.[0-9]+|channels/[a-z]+)\.json", path):
        return set()  # Pages independently verifies signed records and actual DMGs.
    if (path.startswith(".github/workflows/")
            or path in {"scripts/ci-validation.py", "tests/test_ci_validation.py"}):
        return {"rust", "site"}
    if (path.startswith(("src/", "tests/", "scripts/", "models/", "assets/", ".cargo/"))
            or path in {"Cargo.toml", "Cargo.lock", "rust-toolchain.toml", "build.rs",
                        "updates/public-key.txt"}):
        return {"rust"}
    return {"rust", "site"}


def fingerprint(entries):
    inputs = sorted((entry for entry in entries if "rust" in scopes(entry[0])))
    return hashlib.sha256(json.dumps(inputs, separators=(",", ":")).encode()).hexdigest()


def tree_fingerprint(ref="HEAD"):
    entries = []
    for record in command("git", "ls-tree", "-r", "-z", ref).split(b"\0"):
        if record:
            metadata, path = record.decode().split("\t", 1)
            mode, kind, sha = metadata.split()
            entries.append((path, mode, kind, sha))
    return fingerprint(entries)


def changed_paths(base, head):
    if not re.fullmatch(r"[0-9a-f]{40}", base or "") or base == "0" * 40:
        return None
    try:
        # No rename detection: both deleted and added paths must participate.
        paths = command("git", "diff", "--name-only", "--no-renames", "-z", base, head)
        return [p.decode() for p in paths.split(b"\0") if p]
    except (subprocess.SubprocessError, UnicodeError):
        return None


def changed_scopes(base, head, force=False, event_name=None):
    paths = changed_paths(base, head)
    if force or paths is None:
        return {"rust", "site"}
    selected = set().union(*(scopes(path) for path in paths))
    # Only hand site checks to Pages when its push path filter is known to
    # match. GitHub limits those filters to 300 files; larger diffs stay here.
    if (event_name == "push" and len(paths) <= 300
            and any(fnmatch.fnmatchcase(path, pattern) for path in paths for pattern in PAGES_PATHS)):
        selected.discard("site")
    return selected


def emit(values):
    lines = "".join(f"{key}={str(value).lower() if isinstance(value, bool) else value}\n"
                    for key, value in values.items())
    print(lines, end="")
    if os.environ.get("GITHUB_OUTPUT"):
        with open(os.environ["GITHUB_OUTPUT"], "a") as output:
            output.write(lines)


def api(endpoint, binary=False):
    # gh handles GitHub's signed artifact redirect; the token stays out of argv/logs.
    data = command("gh", "api", endpoint)
    return data if binary else json.loads(data)


def runner_identity():
    identity = {key: os.environ.get(key, "")
                for key in ("ImageOS", "ImageVersion", "RUNNER_ARCH")}
    if not all(identity.values()):
        raise ValueError("runner image identity unavailable")
    return identity


def current_advisory_revision():
    revision = command("git", "ls-remote", "https://github.com/RustSec/advisory-db.git", "HEAD")
    fields = revision.decode().split()
    sha = fields[0] if fields else ""
    if not re.fullmatch(r"[0-9a-f]{40}", sha):
        raise ValueError("advisory revision unavailable")
    return sha


def archive_evidence(data, artifact):
    if len(data) > MAX_ARCHIVE_BYTES:
        raise ValueError("oversize evidence archive")
    if artifact.get("digest") != "sha256:" + hashlib.sha256(data).hexdigest():
        raise ValueError("evidence archive digest mismatch")
    with zipfile.ZipFile(io.BytesIO(data)) as archive:
        if archive.namelist() != ["evidence.json"]:
            raise ValueError("unexpected evidence archive entries")
        if archive.getinfo("evidence.json").file_size > MAX_ARCHIVE_BYTES:
            raise ValueError("oversize evidence document")
        proof = json.loads(archive.read("evidence.json"))
        if not isinstance(proof, dict):
            raise ValueError("evidence must be an object")
        return proof


def timestamp(value):
    return datetime.fromisoformat(value.replace("Z", "+00:00"))


def eligible_run(run, repository, now):
    return (run.get("path") == WORKFLOW and run.get("conclusion") == "success"
            and run.get("status") == "completed"
            and run.get("event") in {"pull_request", "push", "workflow_dispatch"}
            and run.get("repository", {}).get("full_name") == repository
            and run.get("head_repository", {}).get("full_name") == repository
            and 0 <= (now - timestamp(run["updated_at"])).total_seconds() <= MAX_AGE_SECONDS)


def completed_gates(jobs, artifact, now):
    for job in jobs:
        if job.get("name") != "Format, test, lint, and audit" or job.get("conclusion") != "success":
            continue
        if not 0 <= (now - timestamp(job["completed_at"])).total_seconds() <= MAX_AGE_SECONDS:
            continue
        if not timestamp(job["started_at"]) <= timestamp(artifact["created_at"]) <= timestamp(job["completed_at"]):
            continue
        passed = {step["name"] for step in job.get("steps", []) if step.get("conclusion") == "success"}
        if set(GATES) <= passed:
            return True
    return False


def matching_evidence(proof, run, repository, digest, runner, advisory):
    return (proof.get("schema") == 1 and proof.get("repository") == repository
            and proof.get("run_id") == run["id"] and proof.get("run_attempt") == run["run_attempt"]
            and proof.get("head_sha") == run["head_sha"]
            and re.fullmatch(r"[0-9a-f]{40}", proof.get("tested_sha", "")) is not None
            and proof.get("input_sha256") == digest and proof.get("runner") == runner
            and proof.get("advisory_revision") == advisory)


def find_evidence(repository, digest, runner, advisory, now, request=api):
    prefix = f"repos/{repository}"
    runs = request(f"{prefix}/actions/workflows/ci.yml/runs?status=success&per_page=20")["workflow_runs"]
    for run in runs:
        if not eligible_run(run, repository, now):
            continue
        artifacts = request(f"{prefix}/actions/runs/{run['id']}/artifacts")["artifacts"]
        for artifact in artifacts:
            if (artifact.get("name") != f"rust-validation-{run['run_attempt']}"
                    or artifact.get("expired") or artifact["size_in_bytes"] > MAX_ARCHIVE_BYTES):
                continue
            try:
                proof = archive_evidence(request(f"{prefix}/actions/artifacts/{artifact['id']}/zip", binary=True), artifact)
                if not matching_evidence(proof, run, repository, digest, runner, advisory):
                    continue
                jobs = request(f"{prefix}/actions/runs/{run['id']}/attempts/{run['run_attempt']}/jobs")["jobs"]
                if not completed_gates(jobs, artifact, now):
                    continue
                # Independently compare the producer's source/policy, not just its
                # claimed hash. A PR merge tree is recorded separately in proof;
                # base-only differences conservatively force a new full run.
                matched = True
                for sha in dict.fromkeys((run["head_sha"], proof["tested_sha"])):
                    tree = request(f"{prefix}/git/trees/{sha}?recursive=1")
                    entries = [(e["path"], e["mode"], e["type"], e["sha"])
                               for e in tree["tree"] if e["type"] != "tree"]
                    if tree.get("truncated") or fingerprint(entries) != digest:
                        matched = False
                        break
                if matched:
                    return f"https://github.com/{repository}/actions/runs/{run['id']}"
            except (ValueError, KeyError, TypeError, zipfile.BadZipFile):
                continue
    return None


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("mode", choices=("scope", "reuse", "record"))
    args = parser.parse_args()
    if args.mode == "scope":
        event = json.loads(Path(os.environ["GITHUB_EVENT_PATH"]).read_text())
        base = (event.get("pull_request", {}).get("base", {}).get("sha")
                if os.environ.get("GITHUB_EVENT_NAME") == "pull_request" else event.get("before"))
        event_name = os.environ.get("GITHUB_EVENT_NAME")
        selected = changed_scopes(base, "HEAD", event_name == "workflow_dispatch", event_name)
        emit({"rust": "rust" in selected, "site": "site" in selected})
    elif args.mode == "reuse":
        source = None
        if os.environ.get("GITHUB_EVENT_NAME") != "workflow_dispatch":
            try:
                source = find_evidence(os.environ["GITHUB_REPOSITORY"], tree_fingerprint(),
                                       runner_identity(), current_advisory_revision(),
                                       datetime.now(timezone.utc))
            except Exception:
                # Reuse is an optimization, never a reason to skip or block the
                # real gate. Treat all unavailable/unrecognized evidence alike.
                print("Reusable validation unavailable; running the full gate.", file=sys.stderr)
        emit({"reused": source is not None})
        if source:
            print(f"Reusing full validation: {source}")
            with open(os.environ["GITHUB_STEP_SUMMARY"], "a") as summary:
                summary.write(f"Rust validation reused from {source}: identical inputs, runner image and RustSec revision; age <24h.\n")
    else:
        command("git", "diff", "--exit-code", "HEAD", "--")
        advisory_dir = Path(os.environ.get("CARGO_HOME", Path.home() / ".cargo")) / "advisory-db"
        proof = {"schema": 1, "repository": os.environ["GITHUB_REPOSITORY"],
                 "run_id": int(os.environ["GITHUB_RUN_ID"]), "run_attempt": int(os.environ["GITHUB_RUN_ATTEMPT"]),
                 "head_sha": os.environ["CI_HEAD_SHA"], "tested_sha": command("git", "rev-parse", "HEAD").decode().strip(),
                 "input_sha256": tree_fingerprint(), "runner": runner_identity(),
                 "advisory_revision": command("git", "-C", str(advisory_dir), "rev-parse", "HEAD").decode().strip()}
        output = Path(os.environ["RUNNER_TEMP"]) / "rust-validation"
        output.mkdir(exist_ok=True)
        (output / "evidence.json").write_text(json.dumps(proof) + "\n")


if __name__ == "__main__":
    main()

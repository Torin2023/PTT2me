# Known Non-Blocking Debt Remediation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fix the verified updater quarantine defect, remove the known site dependency vulnerabilities, and eliminate the deprecated GitHub Pages action warning.

**Architecture:** Keep update digest and path verification fail-closed, but attach and re-read quarantine on the verified DMG descriptor before promotion. Keep Vinext, update its dependency family as one compatible set, and teach the static exporter about the new `_next` and `.vinext` asset roots. Upgrade Pages actions by immutable commit SHA only.

**Tech Stack:** Rust, macOS xattrs, Vinext/Next.js/React/Vite, Node.js tests, GitHub Actions.

**Spec:** `docs/superpowers/specs/2026-09-04-known-debt-remediation-design.md`

## Global Constraints

- Support only Apple Silicon macOS 13+ for the application.
- Do not weaken signed manifest, SHA-256, size, path, symlink, or quarantine checks.
- Do not modify `updates/releases/`, `updates/channels/stable.json`, `updates/public-key.txt`, model manifests, DMGs, or signing material in this branch.
- Keep current public 1.1.1 site facts until the separate 1.1.2 release task.
- Pin third-party GitHub Actions to full commit SHAs.

---

### Task 1: Quarantine verified downloads

**Files:**
- Modify: `src/updater.rs`
- Test: `src/updater/reducer_tests.rs`

**Interfaces:**
- Consumes: the verified partial DMG descriptor inside `cache_verified_download_with_promoter`.
- Produces: `set_descriptor_quarantine(fd: RawFd) -> Result<(), UpdateFailure>` and a promoted DMG whose descriptor reports quarantine.

- [ ] **Step 1: Write the failing regression test**

Add a macOS test that downloads the signed fixture through
`cache_verified_download`, opens the resulting file, and asserts
`descriptor_has_quarantine(file.as_raw_fd()) == Ok(true)`.

- [ ] **Step 2: Verify RED**

Run: `cargo test --features test-support updater::reducer_tests::verified_download_receives_quarantine_before_promotion -- --exact --test-threads=1`

Expected: FAIL because the raw downloaded fixture has no quarantine xattr.

- [ ] **Step 3: Implement the minimal descriptor-based fix**

After `verify_artifact` succeeds on the partial file, call `libc::fsetxattr`
with `com.apple.quarantine` and a standard PTT2me quarantine value. Re-read it
with `descriptor_has_quarantine` and return `QuarantineMissing` if verification
does not succeed. Only then promote and reverify the final file.

- [ ] **Step 4: Verify GREEN and updater regressions**

Run the exact regression test, then all updater reducer tests with one test
thread.

### Task 2: Upgrade and slim the product site

**Files:**
- Modify: `site/package.json`
- Modify: `site/package-lock.json`
- Modify: `site/scripts/export-github-pages.mjs`
- Modify: `site/tests/github-pages-export.test.mjs`
- Modify: `site/build/sites-vite-plugin.ts`
- Delete: `site/db/index.ts`
- Delete: `site/db/schema.ts`
- Delete: `site/drizzle.config.ts`
- Delete: `site/drizzle/meta/_journal.json`

**Interfaces:**
- Consumes: Vinext output at `dist/client/_next/static`, including `_vinext_fonts`.
- Produces: a server-free `pages-dist` whose asset URLs are rooted at `/PTT2me`.

- [ ] **Step 1: Write exporter expectations for the new layout**

Extend `rewritePageHtml` tests with root-relative `/_next/static/...` and
`/.vinext/fonts/...` URLs. Change the build contract to inspect JavaScript
under `dist/client/_next/static` and the exported contract to require `_next`
and `.vinext` content.

- [ ] **Step 2: Verify RED against the old exporter/build**

Run the focused Node test and confirm the new root paths are not rewritten.

- [ ] **Step 3: Upgrade the compatible dependency family**

Use Next 16.3.4, React/React DOM/RSC 19.2.8, Vinext 1.0.0-beta.9, Vite 8.2.2,
Cloudflare Vite plugin 1.54.4, Vite RSC plugin 0.5.34, Vite React plugin 6.1.1,
Wrangler 4.129.0, and Tailwind 4.3.3. Remove Drizzle packages and scripts.

- [ ] **Step 4: Adapt exporter and remove unused starter database code**

Rewrite both `/_next/` and `/.vinext/` asset roots to the GitHub Pages base
path, keep public absolute URLs intact, and stop packaging absent Drizzle
migrations.

- [ ] **Step 5: Verify site**

Run `npm test`, `npm run lint`, `npm run export:pages`, and `npm audit` from
`site/`. Expected: tests and lint pass, export contains no server bundle or
localhost URLs, and audit reports zero vulnerabilities.

### Task 3: Upgrade pinned Pages actions

**Files:**
- Modify: `.github/workflows/pages.yml`
- Test: `tests/release_ci_contracts.sh`

**Interfaces:**
- Consumes: the existing assembled `pages-artifact` directory.
- Produces: upload with `actions/upload-pages-artifact` v5 and deploy with `actions/deploy-pages` v5, both pinned by SHA.

- [ ] **Step 1: Update the CI contract test first**

Require upload SHA `fc324d3547104276b827a68afc52ff2a11cc49c9` and deploy SHA
`368f82528645a54fb793d4d04e342629a3f51346`.

- [ ] **Step 2: Verify RED**

Run `bash tests/release_ci_contracts.sh`; expected failure names the old action
pin.

- [ ] **Step 3: Update workflow pins and comments**

Replace only the two action references and label both as v5.

- [ ] **Step 4: Verify GREEN**

Run the CI contract and shell contract suites.

### Task 4: Full verification and review handoff

**Files:**
- Review all changed files only.

**Interfaces:**
- Consumes: Tasks 1-3.
- Produces: one reviewable remediation PR; release artifacts remain untouched.

- [ ] **Step 1: Run application gate**

Run formatting, all tests with one thread, Clippy with warnings denied, and
RustSec audit without fetching.

- [ ] **Step 2: Run site and release-contract gates**

Repeat site test/lint/audit/export and all relevant shell contract tests.

- [ ] **Step 3: Inspect repository state**

Confirm the diff contains no model, immutable release record, generated build,
DMG, secret, or temporary file.

- [ ] **Step 4: Commit, push, and create the remediation PR**

Use the repository PR template and report macOS/AppKit limitations precisely.

# Site dependency refresh (2026-11-03)

This change is stacked on PR #62, head `059d4e41f8239a3b2830d8b906dc5e103bcf053e`.
It changes the site toolchain only; it does not publish Pages or an application release.

## Coordinated versions

- Next and eslint-config-next: 16.4.5.
- React, React DOM and React Server DOM Webpack: 19.3.1.
- Vite: 8.3.5; vinext: 1.4.2; RSC plugin: 0.5.38.
- Cloudflare Vite plugin: 1.67.0; Wrangler: 4.157.0.
- Renderer Miniflare: 5.20261101.0-beta, matching the Vite plugin's nested
  Wrangler 4.156.1. Direct Wrangler 4.157.0 separately pins Miniflare
  5.20261103.0-beta; the lockfile retains both runtime versions.
- fflate: 0.7.5, the patched 0.7-series release. A scoped `satori` override
  replaces its exact 0.7.3 pin; `@shuding/opentype.js` already accepts ^0.7.3.

Versions and peer constraints were checked against the npm registry. The lockfile
was resolved normally, without `--force`, `--legacy-peer-deps`, or major-version
overrides. Compatible transitive fixes were refreshed in the lockfile.

New vinext output imports native `cloudflare:` modules. The static exporter and
rendered HTML tests now execute the production bundle in local workerd using
Miniflare rather than importing it into Node. The helper reads the build's
compatibility settings and consumes the response before disposing the process.
It does not configure account credentials, remote bindings, or deployment.
Telemetry, the development registry, and the default Cloudflare `cf.json`
lookup/cache are disabled (`cf: false`). Miniflare otherwise still allows worker
outbound fetch; this helper is not a general network sandbox. The current page
does not make those fetches. The static Pages output
continues to exclude JavaScript and the server bundle.

## Automated evidence

Linux cloud checkout; Node 24.22.1, npm 11.9.0:

- Clean `npm ci --ignore-scripts --no-audit` succeeds.
- `GITHUB_PAGES_BASE_PATH=/PTT2me npm test`: build succeeds, 8/8 tests pass,
  no skipped tests.
- `npm run lint`: succeeds.
- `GITHUB_PAGES_BASE_PATH=/PTT2me npm run export:pages`: succeeds.
- `git diff --check`: succeeds.

The default-path build also passed; its conditional project-path assertion was
skipped as designed. The project-path build above exercises that assertion.
No manual testing was introduced. Native application checks are unaffected.

Follow-up on 2026-11-04, Apple Silicon macOS, Node 25.9.0/npm 11.12.1:

- Clean `npm ci --ignore-scripts --no-audit` succeeds with the fflate 0.7.5 lock.
- Project-path `npm test`: production build and 8/8 tests pass, zero skips.
- After adding `cf: false`, the same eight HTML tests and static export pass
  against that unchanged production build; no redundant rebuild was needed.
- `npm audit --package-lock-only --ignore-scripts --json` reports 16 moderate,
  zero high/critical entries. Its nonzero exit is retained, not relabelled clean.
- The fflate update changes only its version, tarball URL and integrity in the
  lockfile. No forced install or major-version override was used.

## Remaining audit risk

A fresh npm audit on the exact PR #62 lockfile reported 47 affected package
entries: 1 critical, 18 high, 26 moderate, 2 low. The initial refresh reduced
this to 20 moderate entries. The 2026-11-04 fflate patch reduces it further to
**16 moderate entries, zero high and zero critical**. These are affected package
counts, not counts of distinct vulnerabilities; the audit is not clean.

One root advisory remains:

- [minimatch globparts amplification](https://github.com/advisories/GHSA-g3hv-75wg-m249):
  the published fix is 11.0.1. Lint dependency chains still require minimatch
  ^3.1.x or ^10.2.2. A global override would cross those compatibility ranges
  and is not applied. These are lint/build dependencies, not shipped JavaScript.

The [fflate ZIP64 advisory](https://github.com/advisories/GHSA-px8p-9vwx-vf98)
is fixed by the published 0.7.5 backport; it is absent from the follow-up audit.
The scoped override is needed because current satori 0.63.2 pins fflate 0.7.3
exactly. It can be removed when upstream accepts the compatible patched version.

The exported Pages artifact contains static HTML, CSS and assets, not an exposed
Next/Worker server or lint process. This reduces deployment exposure but does
not remove build-time dependency risk. Do not feed untrusted archives or glob
patterns into these toolchains. Track compatible upstream repairs separately.
The current Wrangler stack includes a prerelease Miniflare runtime; retain the
pinned lockfile and require the automated export checks on future upgrades.

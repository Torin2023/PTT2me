# Site dependency refresh (2026-11-03)

This change is stacked on PR #62, head `059d4e41f8239a3b2830d8b906dc5e103bcf053e`.
It changes the site toolchain only; it does not publish Pages or an application release.

## Coordinated versions

- Next and eslint-config-next: 16.4.5.
- React, React DOM and React Server DOM Webpack: 19.3.1.
- Vite: 8.3.5; vinext: 1.4.2; RSC plugin: 0.5.38.
- Cloudflare Vite plugin: 1.67.0; Wrangler: 4.157.0.
- Miniflare: 5.20261101.0-beta, explicitly matching Wrangler's runtime dependency.

Versions and peer constraints were checked against the npm registry. The lockfile
was resolved normally, without `--force`, `--legacy-peer-deps`, or major-version
overrides. Compatible transitive fixes were refreshed in the lockfile.

New vinext output imports native `cloudflare:` modules. The static exporter and
rendered HTML tests now execute the production bundle in local workerd using
Miniflare rather than importing it into Node. The helper reads the build's
compatibility settings and consumes the response before disposing the process.
It does not configure account credentials, remote bindings, or deployment.
Telemetry and the development registry are disabled. The static Pages output
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

## Remaining audit risk

A fresh npm audit on the exact PR #62 lockfile reported 47 affected package
entries: 1 critical, 18 high, 26 moderate, 2 low. After this refresh, the audit
reports **20 moderate entries, zero high and zero critical**. These are affected
package counts, not counts of distinct vulnerabilities; the audit is not clean.

Two root advisories account for the remaining dependency chains:

- [minimatch globparts amplification](https://github.com/advisories/GHSA-g3hv-75wg-m249):
  lint tooling still consumes older minimatch majors. A global override to
  minimatch 11 would cross upstream compatibility ranges and is not applied.
- [fflate malformed ZIP64 infinite loop](https://github.com/advisories/GHSA-px8p-9vwx-vf98):
  the satori / @vercel/og / vinext chain retains fflate 0.7.x. No forced override
  or claimed upstream fix is applied.

The exported Pages artifact contains static HTML, CSS and assets, not an exposed
Next/Worker server or lint process. This reduces deployment exposure but does
not remove build-time dependency risk. Do not feed untrusted archives or glob
patterns into these toolchains. Track compatible upstream repairs separately.
The current Wrangler stack includes a prerelease Miniflare runtime; retain the
pinned lockfile and require the automated export checks on future upgrades.

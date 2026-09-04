# Known Non-Blocking Debt Remediation Design

## Goal

Remove the currently known updater, product-site dependency, and GitHub Pages
workflow debt without weakening update verification or changing the published
1.1.1 release records.

## Updater

The downloader already verifies the signed descriptor, exact body size, and
SHA-256 digest before promoting a DMG into the cache. Raw HTTPS downloads do
not receive `com.apple.quarantine` automatically, so the subsequent fail-closed
check rejects and deletes an otherwise valid artifact.

After the content check succeeds, PTT2me will attach a standard quarantine
value to the verified file through its open file descriptor and immediately
verify the attribute through that same descriptor. Promotion and the existing
path/descriptor checks remain unchanged. Cached files that predate this change
and lack quarantine continue to fail closed; only a newly downloaded and
verified file is allowed to receive the attribute.

## Product site

Keep the current Vinext implementation, upgrade the Next/React/Vite/Cloudflare
stack to versions with a clean production audit, and remove unused Drizzle
starter files and dependencies. Adapt the GitHub Pages exporter to Vinext's
new client layout (`_next/static`) and its emitted `.vinext/fonts` paths. Static
output must remain scoped to `/PTT2me`, contain no server bundle, and retain the
current published 1.1.1 product copy until a separate 1.1.2 release change.

## GitHub Pages workflow

Replace the deprecated Pages upload and deployment actions with reviewed,
commit-SHA-pinned v5 releases. Keep the existing permissions, validation job,
artifact assembly, and deployment conditions unchanged.

## Release boundary

This remediation branch does not alter immutable release records, the stable
channel, model manifests, version tags, or DMG assets. After the remediation PR
is merged and CI passes, 1.1.2 is prepared from that exact merge commit in a
separate release branch. Because 1.1.0 and 1.1.1 cannot complete an in-app
download, the 1.1.2 publication must include a Full DMG manual-upgrade bridge
on the site.

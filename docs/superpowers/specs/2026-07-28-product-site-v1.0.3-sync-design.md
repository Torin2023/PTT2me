# PTT2me Product Site v1.0.3 Sync Design

## Goal

Update the existing public PTT2me landing page so every product and release
claim matches Git tag `v1.0.3`, the current GitHub prerelease, and the behavior
documented on `origin/main`.

The update preserves the approved demo-first page structure, visual system,
headline, and primary download action. It does not redesign the site or add a
changelog section.

## Source of Truth

The update uses these current sources:

- Git tag and `origin/main`: `v1.0.3` at commit `11df0f4`;
- package version: `1.0.3`;
- GitHub release: `PTT2me 1.0.3 — unsigned preview`;
- DMG:
  `https://github.com/Torin2023/PTT2me/releases/download/v1.0.3/PTT2me-1.0.3-macos-arm64.dmg`;
- DMG size: `191283969` bytes, displayed as approximately `182 МБ`;
- SHA-256:
  `513ddace2ca4b8d8bc9f9e5da099cc238ea6476f559d376605b80c76a267e2f4`.

The release is public, marked as a prerelease, ad-hoc signed, and not notarized.
The site must describe it as a preview build and must retain the existing
Gatekeeper guidance.

## Product Copy Changes

### Hero

Keep the headline «Говорите — текст уже там» and the existing Fn/waveform
demonstration. Add a visible `Preview 1.0.3` marker near the product eyebrow or
compatibility line.

Change the supporting copy to say that PTT2me inserts the recognized result
into the editable field that owns the cursor when recognition finishes. The
primary CTA downloads the v1.0.3 DMG.

### Value Highlights

Keep the three-card structure:

1. `Без облака` continues to explain local GigaAM v3 recognition.
2. `Одна клавиша` explains that a short Fn/Globe press keeps its configured
   macOS system action, while a hold starts dictation.
3. `Точно в курсор` replaces the generic `В любом приложении` claim. It
   explains that insertion targets the field focused when recognition
   completes.

Avoid claiming universal compatibility. The release has multiple insertion
paths, but protected password fields are intentionally rejected.

### How It Works

Keep the hold, speak, and release sequence. Add one concise note that a press
shorter than 250 ms is replayed to macOS, preserving input-source switching or
another configured system action.

Keep the current 250 ms hold threshold, 180 ms release tail, and 25-second
maximum capture facts.

### Privacy and Insertion

Update the privacy copy to reflect the new insertion order:

1. focused Accessibility selected-text insertion;
2. direct Unicode keyboard insertion;
3. Command-V compatibility fallback only when needed.

State that the first two paths do not modify the pasteboard. The compatibility
fallback preserves all previous pasteboard items and representations, restores
them after insertion, and never overwrites newer pasteboard contents.

The page must not promise which insertion path a specific third-party
application will use.

### Permissions

Keep the three required permissions: Microphone, Input Monitoring, and
Accessibility. Add a short note that, while a permission is missing, the app
offers `Открыть настройки…` and can reopen the exact relevant Privacy &
Security pane.

### Installation and Release Details

Update every visible version, release URL, download URL, and checksum to
v1.0.3. Keep the displayed download size at `182 МБ`.

Label the release as an unsigned preview before the download action. Keep the
installation sequence and explain that macOS may require opening the app
through the contextual `Открыть` command or confirming it in Privacy &
Security.

The footer links to the v1.0.3 release and its published DMG. The release does
not currently include a separate `.sha256` asset, so the site must not link to
a nonexistent checksum file. It displays the verified digest as text instead.

## Metadata and Social Preview

Update the metadata description so it refers to insertion at the focused field
and preserves the local-recognition message.

The existing social card remains valid because the product name, main
headline, palette, and Fn/waveform motif are unchanged. Do not regenerate it
for this factual release sync.

## Technical Scope

The existing `site/` vinext project, package manager, lockfile, single route,
global stylesheet, Sites project ID, and public access mode remain unchanged.

Expected implementation changes:

- `site/app/page.tsx`: release constants and product copy;
- `site/app/globals.css`: only the minimal styling required for the preview
  marker or revised content;
- `site/app/layout.tsx`: description metadata;
- `site/tests/rendered-html.test.mjs`: v1.0.3 contract.

Before editing the site, bring the product-site branch up to date with
`origin/main` without rewriting public history. Resolve only actual merge
conflicts and preserve the existing site files.

## Validation and Publication

The rendered HTML contract must verify:

- `Preview 1.0.3`;
- the v1.0.3 DMG URL;
- the v1.0.3 release URL;
- the new SHA-256;
- the focused-field insertion claim;
- the short-Fn system-action claim;
- absence of the v1.0.2 DMG URL and checksum.

Run the existing site production build, rendered HTML tests, and lint. Package
and publish only the exact validated commit as the next version of the existing
public Sites project. Verify that the production deployment succeeds and that
the site remains public.

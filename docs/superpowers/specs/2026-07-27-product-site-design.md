# PTT2me Product Site Design

## Goal

Create and publish a Russian-language product landing page that explains
PTT2me in one glance and leads Apple Silicon Mac users to download the current
DMG release.

The primary success criterion is a clear path from the first viewport to the
official PTT2me 1.0.2 GitHub release asset. The page must accurately describe
the current application without implying cloud services, Intel support,
notarization, or features the app does not have.

## Audience

The primary audience is a Russian-speaking Apple Silicon Mac user who writes
text in multiple applications and wants a fast, private alternative to typing.
The visitor may not know what push-to-talk or offline speech recognition means,
so the page explains the interaction through a concrete gesture:

1. Hold Fn/Globe.
2. Speak.
3. Release the key and continue working with the pasted text.

## Product Positioning

The core promise is: **«Говорите — текст уже там».**

PTT2me is positioned as a focused macOS utility, not a general transcription
service. The supporting message emphasizes three differentiators:

- Russian speech recognition runs locally with the bundled GigaAM v3 model.
- The app is always available from the menu bar and is controlled with one key.
- Audio, transcripts, history, settings, and application data are not retained
  by PTT2me.

## Page Structure

### Header

A compact header contains the PTT2me wordmark, an anchor link to the workflow,
a link to the GitHub repository, and a small download button. The header remains
visually quiet so the hero remains the focal point.

### Hero

The hero fills most of the first viewport and contains:

- the headline «Говорите — текст уже там»;
- concise supporting copy explaining local Russian dictation with Fn/Globe;
- a primary download button for PTT2me 1.0.2;
- a secondary GitHub link;
- a compatibility line: Apple Silicon, macOS 13+, 182 MB, local recognition;
- a product demonstration composed from CSS and typography.

The demonstration shows an Fn key, a restrained waveform, and a sample Russian
phrase appearing in a text surface. It may animate only as a visual explanation;
it is not an audio recorder and must not request microphone access.

### Value Highlights

Three compact highlights communicate:

- «Без облака» — recognition stays on the Mac;
- «Одна клавиша» — hold Fn/Globe to dictate;
- «В любом приложении» — the non-empty result is pasted into the frontmost app.

The pasteboard-preservation behavior may be mentioned in supporting copy:
PTT2me restores the previous pasteboard contents unless newer contents were
copied during insertion.

### How It Works

A three-step section explains hold, speak, and release. It uses the actual
timing behavior only where helpful:

- a hold shorter than 250 ms is not treated as dictation;
- recording receives a 180 ms tail after release;
- a capture ends automatically after 25 seconds.

The main presentation stays simple; the timings appear as secondary detail.

### Privacy

A dedicated section states exactly what the application does and does not do:

- speech recognition is performed locally;
- audio and recognized text are not saved;
- there is no transcript history, account, analytics, or cloud processing.

The page itself also uses no analytics, account system, or lead form.

### Requirements and Permissions

The requirements section lists Apple Silicon and macOS 13 Ventura or newer. It
explains why PTT2me needs exactly three macOS permissions:

- Microphone, to capture speech;
- Input Monitoring, to detect Fn/Globe;
- Accessibility, to paste recognized text into the frontmost application.

### Installation

The installation section provides a short, honest sequence:

1. Download and open the DMG.
2. Drag PTT2me to Applications.
3. Launch the app and grant the three required permissions.

The current release is ad-hoc signed and not notarized. The page must disclose
that before download and provide a concise macOS opening instruction without
suggesting that Gatekeeper be disabled globally.

### Release Details and Footer

The release area includes:

- version 1.0.2;
- the DMG size, shown as approximately 182 MB;
- SHA-256:
  `1119711c9fee89218d816fb9eb4a03c138c790a51b3a0792970f0c6c17016f53`;
- links to the checksum asset, release notes, and GitHub repository.

The primary download URL is:

`https://github.com/Torin2023/PTT2me/releases/download/v1.0.2/PTT2me-1.0.2-macos-arm64.dmg`

## Visual Direction

The site uses a dark, native-macOS-inspired palette with a warm red accent that
echoes the application's critical status icon. Large, precise typography and
generous spacing make the product feel like a small, trustworthy utility rather
than a startup dashboard.

The visual system relies on typography, borders, subtle gradients, CSS shapes,
and existing icon components. It uses no stock photography and no authored SVG
illustrations. Motion is limited to the hero demonstration and subtle entrance
transitions. Visitors who prefer reduced motion receive a static equivalent.

## Responsive and Accessible Behavior

The page supports desktop, tablet, and narrow mobile layouts. The hero
demonstration stacks below the copy on small screens. Tap targets remain at
least 44 pixels high.

All interactive elements have visible focus states and accessible names. The
document follows a logical heading order, maintains sufficient contrast, and
does not rely on color or animation alone to communicate state. Keyboard users
can reach every link and button.

## Technical Shape

The site is a single route implemented with the Sites starter's existing
vinext structure. It uses one page component and one global stylesheet unless a
small isolated client component is required for the demonstration. There is no
durable data, authentication, upload, external connector, or app-owned API.

The download points directly to the official GitHub release asset rather than
bundling the 182 MB DMG into the site deployment. GitHub links are normal
external links; no GitHub data is fetched at runtime.

The finished site includes product-specific title, description, Open Graph, and
X metadata. A single bespoke social card reuses the final headline, palette,
and Fn/waveform motif.

## Validation

Before publishing:

- the production build must succeed;
- starter preview content and metadata must be removed;
- the primary and secondary links must use the approved GitHub URLs;
- the page must remain usable with motion disabled;
- the page must have no microphone prompt, form submission, or analytics;
- the deployed version must be the exact validated source.

Browser interaction testing is outside this first pass unless explicitly
requested.

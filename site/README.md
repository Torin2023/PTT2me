# PTT2me product site

The PTT2me product site, built with
[vinext](https://github.com/cloudflare/vinext) and exported to GitHub Pages.

## Prerequisites

- Node.js `>=22.13.0`

## Quick Start

```bash
npm install
npm run dev
npm run build
```

## Project Shape

- edit site code under `app/`
- `.openai/hosting.json` identifies the existing Sites project
- `vite.config.ts` builds the Vinext and Cloudflare worker targets
- `scripts/export-github-pages.mjs` produces the static Pages artifact

## Useful Commands

- `npm run dev`: start local development
- `npm run build`: verify the vinext build output
- `npm test`: build the site and verify the rendered product and Pages export
- `npm run export:pages`: write the static site to `pages-dist/`

## Learn More

- [vinext Documentation](https://github.com/cloudflare/vinext)

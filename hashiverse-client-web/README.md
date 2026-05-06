# Hashiverse client app

This is the hashiverse client.

## Builds

- [![build-app](https://github.com/hashiverse/hashiverse/actions/workflows/build-app.yml/badge.svg)](https://github.com/hashiverse/hashiverse/actions/workflows/build-app.yml)

## Checks

- [![check-hashiverse-client-web](https://github.com/hashiverse/hashiverse/actions/workflows/check-hashiverse-client-web.yml/badge.svg)](https://github.com/hashiverse/hashiverse/actions/workflows/check-hashiverse-client-web.yml) — runs on every PR touching `hashiverse-client-web/` or its WASM dependency. Performs `npm run check:ci` (biome lint, no fixes) and `npm run build` (TypeScript type-check + rsbuild production build).

## Translations

- [![check-translations-app](https://github.com/hashiverse/hashiverse/actions/workflows/check-translations-app.yml/badge.svg)](https://github.com/hashiverse/hashiverse/actions/workflows/check-translations-app.yml)

When the badge above is red, run `node hashiverse-client-web/translations/check-translations.mjs` from the repo root and paste the JSON output into a Claude Code session — the `prompt` field in the output instructs Claude on how to update the translations and the state file.

## Get started

- Install dependencies with `npm install` 
- Start the dev server with `npm run dev`
- Build the app for production with `npm run build`
- Preview the production build locally with `npm run preview`
- Lint and auto-fix with `npm run check`; for a read-only CI-style check use `npm run check:ci`

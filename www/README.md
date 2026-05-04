# Hashiverse website

## Builds

- [![build-www](https://github.com/hashiverse/hashiverse/actions/workflows/build-www.yml/badge.svg)](https://github.com/hashiverse/hashiverse/actions/workflows/build-www.yml)

## Translations

- [![check-translations-www](https://github.com/hashiverse/hashiverse/actions/workflows/check-translations-www.yml/badge.svg)](https://github.com/hashiverse/hashiverse/actions/workflows/check-translations-www.yml)

When the badge above is red, run `node www/translations/check-translations.mjs` from the repo root and paste the JSON output into a Claude Code session — the `prompt` field in the output instructs Claude on how to update the translations and the state file.

## Get started

- Install dependencies with `npm install`
- Start the dev server with `npm run dev`
- Build the site for production with `npm run build`
- Preview the production build locally with `npm run preview`
- Full build (including rustdoc and tsdoc dependencies) with `npm run build:full`

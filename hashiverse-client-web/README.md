# Hashiverse client app

This is the hashiverse client.

## Builds

- [![build-app](https://github.com/hashiverse/hashiverse/actions/workflows/build-app.yml/badge.svg)](https://github.com/hashiverse/hashiverse/actions/workflows/build-app.yml)

## Translations

- [![check-translations-app](https://github.com/hashiverse/hashiverse/actions/workflows/check-translations-app.yml/badge.svg)](https://github.com/hashiverse/hashiverse/actions/workflows/check-translations-app.yml)

When the badge above is red, run `node hashiverse-client-web/translations/check-translations.mjs` from the repo root and paste the JSON output into a Claude Code session — the `prompt` field in the output instructs Claude on how to update the translations and the state file.

## Get started

- Install dependencies with `npm install` 
- Start the dev server with `npm run dev`
- Build the app for production with `npm run build`
- Preview the production build locally with `npm run preview`

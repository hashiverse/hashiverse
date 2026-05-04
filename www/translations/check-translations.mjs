#!/usr/bin/env node
import fs from "node:fs";
import path from "node:path";
import crypto from "node:crypto";
import { fileURLToPath } from "node:url";

const here = fileURLToPath(import.meta.url);
const www_root = path.resolve(path.dirname(here), "..");

const translations_dir = path.join(www_root, "translations");
const pages_en_dir = path.join(www_root, "src", "pages", "en");
const i18n_dir = path.join(www_root, "src", "i18n");
const en_strings = path.join(i18n_dir, "en.json");

const state_filename_re = /^state-([a-z]{2,})\.json$/;
const langs = fs
  .readdirSync(translations_dir)
  .map((f) => f.match(state_filename_re))
  .filter(Boolean)
  .map((m) => m[1])
  .sort();
const state_path_for = (lang) => path.join(translations_dir, `state-${lang}.json`);

const sha256_prefix = (buf) => crypto.createHash("sha256").update(buf).digest("hex").slice(0, 4);

function read_json_or_empty(p) {
  if (!fs.existsSync(p)) return {};
  return JSON.parse(fs.readFileSync(p, "utf8"));
}

function walk_astro(dir, out = []) {
  if (!fs.existsSync(dir)) return out;
  for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
    const full = path.join(dir, entry.name);
    if (entry.isDirectory()) walk_astro(full, out);
    else if (entry.name.endsWith(".astro")) out.push(full);
  }
  return out;
}

const rel_posix = (p) => path.relative(www_root, p).split(path.sep).join("/");

const sources = {};
for (const f of walk_astro(pages_en_dir)) {
  sources[rel_posix(f)] = sha256_prefix(fs.readFileSync(f));
}
if (fs.existsSync(en_strings)) {
  sources[rel_posix(en_strings)] = sha256_prefix(fs.readFileSync(en_strings));
}

const actions_required = {};
let total_stale = 0;
let total_missing = 0;
let total_orphaned = 0;

for (const lang of langs) {
  const lang_state = read_json_or_empty(state_path_for(lang));
  const out = { translate: [], create: [], delete: [] };

  for (const [src_path, current_hash] of Object.entries(sources)) {
    const recorded = lang_state[src_path];
    const translated_path = src_path
      .replace(/^src\/pages\/en\//, `src/pages/${lang}/`)
      .replace(/^src\/i18n\/en\.json$/, `src/i18n/${lang}.json`);
    const translated_exists = fs.existsSync(path.join(www_root, translated_path));

    if (!recorded) {
      if (translated_exists) {
        out.translate.push(src_path);
        total_stale++;
      } else {
        out.create.push(src_path);
        total_missing++;
      }
    } else if (recorded !== current_hash) {
      out.translate.push(src_path);
      total_stale++;
    }
  }

  const lang_pages_dir = path.join(www_root, "src", "pages", lang);
  for (const f of walk_astro(lang_pages_dir)) {
    const rel = rel_posix(f);
    const src_equiv = rel.replace(`src/pages/${lang}/`, "src/pages/en/");
    if (!sources[src_equiv]) {
      out.delete.push(rel);
      total_orphaned++;
    }
  }

  actions_required[lang] = out;
}

const summary = {
  total: Object.keys(sources).length,
  languages: langs.length,
  stale: total_stale,
  missing: total_missing,
  orphaned: total_orphaned,
};

const prompt =
  "You are updating translations for the hashiverse www/ site. " +
  "Run `node www/translations/check-translations.mjs` from the repo root to (re)generate this JSON — its stdout is exactly the {prompt, summary, actions_required} structure you are reading, with a fresh actions_required block. Exit code is 1 while any work remains and 0 once everything is registered fresh. " +
  "For every entry under actions_required, do the following based on the source path: " +
  "(a) for `src/pages/en/**/*.astro` — read the English file, translate into the target language preserving imports, JSX tags, class names, and code blocks, write to `src/pages/<lang>/<same-relative-path>` (swap `/en/` for `/<lang>/`). " +
  "(b) for `src/i18n/en.json` — read it, translate each string value into the target language, write to `src/i18n/<lang>.json`. " +
  "Do NOT translate the product name (Hashiverse) or technical/cryptographic terms (e.g. Kademlia, Ed25519, P2P, hashtag). " +
  "After translating, also delete any files listed under actions_required[lang].delete. " +
  "Then update `www/translations/state-<lang>.json` with the new source hashes (set state-<lang>.json[<src_path>] = the first 4 hex chars of SHA-256 of the current English source bytes for every source you translated) and re-run the same command to confirm exit 0.";

const result = { prompt, summary, actions_required };
console.log(JSON.stringify(result, null, 2));

const has_work = total_stale + total_missing + total_orphaned > 0;
process.exit(has_work ? 1 : 0);

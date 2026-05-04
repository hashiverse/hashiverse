#!/usr/bin/env node
import fs from "node:fs";
import path from "node:path";
import crypto from "node:crypto";
import { fileURLToPath } from "node:url";

const here = fileURLToPath(import.meta.url);
const www_root = path.resolve(path.dirname(here), "..");

const state_path = path.join(www_root, "translations", "state.json");
const pages_en_dir = path.join(www_root, "src", "pages", "en");
const i18n_dir = path.join(www_root, "src", "i18n");
const en_strings = path.join(i18n_dir, "en.json");

const sha256 = (buf) => crypto.createHash("sha256").update(buf).digest("hex");

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
  sources[rel_posix(f)] = sha256(fs.readFileSync(f));
}
if (fs.existsSync(en_strings)) {
  sources[rel_posix(en_strings)] = sha256(fs.readFileSync(en_strings));
}

const state = JSON.parse(fs.readFileSync(state_path, "utf8"));

const actions_required = {};
let total_stale = 0;
let total_missing = 0;
let total_orphaned = 0;

for (const lang of Object.keys(state)) {
  const lang_state = state[lang] || {};
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
  languages: Object.keys(state).length,
  stale: total_stale,
  missing: total_missing,
  orphaned: total_orphaned,
};

const prompt = "You are updating translations for the hashiverse www/ site. Run `node www/translations/check-translations.mjs` from the repo root, parse the JSON output, and for every entry under actions_required, do the following based on the source path: (a) for `src/pages/en/**/*.astro` — read the English file, translate into the target language preserving imports, JSX tags, class names, and code blocks, write to `src/pages/<lang>/<same-relative-path>` (swap `/en/` for `/<lang>/`). (b) for `src/i18n/en.json` — read it, translate each string value into the target language, write to `src/i18n/<lang>.json`. Do NOT translate: Hashiverse, Kademlia, Ed25519, ChaCha20Poly1305, proof-of-work, Blake2, Blake3, SHA2, SHA3, ML-DSA, FN-DSA, Whirlpool, Groestl, Skein. After translating, also delete any files listed under actions_required[lang].delete. Then update `www/translations/state.json` with the new source hashes (set state[lang][src_path] = the SHA-256 of the current English source bytes for every source you translated) and re-run the checker to verify exit 0.";

const result = { prompt, summary, actions_required };
console.log(JSON.stringify(result, null, 2));

const has_work = total_stale + total_missing + total_orphaned > 0;
process.exit(has_work ? 1 : 0);

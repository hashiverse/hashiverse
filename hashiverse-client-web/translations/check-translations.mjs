#!/usr/bin/env node
import fs from "node:fs";
import path from "node:path";
import crypto from "node:crypto";
import { fileURLToPath } from "node:url";

const here = fileURLToPath(import.meta.url);
const package_root = path.resolve(path.dirname(here), "..");

const state_path = path.join(package_root, "translations", "state.json");
const en_strings_path = path.join(package_root, "src", "i18n", "locales", "en.json");
const manifest_path = path.join(package_root, "src", "i18n", "locales", "manifest.json");
const public_locales_dir = path.join(package_root, "public", "locales");

const sha256 = (buf) => crypto.createHash("sha256").update(buf).digest("hex");

function flatten_strings(obj, prefix = "", out = {}) {
  for (const [k, v] of Object.entries(obj)) {
    const key = prefix ? `${prefix}.${k}` : k;
    if (v !== null && typeof v === "object" && !Array.isArray(v)) {
      flatten_strings(v, key, out);
    } else if (Array.isArray(v)) {
      v.forEach((item, i) => {
        const idx_key = `${key}[${i}]`;
        if (item !== null && typeof item === "object") {
          flatten_strings(item, idx_key, out);
        } else if (typeof item === "string") {
          out[idx_key] = item;
        }
      });
    } else if (typeof v === "string") {
      out[key] = v;
    }
  }
  return out;
}

function read_json_or_empty(p) {
  if (!fs.existsSync(p)) return {};
  return JSON.parse(fs.readFileSync(p, "utf8"));
}

const en_strings = JSON.parse(fs.readFileSync(en_strings_path, "utf8"));
const en_flat = flatten_strings(en_strings);
const en_hashes = Object.fromEntries(Object.entries(en_flat).map(([k, v]) => [k, sha256(v)]));

const state = JSON.parse(fs.readFileSync(state_path, "utf8"));

const desired_manifest = ["en", ...Object.keys(state).sort()];
const current_manifest_text = fs.existsSync(manifest_path) ? fs.readFileSync(manifest_path, "utf8") : "";
const desired_manifest_text = `${JSON.stringify(desired_manifest, null, "\t")}\n`;
if (current_manifest_text !== desired_manifest_text) {
  fs.writeFileSync(manifest_path, desired_manifest_text);
}

const actions_required = {};
let total_stale = 0;
let total_missing = 0;
let total_orphaned = 0;

for (const lang of Object.keys(state).sort()) {
  const lang_state = state[lang] || {};
  const lang_path = path.join(public_locales_dir, `${lang}.json`);
  const lang_strings = read_json_or_empty(lang_path);
  const lang_flat = flatten_strings(lang_strings);

  const out = { translate: [], create: [], delete: [] };

  for (const [key, en_value] of Object.entries(en_flat)) {
    const new_hash = en_hashes[key];
    const recorded = lang_state[key];
    const lang_value = lang_flat[key];

    if (recorded === undefined || recorded === null) {
      if (lang_value !== undefined) {
        out.translate.push({ key, en: en_value, current_translation: lang_value, new_hash });
        total_stale++;
      } else {
        out.create.push({ key, en: en_value, new_hash });
        total_missing++;
      }
    } else if (recorded !== new_hash) {
      out.translate.push({
        key,
        en: en_value,
        current_translation: lang_value !== undefined ? lang_value : null,
        new_hash,
      });
      total_stale++;
    }
  }

  for (const key of Object.keys(lang_flat)) {
    if (!(key in en_flat)) {
      out.delete.push(key);
      total_orphaned++;
    }
  }

  actions_required[lang] = out;
}

const summary = {
  total_keys: Object.keys(en_flat).length,
  languages: Object.keys(state).length,
  stale: total_stale,
  missing: total_missing,
  orphaned: total_orphaned,
};

const prompt =
  "You are updating translations for hashiverse-client-web. " +
  "Run `node hashiverse-client-web/translations/check-translations.mjs` from the repo root to (re)generate this JSON — its stdout is exactly the {prompt, summary, actions_required} structure you are reading, with a fresh actions_required block. Exit code is 1 while any work remains and 0 once everything is registered fresh. " +
  "For each lang in actions_required, work key-by-key. " +
  "For each entry in `translate` and `create`: write the translated value into the matching nested path in `hashiverse-client-web/public/locales/<lang>.json`, " +
  "AND set `hashiverse-client-web/translations/state.json`[<lang>][<key>] to entry.new_hash. " +
  "Both edits must happen together for every key you fix — if you skip a key, leave both files alone for that key (it stays stale). " +
  "For each entry in `delete`: remove the key from `<lang>.json` AND from `state.json`[<lang>]. " +
  "Do NOT translate the product name (Hashiverse) or technical/cryptographic terms (e.g. Kademlia, Ed25519, P2P, hashtag). " +
  "When you are done, re-run the same command to confirm exit 0.";

const result = { prompt, summary, actions_required };
console.log(JSON.stringify(result, null, 2));

const has_work = total_stale + total_missing + total_orphaned > 0;
process.exit(has_work ? 1 : 0);

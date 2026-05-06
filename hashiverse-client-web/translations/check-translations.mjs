#!/usr/bin/env node
import crypto from "node:crypto";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const here = fileURLToPath(import.meta.url);
const package_root = path.resolve(path.dirname(here), "..");

const translations_dir = path.join(package_root, "translations");
const en_strings_path = path.join(package_root, "src", "i18n", "locales", "en.json");
const manifest_path = path.join(package_root, "src", "i18n", "locales", "manifest.json");
const public_locales_dir = path.join(package_root, "public", "locales");

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

const en_strings = JSON.parse(fs.readFileSync(en_strings_path, "utf8"));
const en_hashes = Object.fromEntries(Object.entries(en_strings).map(([k, v]) => [k, sha256_prefix(v)]));

const desired_manifest = ["en", ...langs];
const current_manifest_text = fs.existsSync(manifest_path) ? fs.readFileSync(manifest_path, "utf8") : "";
// Single-line array form so biome's JSON formatter leaves it alone.
const desired_manifest_text = `[${desired_manifest.map((l) => `"${l}"`).join(", ")}]\n`;
if (current_manifest_text !== desired_manifest_text) {
	fs.writeFileSync(manifest_path, desired_manifest_text);
}

const actions_required = {};
let total_stale = 0;
let total_missing = 0;
let total_orphaned = 0;

for (const lang of langs) {
	const lang_state = read_json_or_empty(state_path_for(lang));
	const lang_strings = read_json_or_empty(path.join(public_locales_dir, `${lang}.json`));

	const out = { translate: [], create: [], delete: [] };

	for (const [key, en_value] of Object.entries(en_strings)) {
		const new_hash = en_hashes[key];
		const recorded = lang_state[key];
		const lang_value = lang_strings[key];

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

	for (const key of Object.keys(lang_strings)) {
		if (!(key in en_strings)) {
			out.delete.push(key);
			total_orphaned++;
		}
	}

	actions_required[lang] = out;
}

const summary = {
	total_keys: Object.keys(en_strings).length,
	languages: langs.length,
	stale: total_stale,
	missing: total_missing,
	orphaned: total_orphaned,
};

const prompt =
	"You are updating translations for hashiverse-client-web. " +
	"Run `node hashiverse-client-web/translations/check-translations.mjs` from the repo root to (re)generate this JSON — its stdout is exactly the {prompt, summary, actions_required} structure you are reading, with a fresh actions_required block. Exit code is 1 while any work remains and 0 once everything is registered fresh. " +
	"For each lang in actions_required, work key-by-key. " +
	"For each entry in `translate` and `create`: write the translated value into `hashiverse-client-web/public/locales/<lang>.json` at the flat key, " +
	"AND set `hashiverse-client-web/translations/state-<lang>.json`[<key>] to entry.new_hash. " +
	"Both edits must happen together for every key you fix — if you skip a key, leave both files alone for that key (it stays stale). " +
	"For each entry in `delete`: remove the key from `<lang>.json` AND from `state-<lang>.json`. " +
	"Do NOT translate the product name (Hashiverse) or technical/cryptographic terms (e.g. Kademlia, Ed25519, P2P, hashtag). " +
	"When you are done, re-run the same command to confirm exit 0.";

const result = { prompt, summary, actions_required };
console.log(JSON.stringify(result, null, 2));

const has_work = total_stale + total_missing + total_orphaned > 0;
process.exit(has_work ? 1 : 0);

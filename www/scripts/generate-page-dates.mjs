import { execFileSync } from "node:child_process";
import { mkdirSync, readdirSync, statSync, writeFileSync } from "node:fs";
import { dirname, join, relative, sep } from "node:path";
import { fileURLToPath } from "node:url";

const __dirname = dirname(fileURLToPath(import.meta.url));
const www_root = join(__dirname, "..");
const pages_root = join(www_root, "src", "pages");
const generated_dir = join(www_root, "src", "generated");
const output_file = join(generated_dir, "page_dates.json");

const page_extensions = [".astro", ".mdx", ".md"];

function walk(dir) {
    const results = [];
    for (const entry of readdirSync(dir)) {
        const full = join(dir, entry);
        if (statSync(full).isDirectory()) {
            results.push(...walk(full));
        } else if (page_extensions.some((ext) => full.endsWith(ext))) {
            results.push(full);
        }
    }
    return results;
}

function file_to_url(file_path) {
    let rel = relative(pages_root, file_path).split(sep).join("/");
    rel = rel.replace(/\.(astro|mdx|md)$/, "");
    if (rel === "index") return "/";
    if (rel.endsWith("/index")) rel = rel.slice(0, -"/index".length);
    return "/" + rel;
}

function git_mtime(file_path) {
    try {
        const out = execFileSync(
            "git",
            ["log", "-1", "--format=%aI", "--", file_path],
            { encoding: "utf8", cwd: www_root, stdio: ["ignore", "pipe", "ignore"] },
        ).trim();
        return out || null;
    } catch {
        return null;
    }
}

const page_files = walk(pages_root);
const result = {};
for (const file of page_files) {
    const url = file_to_url(file);
    const date = git_mtime(file);
    if (date) result[url] = date;
}

mkdirSync(generated_dir, { recursive: true });
writeFileSync(output_file, JSON.stringify(result, null, 2) + "\n", "utf8");
console.log(`generate-page-dates: wrote ${Object.keys(result).length} entries to ${relative(www_root, output_file)}`);

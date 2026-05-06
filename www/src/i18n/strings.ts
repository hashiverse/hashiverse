import en from "./en.json";

type Strings = typeof en;

const all = import.meta.glob<{ default: Strings }>("./*.json", { eager: true });

function deep_merge<T>(base: T, override: unknown): T {
  if (base === null || typeof base !== "object" || Array.isArray(base)) {
    return (override === undefined ? base : override) as T;
  }
  if (override === null || typeof override !== "object" || Array.isArray(override)) {
    return base;
  }
  const result: Record<string, unknown> = { ...(base as Record<string, unknown>) };
  for (const key of Object.keys(override as Record<string, unknown>)) {
    result[key] = deep_merge((base as Record<string, unknown>)[key], (override as Record<string, unknown>)[key]);
  }
  return result as T;
}

const lookup: Record<string, Strings> = {};
for (const [file_path, mod] of Object.entries(all)) {
  const lang = file_path.replace(/^\.\//, "").replace(/\.json$/, "");
  lookup[lang] = deep_merge(en, mod.default);
}

export const supported_locales = Object.keys(lookup).sort();

export const strings_for = (locale: string | undefined): Strings => lookup[locale ?? "en"] ?? lookup.en;

import en from "./en.json";

type Strings = typeof en;

const all = import.meta.glob<{ default: Strings }>("./*.json", { eager: true });

const lookup: Record<string, Strings> = {};
for (const [file_path, mod] of Object.entries(all)) {
  const lang = file_path.replace(/^\.\//, "").replace(/\.json$/, "");
  lookup[lang] = mod.default;
}

export const supported_locales = Object.keys(lookup).sort();

export const strings_for = (locale: string | undefined): Strings => lookup[locale ?? "en"] ?? lookup.en;

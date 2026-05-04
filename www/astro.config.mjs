import { defineConfig } from "astro/config";
import mdx from "@astrojs/mdx";
import fs from "node:fs";
import path from "node:path";

const i18n_dir = path.resolve("./src/i18n");
const locales = fs.readdirSync(i18n_dir)
  .filter((f) => f.endsWith(".json"))
  .map((f) => f.replace(/\.json$/, ""))
  .sort();

export default defineConfig({
  integrations: [mdx()],
  output: "static",
  i18n: {
    defaultLocale: "en",
    locales,
    routing: {
      prefixDefaultLocale: true,
      redirectToDefaultLocale: false,
    },
  },
});

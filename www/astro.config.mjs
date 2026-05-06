import { defineConfig } from "astro/config";
import mdx from "@astrojs/mdx";
import sitemap from "@astrojs/sitemap";
import fs from "node:fs";
import path from "node:path";

const i18n_dir = path.resolve("./src/i18n");
const locales = fs.readdirSync(i18n_dir)
  .filter((f) => f.endsWith(".json"))
  .map((f) => f.replace(/\.json$/, ""))
  .sort();

const sitemap_locales = Object.fromEntries(locales.map((l) => [l, l]));

const fallback = Object.fromEntries(locales.filter((l) => l !== "en").map((l) => [l, "en"]));

export default defineConfig({
  site: "https://www.hashiverse.com",
  integrations: [
    mdx(),
    sitemap({
      i18n: {
        defaultLocale: "en",
        locales: sitemap_locales,
      },
    }),
  ],
  output: "static",
  i18n: {
    defaultLocale: "en",
    locales,
    fallback,
    routing: {
      prefixDefaultLocale: true,
      redirectToDefaultLocale: false,
      fallbackType: "redirect",
    },
  },
});

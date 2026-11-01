import { defineConfig } from "@rsbuild/core";
import { pluginBasicSsl } from "@rsbuild/plugin-basic-ssl";
import { pluginReact } from "@rsbuild/plugin-react";
import locales_manifest from "./src/i18n/locales/manifest.json";

export default defineConfig({
	plugins: [pluginReact(), pluginBasicSsl()],

	source: {
		assetsInclude: [/\.wav$/],
		define: {
			"process.env.PRERELEASE_TAG": JSON.stringify(process.env.PRERELEASE_TAG ?? ""),
		},
	},

	server: {
		// rsbuild 2 defaults to "localhost"; we listen on all interfaces so the dev server is reachable on the machine's real network IP (firewall/port-forward setups, LAN devices).
		host: "0.0.0.0",
	},

	dev: {
		hmr: true,
		liveReload: true,
	},

	output: {
		target: "web",
	},

	html: {
		template: "./public/index.html",
		favicon: "./public/favicon.ico",
		templateParameters: {
			locales_json: JSON.stringify(locales_manifest),
		},
	},
});

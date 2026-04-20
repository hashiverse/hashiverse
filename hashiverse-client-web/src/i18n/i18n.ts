import i18n from "i18next";
import HttpBackend from "i18next-http-backend";
import { initReactI18next } from "react-i18next";
import en from "./locales/en.json";

export const SUPPORTED_LANGUAGES = [
	{ value: "en", label: "English" },
	{ value: "fr", label: "Français" },
	{ value: "he", label: "עברית" },
	{ value: "nl", label: "Nederlands" },
	{ value: "ru", label: "Русский" },
	{ value: "zh", label: "中文" },
];

const supported_values = new Set(SUPPORTED_LANGUAGES.map((l) => l.value));

function detect_language(): string {
	// index.tsx reads the stored language from IndexedDB and calls i18n.changeLanguage() before first render

	for (const lang of navigator.languages ?? [navigator.language]) {
		if (supported_values.has(lang)) return lang;
		const base = lang.split("-")[0];
		if (supported_values.has(base)) return base;
	}

	return "en";
}

i18n
	.use(HttpBackend)
	.use(initReactI18next)
	.init({
		lng: detect_language(),
		fallbackLng: "en",
		partialBundledLanguages: true,
		resources: {
			en: { translation: en },
		},
		backend: {
			loadPath: "/locales/{{lng}}.json",
		},
		interpolation: { escapeValue: false }, // React already escapes
		showSupportNotice: false,
	});

export default i18n;

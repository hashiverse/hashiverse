import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";

const UNITS: [Intl.RelativeTimeFormatUnit, number][] = [
	["year", 365 * 24 * 60 * 60],
	["month", 30 * 24 * 60 * 60],
	["week", 7 * 24 * 60 * 60],
	["day", 24 * 60 * 60],
	["hour", 60 * 60],
	["minute", 60],
	["second", 1],
];

function format_relative(date_millis: number, locale: string): string {
	const delta_seconds = Math.round((date_millis - Date.now()) / 1000);
	const absolute_delta = Math.abs(delta_seconds);

	for (const [unit, threshold] of UNITS) {
		if (absolute_delta >= threshold) {
			const value = Math.round(delta_seconds / threshold);
			return new Intl.RelativeTimeFormat(locale, { numeric: "auto" }).format(value, unit);
		}
	}
	return new Intl.RelativeTimeFormat(locale, { numeric: "auto" }).format(0, "second");
}

function refresh_interval_ms(date_millis: number): number {
	const age_seconds = Math.abs((Date.now() - date_millis) / 1000);
	if (age_seconds < 60) return 10_000;
	if (age_seconds < 3600) return 30_000;
	if (age_seconds < 86400) return 300_000;
	return 1_800_000;
}

export function RelativeTimeAgo({ date }: { date: number }) {
	const { i18n } = useTranslation();
	const [text, set_text] = useState(() => format_relative(date, i18n.language));

	useEffect(() => {
		set_text(format_relative(date, i18n.language));
		const id = setInterval(() => {
			set_text(format_relative(date, i18n.language));
		}, refresh_interval_ms(date));
		return () => clearInterval(id);
	}, [date, i18n.language]);

	return <time dateTime={new Date(date).toISOString()}>{text}</time>;
}

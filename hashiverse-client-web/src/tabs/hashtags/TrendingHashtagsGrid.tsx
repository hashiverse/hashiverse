import { Text, UnstyledButton } from "@mantine/core";
import type React from "react";
import { useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { useNavigate } from "react-router";
import type { HashiverseClientWasmProxy, TrendingHashtag } from "../../Hashiverse.ts";

const INITIAL_DISPLAY_COUNT = 12;
const MAX_DISPLAY_COUNT = 24;
const TRENDING_COLOR_MIN = [255, 100, 100]; // red
const TRENDING_COLOR_MAX = [255, 220, 50]; // yellow
const MARQUEE_SEPARATOR = "\u00A0\u00A0\u00A0\u00A0\u00A0\u00A0";
const MARQUEE_SPEED_PX_PER_SEC = 20;
const USE_STUB_HASHTAGS = false;

const OverflowMarquee: React.FC<{
	children: string;
	style?: React.CSSProperties;
}> = ({ children, style }) => {
	const container_ref = useRef<HTMLDivElement>(null);
	const measure_ref = useRef<HTMLSpanElement>(null);
	const [overflows, overflows_set] = useState(false);
	const [duration, duration_set] = useState(5);

	const check = useCallback(() => {
		const container = container_ref.current;
		const measure = measure_ref.current;
		if (container && measure) {
			const does_overflow = measure.offsetWidth > container.clientWidth;
			overflows_set(does_overflow);
			if (does_overflow) {
				duration_set(measure.offsetWidth / MARQUEE_SPEED_PX_PER_SEC);
			}
		}
	}, []);

	useEffect(() => {
		check();
		const observer = new ResizeObserver(check);
		if (container_ref.current) observer.observe(container_ref.current);
		return () => observer.disconnect();
	}, [check]);

	return (
		<div
			ref={container_ref}
			style={{
				overflow: "hidden",
				whiteSpace: "nowrap",
				width: "100%",
				position: "relative",
			}}
		>
			{/* Hidden measurement span — always present, never animated */}
			<span ref={measure_ref} style={{ visibility: "hidden", position: "absolute", ...style }}>
				{children}
				{MARQUEE_SEPARATOR}
			</span>
			{overflows ? (
				<span
					style={{
						display: "inline-block",
						animation: `hashtag-marquee ${duration}s linear infinite`,
						...style,
					}}
				>
					{children}
					{MARQUEE_SEPARATOR}
					{children}
					{MARQUEE_SEPARATOR}
				</span>
			) : (
				<span style={style}>{children}</span>
			)}
		</div>
	);
};

interface Props {
	hashiverse: HashiverseClientWasmProxy;
}

export const TrendingHashtagsGrid: React.FC<Props> = ({ hashiverse }) => {
	const { t } = useTranslation();
	const navigate = useNavigate();
	const [trending_hashtags, trending_hashtags_set] = useState<TrendingHashtag[]>([]);
	const [expanded, expanded_set] = useState(false);

	useEffect(() => {
		if (USE_STUB_HASHTAGS) {
			console.warn("Using stub trending hashtags");
			trending_hashtags_set([
				{ hashtag: "hashiverse", count: 81120 },
				{ hashtag: "mega", count: 51120 },
				{ hashtag: "mucho", count: 1120 },
				{ hashtag: "decentralized", count: 85 },
				{ hashtag: "cryptoisthefastestwaytolose", count: 64 },
				{ hashtag: "p2p", count: 42 },
				{ hashtag: "privacy", count: 38 },
				{ hashtag: "opensource", count: 27 },
				{ hashtag: "rust1", count: 19 },
				{ hashtag: "rust2", count: 19 },
				{ hashtag: "rust3", count: 19 },
				{ hashtag: "rust4", count: 19 },
				{ hashtag: "rust5", count: 19 },
				{ hashtag: "rust6", count: 19 },
				{ hashtag: "rust7", count: 19 },
				{ hashtag: "rust8", count: 19 },
				{ hashtag: "wasm", count: 12 },
				{ hashtag: "postquantum", count: 7 },
				{ hashtag: "dht", count: 3 },
			]);
			return;
		}

		hashiverse
			.fetch_trending_hashtags_v1(MAX_DISPLAY_COUNT)
			.then((response) => {
				console.log("Received trending hashtags", response.trending_hashtags);
				trending_hashtags_set(response.trending_hashtags);
			})
			.catch(console.error);
	}, [hashiverse.fetch_trending_hashtags_v1]);

	if (trending_hashtags.length === 0) return null;

	const displayed_hashtags = expanded ? trending_hashtags : trending_hashtags.slice(0, INITIAL_DISPLAY_COUNT);
	const has_more = trending_hashtags.length > INITIAL_DISPLAY_COUNT;

	const min_count = Math.min(...trending_hashtags.map((h) => h.count));
	const max_count = Math.max(...trending_hashtags.map((h) => h.count));
	const count_to_color = (count: number): string => {
		if (max_count === min_count) return `rgb(${TRENDING_COLOR_MAX.join(",")})`;
		const log_min = Math.log(min_count || 1);
		const log_max = Math.log(max_count || 1);
		const normalized = (Math.log(count || 1) - log_min) / (log_max - log_min);
		const r = Math.round(TRENDING_COLOR_MIN[0] + (TRENDING_COLOR_MAX[0] - TRENDING_COLOR_MIN[0]) * normalized);
		const g = Math.round(TRENDING_COLOR_MIN[1] + (TRENDING_COLOR_MAX[1] - TRENDING_COLOR_MIN[1]) * normalized);
		const b = Math.round(TRENDING_COLOR_MIN[2] + (TRENDING_COLOR_MAX[2] - TRENDING_COLOR_MIN[2]) * normalized);
		return `rgb(${r},${g},${b})`;
	};

	return (
		<div style={{ padding: "12px", background: "rgba(0,0,0,0.15)" }}>
			<div
				style={{
					display: "flex",
					justifyContent: "space-between",
					alignItems: "center",
					marginBottom: "8px",
				}}
			>
				<Text size="lg" fw={700}>
					{t("banner.trending_hashtags")}
				</Text>
				{has_more && (
					<UnstyledButton onClick={() => expanded_set(!expanded)}>
						<Text size="sm" c="blue">
							{expanded ? t("banner.show_fewer_hashtags") : t("banner.show_more_hashtags")}
						</Text>
					</UnstyledButton>
				)}
			</div>
			<div
				style={{
					display: "grid",
					gridTemplateColumns: "repeat(auto-fill, minmax(100px, 1fr))",
					gap: "8px",
				}}
			>
				{displayed_hashtags.map((displayed_hashtag) => (
					<UnstyledButton
						key={displayed_hashtag.hashtag}
						title={`#${displayed_hashtag.hashtag}`}
						onClick={() => navigate(`/hashtag/${encodeURIComponent(displayed_hashtag.hashtag)}`)}
						style={{
							padding: "4px 4px",
							borderRadius: "8px",
							background: "rgba(255,255,255,0.05)",
							transition: "background 0.15s",
						}}
						onMouseEnter={(e) => (e.currentTarget.style.background = "rgba(255,255,255,0.12)")}
						onMouseLeave={(e) => (e.currentTarget.style.background = "rgba(255,255,255,0.05)")}
					>
						<OverflowMarquee
							style={{
								color: count_to_color(displayed_hashtag.count),
								fontWeight: 600,
								fontSize: "var(--mantine-font-size-sm)",
							}}
						>{`#${displayed_hashtag.hashtag}`}</OverflowMarquee>
					</UnstyledButton>
				))}
			</div>
		</div>
	);
};

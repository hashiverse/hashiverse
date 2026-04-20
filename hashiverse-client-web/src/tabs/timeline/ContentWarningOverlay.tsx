import { Box, Button, Group, Stack, Text } from "@mantine/core";
import { IconAlertTriangle } from "@tabler/icons-react";
import type React from "react";
import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { FEEDBACKS_NEGATIVE, type FeedbackOption } from "../../tools/Feedback.ts";
import type { ContentThresholds, UserSettingsCache } from "../../tools/UserSettingsCache.ts";

// Returns the reveal delay in seconds for a set of triggered negative feedbacks.
// Scales logarithmically with how far above threshold the worst score is, capped at 60s.
function compute_reveal_delay(feedbacks: Uint32Array, triggered: FeedbackOption[], thresholds: ContentThresholds): number {
	let max_delay = 0;
	for (const opt of triggered) {
		const threshold = thresholds[opt.feedback_type] ?? opt.warning_threshold;
		if (threshold === undefined || threshold === 0) continue;
		const score = feedbacks[opt.feedback_type] ?? 0;
		const ratio = score / threshold;
		const delay = 10 * Math.round(Math.log2(ratio + 1));
		if (delay > max_delay) max_delay = delay;
	}
	return max_delay;
}

interface Props {
	feedbacks: Uint32Array | null;
	is_own_post: boolean;
	is_author_followed: boolean;
	user_settings_cache: UserSettingsCache;
	children: React.ReactNode;
}

export const ContentWarningOverlay: React.FC<Props> = ({ feedbacks, is_own_post, is_author_followed, user_settings_cache, children }) => {
	const { t } = useTranslation();
	const [revealed, set_revealed] = useState(false);
	const [countdown, set_countdown] = useState<number | null>(null);

	const triggered: FeedbackOption[] = feedbacks
		? FEEDBACKS_NEGATIVE.filter((opt) => {
				const threshold = user_settings_cache.content_thresholds[opt.feedback_type];
				if (threshold === undefined) return false;
				const score = feedbacks[opt.feedback_type] ?? 0;
				return score >= Math.max(1, threshold);
			})
		: [];

	const delay = feedbacks ? compute_reveal_delay(feedbacks, triggered, user_settings_cache.content_thresholds) : 0;

	const on_reveal_click = useCallback(() => {
		if (delay === 0) {
			set_revealed(true);
			return;
		}
		set_countdown((c) => {
			if (c === null) return delay;
			return Math.max(0, c - 10);
		});
	}, [delay]);

	useEffect(() => {
		if (countdown === null) return;
		if (countdown <= 0) {
			set_revealed(true);
			set_countdown(null);
			return;
		}
		const timer = setTimeout(() => set_countdown((c) => (c ?? 1) - 1), 1000);
		return () => clearTimeout(timer);
	}, [countdown]);

	if (revealed || is_own_post || triggered.length === 0 || (user_settings_cache.skip_warnings_for_followed && is_author_followed)) {
		return <>{children}</>;
	}

	// Both children and overlay occupy the same grid cell — the taller one sizes the container.
	return (
		<Box style={{ display: "grid" }}>
			<Box style={{ gridArea: "1 / 1", minWidth: 0 }}>{children}</Box>
			<Box
				style={{
					gridArea: "1 / 1",
					background: "rgba(0, 0, 0, 0.60)",
					backdropFilter: "blur(3px)",
					display: "flex",
					flexDirection: "column",
					alignItems: "center",
					justifyContent: "center",
					zIndex: 10,
					padding: "16px",
					gap: "8px",
				}}
			>
				<Group gap="xs" align="center">
					<IconAlertTriangle size={20} color="var(--mantine-color-yellow-5)" />
					<Text size="sm" fw={600} c="yellow.4">
						{t("content_warning.flagged_by_community")}
					</Text>
				</Group>

				<Stack gap={4} align="center">
					{triggered.map((opt) => {
						const Icon = opt.icon;
						return (
							<Group key={opt.feedback_type} gap="xs" align="center">
								<Icon size={14} />
								<Text size="xs" c="dimmed">
									{t(opt.title)}
								</Text>
							</Group>
						);
					})}
				</Stack>

				<Button size="xs" variant="subtle" color="gray" onClick={on_reveal_click}>
					{countdown !== null ? `${t("content_warning.revealing_in")} ${countdown}s` : t("content_warning.reveal")}
				</Button>
			</Box>
		</Box>
	);
};

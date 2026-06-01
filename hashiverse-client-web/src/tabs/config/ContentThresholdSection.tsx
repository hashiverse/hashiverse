import { Button, Checkbox, Stack, Text } from "@mantine/core";
import type React from "react";
import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import type { HashiverseClientWasmProxy } from "../../Hashiverse.ts";
import { CollapsiblePanel } from "../../tools/CollapsiblePanel.tsx";
import { ContentThresholdSlider } from "../../tools/ContentThresholdSlider.tsx";
import { FEEDBACK_TYPE_CSAM, FEEDBACKS_NEGATIVE } from "../../tools/Feedback.ts";
import { Toast } from "../../tools/Toast.ts";
import type { ContentThresholds, UserSettingsCache } from "../../tools/UserSettingsCache.ts";

interface Props {
	hashiverse: HashiverseClientWasmProxy;
	user_settings_cache: UserSettingsCache;
}

export const ContentThresholdSection: React.FC<Props> = ({ hashiverse, user_settings_cache }) => {
	const { t } = useTranslation();
	const [thresholds, set_thresholds_state] = useState<ContentThresholds>(user_settings_cache.content_thresholds);
	const [skip_for_followed, set_skip_for_followed] = useState(user_settings_cache.skip_warnings_for_followed);
	const applied_ref = useRef(false);

	// Re-seed local state when the cache updates, but not after a local Apply
	// (Apply already wrote the correct values — re-seeding would flash back to stale cache)
	useEffect(() => {
		if (applied_ref.current) {
			applied_ref.current = false;
			return;
		}
		set_thresholds_state(user_settings_cache.content_thresholds);
	}, [user_settings_cache.content_thresholds]);
	useEffect(() => {
		if (applied_ref.current) {
			applied_ref.current = false;
			return;
		}
		set_skip_for_followed(user_settings_cache.skip_warnings_for_followed);
	}, [user_settings_cache.skip_warnings_for_followed]);

	const handle_apply = async () => {
		try {
			await hashiverse.set_content_thresholds_v1(thresholds);
			await hashiverse.set_skip_warnings_for_followed_v1(skip_for_followed);
			applied_ref.current = true;
			user_settings_cache.invalidate();
			Toast.success(t("toast.content_thresholds_saved"));
		} catch (e) {
			console.error(e);
			Toast.error(t("toast.error_generic"));
		}
	};

	return (
		<CollapsiblePanel title={t("config.content_thresholds.title")}>
			<Text mb="xl">{t("config.content_thresholds.description")}</Text>
			<Stack gap={0}>
				{FEEDBACKS_NEGATIVE.map((fb) => (
					<ContentThresholdSlider
						key={fb.feedback_type}
						label={t(fb.title)}
						icon={fb.icon}
						value={thresholds[fb.feedback_type] ?? fb.warning_threshold ?? 1000}
						onChange={(val) =>
							set_thresholds_state((prev) => ({
								...prev,
								[fb.feedback_type]: val,
							}))
						}
						max_value={fb.feedback_type === FEEDBACK_TYPE_CSAM ? fb.warning_threshold : undefined}
					/>
				))}
			</Stack>
			<Checkbox mt="md" label={t("config.content_thresholds.skip_warnings_for_followed")} checked={skip_for_followed} onChange={(e) => set_skip_for_followed(e.currentTarget.checked)} />
			<Button onClick={handle_apply} mt="md">
				{t("config.content_thresholds.apply")}
			</Button>
		</CollapsiblePanel>
	);
};

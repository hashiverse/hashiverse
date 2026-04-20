import { Button, Progress, Stack, Text, Textarea } from "@mantine/core";
import type React from "react";
import { useCallback, useState } from "react";
import { useTranslation } from "react-i18next";
import { useNavigate } from "react-router";
import { Hashiverse, type HashiverseClientWasmProxy } from "../../Hashiverse.ts";
import { CollapsiblePanel } from "../../tools/CollapsiblePanel.tsx";

interface Props {
	on_login: (hashiverse: HashiverseClientWasmProxy) => void;
}

const RECOMMENDED_KEYPHRASE_LENGTH = 128;

export const KeyphraseSection: React.FC<Props> = ({ on_login }) => {
	const { t } = useTranslation();
	const navigate = useNavigate();
	const [keyphrase, set_keyphrase] = useState("");
	const [loading, set_loading] = useState(false);

	const login = useCallback(async () => {
		const phrase = keyphrase.trim();
		if (!phrase) return;
		set_loading(true);
		try {
			const new_hv = await Hashiverse.create_from_keyphrase(phrase);
			on_login(new_hv);
			navigate("/");
		} finally {
			set_loading(false);
		}
	}, [keyphrase, on_login, navigate]);

	return (
		<CollapsiblePanel title={t("login.keyphrase_title")}>
			<Stack>
				<Text>{t("login.instruction_keyphrase")}</Text>

				<Textarea
					label={t("login.keyphrase_label")}
					placeholder={t("login.keyphrase_placeholder")}
					value={keyphrase}
					onChange={(e) => set_keyphrase(e.currentTarget.value)}
					autosize
					minRows={3}
				/>

				<Progress
					value={Math.min((keyphrase.length / RECOMMENDED_KEYPHRASE_LENGTH) * 100, 100)}
					color={keyphrase.length < RECOMMENDED_KEYPHRASE_LENGTH / 2 ? "red" : keyphrase.length < RECOMMENDED_KEYPHRASE_LENGTH ? "orange" : "green"}
				/>

				<Button onClick={login} loading={loading} disabled={!keyphrase.trim()}>
					{t("login.login_button")}
				</Button>
			</Stack>
		</CollapsiblePanel>
	);
};

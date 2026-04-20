import { Button, Stack, Text } from "@mantine/core";
import type React from "react";
import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { useNavigate } from "react-router";
import { Hashiverse, type HashiverseClientWasmProxy } from "../../Hashiverse.ts";
import { CollapsiblePanel } from "../../tools/CollapsiblePanel.tsx";
import { createPasskeyAndGetPrf, isPlatformAuthenticatorAvailable, signInWithPasskeyAndGetPrf } from "./passkey_utils.ts";

interface Props {
	on_login: (hashiverse: HashiverseClientWasmProxy) => void;
}

type State = "idle" | "sign_in_failed" | "loading";

export const PasskeySection: React.FC<Props> = ({ on_login }) => {
	const { t } = useTranslation();
	const navigate = useNavigate();
	const [supported, set_supported] = useState<boolean | null>(null);
	const [state, set_state] = useState<State>("idle");
	const [error, set_error] = useState<string | null>(null);

	useEffect(() => {
		isPlatformAuthenticatorAvailable().then((available) => set_supported(available));
	}, []);

	const do_login = useCallback(
		async (prf_hex: string) => {
			const new_hv = await Hashiverse.create_from_keyphrase(prf_hex);
			on_login(new_hv);
			navigate("/");
		},
		[on_login, navigate],
	);

	const handle_sign_in = useCallback(async () => {
		set_state("loading");
		set_error(null);
		try {
			const prf_hex = await signInWithPasskeyAndGetPrf();
			await do_login(prf_hex);
		} catch (e) {
			set_error(e instanceof Error ? e.message : String(e));
			set_state("sign_in_failed");
		}
	}, [do_login]);

	const handle_create = useCallback(async () => {
		set_state("loading");
		set_error(null);
		try {
			const prf_hex = await createPasskeyAndGetPrf();
			await do_login(prf_hex);
		} catch (e) {
			set_error(e instanceof Error ? e.message : String(e));
			set_state("sign_in_failed");
		}
	}, [do_login]);

	return (
		<CollapsiblePanel title={t("login.passkey_title")}>
			<Stack>
				{supported === null ? null : !supported ? (
					<Text c="dimmed">{t("login.passkey_unsupported")}</Text>
				) : (
					<>
						<Text>
							{t("login.instruction_passkey", {
								domain: window.location.hostname,
							})}
						</Text>
						{error && (
							<Text c="red" size="sm">
								{error}
							</Text>
						)}
						<Button onClick={handle_sign_in} loading={state === "loading"}>
							{t("login.passkey_sign_in_button")}
						</Button>
						{state === "sign_in_failed" && (
							<>
								<Text size="sm" c="dimmed">
									{t("login.passkey_no_passkey_hint")}
								</Text>
								<Button variant="default" onClick={handle_create}>
									{t("login.passkey_create_button")}
								</Button>
							</>
						)}
					</>
				)}
			</Stack>
		</CollapsiblePanel>
	);
};

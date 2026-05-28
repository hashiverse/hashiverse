import { Button, Group, Modal, Text } from "@mantine/core";
import { useDisclosure } from "@mantine/hooks";
import type React from "react";
import { useEffect, useRef } from "react";
import { useTranslation } from "react-i18next";
import { useNavigate } from "react-router";
import type { HashiverseClientWasmProxy } from "../../Hashiverse.ts";
import { redirect_to_login_with_return } from "../../tools/PostLoginRedirect.ts";
import type { UserSettingsCache } from "../../tools/UserSettingsCache.ts";
import { type ComposeOpenContext, register_compose_dialog } from "./ComposeDialogStore.ts";
import { ComposeEditor, type ComposeEditorHandle } from "./ComposeEditor.tsx";

interface Props {
	hashiverse: HashiverseClientWasmProxy;
	user_settings_cache: UserSettingsCache;
}

export const ComposeDialog: React.FC<Props> = ({ hashiverse, user_settings_cache }) => {
	const { t } = useTranslation();
	const navigate = useNavigate();
	const [opened, { open, close }] = useDisclosure(false);
	const [needs_login_opened, { open: needs_login_open, close: needs_login_close }] = useDisclosure(false);
	const ref_on_posted = useRef<(() => void | Promise<void>) | undefined>(undefined);
	const ref_editor = useRef<ComposeEditorHandle>(null);

	useEffect(() => {
		register_compose_dialog(async (context?: ComposeOpenContext) => {
			if (!user_settings_cache.is_logged_in) {
				needs_login_open();
				return;
			}
			ref_on_posted.current = context?.on_posted;
			if (context?.initial_html) ref_editor.current?.set_content(context.initial_html);
			if (context?.share_image_blob) await ref_editor.current?.insert_image_blob(context.share_image_blob);
			if (context?.share_youtube_url) ref_editor.current?.insert_youtube_url(context.share_youtube_url);
			if (context?.share_url) ref_editor.current?.insert_url_preview(context.share_url);
			open();
		});
	}, [open, needs_login_open, user_settings_cache]);

	useEffect(() => {
		if (opened) ref_editor.current?.focus();
	}, [opened]);

	return (
		<>
			<Modal
				opened={opened}
				onClose={close}
				title={t("nav.compose")}
				keepMounted
				fullScreen
				closeOnEscape={false}
				onKeyDown={(e) => {
					if (e.key === "Escape" && !(e.target as HTMLElement).closest('[data-type="inline-math-editor"],[data-type="block-math-editor"]')) close();
				}}
				styles={{
					content: {
						display: "flex",
						flexDirection: "column",
						maxWidth: "550px",
						margin: "0 auto",
					},
					body: {
						flex: "1 1 0",
						minHeight: 0,
						padding: 0,
						display: "flex",
						flexDirection: "column",
					},
				}}
			>
				<ComposeEditor
					ref={ref_editor}
					hashiverse={hashiverse}
					restore_draft={true}
					on_posted={async () => {
						await ref_on_posted.current?.();
						ref_on_posted.current = undefined;
					}}
					on_submit_complete={close}
				/>
			</Modal>

			<Modal opened={needs_login_opened} onClose={needs_login_close} title={t("not_logged_in.needs_login_title")} size="sm" centered>
				<Text mb="md">{t("not_logged_in.needs_login_message")}</Text>
				<Group justify="flex-end">
					<Button variant="default" onClick={needs_login_close}>
						{t("bio.cancel")}
					</Button>
					<Button
						onClick={() => {
							needs_login_close();
							redirect_to_login_with_return(navigate, "/compose");
						}}
					>
						{t("not_logged_in.log_in")}
					</Button>
				</Group>
			</Modal>
		</>
	);
};

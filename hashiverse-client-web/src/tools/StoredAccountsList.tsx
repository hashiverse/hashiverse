import { ActionIcon, Button, Group, Loader, Modal, Stack, Text, Tooltip } from "@mantine/core";
import { useDisclosure } from "@mantine/hooks";
import { IconTrash } from "@tabler/icons-react";
import type React from "react";
import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { useNavigate } from "react-router";
import { Hashiverse, type HashiverseClientWasmProxy } from "../Hashiverse.ts";
import { useCachedBio } from "./BioCache.ts";
import { UserImageControl } from "./UserImageControl.tsx";
import { UserNameControl } from "./UserNameControl.tsx";

interface RowProps {
	key_public: string;
	hashiverse: HashiverseClientWasmProxy;
	on_select: (key_public: string) => void;
	on_remove: (key_public: string) => void;
	selecting: boolean;
}

const AccountRow: React.FC<RowProps> = ({ key_public, hashiverse, on_select, on_remove, selecting }) => {
	const { t } = useTranslation();
	const bio = useCachedBio(hashiverse, key_public);
	const [confirm_open, { open: open_confirm, close: close_confirm }] = useDisclosure(false);

	return (
		<>
			<Group justify="space-between" wrap="nowrap">
				<Button
					variant="subtle"
					justify="flex-start"
					leftSection={<UserImageControl client_id={key_public} selfie={bio?.selfie} avatar={bio?.avatar} radius="xl" size="sm" />}
					onClick={() => on_select(key_public)}
					disabled={selecting}
					style={{ flex: 1, minWidth: 0 }}
				>
					<UserNameControl client_id={key_public} nickname={bio?.nickname} />
				</Button>
				<Tooltip label={t("login_management.remove")}>
					<ActionIcon color="red" variant="subtle" onClick={open_confirm} disabled={selecting} style={{ flexShrink: 0 }}>
						<IconTrash size={16} />
					</ActionIcon>
				</Tooltip>
			</Group>

			<Modal opened={confirm_open} onClose={close_confirm} title={t("login_management.remove_confirm_title")} size="sm" centered>
				<Text mb="md">{t("login_management.remove_confirm_message")}</Text>
				<Group justify="flex-end">
					<Button variant="default" onClick={close_confirm}>
						{t("login_management.cancel")}
					</Button>
					<Button
						color="red"
						onClick={() => {
							close_confirm();
							on_remove(key_public);
						}}
					>
						{t("login_management.confirm")}
					</Button>
				</Group>
			</Modal>
		</>
	);
};

interface Props {
	hashiverse: HashiverseClientWasmProxy;
	on_login: (hashiverse: HashiverseClientWasmProxy) => void;
	intro?: React.ReactNode;
}

export const StoredAccountsList: React.FC<Props> = ({ hashiverse, on_login, intro }) => {
	const { t } = useTranslation();
	const navigate = useNavigate();
	const [stored_keys, set_stored_keys] = useState<string[]>([]);
	const [loading, set_loading] = useState(true);
	const [selecting, set_selecting] = useState(false);
	const [remove_all_open, { open: open_remove_all, close: close_remove_all }] = useDisclosure(false);

	const refresh = useCallback(() => {
		hashiverse
			.list_stored_key_ids_v1()
			.then((keys) => set_stored_keys(keys as string[]))
			.catch(() => {})
			.finally(() => set_loading(false));
	}, [hashiverse]);

	useEffect(() => {
		refresh();
	}, [refresh]);

	const on_select = useCallback(
		async (key_public: string) => {
			set_selecting(true);
			try {
				const new_hv = await Hashiverse.create_from_stored_key(key_public);
				on_login(new_hv);
				navigate("/");
			} finally {
				set_selecting(false);
			}
		},
		[on_login, navigate],
	);

	const on_remove = useCallback(
		async (key_public: string) => {
			await hashiverse.delete_stored_key_v1(key_public).catch(() => {});
			refresh();
		},
		[hashiverse, refresh],
	);

	const remove_all = useCallback(async () => {
		await hashiverse.delete_all_stored_keys_v1().catch(() => {});
		refresh();
	}, [hashiverse, refresh]);

	if (loading) {
		return (
			<Group justify="center" p="md">
				<Loader size="sm" />
			</Group>
		);
	}

	return (
		<>
			<Stack>
				{stored_keys.length === 0 ? (
					<Text>{t("login_management.stored_keys_empty")}</Text>
				) : (
					<Stack>
						{intro}
						{stored_keys.map((key) => (
							<AccountRow key={key} key_public={key} hashiverse={hashiverse} on_select={on_select} on_remove={on_remove} selecting={selecting} />
						))}
						<Button color="red" variant="light" leftSection={<IconTrash size={16} />} onClick={open_remove_all}>
							{t("login_management.remove_all")}
						</Button>
					</Stack>
				)}
			</Stack>

			<Modal opened={remove_all_open} onClose={close_remove_all} title={t("login_management.remove_all_confirm_title")} size="sm" centered>
				<Text mb="md">{t("login_management.remove_all_confirm_message")}</Text>
				<Group justify="flex-end">
					<Button variant="default" onClick={close_remove_all}>
						{t("login_management.cancel")}
					</Button>
					<Button
						color="red"
						onClick={() => {
							close_remove_all();
							remove_all();
						}}
					>
						{t("login_management.confirm")}
					</Button>
				</Group>
			</Modal>
		</>
	);
};

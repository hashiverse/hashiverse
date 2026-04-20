import { type ComboboxItem, type ComboboxLikeRenderOptionInput, Group, Modal, Select, Text } from "@mantine/core";
import { useDisclosure } from "@mantine/hooks";
import { IconSearch } from "@tabler/icons-react";
import type React from "react";
import { useRef, useState } from "react";
import type { Bio, HashiverseClientWasmProxy } from "../../Hashiverse.ts";
import { search_mentions } from "../../tools/MentionStore.ts";
import { UserImageControl } from "../../tools/UserImageControl.tsx";
import { UserNameControl } from "../../tools/UserNameControl.tsx";

interface Props {
	ref_user_search_dialog_control_manager: React.RefObject<UserSearchDialogControlManager | null>;
	hashiverse: HashiverseClientWasmProxy;
}

export interface UserSearchDialogControlManager {
	modal_command_open: () => Promise<Bio | null>;
}

export const UserSearchDialogControl: React.FC<Props> = ({ ref_user_search_dialog_control_manager }) => {
	const [modal_opened, modal_commands] = useDisclosure(false);
	const [searchValue, setSearchValue] = useState("");
	const [users, setUsers] = useState<Bio[]>([]);

	const handleSearchChange = (value: string) => {
		setSearchValue(value);
		if (value.trim()) {
			setUsers(search_mentions(value));
		} else {
			setUsers([]);
		}
	};

	const resolveRef = useRef<((bio: Bio | null) => void) | null>(null);
	ref_user_search_dialog_control_manager.current = {
		modal_command_open: () => {
			return new Promise<Bio | null>((resolve, reject) => {
				if (resolveRef.current) {
					reject("Modal is already open");
					return;
				}
				resolveRef.current = resolve;
				modal_commands.open();
			});
		},
	};

	const select_on_change = (_value: string | null, option: ComboboxItem) => {
		if (resolveRef.current) {
			resolveRef.current(option as unknown as Bio);
			resolveRef.current = null;
		}
		setSearchValue("");
		modal_commands.close();
	};

	const modal_on_close = () => {
		if (resolveRef.current) {
			resolveRef.current(null);
			resolveRef.current = null;
		}
		modal_commands.close();
	};

	const select_render_option = (item: ComboboxLikeRenderOptionInput<ComboboxItem>) => {
		const bio = item.option as unknown as Bio;
		return (
			<Group wrap="nowrap">
				<UserImageControl client_id={bio.client_id} selfie={bio.selfie} avatar={bio.avatar} radius="xl" size="lg" />
				<div>
					<UserNameControl client_id={bio.client_id} nickname={bio.nickname} />
					<Text>{bio.status}</Text>
				</div>
			</Group>
		);
	};

	return (
		<Modal
			opened={modal_opened}
			onClose={modal_on_close}
			title={"@mention a user"}
			style={{ position: "relative" }}
			zIndex={400}
			onKeyDown={(e) => {
				if (e.key === "Escape") e.stopPropagation();
			}}
		>
			<Select
				data-autofocus
				placeholder="Type to search users"
				clearable
				searchable
				value={searchValue}
				searchValue={searchValue}
				onSearchChange={handleSearchChange}
				filter={({ options }) => options}
				data={users.map((bio) => ({
					...bio,
					value: bio.client_id,
					label: bio.nickname || bio.client_id,
				}))}
				rightSection={<IconSearch />}
				nothingFoundMessage="No users found"
				renderOption={select_render_option}
				maxDropdownHeight={600}
				comboboxProps={{ zIndex: 500 }}
				onChange={select_on_change}
			/>
		</Modal>
	);
};

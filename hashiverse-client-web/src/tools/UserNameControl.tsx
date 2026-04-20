import { Text, type TextProps, Tooltip } from "@mantine/core";
import type React from "react";
import { useTranslation } from "react-i18next";

interface UserNameControlProps {
	client_id: string;
	nickname?: string | null;
	tooltip?: string | null;
	onClick?: () => void;
	fw?: React.CSSProperties["fontWeight"];
	size?: TextProps["size"];
}

export const UserNameControl: React.FC<UserNameControlProps> = ({ client_id, nickname, tooltip, onClick, fw, size }) => {
	const { t } = useTranslation();
	const style = onClick ? { cursor: "pointer" } : undefined;
	const short_id = `${client_id.substring(0, 8)}...`;

	const text = nickname ? (
		<Text size={size} fw={fw} truncate="end" onClick={onClick} style={style}>
			{nickname}{" "}
			<Text span size={size} fw={fw}>
				({short_id})
			</Text>
		</Text>
	) : (
		<Text size={size} fw={fw} truncate="end" onClick={onClick} style={style}>
			{t("user.anonymous")}: {short_id}
		</Text>
	);

	if (!tooltip) return text;

	return <Tooltip label={tooltip}>{text}</Tooltip>;
};

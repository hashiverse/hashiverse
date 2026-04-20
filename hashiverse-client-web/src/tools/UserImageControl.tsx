import { Avatar, type AvatarProps } from "@mantine/core";
import type React from "react";
import { Tools } from "./Tools.ts";

interface UserImageControlProps extends Omit<AvatarProps, "src"> {
	client_id: string;
	selfie?: string | null;
	avatar?: string | null;
	onClick?: React.MouseEventHandler<HTMLDivElement>;
}

export const UserImageControl: React.FC<UserImageControlProps> = ({ client_id, selfie, avatar, ...rest }) => {
	const src = selfie || Tools.create_avatar(client_id, avatar);
	return <Avatar src={src} {...rest} />;
};

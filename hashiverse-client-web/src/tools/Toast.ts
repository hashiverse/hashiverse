import { notifications } from "@mantine/notifications";

export const Toast = {
	success(message: string): void {
		notifications.show({ color: "teal", message, autoClose: 3000 });
	},
	error(message: string): void {
		notifications.show({ color: "red", message, autoClose: 5000 });
	},
};

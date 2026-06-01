import { notifications } from "@mantine/notifications";

const success_styles = {
	root: {
		backgroundColor: "var(--mantine-color-green-9)",
		borderColor: "var(--mantine-color-green-9)",
	},
	title: { color: "var(--mantine-color-white)" },
	description: { color: "var(--mantine-color-white)" },
	closeButton: {
		color: "var(--mantine-color-white)",
		"&:hover": { backgroundColor: "var(--mantine-color-green-8)" },
	},
};

const error_styles = {
	root: {
		backgroundColor: "var(--mantine-color-red-7)",
		borderColor: "var(--mantine-color-red-7)",
	},
	title: { color: "var(--mantine-color-white)" },
	description: { color: "var(--mantine-color-white)" },
	closeButton: {
		color: "var(--mantine-color-white)",
		"&:hover": { backgroundColor: "var(--mantine-color-red-8)" },
	},
};

export const Toast = {
	success(message: string): void {
		notifications.show({ message, autoClose: 3000, color: "green", styles: success_styles });
	},
	error(message: string): void {
		notifications.show({ message, autoClose: 5000, color: "red", styles: error_styles });
	},
};

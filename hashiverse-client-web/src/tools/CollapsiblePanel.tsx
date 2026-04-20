import { Box, Collapse, Group, Text, UnstyledButton } from "@mantine/core";
import { IconChevronDown } from "@tabler/icons-react";
import type React from "react";
import { useState } from "react";

interface Props {
	title: string;
	children: React.ReactNode;
	defaultOpen?: boolean;
}

export const CollapsiblePanel: React.FC<Props> = ({ title, children, defaultOpen = false }) => {
	const [open, set_open] = useState(defaultOpen);

	return (
		<Box style={{ borderBottom: "1px solid var(--mantine-color-dark-4)" }}>
			<UnstyledButton
				onClick={() => set_open((o) => !o)}
				style={{
					width: "100%",
					padding: "12px 12px",
					backgroundColor: "var(--mantine-color-dark-6)",
				}}
			>
				<Group justify="space-between" align="center">
					<Text fw={600}>{title}</Text>
					<IconChevronDown
						size={16}
						style={{
							transform: open ? "rotate(180deg)" : "rotate(0deg)",
							transition: "transform 200ms ease",
						}}
					/>
				</Group>
			</UnstyledButton>
			<Collapse in={open}>
				<Box pr="md" pb="md" pt="md" pl="xl">
					{children}
				</Box>
			</Collapse>
		</Box>
	);
};

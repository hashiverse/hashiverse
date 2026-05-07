import { Anchor, Stack, Text, Title } from "@mantine/core";
import type React from "react";
import { useNavigate } from "react-router";
import { TabHeader } from "../TabHeader.tsx";

const SUBPAGES: { path: string; label: string; description: string }[] = [{ path: "/geeks/peers", label: "Peers", description: "Dump every peer currently tracked by the local PeerTracker." }];

export const GeeksTab: React.FC = () => {
	const navigate = useNavigate();

	return (
		<div className="FullColumnChildAndParent FullColumnChildScrollable">
			<TabHeader />
			<Stack p="md" gap="md">
				<Title order={2}>Geeks</Title>
				<Stack gap="xs">
					{SUBPAGES.map((subpage) => (
						<Stack key={subpage.path} gap={2}>
							<Anchor onClick={() => navigate(subpage.path)} style={{ cursor: "pointer" }}>
								{subpage.label}
							</Anchor>
							<Text>{subpage.description}</Text>
						</Stack>
					))}
				</Stack>
			</Stack>
		</div>
	);
};

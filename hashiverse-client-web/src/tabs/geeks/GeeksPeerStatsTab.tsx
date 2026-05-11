import { Code, Group, Stack, Text, Title, Tooltip } from "@mantine/core";
import type React from "react";
import { useEffect, useState } from "react";
import { useParams } from "react-router";
import type { HashiverseClientWasmProxy } from "../../Hashiverse.ts";
import { ErrorTab } from "../ErrorTab.tsx";
import { TabHeader } from "../TabHeader.tsx";

interface Props {
	hashiverse: HashiverseClientWasmProxy;
}

export const GeeksPeerStatsTab: React.FC<Props> = ({ hashiverse }) => {
	const { peer_id_hex } = useParams();
	const [doc, set_doc] = useState<unknown | null>(null);
	const [error, set_error] = useState<string | null>(null);
	const [loading, set_loading] = useState(false);

	useEffect(() => {
		if (!peer_id_hex) return;
		set_loading(true);
		set_error(null);
		hashiverse
			.get_peer_stats_v1(peer_id_hex)
			.then((result) => {
				set_doc(result);
			})
			.catch((e: unknown) => {
				set_error(e instanceof Error ? e.message : String(e));
			})
			.finally(() => {
				set_loading(false);
			});
	}, [hashiverse, peer_id_hex]);

	if (!peer_id_hex) return <ErrorTab />;

	return (
		<div className="FullColumnChildAndParent FullColumnChildScrollable">
			<TabHeader />
			<Stack p="md" gap="md">
				<Group justify="space-between" align="center">
					<Title order={2}>Peer stats</Title>
					<Tooltip label={peer_id_hex} withArrow>
						<Text size="xs" ff="monospace" style={{ cursor: "copy" }} onClick={() => navigator.clipboard?.writeText(peer_id_hex)}>
							{peer_id_hex.slice(0, 16)}…
						</Text>
					</Tooltip>
				</Group>
				{loading && <Text c="dimmed">Loading…</Text>}
				{error && (
					<Text c="red" size="sm">
						{error}
					</Text>
				)}
				{doc !== null && !loading && !error && (
					<Code block style={{ whiteSpace: "pre-wrap", wordBreak: "break-word" }}>
						{JSON.stringify(doc, null, 2)}
					</Code>
				)}
			</Stack>
		</div>
	);
};

import { Anchor, Button, Group, Stack, Table, Text, Title, Tooltip } from "@mantine/core";
import type React from "react";
import { useCallback, useEffect, useMemo, useState } from "react";
import { useNavigate } from "react-router";
import type { HashiverseClientWasmProxy, PeerInfoV1 } from "../../Hashiverse.ts";
import { TabHeader } from "../TabHeader.tsx";

interface Props {
	hashiverse: HashiverseClientWasmProxy;
}

type SortKey = keyof PeerInfoV1;
type SortDirection = "asc" | "desc";

const COLUMNS: { key: SortKey; label: string }[] = [
	{ key: "peer_id_hex", label: "Peer ID" },
	{ key: "address", label: "Address" },
	{ key: "version", label: "Version" },
	{ key: "timestamp_millis", label: "Last seen" },
	{ key: "pow_initial", label: "PoW initial" },
	{ key: "pow_current_day", label: "PoW day" },
	{ key: "pow_current_month", label: "PoW month" },
];

function compare_values(a: string | number, b: string | number, direction: SortDirection): number {
	if (a < b) return direction === "asc" ? -1 : 1;
	if (a > b) return direction === "asc" ? 1 : -1;
	return 0;
}

function format_relative(timestamp_millis: number): string {
	const delta_ms = Date.now() - timestamp_millis;
	if (delta_ms < 0) return "in the future";
	const seconds = Math.floor(delta_ms / 1000);
	if (seconds < 60) return `${seconds}s ago`;
	const minutes = Math.floor(seconds / 60);
	if (minutes < 60) return `${minutes}m ago`;
	const hours = Math.floor(minutes / 60);
	if (hours < 24) return `${hours}h ago`;
	const days = Math.floor(hours / 24);
	return `${days}d ago`;
}

export const GeeksPeersTab: React.FC<Props> = ({ hashiverse }) => {
	const navigate = useNavigate();
	const [peers, set_peers] = useState<PeerInfoV1[] | null>(null);
	const [error, set_error] = useState<string | null>(null);
	const [loading, set_loading] = useState(false);
	const [sort_key, set_sort_key] = useState<SortKey>("timestamp_millis");
	const [sort_direction, set_sort_direction] = useState<SortDirection>("desc");

	const refresh = useCallback(async () => {
		set_loading(true);
		set_error(null);
		try {
			const result = await hashiverse.get_all_known_peers_v1();
			set_peers(result);
		} catch (e) {
			set_error(e instanceof Error ? e.message : String(e));
		} finally {
			set_loading(false);
		}
	}, [hashiverse]);

	useEffect(() => {
		refresh();
	}, [refresh]);

	const sorted_peers = useMemo(() => {
		if (!peers) return null;
		const copy = [...peers];
		copy.sort((a, b) => compare_values(a[sort_key], b[sort_key], sort_direction));
		return copy;
	}, [peers, sort_key, sort_direction]);

	const on_header_click = (key: SortKey) => {
		if (key === sort_key) {
			set_sort_direction((prev) => (prev === "asc" ? "desc" : "asc"));
		} else {
			set_sort_key(key);
			set_sort_direction("asc");
		}
	};

	return (
		<div className="FullColumnChildAndParent FullColumnChildScrollable">
			<TabHeader />
			<Stack p="md" gap="md">
				<Group justify="space-between" align="center">
					<Title order={2}>Peers</Title>
					<Group gap="sm">
						<Text size="sm" c="dimmed">
							{peers ? `${peers.length} known` : "loading…"}
						</Text>
						<Button onClick={refresh} loading={loading} size="xs">
							Refresh
						</Button>
					</Group>
				</Group>
				{error && (
					<Text c="red" size="sm">
						{error}
					</Text>
				)}
				{sorted_peers && sorted_peers.length === 0 && <Text c="dimmed">No peers tracked yet.</Text>}
				{sorted_peers && sorted_peers.length > 0 && (
					<Table.ScrollContainer minWidth={900} type="native">
						<Table striped withTableBorder withColumnBorders highlightOnHover stickyHeader>
							<Table.Thead>
								<Table.Tr>
									{COLUMNS.map((column) => {
										const is_sorted = column.key === sort_key;
										const indicator = is_sorted ? (sort_direction === "asc" ? " ▲" : " ▼") : "";
										return (
											<Table.Th key={column.key} style={{ cursor: "pointer", userSelect: "none" }} onClick={() => on_header_click(column.key)}>
												{column.label}
												{indicator}
											</Table.Th>
										);
									})}
								</Table.Tr>
							</Table.Thead>
							<Table.Tbody>
								{sorted_peers.map((peer) => (
									<Table.Tr key={peer.peer_id_hex}>
										<Table.Td>
											<Tooltip label={peer.peer_id_hex} withArrow>
												<Anchor size="xs" ff="monospace" onClick={() => navigate(`/geeks/peer-stats/${peer.peer_id_hex}`)} style={{ cursor: "pointer" }}>
													{peer.peer_id_hex.slice(0, 16)}…
												</Anchor>
											</Tooltip>
										</Table.Td>
										<Table.Td>
											<Text size="xs" ff="monospace">
												{peer.address}
											</Text>
										</Table.Td>
										<Table.Td>
											<Text size="xs">{peer.version}</Text>
										</Table.Td>
										<Table.Td>
											<Tooltip label={new Date(peer.timestamp_millis).toISOString()} withArrow>
												<Text size="xs">{format_relative(peer.timestamp_millis)}</Text>
											</Tooltip>
										</Table.Td>
										<Table.Td>
											<Text size="xs" ta="right">
												{peer.pow_initial}
											</Text>
										</Table.Td>
										<Table.Td>
											<Text size="xs" ta="right">
												{peer.pow_current_day}
											</Text>
										</Table.Td>
										<Table.Td>
											<Text size="xs" ta="right">
												{peer.pow_current_month}
											</Text>
										</Table.Td>
									</Table.Tr>
								))}
							</Table.Tbody>
						</Table>
					</Table.ScrollContainer>
				)}
			</Stack>
		</div>
	);
};

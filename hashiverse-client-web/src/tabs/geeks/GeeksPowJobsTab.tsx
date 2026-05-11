import { Group, Progress, Stack, Table, Text, Title } from "@mantine/core";
import type React from "react";
import { useCallback, useEffect, useRef, useState } from "react";
import type { HashiverseClientWasmProxy, PowJobStatusV1 } from "../../Hashiverse.ts";
import { TabHeader } from "../TabHeader.tsx";

interface Props {
	hashiverse: HashiverseClientWasmProxy;
}

const POLL_INTERVAL_MS = 500;

export const GeeksPowJobsTab: React.FC<Props> = ({ hashiverse }) => {
	const [active_jobs, set_active_jobs] = useState<PowJobStatusV1[] | null>(null);
	const [error, set_error] = useState<string | null>(null);
	const in_flight_ref = useRef(false);

	const refresh = useCallback(async () => {
		if (in_flight_ref.current) return;
		in_flight_ref.current = true;
		try {
			const result = await hashiverse.get_active_pow_jobs_v1();
			set_active_jobs(result);
			set_error(null);
		} catch (e) {
			set_error(e instanceof Error ? e.message : String(e));
		} finally {
			in_flight_ref.current = false;
		}
	}, [hashiverse]);

	useEffect(() => {
		refresh();
		const handle = setInterval(refresh, POLL_INTERVAL_MS);
		return () => clearInterval(handle);
	}, [refresh]);

	return (
		<div className="FullColumnChildAndParent FullColumnChildScrollable">
			<TabHeader />
			<Stack p="md" gap="md">
				<Group justify="space-between" align="center">
					<Title order={2}>Current PoW jobs</Title>
					<Text size="sm" c="dimmed">
						{active_jobs ? `${active_jobs.length} active` : "loading…"}
					</Text>
				</Group>
				{error && (
					<Text c="red" size="sm">
						{error}
					</Text>
				)}
				{active_jobs && active_jobs.length === 0 && <Text c="dimmed">No active PoW jobs.</Text>}
				{active_jobs && active_jobs.length > 0 && (
					<Table.ScrollContainer minWidth={600} type="native">
						<Table striped withTableBorder withColumnBorders highlightOnHover stickyHeader>
							<Table.Thead>
								<Table.Tr>
									<Table.Th>Label</Table.Th>
									<Table.Th style={{ textAlign: "right" }}>Target PoW</Table.Th>
									<Table.Th style={{ textAlign: "right" }}>Best so far</Table.Th>
									<Table.Th>Progress</Table.Th>
								</Table.Tr>
							</Table.Thead>
							<Table.Tbody>
								{active_jobs.map((job, index) => {
									const progress_percent = job.pow_min === 0 ? 100 : Math.min(100, (job.best_pow_so_far / job.pow_min) * 100);
									return (
										<Table.Tr key={`${job.label}-${index}`}>
											<Table.Td>
												<Text size="xs" ff="monospace">
													{job.label}
												</Text>
											</Table.Td>
											<Table.Td>
												<Text size="xs" ta="right">
													{job.pow_min}
												</Text>
											</Table.Td>
											<Table.Td>
												<Text size="xs" ta="right">
													{job.best_pow_so_far}
												</Text>
											</Table.Td>
											<Table.Td>
												<Progress value={progress_percent} size="md" />
											</Table.Td>
										</Table.Tr>
									);
								})}
							</Table.Tbody>
						</Table>
					</Table.ScrollContainer>
				)}
			</Stack>
		</div>
	);
};

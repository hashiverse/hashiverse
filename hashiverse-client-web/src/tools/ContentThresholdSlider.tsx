import { Box, Group, Slider, Text } from "@mantine/core";
import type React from "react";

export const THRESHOLD_STEPS = [0, 10, 100, 1_000, 10_000, 100_000, 1_000_000_000] as const;
export type ThresholdValue = (typeof THRESHOLD_STEPS)[number];

const STEP_LABELS = ["0", "10", "100", "1K", "10K", "100K", "∞"];

const MARKS = THRESHOLD_STEPS.map((_, i) => ({
	value: i,
	label: STEP_LABELS[i],
}));

function value_to_pos(value: number): number {
	const steps = THRESHOLD_STEPS as readonly number[];
	const exact = steps.indexOf(value);
	if (exact >= 0) return exact;
	for (let i = 0; i < steps.length; i++) {
		if (steps[i] >= value) return i;
	}
	return steps.length - 1;
}

function pos_to_value(pos: number): number {
	return (THRESHOLD_STEPS as readonly number[])[Math.max(0, Math.min(pos, THRESHOLD_STEPS.length - 1))];
}

interface Props {
	label: string;
	icon: React.FC<{ size?: number }>;
	value: number;
	onChange: (value: number) => void;
	disabled?: boolean;
	max_value?: number;
}

export const ContentThresholdSlider: React.FC<Props> = ({ label, icon: Icon, value, onChange, disabled = false, max_value }) => {
	const max_pos = max_value !== undefined ? value_to_pos(max_value) : undefined;
	const handle_change = (pos: number) => {
		const clamped = max_pos !== undefined ? Math.min(pos, max_pos) : pos;
		onChange(pos_to_value(clamped));
	};
	return (
		<Box mb={44}>
			<Group gap="xs" mb="sm">
				<Icon size={16} />
				<Text size="sm" fw={500} tt="capitalize" c={disabled ? "dimmed" : undefined}>
					{label}
				</Text>
			</Group>
			<Slider min={0} max={6} step={1} value={value_to_pos(value)} onChange={handle_change} marks={MARKS} label={(pos) => STEP_LABELS[pos]} disabled={disabled} />
		</Box>
	);
};

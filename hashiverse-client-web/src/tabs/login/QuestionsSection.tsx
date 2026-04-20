import { Button, Progress, Stack, Text, TextInput } from "@mantine/core";
import type React from "react";
import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { useNavigate } from "react-router";
import { Hashiverse, type HashiverseClientWasmProxy } from "../../Hashiverse.ts";
import { CollapsiblePanel } from "../../tools/CollapsiblePanel.tsx";
import { LOGIN_QUESTIONS_FIXED, LOGIN_QUESTIONS_VARIABLE, question_to_i18n_key } from "./login_questions.ts";

interface Props {
	on_login: (hashiverse: HashiverseClientWasmProxy) => void;
}

export const QuestionsSection: React.FC<Props> = ({ on_login }) => {
	const { t } = useTranslation();
	const navigate = useNavigate();

	const NUM_QUESTIONS_FIXED = LOGIN_QUESTIONS_FIXED.length;
	const NUM_QUESTIONS_VARIABLE = 5;

	const [answers_fixed, set_answers_fixed] = useState<string[]>(empty(NUM_QUESTIONS_FIXED));
	const [questions_variable, set_questions_variable] = useState<string[]>(empty(NUM_QUESTIONS_VARIABLE));
	const [answers_variable, set_answers_variable] = useState<string[]>(empty(NUM_QUESTIONS_VARIABLE));
	const [passphrase_fixed, set_passphrase_fixed] = useState("");
	const [passphrase_variable, set_passphrase_variable] = useState("");
	const [passphrase, set_passphrase] = useState("");
	const [loading, set_loading] = useState(false);
	const [answers_total_length, set_answers_total_length] = useState(0);

	useEffect(() => {
		(async () => {
			const fixed_hash = await hash(answers_fixed.map(strip).join("."));
			set_passphrase_fixed(fixed_hash);

			const all = await Promise.all(
				LOGIN_QUESTIONS_VARIABLE.map(async (q) => ({
					q,
					h: await hash(q + fixed_hash),
				})),
			);
			all.sort((a, b) => a.h.localeCompare(b.h));
			set_questions_variable(all.slice(0, NUM_QUESTIONS_VARIABLE).map((a) => a.q));
			set_answers_variable(empty(NUM_QUESTIONS_VARIABLE));
		})();
	}, [answers_fixed]);

	useEffect(() => {
		(async () => set_passphrase_variable(await hash(answers_variable.map(strip).join("."))))();
	}, [answers_variable]);

	useEffect(() => {
		let total = [...answers_fixed, ...answers_variable].join("").length;
		total = Math.min(total, 100);
		set_answers_total_length(total);
	}, [answers_fixed, answers_variable]);

	useEffect(() => {
		(async () => set_passphrase(await hash(passphrase_fixed + passphrase_variable)))();
	}, [passphrase_fixed, passphrase_variable]);

	const login = useCallback(async () => {
		if (!passphrase) return;
		set_loading(true);
		try {
			const new_hv = await Hashiverse.create_from_keyphrase(passphrase);
			on_login(new_hv);
			navigate("/");
		} finally {
			set_loading(false);
		}
	}, [passphrase, on_login, navigate]);

	return (
		<CollapsiblePanel title={t("login.questions_title")}>
			<Stack>
				<Text>{t("login.instruction_questions")}</Text>

				<Progress value={answers_total_length} color={answers_total_length < 32 ? "red" : answers_total_length < 64 ? "orange" : "green"} />

				{LOGIN_QUESTIONS_FIXED.map((question, index) => (
					<TextInput
						key={`fixed-${index}-${question}`}
						label={t(`login.question.${question_to_i18n_key(question)}`, {
							defaultValue: question,
						})}
						value={answers_fixed[index]}
						onChange={(e) => {
							const a = [...answers_fixed];
							a[index] = e.target.value;
							set_answers_fixed(a);
						}}
					/>
				))}

				<Progress value={answers_total_length} color={answers_total_length < 32 ? "red" : answers_total_length < 64 ? "orange" : "green"} />

				{questions_variable.map((question, index) => (
					<TextInput
						key={`variable-${index}-${question}`}
						label={t(`login.question.${question_to_i18n_key(question)}`, {
							defaultValue: question,
						})}
						value={answers_variable[index]}
						onChange={(e) => {
							const a = [...answers_variable];
							a[index] = e.target.value;
							set_answers_variable(a);
						}}
					/>
				))}

				<Progress value={answers_total_length} color={answers_total_length < 32 ? "red" : answers_total_length < 64 ? "orange" : "green"} />

				<Button onClick={login} loading={loading} disabled={!passphrase}>
					{t("login.login_button")}
				</Button>
			</Stack>
		</CollapsiblePanel>
	);
};

// Utilities

const empty = (n: number) => Array(n).fill("");

const strip = (str: string) => str.toLowerCase().replace(/[^a-z0-9]+/gi, "");

const hash = async (str: string) => {
	const data = new TextEncoder().encode(str);
	const buf = await crypto.subtle.digest("SHA-256", data);
	return Array.from(new Uint8Array(buf))
		.map((b) => b.toString(16).padStart(2, "0"))
		.join("");
};

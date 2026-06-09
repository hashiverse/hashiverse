import type React from "react";
import { useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { useNavigate } from "react-router";
import type { HashiverseClientWasmProxy } from "../Hashiverse.ts";
import { Spinner } from "./Spinner.tsx";

interface Props {
	hashiverse: HashiverseClientWasmProxy;
}

const POLL_INTERVAL_MS = 1000;
// Treat work in the last second as "still busy" so a once-a-second poll doesn't flicker and still
// catches bursts that began and ended between two polls. Matches POLL_INTERVAL_MS.
const BUSY_WINDOW_MS = 1000;

/// A small spinning globe pinned to the bottom-left corner whenever there is background PoW work
/// (e.g. a post still propagating to its servers). Clicking it opens the geeks PoW-jobs page, and
/// while work is pending it warns before the tab unloads (close/reload) — the only "navigate away"
/// that kills the worker and loses in-flight background work (in-app navigation keeps it running).
export const PowBusyIndicator: React.FC<Props> = ({ hashiverse }) => {
	const [busy, set_busy] = useState(false);
	// Mirror of `busy` readable synchronously from the (non-React) beforeunload handler.
	const busy_ref = useRef(false);
	const in_flight_ref = useRef(false);
	const navigate = useNavigate();
	const { t } = useTranslation();

	const refresh = useCallback(async () => {
		if (in_flight_ref.current) return;
		in_flight_ref.current = true;
		try {
			const is_busy = await hashiverse.is_pow_busy_v1(BUSY_WINDOW_MS);
			busy_ref.current = is_busy;
			set_busy(is_busy);
		} catch {
			// If the worker isn't reachable, assume idle rather than getting stuck spinning.
			busy_ref.current = false;
			set_busy(false);
		} finally {
			in_flight_ref.current = false;
		}
	}, [hashiverse]);

	useEffect(() => {
		refresh();
		const handle = setInterval(refresh, POLL_INTERVAL_MS);
		return () => clearInterval(handle);
	}, [refresh]);

	// Warn before the tab closes/reloads while background work is still pending (the worker, and so
	// the in-flight posts, would be lost). The prompt text is browser-controlled.
	useEffect(() => {
		const on_before_unload = (event: BeforeUnloadEvent) => {
			if (busy_ref.current) {
				event.preventDefault();
				event.returnValue = "";
			}
		};
		window.addEventListener("beforeunload", on_before_unload);
		return () => window.removeEventListener("beforeunload", on_before_unload);
	}, []);

	// Always mounted so we can fade the opacity in/out; when idle it's invisible and non-interactive.
	return (
		<button
			type="button"
			onClick={() => navigate("/geeks/pow-jobs")}
			title={t("tooltip.background_work_in_progress")}
			aria-label={t("tooltip.background_work_in_progress")}
			aria-hidden={!busy}
			tabIndex={busy ? 0 : -1}
			style={{
				position: "fixed",
				bottom: 12,
				left: 12,
				zIndex: 200,
				padding: 0,
				border: "none",
				background: "transparent",
				lineHeight: 0,
				opacity: busy ? 1 : 0,
				pointerEvents: busy ? "auto" : "none",
				cursor: "pointer",
				transition: "opacity 400ms ease-in-out",
			}}
		>
			<Spinner size={28} />
		</button>
	);
};

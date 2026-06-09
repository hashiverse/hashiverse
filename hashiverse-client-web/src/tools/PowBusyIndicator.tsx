import type React from "react";
import { useCallback, useEffect, useRef, useState } from "react";
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
/// (e.g. a post still propagating to its servers). Clicking it opens the geeks PoW-jobs page.
export const PowBusyIndicator: React.FC<Props> = ({ hashiverse }) => {
	const [busy, set_busy] = useState(false);
	const in_flight_ref = useRef(false);
	const navigate = useNavigate();

	const refresh = useCallback(async () => {
		if (in_flight_ref.current) return;
		in_flight_ref.current = true;
		try {
			set_busy(await hashiverse.is_pow_busy_v1(BUSY_WINDOW_MS));
		} catch {
			// If the worker isn't reachable, assume idle rather than getting stuck spinning.
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

	// Always mounted so we can fade the opacity in/out; when idle it's invisible and non-interactive.
	return (
		<button
			type="button"
			onClick={() => navigate("/geeks/pow-jobs")}
			title="Background work in progress — click for details"
			aria-label="Background work in progress"
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

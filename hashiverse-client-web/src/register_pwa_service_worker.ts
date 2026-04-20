/**
 * Registers the PWA (progressive web app) service worker.
 * Only runs in production builds — import.meta.env.PROD is false during `rsbuild dev`.
 * Safe alongside HashiverseWorker: service workers and dedicated workers use separate browser APIs.
 *
 * @module
 */
export function register_pwa_service_worker(): void {
	if (!import.meta.env.PROD) return;
	if (!("serviceWorker" in navigator)) return;

	window.addEventListener("load", () => {
		navigator.serviceWorker
			.register("/pwa_service_worker.js", { scope: "/" })
			.then((registration) => {
				console.log("[SW] Registered, scope:", registration.scope);
				registration.addEventListener("updatefound", () => {
					const new_worker = registration.installing;
					if (!new_worker) return;
					new_worker.addEventListener("statechange", () => {
						if (new_worker.state === "installed" && navigator.serviceWorker.controller) {
							console.log("[SW] New version available, activating.");
						}
					});
				});
			})
			.catch((error) => console.error("[SW] Registration failed:", error));
	});
}

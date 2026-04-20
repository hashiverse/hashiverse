// Service Worker for Hashiverse PWA
// Strategy: cache-first for same-origin GETs (app shell + WASM), pass-through for everything else.
// skipWaiting + clients.claim ensures new SW activates immediately on deploy.
// Auto-update: rsbuild uses content-hash filenames; pwa_service_worker.js has no hash so the browser
// byte-compares it on every navigation and installs a new SW when it changes.

const CACHE_NAME = "hashiverse-shell-v1";

// Path prefixes for P2P/network calls that must never be cached
const PASS_THROUGH_PREFIXES = [];

function shouldPassThrough(url) {
	const { pathname } = new URL(url);
	return PASS_THROUGH_PREFIXES.some((prefix) => pathname.startsWith(prefix));
}

self.addEventListener("install", (event) => {
	self.skipWaiting();
	event.waitUntil(caches.open(CACHE_NAME).then((cache) => cache.addAll(["/"])));
});

self.addEventListener("activate", (event) => {
	event.waitUntil(Promise.all([self.clients.claim(), caches.keys().then((keys) => Promise.all(keys.filter((k) => k !== CACHE_NAME).map((k) => caches.delete(k))))]));
});

async function handleShareTarget(request) {
	const data = await request.formData();
	const text = data.get("text") ?? "";
	const url = data.get("url") ?? "";
	const file = data.get("media");

	let has_file = false;
	if (file && file.size > 0) {
		const share_cache = await caches.open("hashiverse-share-v1");
		await share_cache.put("/share-incoming-file", new Response(file, { headers: { "Content-Type": file.type } }));
		has_file = true;
	}

	const params = new URLSearchParams();
	params.set("share", "1");
	if (text) params.set("text", text);
	if (url) params.set("url", url);
	if (has_file) params.set("has_file", "true");

	return Response.redirect(`/#/?${params.toString()}`, 303);
}

self.addEventListener("fetch", (event) => {
	const { request } = event;
	const req_url = new URL(request.url);
	if (req_url.pathname === "/share-target" && request.method === "POST") {
		event.respondWith(handleShareTarget(request));
		return;
	}
	if (request.method !== "GET") return;
	const url = new URL(request.url);
	if (url.protocol !== "http:" && url.protocol !== "https:") return;
	if (url.origin !== self.location.origin || shouldPassThrough(request.url)) return;

	event.respondWith(
		caches.open(CACHE_NAME).then(async (cache) => {
			const cached = await cache.match(request);
			if (cached) return cached;
			const response = await fetch(request);
			if (response.ok) cache.put(request, response.clone());
			return response;
		}),
	);
});

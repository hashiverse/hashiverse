export const HASHIVERSE_DOMAINS = ["app.hashiverse.com", "app.hashiverse.eu", "app.hashiverse.ch"] as const;

export function get_alternate_domain_urls(hash_path: string): { display_name: string; url: string }[] {
	const current_hostname = window.location.hostname;
	return HASHIVERSE_DOMAINS.filter((d) => d !== current_hostname).map((d) => ({
		display_name: d.replace(/^app\./, ""),
		url: `https://${d}/${hash_path}`,
	}));
}

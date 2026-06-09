// Pure, easily testable: maps the current hostname to the app host URL.
export function app_url_for_host(host: string): string {
    if (host === "localhost" || host === "127.0.0.1") {
        return "http://localhost:3000";
    }
    // Replace a leading "www" (followed by "." or "-") with "app", rather than
    // prepending. host.slice(3) keeps the separator: ".hashiverse.ch" / "-hashiverse-ch.b-cdn.net".
    const app_host = host.startsWith("www") ? `app${host.slice(3)}` : `app.${host}`;
    return `https://${app_host}`;
}

// Wires the computed href onto an anchor by id; no-op if the element is absent.
export function set_app_link(id: string): void {
    const anchor = document.getElementById(id) as HTMLAnchorElement | null;
    if (!anchor) return;
    anchor.href = app_url_for_host(window.location.hostname);
}

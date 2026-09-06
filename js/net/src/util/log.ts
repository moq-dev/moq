/** Renders a relay URL without user credentials, query parameters, or fragment. */
export function redact(url: URL): string {
	return `${url.origin}${url.pathname}`;
}

/** Whether the consumer environment enables development diagnostics. */
export function dev(): boolean {
	// Bundlers expose a boolean DEV; Bun exposes environment variables as strings.
	// @ts-ignore - Some environments don't recognize import.meta.env
	const env = import.meta.env;
	if (typeof env === "undefined") return false;
	if (typeof env.DEV === "boolean") return env.DEV;
	return env.NODE_ENV !== "production" && env.MODE !== "production";
}

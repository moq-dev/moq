/** Renders a relay URL without user credentials, query parameters, or fragment. */
export function redact(url: URL): string {
	return `${url.origin}${url.pathname}`;
}

/**
 * Renders a URL for logging as its origin plus path, dropping the query and fragment.
 *
 * A relay URL carries its auth token in `?jwt=`, so this is the only safe way to log one.
 */
export function redact(url: URL): string {
	return `${url.origin}${url.pathname}`;
}

/**
 * Whether to emit diagnostics, matching the `DEV` check in `@moq/signals`.
 *
 * Read per call instead of cached in a module constant so the value tracks the
 * environment the consumer's bundler defines, not whenever this module loaded.
 */
export function dev(): boolean {
	// @ts-ignore - Some environments don't recognize import.meta.env
	return typeof import.meta.env !== "undefined" && import.meta.env?.MODE !== "production";
}

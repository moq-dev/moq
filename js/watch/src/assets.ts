let hosted: URL | undefined;

/**
 * Load the audio worklet from files under `url` instead of blob: URLs, for pages whose CSP refuses blob:.
 *
 * Copy `@moq/watch/assets/*` into that directory, again on every upgrade. Applies to media started afterwards.
 */
export function assets(url: string | URL): void {
	const base = new URL(url, document.baseURI);
	// Without the trailing slash, the files would resolve beside the directory instead of inside it.
	if (!base.pathname.endsWith("/")) throw new Error(`assets URL must end with "/": ${base}`);
	hosted = base;
}

/** The directory set by {@link assets}, or undefined to use blob: URLs. */
export function hostedAssets(): URL | undefined {
	return hosted;
}

/** One package's directory of self-hosted worklets, workers, and wasm, for pages whose CSP refuses blob:. */
export class Directory {
	#url: URL | undefined;

	/** Load from files under `url`, which must end with "/" and have no query or fragment, instead of blob: URLs and bundler paths. */
	set(url: string | URL): void {
		const base = new URL(url, document.baseURI);
		// Without the trailing slash, the files would resolve beside the directory instead of inside it.
		if (!base.pathname.endsWith("/")) throw new Error(`assets URL must end with "/": ${base}`);
		// File names resolve against the path alone and libav appends its own to the href, so a query or fragment would be dropped or corrupt the path.
		if (base.search || base.hash) throw new Error(`assets URL must not have a query or fragment: ${base}`);
		this.#url = base;
	}

	/** The directory, or undefined to use blob: URLs and bundler paths. */
	get url(): URL | undefined {
		return this.#url;
	}
}

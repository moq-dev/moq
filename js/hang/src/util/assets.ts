/** One package's directory of self-hosted worklets, workers, and wasm, for pages whose CSP refuses blob:. */
export class Directory {
	#url: URL | undefined;

	/** Load from files under `url`, which must end with "/" and have no query or fragment, instead of blob: URLs and bundler paths. */
	set(url: string | URL): void {
		const base = new URL(url, document.baseURI);
		// Files resolve inside the directory and libav appends "/<file>" to the href minus its last character,
		// so the href must be exactly the directory: a trailing "/" and no query or fragment, even an empty one.
		if (base.href !== new URL(".", base).href) {
			throw new Error(`assets URL must end with "/" and have no query or fragment: ${base}`);
		}
		this.#url = base;
	}

	/** The directory, or undefined to use blob: URLs and bundler paths. */
	get url(): URL | undefined {
		return this.#url;
	}
}

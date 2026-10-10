declare module "*?worklet" {
	/** Resolves the script's URL: the hosted file under `base`, or a blob: URL without one. */
	const url: (base?: URL) => Promise<string>;
	export default url;
}

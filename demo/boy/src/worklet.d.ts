// moq-boy's tsc follows @moq/watch into decoder.ts, which imports a ?worklet URL.
// This program does not include the shared declaration.
declare module "*?worklet" {
	/** Resolves the script's URL: the hosted file under `base`, or a blob: URL without one. */
	const url: (base?: URL) => Promise<string>;
	export default url;
}

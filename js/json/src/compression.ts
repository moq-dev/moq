/** How a JSON track compresses its frames. `"none"` (the default) is uncompressed JSON. */
export type Compression = "none" | "deflate";

/** Whether `compression` is group-scoped DEFLATE. */
export function isDeflate(compression?: Compression): boolean {
	switch (compression) {
		case undefined:
		case "none":
			return false;
		case "deflate":
			return true;
		default:
			throw new Error(`unsupported compression: ${String(compression)}`);
	}
}

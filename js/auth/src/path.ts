/**
 * Segment-aware root matching, mirroring `@moq/net`'s path module.
 *
 * A token's root and a connection's path are literal paths, so overlapping them is
 * prefix arithmetic; everything beneath is a pattern from `@moq/pattern`, re-exported
 * from this package so a token minting service does not pull in the networking
 * stack. The normalization and boundary rules must stay identical to the Rust
 * `moq-auth` crate's `path` module, which mints and checks the same tokens. Minting
 * checks a root with {@link validate} instead, so a token never names a path no peer
 * can spell.
 *
 * Every function below but {@link validate} assumes its arguments are already {@link normalize}d.
 *
 * @internal
 * @module
 */

/** The most UTF-8 bytes a path may hold, matching `@moq/net`'s `Path.MAX_BYTES`. */
const MAX_BYTES = 4096;

const utf8 = new TextEncoder();

/**
 * Throw on a root `@moq/net`'s `Path.from` and `Path.decode` refuse: one longer than 4096
 * bytes, with a leading, trailing, or doubled `/`, or with a `\` before anything but `\` or `/`.
 */
export function validate(root: string): void {
	const invalid = (reason: string) => new Error(`invalid root ${JSON.stringify(root)} ${reason}`);
	if (utf8.encode(root).length > MAX_BYTES) throw invalid(`is longer than ${MAX_BYTES} bytes`);
	if (root.startsWith("/") || root.endsWith("/") || root.includes("//")) throw invalid("contains an empty segment");
	if (/\\(?![\\/])/.test(root.replaceAll("\\\\", ""))) {
		throw invalid("contains a `\\` that escapes neither `\\` nor `/`");
	}
}

/**
 * Trim leading and trailing slashes and collapse consecutive ones, so all slashes
 * are implicit at boundaries and `/foo//bar/` is the same path as `foo/bar`.
 */
export function normalize(path: string): string {
	return path
		.split("/")
		.filter((part) => part !== "")
		.join("/");
}

/**
 * `path` with `prefix` and its trailing delimiter removed, or `undefined` when
 * `prefix` is not a segment-aligned prefix of `path`.
 */
export function stripPrefix(path: string, prefix: string): string | undefined {
	if (prefix === "") return path;
	if (!path.startsWith(prefix)) return undefined;

	const rest = path.slice(prefix.length);
	if (rest === "") return "";
	if (rest.startsWith("/")) return rest.slice(1);
	return undefined;
}

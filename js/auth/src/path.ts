/**
 * Segment-aware root matching, mirroring `@moq/net`'s path module.
 *
 * A token's root and a connection's path are literal paths, so overlapping them is
 * prefix arithmetic; everything beneath is a pattern from `@moq/pattern`, re-exported
 * from this package so a token minting service does not pull in the networking
 * stack. The normalization and boundary rules must stay identical to the Rust
 * `moq-auth` crate's `path` module, which mints and checks the same tokens.
 *
 * Every function below assumes its arguments are already {@link normalize}d.
 *
 * @internal
 * @module
 */

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

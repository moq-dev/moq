/**
 * Segment-aware path matching, mirroring `@moq/net`'s path module.
 *
 * @moq/token is deliberately free of other @moq/* dependencies so a token minting
 * service doesn't pull in the networking stack, so the handful of prefix operations
 * {@link authorize} needs live here instead. The normalization and boundary rules
 * must stay identical to the Rust `moq-token` crate's `path` module, which mints and
 * checks the same tokens.
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
 * True when `path` starts with `prefix` on a segment boundary, so `foo` does not
 * match `foobar`. The empty prefix matches everything.
 */
export function hasPrefix(path: string, prefix: string): boolean {
	return stripPrefix(path, prefix) !== undefined;
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

/** Join two relative paths, skipping the delimiter when either side is empty. */
export function join(base: string, other: string): string {
	if (base === "") return other;
	if (other === "") return base;
	return `${base}/${other}`;
}

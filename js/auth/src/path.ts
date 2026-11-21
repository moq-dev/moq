/**
 * Segment-aware root matching, mirroring `@moq/net`'s path module.
 *
 * A token's root and a connection's path are literal paths, so overlapping them is
 * prefix arithmetic; everything beneath is a pattern from `@moq/pattern`, re-exported
 * from this package so a token minting service does not pull in the networking
 * stack. The normalization and boundary rules must stay identical to the Rust
 * `moq-auth` crate's `path` module, which mints and checks the same tokens. Minting
 * checks a root with {@link signable} instead, so a token never names a path no peer
 * can spell.
 *
 * Every function below but {@link validate} and {@link signable} assumes its arguments are
 * already {@link normalize}d.
 *
 * @internal
 * @module
 */

import { parts } from "@moq/pattern";

/** The most UTF-8 bytes a path may hold, matching `@moq/net`'s path limit. */
const MAX_BYTES = 4096;

const utf8 = new TextEncoder();

/**
 * Throw on a root longer than 4096 bytes, with a lone surrogate, with a `\` before anything but
 * `\` or `/`, or with a leading, trailing, or doubled `/` no `\` escapes. Parsing a key's scope
 * checks this, so keys minted before {@link signable} tightened it still load.
 */
export function validate(root: string): void {
	const invalid = (reason: string) => new Error(`invalid root ${JSON.stringify(root)} ${reason}`);
	// The `u` flag reads a pair as one scalar, so only a lone surrogate matches.
	if (/\p{Surrogate}/u.test(root)) throw invalid("contains a lone surrogate");
	if (utf8.encode(root).length > MAX_BYTES) throw invalid(`is longer than ${MAX_BYTES} bytes`);
	if (/\\(?![\\/])/.test(root.replaceAll("\\\\", ""))) {
		throw invalid("contains a `\\` that escapes neither `\\` nor `/`");
	}
	if (parts(root).includes("")) throw invalid("contains an empty segment");
}

/**
 * Throw on a root `@moq/net`'s `Path.from` refuses, so a token or key scope is never minted for
 * a root no session can dial.
 */
export function signable(root: string): void {
	validate(root);
	// TEMPORARY MIGRATION, delete with `@moq/net`'s at the next version cut: the previous release
	// splits at every `/`, escaped or not, so `Path.from` refuses a `//` or a trailing `/` even
	// when a `\` escapes it.
	if (root.endsWith("/") || root.includes("//")) {
		throw new Error(
			`invalid root ${JSON.stringify(root)} contains a \`//\` or a trailing \`/\`, which the previous release re-spells`,
		);
	}
}

/**
 * Trim leading and trailing slashes and collapse consecutive ones, so all slashes
 * are implicit at boundaries and `/foo//bar/` is the same path as `foo/bar`. An escaped
 * `/` is part of its segment, so it stays.
 */
export function normalize(path: string): string {
	return parts(path)
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
	// A prefix ending in a lone `\` escapes the `/` after it, so it ends inside a segment.
	const escaped = (prefix.length - prefix.replace(/\\+$/, "").length) % 2 === 1;
	if (rest.startsWith("/") && !escaped) return rest.slice(1);
	return undefined;
}

/**
 * A broadcast path that provides safe prefix matching operations.
 *
 * This module provides path-aware operations that respect delimiter boundaries,
 * preventing issues like "foo" matching "foobar".
 *
 * Paths are automatically trimmed of leading and trailing slashes on creation,
 * making all slashes implicit at boundaries.
 * All paths are RELATIVE; you cannot join with a leading slash to make an absolute path.
 *
 * @example
 * ```typescript
 * // Creation automatically trims slashes
 * const path1 = Path.from("/foo/bar/");
 * const path2 = Path.from("foo/bar");
 * console.log(path1 === path2); // true
 *
 * // Multiple arguments are joined with "/"
 * const path3 = Path.from("api", "v1", "users");
 * console.log(path3); // "api/v1/users"
 *
 * // Safe prefix matching
 * const base = Path.from("api/v1");
 * console.log(Path.hasPrefix(Path.from("api"), base)); // true
 * console.log(Path.hasPrefix(Path.from("api/v1"), base)); // true
 *
 * const joined = Path.join(base, Path.from("users"));
 * console.log(joined); // "api/v1/users"
 * ```
 */
export type Valid = string & { __brand: "Name" };

/**
 * Maximum number of slash-separated parts in a path.
 *
 * Matches the IETF moq-transport limit of 32 fields in a namespace tuple.
 * moq-lite enforces the same bound when decoding paths off the wire.
 */
export const MAX_PARTS = 32;

/** Build a path from one or more components, joining with "/" and trimming redundant slashes. */
export function from(...paths: string[]): Valid {
	// Join paths with "/" and then remove leading and trailing slashes, and collapse multiple slashes into one.
	const joined = paths.join("/");
	return joined.replace(/\/+/g, "/").replace(/^\/+/, "").replace(/\/+$/, "") as Valid;
}

/** Split a path into its slash-separated parts. The empty path has no parts. */
export function parts(path: Valid): string[] {
	return path === "" ? [] : path.split("/");
}

/**
 * Validate an untrusted wire string as a path, enforcing {@link MAX_PARTS}.
 *
 * Throws when the path has too many parts; use at wire decode sites.
 */
export function decode(raw: string): Valid {
	const path = from(raw);
	return encode(path);
}

/**
 * Validate a path before writing it to the wire, enforcing {@link MAX_PARTS}.
 *
 * Throws when the path has too many parts; use at wire encode sites so we never
 * emit a path the remote side is required to reject.
 */
export function encode(path: Valid): Valid {
	if (parts(path).length > MAX_PARTS) {
		throw new Error(`path exceeds ${MAX_PARTS} parts`);
	}
	return path;
}

/**
 * Check if a path has the given prefix, respecting path boundaries.
 *
 * Unlike String.startsWith, this ensures that "foo" does not match "foobar".
 * The prefix must either:
 * - Be exactly equal to the path
 * - Be followed by a '/' delimiter in the original path
 * - Be empty (matches everything)
 *
 * @example
 * ```typescript
 * const path = Path.from("foo/bar");
 * console.log(Path.hasPrefix(Path.from("foo"), path)); // true
 * console.log(Path.hasPrefix(Path.from("foo/"), path)); // true (trailing slash ignored)
 * console.log(Path.hasPrefix(Path.from("fo"), path)); // false
 *
 * const path2 = Path.from("foobar");
 * console.log(Path.hasPrefix(Path.from("foo"), path2)); // false
 * ```
 */
export function hasPrefix(prefix: Valid, path: Valid): boolean {
	if (prefix === "") {
		return true;
	}

	if (!path.startsWith(prefix)) {
		return false;
	}

	// Check if the prefix is the exact match
	if (path.length === prefix.length) {
		return true;
	}

	// Otherwise, ensure the character after the prefix is a delimiter
	return path[prefix.length] === "/";
}

/**
 * Strip the given prefix from a path, returning the suffix.
 *
 * Returns null if the prefix doesn't match according to hasPrefix rules.
 *
 * @example
 * ```typescript
 * const path = Path.from("foo/bar/baz");
 * const suffix = Path.stripPrefix(Path.from("foo"), path);
 * console.log(suffix); // "bar/baz"
 *
 * const suffix2 = Path.stripPrefix(Path.from("foo/"), path);
 * console.log(suffix2); // "bar/baz"
 *
 * const noMatch = Path.stripPrefix(Path.from("notfound"), path);
 * console.log(noMatch); // null
 * ```
 */
export function stripPrefix(prefix: Valid, path: Valid): Valid | null {
	if (!hasPrefix(prefix, path)) {
		return null;
	}

	// Handle empty prefix case
	if (prefix === "") {
		return path;
	}

	// If prefix matches exactly, return empty
	if (path.length === prefix.length) {
		return "" as Valid;
	}

	// For non-empty prefix that's shorter, skip the prefix and the following slash
	return path.slice(prefix.length + 1) as Valid;
}

/**
 * Join two path components together.
 *
 * @example
 * ```typescript
 * const base = Path.from("foo");
 * const joined = Path.join(base, Path.from("bar"));
 * console.log(joined); // "foo/bar"
 * ```
 */
export function join(path: Valid, other: Valid): Valid {
	if (path === "") {
		return other;
	} else if (other === "") {
		return path;
	} else {
		// Since paths are trimmed, we always need to add a slash
		return `${path}/${other}` as Valid;
	}
}

/** The empty path, which is a prefix of every path. */
export function empty(): Valid {
	return "" as Valid;
}

/**
 * Normalize a relative path reference: trim leading/trailing slashes, drop empty
 * segments, and drop `.` segments (no-ops, matching POSIX). `..` is preserved and
 * only interpreted by {@link resolve}.
 *
 * Mirrors the Rust `PathRelative::new` normalization, so JS and Rust agree
 * byte-for-byte on the stored form. Two callers comparing normalized strings can
 * detect that `""`, `"."`, `"/./"` etc. all mean "no reference".
 */
export function normalizeRelative(rel: string): string {
	return rel
		.split("/")
		.filter((s) => s !== "" && s !== ".")
		.join("/");
}

/**
 * Resolve a relative path reference against a base path.
 *
 * `..` segments pop the last segment of the base; other segments are appended.
 * `.` and empty segments are no-ops. Excess `..` once the base is empty is also a
 * no-op (subsequent named segments still append). An empty / normalized-empty `rel`
 * returns the base path unchanged.
 *
 * Mirrors the Rust `Path::resolve`, used by hang catalogs to express
 * cross-broadcast track references (a rendition's `broadcast` field).
 *
 * @example
 * ```typescript
 * Path.resolve(Path.from("a/b/c"), "../source"); // "a/b/source"
 * Path.resolve(Path.from("a/b"), "x/y");         // "a/b/x/y"
 * Path.resolve(Path.from("a"), "../../x");       // "x"
 * Path.resolve(Path.from("a/b"), "./c");         // "a/b/c"
 * ```
 */
export function resolve(base: Valid, rel: string): Valid {
	const segments = base === "" ? [] : base.split("/");

	for (const seg of rel.split("/")) {
		if (seg === "" || seg === ".") {
			continue;
		}
		if (seg === "..") {
			segments.pop();
		} else {
			segments.push(seg);
		}
	}

	return segments.join("/") as Valid;
}

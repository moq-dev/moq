/**
 * A broadcast path that provides safe prefix matching operations.
 *
 * This module provides path-aware operations that respect delimiter boundaries,
 * preventing issues like "foo" matching "foobar".
 *
 * A path is a run of non-empty segments separated by `/`, so it has exactly one spelling:
 * {@link from} throws on a leading, trailing, or doubled slash rather than collapsing it.
 * A `\` escapes the `\` or `/` after it, which is how a moq-transport namespace field holding
 * either one is spelled, and any other `\` is refused. Segments still split at every `/`.
 * The empty path is the root, which prefixes every path; a prefix needs no trailing slash.
 * A path names a point in the origin's tree from its root. The same type names an exact
 * broadcast and the prefix a route or announcement covers, so a broadcast's own path is
 * the prefix it announces. See {@link Relative} for `..`-style references.
 *
 * {@link Pattern} and {@link Patterns} are re-exported from `@moq/pattern`, the owner of
 * the v1 grammar and algebra.
 *
 * @example
 * ```typescript
 * // Multiple arguments are joined with "/", and an empty one is the root
 * const path = Path.from("api", "", "v1/users");
 * console.log(path); // "api/v1/users"
 *
 * // A slash at a boundary or a doubled one is an empty segment
 * Path.from("/api/v1"); // throws
 *
 * // Safe prefix matching
 * const base = Path.from("api/v1");
 * console.log(Path.hasPrefix(Path.from("api"), base)); // true
 * console.log(Path.hasPrefix(Path.from("api/v1"), base)); // true
 *
 * const joined = Path.join(base, Path.from("users"));
 * console.log(joined); // "api/v1/users"
 * ```
 * @module
 */

/** A broadcast path: non-empty segments separated by `/`, or the empty root. */
export type Valid = string & { __brand: "Name" };

/**
 * A relative reference from one broadcast to another, as a hang catalog carries it.
 *
 * It may contain `..` and is meaningful only once {@link resolve}d against a base
 * {@link Valid} path. It never crosses the wire on its own. Build one with
 * {@link normalizeRelative} or {@link relative}.
 */
export type Relative = string & { __brand: "Relative" };

/**
 * Maximum number of slash-separated parts in a path.
 *
 * Matches the IETF moq-transport limit of 32 fields in a namespace tuple.
 * moq-lite enforces the same bound when decoding paths off the wire.
 */
export const MAX_PARTS = 32;

/**
 * Maximum UTF-8 length of a path in bytes.
 *
 * Matches the IETF moq-transport limit of 4096 bytes for a namespace. Its tuple fields
 * drop the path's separators and escapes, so every path fits both wires.
 */
export const MAX_BYTES = 4096;

const utf8 = new TextEncoder();

/**
 * Build a path by joining components with "/", where an empty component is the root.
 *
 * Throws when a component has a leading, trailing, or doubled `/`, or a `\` before anything
 * but `\` or `/`.
 */
export function from(...paths: string[]): Valid {
	for (const path of paths) {
		const error = spelling(path);
		if (error) throw new Error(error);
	}
	return paths.filter((path) => path !== "").join("/") as Valid;
}

// Why `path` is not the one spelling of a path, or undefined when it is.
function spelling(path: string): string | undefined {
	if (path.startsWith("/") || path.endsWith("/") || path.includes("//")) {
		return "path contains an empty segment";
	}
	// Pairs of `\` are escaped backslashes, so any `\` left must escape a `/`.
	if (path.includes("\\") && /\\(?!\/)/.test(path.replaceAll("\\\\", ""))) {
		return "path contains a `\\` that escapes neither `\\` nor `/`";
	}
	return undefined;
}

/** Split a path into its slash-separated parts. The empty path has no parts. */
export function parts(path: Valid): string[] {
	return path === "" ? [] : path.split("/");
}

/**
 * Validate an untrusted wire string as a path, enforcing {@link MAX_PARTS} and {@link MAX_BYTES}.
 *
 * Throws when the path has an empty segment or is too deep or too long; use at wire decode sites.
 */
export function decode(raw: string): Valid {
	return encode(from(raw));
}

/**
 * Validate a path before writing it to the wire, enforcing {@link MAX_PARTS} and {@link MAX_BYTES}.
 *
 * Throws when the path is too deep or too long; use at wire encode sites so we never
 * emit a path the remote side is required to reject.
 */
export function encode(path: Valid): Valid {
	const error = check(path);
	if (error) throw new Error(error);
	return path;
}

/** Why `path` breaks the wire limits, or undefined when it fits. @internal */
export function check(path: Valid): string | undefined {
	// Count separators with an early exit instead of splitting, so a hostile path never allocates per part.
	let count = path === "" ? 0 : 1;
	for (let i = path.indexOf("/"); i !== -1; i = path.indexOf("/", i + 1)) {
		if (++count > MAX_PARTS) return `path exceeds ${MAX_PARTS} parts`;
	}
	// A UTF-16 unit is at most 3 UTF-8 bytes, so only a long path needs encoding to measure.
	if (path.length > MAX_BYTES / 3 && utf8.encode(path).length > MAX_BYTES) {
		return `path exceeds ${MAX_BYTES} bytes`;
	}
	return undefined;
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
		// Neither side has a boundary slash, so exactly one joins them
		return `${path}/${other}` as Valid;
	}
}

/** The empty path, which is a prefix of every path. */
export function empty(): Valid {
	return "" as Valid;
}

/**
 * Parse a relative path reference and drop its redundant `.` segments. It is spelled like
 * a path, so this throws on a leading, trailing, or doubled `/` or a lone `\` rather than
 * collapsing it. A reference made only of `.` segments normalizes to `.` because it names
 * the base's parent, while empty names the base. `..` is preserved and only interpreted
 * by {@link resolve}.
 *
 * Mirrors the Rust `path::Relative::new`, so JS and Rust agree byte-for-byte on the stored
 * form. Two callers comparing normalized strings can detect equivalent references while
 * preserving the distinction between `""` and `"."`.
 */
export function normalizeRelative(rel: string): Relative {
	from(rel);
	const raw = rel.split("/");
	if (!raw.includes(".")) return rel as Relative;

	const normalized = raw.filter((s) => s !== ".").join("/");
	// Dropping a `.` after an escaped `/` strands its `\`, so the result is checked again.
	return from(normalized === "" ? "." : normalized) as string as Relative;
}

/**
 * Resolve a relative path reference against a base path.
 *
 * A non-empty reference replaces the last segment of the base, matching relative URL
 * resolution. `..` segments then pop another segment; other segments are appended.
 * `.` segments are no-ops. Excess `..` once the base is empty is also a no-op
 * (subsequent named segments still append). An empty `rel` returns the base unchanged.
 *
 * Throws when the result ends a segment in a lone `\` because resolution dropped the
 * escaped `/` after it.
 *
 * Mirrors the Rust `Path::resolve`, used by hang catalogs to express
 * cross-broadcast track references (a rendition's `broadcast` field).
 *
 * @example
 * ```typescript
 * Path.resolve(Path.from("a/b/c"), "./source");  // "a/b/source"
 * Path.resolve(Path.from("a/b"), ".");           // "a"
 * Path.resolve(Path.from("a/b/c"), "../source"); // "a/source"
 * ```
 */
export function resolve(base: Valid, rel: Relative): Valid {
	if (rel === "") return base;

	const segments = base === "" ? [] : base.split("/");
	segments.pop();

	for (const seg of rel.split("/")) {
		if (seg === ".") {
			continue;
		}
		if (seg === "..") {
			segments.pop();
		} else {
			segments.push(seg);
		}
	}

	return from(segments.join("/"));
}

/**
 * Resolve a relative path, returning `undefined` if it escapes above the root or
 * {@link resolve} would throw.
 *
 * Unlike {@link resolve}, this distinguishes a valid reference to the empty root
 * path from excess `..` segments. Use it for untrusted catalog references that
 * must not be clamped to the root.
 */
export function tryResolve(base: Valid, rel: Relative): Valid | undefined {
	if (rel === "") return base;

	const segments = base === "" ? [] : base.split("/");
	segments.pop();

	for (const seg of rel.split("/")) {
		if (seg === ".") {
			continue;
		}
		if (seg === "..") {
			if (segments.pop() === undefined) return undefined;
		} else {
			segments.push(seg);
		}
	}

	const resolved = segments.join("/");
	return spelling(resolved) === undefined ? (resolved as Valid) : undefined;
}

/**
 * Express `target` relative to `base`: the inverse of {@link resolve}.
 *
 * The result round-trips (`resolve(base, relative(target, base)) === target`) and never
 * walks above the root, so {@link tryResolve} accepts it too.
 *
 * A relative reference replaces the last segment of the base, matching relative URL
 * resolution, so a target nested under the base repeats the base's own last segment.
 *
 * The empty reference names the base itself, so that is what a self-reference returns.
 *
 * Returns `undefined` for a target no reference can name: a path segment may literally be
 * `.` or `..`, which resolution reads as navigation instead of as a name. Only the segments
 * past the shared prefix matter, since the rest are never emitted.
 *
 * Mirrors the Rust `Path::relative`, used to author the cross-broadcast track
 * references a hang catalog carries. Note the argument order: the target comes first,
 * the opposite of Node's `path.relative(from, to)`.
 *
 * @example
 * ```typescript
 * Path.relative(Path.from("a/b/c"), Path.from("a/b")); // "b/c"
 * Path.relative(Path.from("a/c"), Path.from("a/b"));   // "c"
 * Path.relative(Path.from("a"), Path.from("a/b"));     // "."
 * Path.relative(Path.from("a/b"), Path.from("a/b"));   // ""
 * Path.relative(Path.from("a/.."), Path.from("a/b"));  // undefined
 * ```
 */
export function relative(target: Valid, base: Valid): Relative | undefined {
	// Only the empty reference can name a base whose last segment is itself `.` or `..`,
	// since resolution replaces that segment rather than emitting it.
	if (target === base) return "" as Relative;

	// Resolution replaces the base's last segment, so walk from its parent.
	const dir = base === "" ? [] : base.split("/");
	dir.pop();

	const parts = target === "" ? [] : target.split("/");

	let common = 0;
	while (common < dir.length && common < parts.length && dir[common] === parts[common]) {
		common += 1;
	}

	const down = parts.slice(common);
	// Resolution would walk on these instead of naming them.
	if (down.some((part) => part === "." || part === "..")) return undefined;

	const rel = Array(dir.length - common)
		.fill("..")
		.concat(down);

	// An empty reference resolves to the base itself, so name the parent explicitly.
	return (rel.length === 0 ? "." : rel.join("/")) as Relative;
}

/** Path patterns: grammar and algebra owned by `@moq/pattern`. */
export {
	compareSpecificity,
	InvalidPattern,
	Pattern,
	Patterns,
	type Segment,
	type Specificity,
} from "@moq/pattern";

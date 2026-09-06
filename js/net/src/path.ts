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
 * segments, and drop redundant `.` segments. A reference made only of `.` segments
 * normalizes to `.` because it names the base's parent, while empty names the base.
 * `..` is preserved and only interpreted by {@link resolve}.
 *
 * Mirrors the Rust `PathRelative::new` normalization, so JS and Rust agree
 * byte-for-byte on the stored form. Two callers comparing normalized strings can
 * detect equivalent references while preserving the distinction between `""` and `"."`.
 */
export function normalizeRelative(rel: string): string {
	const raw = rel.split("/");
	const normalized = raw.filter((s) => s !== "" && s !== ".").join("/");

	return normalized === "" && raw.includes(".") ? "." : normalized;
}

/**
 * Resolve a relative path reference against a base path.
 *
 * A non-empty reference replaces the last segment of the base, matching relative URL
 * resolution. `..` segments then pop another segment; other segments are appended.
 * `.` and empty segments are no-ops. Excess `..` once the base is empty is also a no-op
 * (subsequent named segments still append). An empty `rel` returns the base unchanged.
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
export function resolve(base: Valid, rel: string): Valid {
	if (rel === "") return base;

	const segments = base === "" ? [] : base.split("/");
	segments.pop();

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

/**
 * Resolve a relative path, returning `undefined` if it escapes above the root.
 *
 * Unlike {@link resolve}, this distinguishes a valid reference to the empty root
 * path from excess `..` segments. Use it for untrusted catalog references that
 * must not be clamped to the root.
 */
export function tryResolve(base: Valid, rel: string): Valid | undefined {
	if (rel === "") return base;

	const segments = base === "" ? [] : base.split("/");
	segments.pop();

	for (const seg of rel.split("/")) {
		if (seg === "" || seg === ".") {
			continue;
		}
		if (seg === "..") {
			if (segments.pop() === undefined) return undefined;
		} else {
			segments.push(seg);
		}
	}

	return segments.join("/") as Valid;
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
export function relative(target: Valid, base: Valid): string | undefined {
	// Only the empty reference can name a base whose last segment is itself `.` or `..`,
	// since resolution replaces that segment rather than emitting it.
	if (target === base) return "";

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
	return rel.length === 0 ? "." : rel.join("/");
}

/**
 * One segment of a {@link Pattern}.
 *
 * A literal is never empty and never contains `/` or `*`. A wildcard (`*`) matches any
 * one segment. A partial (`prefix*suffix`) matches any one segment that starts with the
 * prefix and ends with the suffix without overlap; either may be empty, not both. A
 * globstar (`**`) matches any run of zero or more segments, at most once per pattern.
 */
export type Segment =
	| { readonly kind: "literal"; readonly value: string }
	| { readonly kind: "wildcard" }
	| { readonly kind: "partial"; readonly prefix: string; readonly suffix: string }
	| { readonly kind: "globstar" };

/** Why a string or a segment list is not a valid {@link Pattern}. */
export type ErrorCode =
	/** A segment is empty: a leading, trailing, or doubled `/`. */
	| "empty-segment"
	/** A segment's kind, fields, or wildcard syntax is invalid. */
	| "invalid-segment"
	/** More than one `**`. */
	| "multiple-globstars"
	/** More than {@link Pattern.MAX_SEGMENTS} segments. */
	| "too-many-segments";

/** Thrown when a pattern's text or segments violate the grammar. */
export class PatternError extends Error {
	/** Which rule was broken. */
	readonly code: ErrorCode;

	constructor(code: ErrorCode, message: string) {
		super(message);
		this.name = "PatternError";
		this.code = code;
	}
}

/**
 * How much of a path a pattern pins down, for ranking the patterns that match one path.
 *
 * Compare with {@link compareSpecificity}. The order agrees with containment: when `a`
 * matches a strict superset of `b`'s paths, `a` ranks below `b`. Patterns that tie without
 * being equal (`* /a` and `* /b`) form one tier.
 */
export interface Specificity {
	/** Literal segments; more is more specific. */
	readonly literals: number;
	/** Whether there is no `**`; exact beats not. */
	readonly exact: boolean;
	/** Partial (`prefix*suffix`) segments; more is more specific. */
	readonly partials: number;
	/** `*` segments; more is more specific. */
	readonly wildcards: number;
	/** Bytes pinned by partial segments; more is more specific. */
	readonly pinned: number;
	/** Leading literal segments; longer is more specific. */
	readonly head: number;
}

/** Positive when `a` is more specific than `b`, negative when less, zero for one tier. */
export function compareSpecificity(a: Specificity, b: Specificity): number {
	if (a.literals !== b.literals) return a.literals - b.literals;
	if (a.exact !== b.exact) return a.exact ? 1 : -1;
	if (a.partials !== b.partials) return a.partials - b.partials;
	if (a.wildcards !== b.wildcards) return a.wildcards - b.wildcards;
	if (a.pinned !== b.pinned) return a.pinned - b.pinned;
	return a.head - b.head;
}

const WILDCARD: Segment = { kind: "wildcard" };
const GLOBSTAR: Segment = { kind: "globstar" };
const UTF8 = new TextEncoder();

/** The non-empty segments of a path, normalized like a broadcast path. */
function splitPath(path: string): string[] {
	return path.split("/").filter((part) => part !== "");
}

function invalidSegment(text: string): PatternError {
	return new PatternError("invalid-segment", `invalid pattern segment: ${JSON.stringify(text)}`);
}

function parseSegment(text: string): Segment {
	if (text === "") throw new PatternError("empty-segment", "empty path segment");
	if (text === "*") return WILDCARD;
	if (text === "**") return GLOBSTAR;
	if (text.includes("/")) throw invalidSegment(text);
	const star = text.indexOf("*");
	if (star < 0) return { kind: "literal", value: text };
	const prefix = text.slice(0, star);
	const suffix = text.slice(star + 1);
	// More than one star in a segment is reserved.
	if (suffix.includes("*")) throw invalidSegment(text);
	return { kind: "partial", prefix, suffix };
}

function segmentText(segment: Segment): string {
	switch (segment.kind) {
		case "literal":
			return segment.value;
		case "wildcard":
			return "*";
		case "partial":
			return `${segment.prefix}*${segment.suffix}`;
		case "globstar":
			return "**";
	}
}

function freezeSegment(segment: Segment): Segment {
	// Copy fields explicitly so structurally typed objects may use inherited getters.
	switch (segment?.kind) {
		case "literal":
			return Object.freeze({ kind: "literal", value: segment.value });
		case "partial":
			return Object.freeze({ kind: "partial", prefix: segment.prefix, suffix: segment.suffix });
		case "wildcard":
			return Object.freeze({ kind: "wildcard" });
		case "globstar":
			return Object.freeze({ kind: "globstar" });
		default:
			throw new PatternError("invalid-segment", "unknown pattern segment kind");
	}
}

/** Whether every segment `a` matches, `b` matches too. `**` is handled structurally. */
function covers(a: Segment, b: Segment): boolean {
	switch (a.kind) {
		case "wildcard":
			return b.kind !== "globstar";
		case "literal":
			return b.kind === "literal" && a.value === b.value;
		case "partial":
			if (b.kind === "literal") return matchesPart(a, b.value);
			// `p*s` covers `p'*s'` exactly when `p` starts `p'` and `s` ends `s'`: the
			// middle is free on both sides, so nothing else can constrain it.
			return b.kind === "partial" && b.prefix.startsWith(a.prefix) && b.suffix.endsWith(a.suffix);
		case "globstar":
			return false;
	}
}

function compatible(a: Segment, b: Segment): boolean {
	if (a.kind === "partial" && b.kind === "partial") {
		// Two partials meet when one prefix starts the other and one suffix ends the
		// other: the longer prefix followed by the longer suffix matches both.
		return (
			(a.prefix.startsWith(b.prefix) || b.prefix.startsWith(a.prefix)) &&
			(a.suffix.endsWith(b.suffix) || b.suffix.endsWith(a.suffix))
		);
	}
	return covers(a, b) || covers(b, a);
}

function matchesPart(segment: Segment, part: string): boolean {
	switch (segment.kind) {
		case "literal":
			return segment.value === part;
		case "wildcard":
			return true;
		case "partial":
			return (
				part.length >= segment.prefix.length + segment.suffix.length &&
				part.startsWith(segment.prefix) &&
				part.endsWith(segment.suffix)
			);
		case "globstar":
			return false;
	}
}

/** Segment-wise from the front. */
function matchesRun(segments: readonly Segment[], parts: readonly string[]): boolean {
	return segments.every((segment, i) => matchesPart(segment, parts[i]));
}

/** Segment-wise from the back. */
function matchesTail(segments: readonly Segment[], parts: readonly string[]): boolean {
	return segments.every((segment, i) => matchesPart(segment, parts[parts.length - segments.length + i]));
}

function coversRun(ours: readonly Segment[], theirs: readonly Segment[]): boolean {
	return ours.every((a, i) => (i < theirs.length ? covers(a, theirs[i]) : a.kind === "wildcard"));
}

function compatibleRun(a: readonly Segment[], b: readonly Segment[]): boolean {
	const n = Math.min(a.length, b.length);
	for (let i = 0; i < n; i++) if (!compatible(a[i], b[i])) return false;
	return true;
}

function reversed<T>(list: readonly T[]): T[] {
	return [...list].reverse();
}

/**
 * A pattern over broadcast paths: literal segments, `*` for one segment, `prefix*suffix`
 * for one segment with a known start and end, and at most one `**` for any run of
 * segments. Every segment kind matches whole segments, and a pattern is exact: `foo`
 * matches only `foo`, and a subtree is `foo/**`.
 *
 * Build one with {@link Pattern.parse}, {@link Pattern.from} (segments), or
 * {@link Pattern.literal} and {@link Pattern.subtree} (from a path). Two patterns are
 * {@link Pattern.equals | equal} when their text is, and the text is canonical: equal
 * patterns match the same paths, and only they do. Construction moves `**` before
 * adjacent `*` segments, so `* /**` prints as `** /*`.
 *
 * @public
 */
export class Pattern {
	/** The most segments a pattern may have, matching the path limit on the wire. */
	static readonly MAX_SEGMENTS = 32;

	/** The canonical text: segments joined by `/`, wildcards as `*` and `**`. */
	readonly text: string;
	/** The segments, in order. */
	readonly segments: readonly Segment[];

	// Index of the `**` segment, if any.
	readonly #globstar: number | undefined;
	// Length of the literal head within `text`.
	readonly #head: number;

	private constructor(segments: readonly Segment[]) {
		if (segments.length > Pattern.MAX_SEGMENTS) {
			throw new PatternError("too-many-segments", `more than ${Pattern.MAX_SEGMENTS} segments`);
		}
		const owned = segments.map(freezeSegment);
		segments = owned;

		let globstar: number | undefined;
		for (const [i, segment] of segments.entries()) {
			if (segment.kind === "literal") {
				if (typeof segment.value !== "string") {
					throw new PatternError("invalid-segment", "literal value must be a string");
				}
				if (segment.value === "") throw new PatternError("empty-segment", "empty path segment");
				if (segment.value.includes("*") || segment.value.includes("/")) throw invalidSegment(segment.value);
			} else if (segment.kind === "partial") {
				const { prefix, suffix } = segment;
				if (typeof prefix !== "string" || typeof suffix !== "string") {
					throw new PatternError("invalid-segment", "partial prefix and suffix must be strings");
				}
				if ((prefix === "" && suffix === "") || /[*/]/.test(prefix) || /[*/]/.test(suffix)) {
					throw invalidSegment(`${prefix}*${suffix}`);
				}
			} else if (segment.kind === "globstar") {
				if (globstar !== undefined) throw new PatternError("multiple-globstars", "more than one ** segment");
				globstar = i;
			}
		}

		// Adjacent `*` and `**` commute; keep `**` first for one language identity.
		if (globstar !== undefined) {
			while (globstar > 0 && owned[globstar - 1].kind === "wildcard") {
				[owned[globstar - 1], owned[globstar]] = [owned[globstar], owned[globstar - 1]];
				globstar--;
			}
		}

		let head = 0;
		let inHead = true;
		let text = "";
		for (const [i, segment] of segments.entries()) {
			if (i > 0) text += "/";
			if (segment.kind !== "literal") inHead = false;
			text += segmentText(segment);
			if (inHead) head = text.length;
		}

		this.segments = Object.freeze([...segments]);
		this.text = text;
		this.#globstar = globstar;
		this.#head = head;
		Object.freeze(this);
	}

	/**
	 * Parse a pattern's text. Throws {@link PatternError} on invalid syntax.
	 *
	 * Unlike a path, slashes are not normalized: a leading, trailing, or doubled `/` is
	 * an error, so a typo cannot silently widen a grant.
	 */
	static parse(text: string): Pattern {
		if (text === "") return new Pattern([]);
		return new Pattern(text.split("/").map(parseSegment));
	}

	/** A pattern from its segments, validating the grammar. Throws {@link PatternError}. */
	static from(segments: Iterable<Segment>): Pattern {
		return new Pattern([...segments]);
	}

	/**
	 * The pattern matching exactly `path`.
	 *
	 * The path is normalized like a broadcast path (slashes trimmed and collapsed).
	 * Throws when a segment is `*` or `**` or contains `*`: those are wildcards, and a
	 * path using them cannot be named by a pattern.
	 */
	static literal(path: string): Pattern {
		return new Pattern(splitPath(path).map((value) => ({ kind: "literal", value })));
	}

	/** The pattern matching `path` and everything beneath it: `path/**`. The empty path yields `**`. */
	static subtree(path: string): Pattern {
		return new Pattern([...splitPath(path).map((value): Segment => ({ kind: "literal", value })), GLOBSTAR]);
	}

	/** The pattern matching every path: `**`. */
	static all(): Pattern {
		return new Pattern([GLOBSTAR]);
	}

	/** The empty pattern, which matches only the empty path. */
	static empty(): Pattern {
		return new Pattern([]);
	}

	/** Order two patterns by text, so a sorted list is deterministic. */
	static compare(a: Pattern, b: Pattern): number {
		// Scalar order agrees with UTF-8 and keeps unpaired JS surrogates distinct.
		const left = Array.from(a.text, (char) => char.codePointAt(0) ?? 0);
		const right = Array.from(b.text, (char) => char.codePointAt(0) ?? 0);
		for (let i = 0; i < Math.min(left.length, right.length); i++) {
			if (left[i] !== right[i]) return left[i] - right[i];
		}
		return left.length - right.length;
	}

	/** The canonical text. */
	toString(): string {
		return this.text;
	}

	/** Serializes as the canonical text, matching the Rust crate's serde form. */
	toJSON(): string {
		return this.text;
	}

	/** Whether the two patterns are the same pattern. */
	equals(other: Pattern): boolean {
		return this.text === other.text;
	}

	/**
	 * The literal segments before the first wildcard, as a path.
	 *
	 * Every matching path starts with it, so it is where a tree walk starts. Empty when
	 * the pattern starts with a wildcard; the whole pattern when it has none.
	 */
	get head(): string {
		return this.text.slice(0, this.#head);
	}

	/** Whether the pattern has no wildcards, so it matches exactly one path. */
	get isLiteral(): boolean {
		return this.#head === this.text.length;
	}

	/** Whether the pattern has a `**`, so it matches paths of more than one length. */
	get hasGlobstar(): boolean {
		return this.#globstar !== undefined;
	}

	/** Whether `path` is in the set this pattern describes. The path is normalized like a broadcast path. */
	matches(path: string): boolean {
		const parts = splitPath(path);
		if (this.#globstar === undefined) {
			return parts.length === this.segments.length && matchesRun(this.segments, parts);
		}
		const [head, tail] = this.#split();
		return parts.length >= head.length + tail.length && matchesRun(head, parts) && matchesTail(tail, parts);
	}

	/**
	 * Whether every path `other` matches, this pattern matches too.
	 *
	 * This is the authorization check: a grant contains a request when the request
	 * cannot name a path outside it. A pattern contains itself.
	 */
	contains(other: Pattern): boolean {
		if (this.#globstar === undefined) {
			if (other.#globstar !== undefined) return false;
			return (
				this.segments.length === other.segments.length &&
				this.segments.every((a, i) => covers(a, other.segments[i]))
			);
		}

		const [head, tail] = this.#split();
		if (other.#globstar === undefined) {
			return (
				other.segments.length >= head.length + tail.length &&
				head.every((a, i) => covers(a, other.segments[i])) &&
				tail.every((a, i) => covers(a, other.segments[other.segments.length - tail.length + i]))
			);
		}

		// The other's `**` can be arbitrarily long, so any of our segments that reach past
		// the other's head or tail must be `*`; and the other's shortest path (its `**`
		// empty) must still be long enough for ours.
		const [otherHead, otherTail] = other.#split();
		return (
			head.length + tail.length <= otherHead.length + otherTail.length &&
			coversRun(head, otherHead) &&
			coversRun(reversed(tail), reversed(otherTail))
		);
	}

	/** Whether some path matches both patterns. */
	overlaps(other: Pattern): boolean {
		if (this.#globstar === undefined) {
			if (other.#globstar !== undefined) return other.overlaps(this);
			return this.segments.length === other.segments.length && compatibleRun(this.segments, other.segments);
		}

		const [head, tail] = this.#split();
		if (other.#globstar === undefined) {
			return (
				other.segments.length >= head.length + tail.length &&
				compatibleRun(head, other.segments) &&
				compatibleRun(reversed(tail), reversed(other.segments))
			);
		}

		// A path long enough keeps the heads and tails apart, so the only constraints are
		// segment-wise where the heads and tails overlap.
		const [otherHead, otherTail] = other.#split();
		return compatibleRun(head, otherHead) && compatibleRun(reversed(tail), reversed(otherTail));
	}

	/** How much of a path this pattern pins down. See {@link Specificity}. */
	specificity(): Specificity {
		let head = 0;
		while (head < this.segments.length && this.segments[head].kind === "literal") head++;
		let pinned = 0;
		for (const s of this.segments) {
			if (s.kind === "partial") pinned += UTF8.encode(s.prefix).length + UTF8.encode(s.suffix).length;
		}
		return {
			literals: this.segments.filter((s) => s.kind === "literal").length,
			exact: this.#globstar === undefined,
			partials: this.segments.filter((s) => s.kind === "partial").length,
			wildcards: this.segments.filter((s) => s.kind === "wildcard").length,
			pinned,
			head,
		};
	}

	/**
	 * The patterns that, relative to `root`, match exactly the paths this pattern matches
	 * beneath `root`.
	 *
	 * This is how a grant or an advertisement is presented inside a rooted view. It is a
	 * set because `**` may consume the root or stop short of it: `** /a` rebased at `a` is
	 * both the empty pattern (the root itself) and `** /a` (deeper paths ending in `a`).
	 * Empty when nothing under `root` matches. The root is normalized like a broadcast path.
	 */
	rebase(root: string): Patterns {
		const parts = splitPath(root);
		const out = new Patterns();

		if (this.#globstar === undefined) {
			if (parts.length <= this.segments.length && matchesRun(this.segments.slice(0, parts.length), parts)) {
				out.insert(new Pattern(this.segments.slice(parts.length)));
			}
			return out;
		}

		const [head, tail] = this.#split();
		if (parts.length <= head.length) {
			if (matchesRun(head.slice(0, parts.length), parts)) {
				out.insert(new Pattern(this.segments.slice(parts.length)));
			}
			return out;
		}
		if (!matchesRun(head, parts)) return out;

		// The root reaches into the `**`. Either the `**` swallows the rest of the root and
		// stays open, or it closed inside the root and some of the tail already matched the
		// root's last segments.
		const rest = parts.slice(head.length);
		out.insert(new Pattern(this.segments.slice(this.#globstar)));
		for (let consumed = 1; consumed <= Math.min(tail.length, rest.length); consumed++) {
			if (matchesRun(tail.slice(0, consumed), rest.slice(rest.length - consumed))) {
				out.insert(new Pattern(tail.slice(consumed)));
			}
		}
		return out;
	}

	/**
	 * This pattern placed beneath a literal `root`: the same paths, named from the root's
	 * parent. The inverse of {@link rebase} for a single pattern.
	 *
	 * The root is normalized and validated like {@link Pattern.literal}, and the result
	 * must fit {@link Pattern.MAX_SEGMENTS}.
	 */
	rooted(root: string): Pattern {
		return new Pattern([
			...splitPath(root).map((value): Segment => ({ kind: "literal", value })),
			...this.segments,
		]);
	}

	/** The segments before and after the `**`. Only meaningful when there is one. */
	#split(): [readonly Segment[], readonly Segment[]] {
		if (this.#globstar === undefined) return [this.segments, []];
		return [this.segments.slice(0, this.#globstar), this.segments.slice(this.#globstar + 1)];
	}
}

/**
 * A union of patterns, reduced so no member is contained by another.
 *
 * This is the shape of a grant (the paths a token may publish) and of a rebased pattern
 * (see {@link Pattern.rebase}). Members are kept in canonical order, so two unions
 * describing the same reduced set are {@link Patterns.equals | equal}.
 *
 * Containment is per member: {@link Patterns.contains} holds when one pattern in the union
 * contains the candidate. A candidate covered only jointly by several members (`a/**`
 * against `a`, `a/*`, and `a/* /**`) is refused, which keeps the check linear and its answer
 * easy to predict. A grant that means a subtree writes `a/**`.
 *
 * @public
 */
export class Patterns implements Iterable<Pattern> {
	#members: Pattern[] = [];

	/** A union of the given patterns, reduced. Empty when none are given. */
	constructor(patterns?: Iterable<Pattern>) {
		if (patterns) for (const pattern of patterns) this.insert(pattern);
	}

	/**
	 * Add a pattern, dropping members it contains.
	 *
	 * Returns false when a member already contains it, leaving the union unchanged.
	 */
	insert(pattern: Pattern): boolean {
		if (this.contains(pattern)) return false;
		this.#members = this.#members.filter((member) => !pattern.contains(member));
		let at = 0;
		while (at < this.#members.length && Pattern.compare(this.#members[at], pattern) < 0) at++;
		this.#members.splice(at, 0, pattern);
		return true;
	}

	/** Whether any member matches `path`. */
	matches(path: string): boolean {
		return this.#members.some((member) => member.matches(path));
	}

	/** Whether some member contains `pattern`. See the class docs for why this is per member. */
	contains(pattern: Pattern): boolean {
		return this.#members.some((member) => member.contains(pattern));
	}

	/** Whether every member of `other` is contained here: `other` grants nothing this union does not. */
	covers(other: Patterns): boolean {
		return other.#members.every((pattern) => this.contains(pattern));
	}

	/** Whether any member overlaps `pattern`. */
	overlaps(pattern: Pattern): boolean {
		return this.#members.some((member) => member.overlaps(pattern));
	}

	/** Every member rebased at `root`, as one union. See {@link Pattern.rebase}. */
	rebase(root: string): Patterns {
		const out = new Patterns();
		for (const member of this.#members) for (const pattern of member.rebase(root)) out.insert(pattern);
		return out;
	}

	/** Every member placed beneath `root`. See {@link Pattern.rooted}. */
	rooted(root: string): Patterns {
		return new Patterns(this.#members.map((member) => member.rooted(root)));
	}

	/** The number of members. */
	get size(): number {
		return this.#members.length;
	}

	/** The members, in canonical order. */
	[Symbol.iterator](): Iterator<Pattern> {
		return this.#members[Symbol.iterator]();
	}

	/** The members as a fresh array, in canonical order. */
	toArray(): Pattern[] {
		return [...this.#members];
	}

	/** Serializes as a list of texts, matching the Rust crate's serde form. */
	toJSON(): string[] {
		return this.#members.map((member) => member.text);
	}

	/** Whether the two unions have the same reduced pattern members. */
	equals(other: Patterns): boolean {
		return this.size === other.size && this.#members.every((member, i) => member.equals(other.#members[i]));
	}
}

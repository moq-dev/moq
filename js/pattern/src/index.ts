/**
 * Exact path patterns for Media over QUIC: grammar, matching, and set algebra.
 *
 * A {@link Pattern} describes a set of broadcast paths. {@link Patterns} is an unordered
 * union reduced by exact containment. Matching is linear. {@link Pattern.literal}
 * rejects `*` because it is reserved for pattern syntax.
 *
 * `@moq/net` and `@moq/token` re-export this package. The Rust twin is `moq-pattern`.
 *
 * ## Grammar
 *
 * A pattern is canonical `/`-separated segments:
 *
 * - a literal;
 * - `*`, matching one complete segment;
 * - `lit*lit`, with one `*` matching bytes inside one segment;
 * - `**`, matching zero or more complete segments, at most once per pattern.
 *
 * Patterns are exact: `foo` matches only `foo`, `foo/**` matches its subtree including
 * `foo`, `**` matches every path, and the empty pattern matches only the current root.
 * Parse rejects leading, trailing, or repeated `/`, more than one `*` in a segment,
 * `**` mixed with literal bytes, more than one `**`, and more than
 * {@link Pattern.MAX_SEGMENTS} (32) segments. Construction moves `**` before adjacent
 * `*` segments, so `* /**` is `** /*`.
 *
 * ## Algebra
 *
 * {@link Pattern.matches}, {@link Pattern.overlaps}, {@link Pattern.contains},
 * {@link Pattern.head}, {@link Pattern.specificity}, and set-valued
 * {@link Pattern.rebase}. A rebase never picks one lossy residual: `** /a` at `a` is
 * both the empty pattern and `** /a`. A union reduces per member; a candidate covered
 * only jointly by several members is refused.
 *
 * ## CAT / C4M
 *
 * Common Access Token and `draft-ietf-moq-c4m-01` match namespace fields positionally:
 * exact, prefix, or suffix per field, with a trailing `nil` for exact depth. Without
 * `nil`, longer namespaces that start with the matching fields are in scope.
 *
 * That common subset is `foo/bar` (exact fields plus `nil`), `foo/bar/**` (no `nil`),
 * `foo*` / `*foo` (prefix / suffix on a field), `*` (prefix of the empty byte string),
 * and `pid/ * /chat` (exact, any field, exact, `nil`). Richer MoQ forms, kept explicit
 * rather than claimed as CAT gaps: `**` not at the end (`** /a`, `a/** /b`) because C4M
 * is positional from the front, and `foo*bar` because a C4M match object is exact,
 * prefix, or suffix, not both.
 *
 * ## Literal paths
 *
 * `@moq/net`'s `Path` stays a coordinate. Roots, joins, exact names, URL paths, and
 * object-store keys keep their own types. {@link Pattern.literal} rejects `*`; literal
 * path construction and wire decoding retain their existing behavior.
 *
 * @module
 */

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

	/** Create an error with the violated grammar rule in `code` and a human-readable `message`. */
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
const MAX_PATTERN_SEGMENTS = 32;

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
	static get MAX_SEGMENTS(): number {
		return MAX_PATTERN_SEGMENTS;
	}

	/** The canonical text: segments joined by `/`, wildcards as `*` and `**`. */
	readonly text: string;
	/** The segments, in order. */
	readonly segments: readonly Segment[];

	// Index of the `**` segment, if any.
	readonly #globstar: number | undefined;
	// Length of the literal head within `text`.
	readonly #head: number;

	private constructor(segments: readonly Segment[]) {
		if (segments.length > MAX_PATTERN_SEGMENTS) {
			throw new PatternError("too-many-segments", `more than ${MAX_PATTERN_SEGMENTS} segments`);
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

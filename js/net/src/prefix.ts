/**
 * Grant prefixes and a path segment's escaping.
 *
 * A grant is a {@link Prefixes} set: each {@link Prefix} covers a path and everything beneath
 * it, the empty prefix covering every path. A grant never holds a wildcard: parsing one fails
 * loudly with an {@link InvalidPrefix} of code `wildcard`. The Rust twin is `moq_net::path`.
 *
 * A separate entrypoint (`@moq/net/prefix`) so a token minting service reads grants without
 * loading the wire stack.
 *
 * @module
 */

/**
 * The segments of `path`, split only at a `/` no `\` escapes and still escaped as written.
 *
 * The empty path has none. Anything else yields one more segment than it has separators,
 * empty ones included, so a caller that requires non-empty segments checks them itself.
 * With `limit`, stops after that many segments.
 *
 * @example
 * ```typescript
 * parts("room/a\\/b/cam"); // ["room", "a\\/b", "cam"]
 * ```
 */
export function parts(path: string, limit?: number): string[] {
	if (path === "") return [];
	// Without a `\`, every `/` separates.
	if (!path.includes("\\")) return limit === undefined ? path.split("/") : path.split("/", limit);
	const max = limit ?? Number.POSITIVE_INFINITY;
	const out: string[] = [];
	let start = 0;
	for (let i = 0; i < path.length && out.length < max; i++) {
		const c = path[i];
		if (c === "\\") {
			// The escaped character is never a separator.
			i++;
		} else if (c === "/") {
			out.push(path.slice(start, i));
			start = i + 1;
		}
	}
	if (out.length < max) out.push(path.slice(start));
	return out;
}

/**
 * A segment's content: each escaped `\` or `/` as the character itself.
 *
 * Expects a segment as {@link parts} yields it from a valid path, where every `\` escapes
 * the character after it.
 */
// biome-ignore lint/suspicious/noShadowRestrictedNames: the name mirrors Rust; the deprecated global is unused.
export function unescape(segment: string): string {
	return segment.includes("\\") ? segment.replace(/\\(.)/gs, "$1") : segment;
}

/** The segment holding `content`: each `\` or `/` escaped by a `\`. The inverse of {@link unescape}. */
// biome-ignore lint/suspicious/noShadowRestrictedNames: the name mirrors Rust; the deprecated global is unused.
export function escape(content: string): string {
	return /[\\/]/.test(content) ? content.replace(/[\\/]/g, "\\$&") : content;
}

// Whether `segment` is one escaped segment: every `\` escapes a `\` or `/`, and no `/` is bare.
function isSegment(segment: string): boolean {
	return /^(?:[^\\/]|\\[\\/])*$/.test(segment);
}

// The most segments a prefix may have, matching the path limit on the wire.
const MAX_SEGMENTS = 32;

/** The non-empty segments of a path, normalized like a broadcast path. */
function splitPath(path: string): string[] {
	return parts(path).filter((part) => part !== "");
}

/** Thrown when a grant member is not a valid {@link Prefix}. */
export class InvalidPrefix extends Error {
	/** Which rule was broken. */
	readonly code: InvalidPrefix.Code;

	/** Create an error with the broken rule in `code` and a human-readable `message`. */
	constructor(code: InvalidPrefix.Code, message: string, options?: ErrorOptions) {
		super(message, options);
		this.name = "InvalidPrefix";
		this.code = code;
	}
}

/** Types for {@link InvalidPrefix}. */
export namespace InvalidPrefix {
	/** Why a string is not a valid {@link Prefix}. */
	export type Code =
		/** It holds a `*`: a grant is a prefix, never a wildcard. */
		| "wildcard"
		/** It is not a valid path: an empty segment, a bad escape, or too many segments. */
		| "path";
}

// Order two strings by code point, which agrees with UTF-8 byte order and so with the Rust crate.
function compareText(a: string, b: string): number {
	const n = Math.min(a.length, b.length);
	for (let i = 0; i < n; i++) {
		let x = a.charCodeAt(i);
		let y = b.charCodeAt(i);
		if (x === y) continue;
		// UTF-16 sorts surrogates below U+E000..U+FFFF; code point order puts them above.
		if (x >= 0xd800 && y >= 0xd800) {
			x = x < 0xe000 ? x + 0x2000 : x - 0x800;
			y = y < 0xe000 ? y + 0x2000 : y - 0x800;
		}
		return x - y;
	}
	return a.length - b.length;
}

// Order two segment lists segment by segment, so a list sorts right before every list it starts.
function compareSegments(a: readonly string[], b: readonly string[]): number {
	const n = Math.min(a.length, b.length);
	for (let i = 0; i < n; i++) {
		const order = compareText(a[i], b[i]);
		if (order !== 0) return order;
	}
	return a.length - b.length;
}

// Whether `inner` starts with every segment of `outer`.
function startsWith(outer: readonly string[], inner: readonly string[]): boolean {
	return outer.length <= inner.length && outer.every((segment, i) => segment === inner[i]);
}

function wildcard(text: string): InvalidPrefix {
	const hint =
		text === "**"
			? 'write the empty prefix "" for everything'
			: text.endsWith("/**") && !text.slice(0, -3).includes("*")
				? `write ${JSON.stringify(text.slice(0, -3))} for its subtree`
				: "wildcards are refused";
	return new InvalidPrefix(
		"wildcard",
		`${JSON.stringify(text)} is a wildcard, but a grant is a prefix covering its whole subtree: ${hint}`,
	);
}

/**
 * One grant member: a path covering itself and every path beneath it. The empty prefix
 * covers every path.
 *
 * A prefix is never a wildcard: `foo` covers `foo` and its subtree, so a grant writes `foo`
 * where releases before 0.22 wrote `foo/**`, and the empty prefix where they wrote `**`.
 * Build one with {@link Prefix.parse} or {@link Prefix.all}.
 *
 * @public
 */
export class Prefix {
	/** The canonical text: segments joined by `/`, escapes as written. */
	readonly text: string;
	/** The segments, still escaped as written. */
	readonly segments: readonly string[];

	private constructor(segments: readonly string[]) {
		this.segments = Object.freeze([...segments]);
		this.text = segments.join("/");
		Object.freeze(this);
	}

	/**
	 * Parse a grant member. Throws {@link InvalidPrefix}: `wildcard` for any `*`, and `path`
	 * for a leading, trailing, or doubled `/`, a `\` that escapes neither `\` nor `/`, or more
	 * than 32 segments.
	 */
	static parse(text: string): Prefix {
		if (text.includes("*")) throw wildcard(text);
		const invalid = (reason: string) =>
			new InvalidPrefix("path", `invalid prefix ${JSON.stringify(text)}: ${reason}`);
		// One past the limit is enough to refuse, without splitting a peer's megabytes of `a/a/...` first.
		const segments = parts(text, MAX_SEGMENTS + 1);
		if (segments.length > MAX_SEGMENTS) throw invalid(`more than ${MAX_SEGMENTS} segments`);
		for (const segment of segments) {
			if (segment === "") throw invalid("empty path segment");
			if (!isSegment(segment)) throw invalid(`invalid segment ${JSON.stringify(segment)}`);
		}
		return new Prefix(segments);
	}

	/** The empty prefix, which covers every path. */
	static all(): Prefix {
		return new Prefix([]);
	}

	/** Order two prefixes segment by segment, so a prefix sorts right before everything beneath it. */
	static compare(a: Prefix, b: Prefix): number {
		return compareSegments(a.segments, b.segments);
	}

	/** The canonical text. */
	toString(): string {
		return this.text;
	}

	/** Serializes as the canonical text, matching the Rust crate's serde form. */
	toJSON(): string {
		return this.text;
	}

	/** Whether the two are the same prefix. */
	equals(other: Prefix): boolean {
		return this.text === other.text;
	}

	/** Whether this is the empty prefix, covering every path. */
	get isEmpty(): boolean {
		return this.text === "";
	}

	/** Whether `path`, normalized like a broadcast path, is this prefix or beneath it. */
	matches(path: string): boolean {
		return startsWith(this.segments, splitPath(path));
	}

	/** Whether `path`, normalized like a broadcast path, and this prefix share a path: one lies beneath the other. */
	overlaps(path: string): boolean {
		const segments = splitPath(path);
		return startsWith(this.segments, segments) || startsWith(segments, this.segments);
	}

	/**
	 * This prefix placed beneath the path `root`, normalized like a broadcast path (slashes
	 * trimmed and collapsed). Throws {@link InvalidPrefix} when `root` holds a wildcard or a
	 * malformed escape, or the result has more than 32 segments.
	 */
	rooted(root: string): Prefix {
		const segments = [...Prefix.parse(splitPath(root).join("/")).segments, ...this.segments];
		if (segments.length > MAX_SEGMENTS) {
			throw new InvalidPrefix("path", `more than ${MAX_SEGMENTS} segments`);
		}
		return new Prefix(segments);
	}

	/**
	 * This prefix as seen from the path `root`, normalized like a broadcast path: the rest
	 * beneath it, or the empty prefix when this one covers `root` already. Undefined when the
	 * two share no path.
	 */
	rebase(root: string): Prefix | undefined {
		const base = splitPath(root);
		if (startsWith(this.segments, base)) return Prefix.all();
		if (startsWith(base, this.segments)) return new Prefix(this.segments.slice(base.length));
		return undefined;
	}
}

/**
 * A grant: a set of {@link Prefix}es, reduced so no member lies beneath another, and sorted
 * segment by segment, so two sets covering the same paths are {@link Prefixes.equals | equal}.
 * The empty set grants nothing.
 *
 * Every operation is near-linear in the members on every shape: a prefix's ancestors sort
 * right before it, so finding the member covering a path is one binary search.
 *
 * @public
 */
export class Prefixes implements Iterable<Prefix> {
	#members: Prefix[] = [];

	/** The reduced set of the given prefixes. Empty when none are given. */
	constructor(prefixes?: Iterable<Prefix>) {
		if (!prefixes) return;
		const sorted = [...prefixes].sort(Prefix.compare);
		// Sorted, a prefix's subtree is the run right after it, so one sweep against the last
		// member kept drops every covered one.
		for (const prefix of sorted) {
			const last = this.#members[this.#members.length - 1];
			if (!last || !startsWith(last.segments, prefix.segments)) this.#members.push(prefix);
		}
	}

	/** The set holding the empty prefix, which covers every path. */
	static all(): Prefixes {
		return new Prefixes([Prefix.all()]);
	}

	/** Parse each text as a {@link Prefix} into one set. Throws {@link InvalidPrefix}. */
	static parse(texts: Iterable<string>): Prefixes {
		return new Prefixes(Array.from(texts, (text) => Prefix.parse(text)));
	}

	/**
	 * Add a prefix, dropping the members beneath it.
	 *
	 * Returns false when a member already covers it, leaving the set unchanged.
	 */
	insert(prefix: Prefix): boolean {
		if (this.#covering(prefix.segments)) return false;
		const at = this.#after(prefix.segments);
		let end = at;
		while (end < this.#members.length && startsWith(prefix.segments, this.#members[end].segments)) end++;
		this.#members.splice(at, end - at, prefix);
		return true;
	}

	/**
	 * Whether some member covers `path`, normalized like a broadcast path. A member covering a
	 * path covers its whole subtree, so this also says whether the set grants everything beneath it.
	 */
	matches(path: string): boolean {
		return this.#covering(splitPath(path));
	}

	/** Whether some member lies at, above, or beneath `path`, normalized like a broadcast path. */
	overlaps(path: string): boolean {
		const segments = splitPath(path);
		if (this.#covering(segments)) return true;
		// Anything beneath the path sorts right after it.
		const next = this.#members[this.#after(segments)];
		return next !== undefined && startsWith(segments, next.segments);
	}

	/** Whether every member of `other` is covered here: `other` grants nothing this set does not. */
	covers(other: Prefixes): boolean {
		return other.#members.every((prefix) => this.#covering(prefix.segments));
	}

	/** The paths in both sets: each member of one that the other covers. */
	intersect(other: Prefixes): Prefixes {
		return new Prefixes([
			...this.#members.filter((prefix) => other.#covering(prefix.segments)),
			...other.#members.filter((prefix) => this.#covering(prefix.segments)),
		]);
	}

	/** What this set covers beneath `root`, named relative to it. See {@link Prefix.rebase}. */
	rebase(root: string): Prefixes {
		return new Prefixes(this.#members.flatMap((member) => member.rebase(root) ?? []));
	}

	/** Every member placed beneath `root`. See {@link Prefix.rooted}. */
	rooted(root: string): Prefixes {
		return new Prefixes(this.#members.map((member) => member.rooted(root)));
	}

	/** The number of members. */
	get size(): number {
		return this.#members.length;
	}

	/** The members, in canonical order. */
	[Symbol.iterator](): Iterator<Prefix> {
		return this.#members[Symbol.iterator]();
	}

	/** The members as a fresh array, in canonical order. */
	toArray(): Prefix[] {
		return [...this.#members];
	}

	/** Serializes as a list of texts, matching the Rust crate's serde form. */
	toJSON(): string[] {
		return this.#members.map((member) => member.text);
	}

	/** Whether the two sets cover the same paths. */
	equals(other: Prefixes): boolean {
		return this.size === other.size && this.#members.every((member, i) => member.equals(other.#members[i]));
	}

	// Whether the last member at or before `segments` covers them. In a reduced, sorted set it is
	// the only candidate: any covering member sorts before them, with only its own subtree, which
	// holds no other member, in between.
	#covering(segments: readonly string[]): boolean {
		const at = this.#after(segments);
		const member = this.#members[at] && compareSegments(this.#members[at].segments, segments) === 0 ? at : at - 1;
		return member >= 0 && startsWith(this.#members[member].segments, segments);
	}

	// The index of the first member sorting at or after `segments`.
	#after(segments: readonly string[]): number {
		let lo = 0;
		let hi = this.#members.length;
		while (lo < hi) {
			const mid = (lo + hi) >>> 1;
			if (compareSegments(this.#members[mid].segments, segments) < 0) lo = mid + 1;
			else hi = mid;
		}
		return lo;
	}
}

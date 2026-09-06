import { describe, expect, test } from "bun:test";
import { join } from "node:path";
import { compareSpecificity, type ErrorCode, Pattern, PatternError, Patterns, type Segment } from "./path.ts";

// The golden vectors live with the Rust crate, so both implementations replay one file.
interface Vectors {
	parse: { text: string; canonical?: string; segments?: Segment[] | null; error?: ErrorCode }[];
	literal: ({ path: string; pattern: string } | { path: string; error: ErrorCode })[];
	subtree: { path: string; pattern: string }[];
	head: { pattern: string; head: string; literal: boolean; globstar: boolean }[];
	matches: { pattern: string; path: string; expect: boolean }[];
	contains: { outer: string; inner: string; expect: boolean }[];
	overlaps: { a: string; b: string; expect: boolean }[];
	specificity: { descending: string[][]; equal: string[][] };
	rebase: { pattern: string; root: string; expect: string[] }[];
	rooted: ({ pattern: string; root: string; expect: string } | { pattern: string; root: string; error: ErrorCode })[];
	union: { input: string[]; reduced: string[] }[];
	unionContains: { union: string[]; pattern: string; expect: boolean }[];
}
const vectors = JSON.parse(
	await Bun.file(join(import.meta.dir, "../../../rs/moq-net/tests/pattern.json")).text(),
) as Vectors;

function errorCode(fn: () => unknown): ErrorCode | "ok" {
	try {
		fn();
		return "ok";
	} catch (err) {
		if (err instanceof PatternError) return err.code;
		throw err;
	}
}

function texts(patterns: Iterable<Pattern>): string[] {
	return [...patterns].map((p) => p.text);
}

describe("vectors", () => {
	test("parse", () => {
		for (const c of vectors.parse) {
			if (c.error) {
				expect(
					errorCode(() => Pattern.parse(c.text)),
					c.text,
				).toBe(c.error);
				continue;
			}
			const got = Pattern.parse(c.text);
			expect(got.text, c.text).toBe(c.canonical ?? c.text);
			if (c.segments) {
				expect(got.segments, c.text).toEqual(c.segments);
				expect(Pattern.from(c.segments).equals(got), c.text).toBe(true);
			}
		}
	});

	test("literal and subtree", () => {
		for (const c of vectors.literal) {
			if ("error" in c)
				expect(
					errorCode(() => Pattern.literal(c.path)),
					c.path,
				).toBe(c.error);
			else expect(Pattern.literal(c.path).text, c.path).toBe(c.pattern);
		}
		for (const c of vectors.subtree) expect(Pattern.subtree(c.path).text, c.path).toBe(c.pattern);
	});

	test("head", () => {
		for (const c of vectors.head) {
			const p = Pattern.parse(c.pattern);
			expect(p.head, c.pattern).toBe(c.head);
			expect(p.isLiteral, c.pattern).toBe(c.literal);
			expect(p.hasGlobstar, c.pattern).toBe(c.globstar);
		}
	});

	test("matches", () => {
		for (const c of vectors.matches) {
			expect(Pattern.parse(c.pattern).matches(c.path), `${c.pattern} matches ${c.path}`).toBe(c.expect);
		}
	});

	test("contains", () => {
		for (const c of vectors.contains) {
			expect(Pattern.parse(c.outer).contains(Pattern.parse(c.inner)), `${c.outer} contains ${c.inner}`).toBe(
				c.expect,
			);
		}
	});

	test("overlaps", () => {
		for (const c of vectors.overlaps) {
			const a = Pattern.parse(c.a);
			const b = Pattern.parse(c.b);
			expect(a.overlaps(b), `${c.a} overlaps ${c.b}`).toBe(c.expect);
			expect(b.overlaps(a), `${c.b} overlaps ${c.a}`).toBe(c.expect);
		}
	});

	test("specificity", () => {
		for (const list of vectors.specificity.descending) {
			for (let i = 1; i < list.length; i++) {
				const a = Pattern.parse(list[i - 1]).specificity();
				const b = Pattern.parse(list[i]).specificity();
				expect(compareSpecificity(a, b), `${list[i - 1]} outranks ${list[i]}`).toBeGreaterThan(0);
			}
		}
		for (const [a, b] of vectors.specificity.equal) {
			expect(
				compareSpecificity(Pattern.parse(a).specificity(), Pattern.parse(b).specificity()),
				`${a} ties ${b}`,
			).toBe(0);
		}
	});

	test("rebase", () => {
		for (const c of vectors.rebase) {
			expect(texts(Pattern.parse(c.pattern).rebase(c.root)), `${c.pattern} at ${c.root}`).toEqual(
				texts(new Patterns(c.expect.map((t) => Pattern.parse(t)))),
			);
		}
	});

	test("rooted", () => {
		for (const c of vectors.rooted) {
			const p = Pattern.parse(c.pattern);
			if ("error" in c)
				expect(
					errorCode(() => p.rooted(c.root)),
					`${c.pattern} under ${c.root}`,
				).toBe(c.error);
			else expect(p.rooted(c.root).text, `${c.pattern} under ${c.root}`).toBe(c.expect);
		}
	});

	test("union", () => {
		for (const c of vectors.union) {
			const set = new Patterns(c.input.map((t) => Pattern.parse(t)));
			expect(texts(set), JSON.stringify(c.input)).toEqual(c.reduced);
		}
		for (const c of vectors.unionContains) {
			const set = new Patterns(c.union.map((t) => Pattern.parse(t)));
			expect(set.contains(Pattern.parse(c.pattern)), `${JSON.stringify(c.union)} contains ${c.pattern}`).toBe(
				c.expect,
			);
		}
	});
});

// Every pattern over an alphabet of segments with up to `maxPattern` of them, and every
// path over `parts` with up to `max` segments. Two alphabets: one exercises the segment
// structure (globstar head and tail interplay needs three-segment patterns and
// six-segment paths for every witness to fit), the other exercises partial segments
// against multi-byte parts.
interface Alphabet {
	segments: Segment[];
	maxPattern: number;
	parts: string[];
	maxPath: number;
}

const ALPHABETS: Alphabet[] = [
	{
		segments: [
			{ kind: "literal", value: "a" },
			{ kind: "literal", value: "b" },
			{ kind: "wildcard" },
			{ kind: "globstar" },
		],
		maxPattern: 3,
		parts: ["a", "b", "c"],
		maxPath: 6,
	},
	{
		segments: [
			{ kind: "literal", value: "a" },
			{ kind: "literal", value: "ab" },
			{ kind: "partial", prefix: "a", suffix: "" },
			{ kind: "partial", prefix: "", suffix: "b" },
			{ kind: "partial", prefix: "a", suffix: "b" },
			{ kind: "wildcard" },
			{ kind: "globstar" },
		],
		maxPattern: 2,
		parts: ["a", "b", "ab", "ba", "aab"],
		maxPath: 4,
	},
];

function allPatterns(alphabet: Alphabet): Pattern[] {
	const out = [Pattern.empty()];
	let layer: Segment[][] = [[]];
	for (let len = 0; len < alphabet.maxPattern; len++) {
		const next: Segment[][] = [];
		for (const prefix of layer) {
			for (const choice of alphabet.segments) {
				const segments = [...prefix, choice];
				try {
					out.push(Pattern.from(segments));
					next.push(segments);
				} catch (err) {
					if (!(err instanceof PatternError)) throw err;
				}
			}
		}
		layer = next;
	}
	return out;
}

function allPaths(alphabet: Alphabet, max: number): string[] {
	const out = [""];
	let layer: string[][] = [[]];
	for (let len = 0; len < max; len++) {
		const next: string[][] = [];
		for (const prefix of layer) {
			for (const part of alphabet.parts) {
				const parts = [...prefix, part];
				out.push(parts.join("/"));
				next.push(parts);
			}
		}
		layer = next;
	}
	return out;
}

function joinPath(root: string, rel: string): string {
	return root === "" ? rel : rel === "" ? root : `${root}/${rel}`;
}

describe("exhaustive", () => {
	test("contains and overlaps agree with matching", () => {
		for (const alphabet of ALPHABETS) {
			const patterns = allPatterns(alphabet);
			const paths = allPaths(alphabet, alphabet.maxPath);
			// Which paths each pattern matches, computed once so the pairwise checks
			// compare tables instead of re-matching strings.
			const table = patterns.map((pattern) => paths.map((p) => pattern.matches(p)));
			for (const [i, a] of patterns.entries()) {
				for (const [j, b] of patterns.entries()) {
					let contains = true;
					let overlaps = false;
					for (let k = 0; k < paths.length; k++) {
						if (table[j][k] && !table[i][k]) contains = false;
						if (table[i][k] && table[j][k]) overlaps = true;
					}
					expect(a.contains(b), `${a} contains ${b}`).toBe(contains);
					expect(a.overlaps(b), `${a} overlaps ${b}`).toBe(overlaps);
					expect(a.equals(b), `${a} equals ${b}`).toBe(contains && b.contains(a));
					expect(new Patterns([a, b]).equals(new Patterns([b, a]))).toBe(true);
					if (contains && !b.contains(a)) {
						expect(
							compareSpecificity(a.specificity(), b.specificity()),
							`${a} ranks below ${b}`,
						).toBeLessThan(0);
					}
				}
			}
		}
	});

	test("rebase matches exactly what lies beneath the root", () => {
		for (const alphabet of ALPHABETS) {
			const relative = allPaths(alphabet, 3);
			for (const pattern of allPatterns(alphabet)) {
				for (const root of allPaths(alphabet, 2)) {
					const rebased = pattern.rebase(root);
					for (const rel of relative) {
						expect(rebased.matches(rel), `${pattern} at ${root} on ${rel}`).toBe(
							pattern.matches(joinPath(root, rel)),
						);
					}
				}
			}
		}
	});

	test("rooted inverts rebase", () => {
		for (const alphabet of ALPHABETS) {
			for (const pattern of allPatterns(alphabet)) {
				for (const root of allPaths(alphabet, 2)) {
					const rooted = pattern.rooted(root);
					expect(rooted.rebase(root).contains(pattern), `${pattern} under ${root}`).toBe(true);
					for (const p of allPaths(alphabet, 3)) {
						expect(rooted.matches(joinPath(root, p)), `${rooted} on ${joinPath(root, p)}`).toBe(
							pattern.matches(p),
						);
					}
				}
			}
		}
	});

	test("text round trips", () => {
		for (const alphabet of ALPHABETS) {
			for (const pattern of allPatterns(alphabet)) {
				expect(Pattern.parse(pattern.text).equals(pattern)).toBe(true);
				expect(Pattern.from(pattern.segments).equals(pattern)).toBe(true);
				expect(JSON.parse(JSON.stringify(pattern))).toBe(pattern.text);
			}
		}
	});
});

describe("random text", () => {
	test("canonical text is a parse fixed point", () => {
		// Deterministic xorshift so a failure reproduces.
		let state = 0x9e3779b9;
		const next = () => {
			state ^= state << 13;
			state ^= state >>> 17;
			state ^= state << 5;
			return state >>> 0;
		};
		const pieces = ["a", "b", "*", "**", "/", "", "a*", ".hang", "//", "*."];
		for (let i = 0; i < 20_000; i++) {
			const len = next() % 8;
			let text = "";
			for (let j = 0; j < len; j++) text += pieces[next() % pieces.length];
			let pattern: Pattern;
			try {
				pattern = Pattern.parse(text);
			} catch (err) {
				if (err instanceof PatternError) continue;
				throw err;
			}
			expect(Pattern.parse(pattern.text).equals(pattern)).toBe(true);
			expect(pattern.contains(pattern) && pattern.overlaps(pattern)).toBe(true);
		}
	});
});

describe("Patterns", () => {
	test("insert reduces by containment", () => {
		const set = new Patterns();
		expect(set.insert(Pattern.parse("a/b"))).toBe(true);
		expect(set.insert(Pattern.parse("a/b"))).toBe(false);
		expect(set.insert(Pattern.parse("a/*"))).toBe(true);
		expect(texts(set)).toEqual(["a/*"]);
		expect(set.insert(Pattern.parse("**"))).toBe(true);
		expect(texts(set)).toEqual(["**"]);
		expect(set.size).toBe(1);
	});

	test("equality is set equality", () => {
		const a = new Patterns([Pattern.parse("a"), Pattern.parse("b")]);
		const b = new Patterns([Pattern.parse("b"), Pattern.parse("a"), Pattern.parse("a")]);
		expect(a.equals(b)).toBe(true);
		expect(a.covers(b) && b.covers(a)).toBe(true);
		expect(new Patterns().covers(a)).toBe(false);
		expect(a.covers(new Patterns())).toBe(true);
		expect(JSON.stringify(a)).toBe('["a","b"]');
	});
});

test("patterns own immutable segment records", () => {
	const literal = { kind: "literal" as const, value: "a" };
	const partial = { kind: "partial" as const, prefix: "b", suffix: "c" };
	const pattern = Pattern.from([literal, partial]);
	literal.value = "x";
	partial.prefix = "y";
	partial.suffix = "z";
	expect(pattern.text).toBe("a/b*c");
	expect(pattern.matches("a/bc")).toBe(true);
	expect(pattern.matches("x/yz")).toBe(false);
	expect(pattern.contains(Pattern.parse("a/b*c"))).toBe(true);
	expect(pattern.equals(Pattern.parse("a/b*c"))).toBe(true);
	for (const source of [pattern, Pattern.parse("a/*/**"), Pattern.literal("a"), Pattern.subtree("a")]) {
		for (const segment of source.segments) expect(Object.isFrozen(segment)).toBe(true);
	}
});

test("equivalent wildcard placements have one identity", () => {
	for (const [a, b] of [
		["*/**", "**/*"],
		["a/*/**/*/b", "a/**/*/*/b"],
	]) {
		const left = Pattern.parse(a);
		const right = Pattern.parse(b);
		expect(left.equals(right)).toBe(true);
		expect(new Patterns([left, right]).equals(new Patterns([right, left]))).toBe(true);
	}
});

test("ordering keeps distinct JS strings distinct", () => {
	const a = Pattern.literal("\ud800");
	const b = Pattern.literal("\ufffd");
	expect(Pattern.compare(a, b)).not.toBe(0);
	expect(new Patterns([a, b]).equals(new Patterns([b, a]))).toBe(true);
});

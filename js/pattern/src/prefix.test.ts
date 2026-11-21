import { describe, expect, test } from "bun:test";
import { InvalidPrefix, Pattern, Prefix, Prefixes } from "./index.ts";

function code(fn: () => unknown): InvalidPrefix.Code | "ok" {
	try {
		fn();
		return "ok";
	} catch (err) {
		if (err instanceof InvalidPrefix) return err.code;
		throw err;
	}
}

function set(...texts: string[]): Prefixes {
	return Prefixes.parse(texts);
}

describe("Prefix", () => {
	test("refuses every wildcard loudly", () => {
		for (const text of ["**", "a/**", "*", "cam-*.hang", "a*", "a/*/b", "**/a", "a\\*"]) {
			expect(
				code(() => Prefix.parse(text)),
				text,
			).toBe("wildcard");
		}
		expect(() => Prefix.parse("live/**")).toThrow('write "live" for its subtree');
		expect(() => Prefix.parse("**")).toThrow('write the empty prefix "" for everything');
	});

	test("refuses malformed paths", () => {
		for (const text of ["/a", "a/", "a//b", "a\\b", Array(33).fill("a").join("/")]) {
			expect(
				code(() => Prefix.parse(text)),
				text,
			).toBe("path");
		}
		expect(code(() => Prefix.parse(Array(32).fill("a").join("/")))).toBe("ok");
	});

	test("the empty prefix covers everything", () => {
		const all = Prefix.parse("");
		expect(all.equals(Prefix.all())).toBe(true);
		for (const path of ["", "a", "a/b/c"]) expect(all.matches(path)).toBe(true);
		expect(all.pattern().text).toBe("**");
		expect(all.isEmpty).toBe(true);
		expect(Prefix.parse("a").isEmpty).toBe(false);
	});

	test("matches segment by segment", () => {
		const room = Prefix.parse("room");
		expect(room.matches("room")).toBe(true);
		expect(room.matches("room/alice")).toBe(true);
		expect(room.matches("roomy")).toBe(false);
		expect(room.matches("room-b/alice")).toBe(false);
		expect(room.matches("")).toBe(false);
		expect(room.pattern().text).toBe("room/**");
	});

	test("respects escapes", () => {
		const escaped = Prefix.parse("a\\/b");
		expect(escaped.segments).toEqual(["a\\/b"]);
		expect(escaped.matches("a\\/b/c")).toBe(true);
		expect(escaped.matches("a/b/c")).toBe(false);
		expect(Prefix.parse("a").matches("a\\/b")).toBe(false);
		expect(Prefix.parse("a\\\\").matches("a\\\\/b")).toBe(true);
	});

	test("overlaps a path above or beneath it", () => {
		const ab = Prefix.parse("a/b");
		expect(ab.overlaps("a")).toBe(true);
		expect(ab.overlaps("a/b/c")).toBe(true);
		expect(ab.overlaps("")).toBe(true);
		expect(ab.overlaps("a/c")).toBe(false);
		expect(ab.overlaps("a/bc")).toBe(false);
		// A path no prefix can spell still gets an answer.
		expect(Prefix.parse("cam").matches("cam/*")).toBe(true);
		expect(Prefix.parse("cam/x").overlaps("cam/*")).toBe(false);
	});

	test("rooted and rebase", () => {
		expect(Prefix.parse("b").rooted("a").text).toBe("a/b");
		expect(Prefix.all().rooted("a").text).toBe("a");
		expect(code(() => Prefix.parse("b").rooted("a/*"))).toBe("wildcard");
		// A root is normalized like a broadcast path, as a pattern's is.
		expect(Prefix.parse("b").rooted("/a//c/").text).toBe("a/c/b");
		expect(Prefix.parse("a/b").rebase("/a//")?.text).toBe("b");
		// A root ending inside an escape is no segment of a prefix.
		expect(Prefix.parse("a\\/b").rebase("a\\")).toBeUndefined();
		expect(Prefix.parse("a\\/b").rebase("a\\/b")?.text).toBe("");
		expect(code(() => Prefix.parse(Array(30).fill("a").join("/")).rooted("x/y/z"))).toBe("path");

		expect(Prefix.parse("a/b").rebase("a")?.text).toBe("b");
		expect(Prefix.parse("a").rebase("a")?.text).toBe("");
		expect(Prefix.parse("a").rebase("a/b")?.text).toBe("");
		expect(Prefix.parse("a").rebase("c")).toBeUndefined();
		expect(Prefix.parse("a").rebase("")?.text).toBe("a");
	});

	test("orders segment by segment", () => {
		const sorted = ["a-b", "a/b", "a", "ab", "a/b/c"].map((text) => Prefix.parse(text)).sort(Prefix.compare);
		expect(sorted.map((prefix) => prefix.text)).toEqual(["a", "a/b", "a/b/c", "a-b", "ab"]);
	});
});

describe("Prefixes", () => {
	test("reduces and sorts", () => {
		expect(set("a-b", "a/b", "a", "ab").toJSON()).toEqual(["a", "a-b", "ab"]);
		expect(set("x", "", "y").toJSON()).toEqual([""]);
		expect(set().size).toBe(0);
		expect(set("b", "a").equals(set("a", "b", "a/c"))).toBe(true);
		expect(Prefixes.all().toJSON()).toEqual([""]);
	});

	test("insert drops the members beneath it", () => {
		const prefixes = set("a/b", "a/c", "a-b", "b");
		expect(prefixes.insert(Prefix.parse("a/b/c"))).toBe(false);
		expect(prefixes.insert(Prefix.parse("a"))).toBe(true);
		expect(prefixes.toJSON()).toEqual(["a", "a-b", "b"]);
		expect(prefixes.insert(Prefix.parse("0"))).toBe(true);
		expect(prefixes.toJSON()).toEqual(["0", "a", "a-b", "b"]);
	});

	test("matches and overlaps paths", () => {
		const grant = set("a/b", "a-b", "c");
		expect(grant.matches("a/b/x")).toBe(true);
		expect(grant.matches("a/b")).toBe(true);
		expect(grant.matches("a")).toBe(false);
		expect(grant.matches("a/c")).toBe(false);
		expect(grant.matches("a-b")).toBe(true);
		expect(grant.matches("c/d")).toBe(true);
		expect(grant.overlaps("a")).toBe(true);
		expect(grant.overlaps("")).toBe(true);
		expect(grant.overlaps("a/b/c")).toBe(true);
		expect(grant.overlaps("a/c")).toBe(false);
		expect(grant.overlaps("b")).toBe(false);
		expect(grant.overlaps("a/bc")).toBe(false);
		expect(grant.matches("c/*")).toBe(true);
		expect(set().matches("")).toBe(false);
		expect(set().overlaps("")).toBe(false);
		expect(Prefixes.all().matches("anything/at/all")).toBe(true);
	});

	test("covers", () => {
		expect(set("a", "b").covers(set("a/x", "b"))).toBe(true);
		expect(set("a/x").covers(set("a"))).toBe(false);
		expect(set("a").covers(set())).toBe(true);
		expect(set().covers(set("a"))).toBe(false);
	});

	test("intersect keeps the deeper of each overlapping pair", () => {
		expect(
			set("a", "b/c")
				.intersect(set("a/x", "b", "d"))
				.toJSON(),
		).toEqual(["a/x", "b/c"]);
		expect(set("a").intersect(set("a")).toJSON()).toEqual(["a"]);
		expect(Prefixes.all().intersect(set("x", "y")).toJSON()).toEqual(["x", "y"]);
		expect(set("a").intersect(set("b")).size).toBe(0);
	});

	test("rebase and rooted", () => {
		expect(set("a/b", "a/c", "d").rebase("a").toJSON()).toEqual(["b", "c"]);
		expect(set("a", "d").rebase("a/b").toJSON()).toEqual([""]);
		expect(set("b", "c").rooted("a").toJSON()).toEqual(["a/b", "a/c"]);
	});

	test("patterns are each member's subtree", () => {
		expect(set("", "x").patterns().toJSON()).toEqual(["**"]);
		expect(set("a", "b/c").patterns().toJSON()).toEqual(["a/**", "b/c/**"]);
		expect(set("a").patterns().matches("a/z")).toBe(true);
		expect(set("a").patterns().contains(Pattern.parse("a/*"))).toBe(true);
	});

	test("near-linear on a large grant", () => {
		// A member per path and a query per member: quadratic work would take seconds here.
		const n = 50_000;
		const texts = Array.from({ length: n }, (_, i) => `room/${i}/cam`);
		const grant = Prefixes.parse(texts);
		const wider = Prefixes.parse(Array.from({ length: n }, (_, i) => `room/${i}`));
		expect(grant.size).toBe(n);
		expect(wider.covers(grant)).toBe(true);
		expect(grant.covers(wider)).toBe(false);
		expect(wider.intersect(grant).size).toBe(n);
	});
});

import { expect, spyOn, test } from "bun:test";
import * as Path from "./path.ts";

/** Brand a literal as a relative reference; the tests feed raw strings on purpose. */
const asRel = (s: string) => s as Path.Relative;

test("from refuses empty segments", () => {
	for (const raw of ["/foo", "foo/", "/foo/bar/", "foo//bar", "/", "//"]) {
		expect(() => Path.from(raw)).toThrow("empty segment");
		expect(() => Path.from("ok", raw)).toThrow("empty segment");
	}
	expect(Path.from("foo/bar")).toBe("foo/bar" as Path.Valid);
});

test("from refuses a backslash that escapes neither a backslash nor a slash", () => {
	for (const raw of ["\\", "a\\", "\\a", "a\\b", "a\\\\\\b", "a\\.b/c"]) {
		expect(() => Path.from(raw)).toThrow("escapes neither");
		expect(() => Path.normalizeRelative(raw)).toThrow("escapes neither");
	}
	// An escape takes exactly the byte after it, so runs of backslashes pair up.
	for (const raw of ["\\\\", "a\\/b", "\\/a", "a\\\\/b", "a\\\\\\/b"]) {
		expect(Path.from(raw)).toBe(raw as Path.Valid);
	}
	// A component ending in half an escape is refused even though the join would complete it.
	expect(() => Path.from("a\\", "b")).toThrow("escapes neither");
});

test("resolve refuses a stranded escape", () => {
	// Segments split at an escaped `/` too, so popping past one strands its `\`.
	const base = Path.from("a\\/b/c");
	expect(() => Path.resolve(base, asRel(".."))).toThrow("escapes neither");
	expect(Path.tryResolve(base, asRel(".."))).toBeUndefined();
	expect(Path.resolve(base, asRel("d"))).toBe(Path.from("a\\/b/d"));
});

test("from treats the empty path as the root", () => {
	expect(Path.from("")).toBe(Path.empty());
	expect(Path.from()).toBe(Path.empty());
});

test("hasPrefix matches exact paths", () => {
	const path = Path.from("foo/bar");
	expect(Path.hasPrefix(Path.from("foo/bar"), path)).toBe(true);
});

test("hasPrefix matches proper prefixes", () => {
	const path = Path.from("foo/bar/baz");
	expect(Path.hasPrefix(Path.from("foo"), path)).toBe(true);
	expect(Path.hasPrefix(Path.from("foo/bar"), path)).toBe(true);
});

test("hasPrefix does not match partial segment prefixes", () => {
	const path = Path.from("foobar");
	expect(Path.hasPrefix(Path.from("foo"), path)).toBe(false);

	const path2 = Path.from("foo/bar");
	expect(Path.hasPrefix(Path.from("fo"), path2)).toBe(false);
});

test("hasPrefix handles empty prefix", () => {
	const path = Path.from("foo/bar");
	expect(Path.hasPrefix(Path.empty(), path)).toBe(true);
});

test("stripPrefix strips valid prefixes", () => {
	const path = Path.from("foo/bar/baz");

	const suffix1 = Path.stripPrefix(Path.from("foo"), path);
	expect(suffix1).toBe("bar/baz" as Path.Valid);

	const suffix2 = Path.stripPrefix(Path.from("foo/bar"), path);
	expect(suffix2).toBe("baz" as Path.Valid);

	const suffix3 = Path.stripPrefix(Path.from("foo/bar/baz"), path);
	expect(suffix3).toBe("" as Path.Valid);
});

test("stripPrefix returns null for invalid prefixes", () => {
	const path = Path.from("foo/bar");
	expect(Path.stripPrefix(Path.from("notfound"), path)).toBe(null);
	expect(Path.stripPrefix(Path.from("fo"), path)).toBe(null);
});

test("stripPrefix handles empty prefix", () => {
	const path = Path.from("foo/bar");
	const result = Path.stripPrefix(Path.empty(), path);
	expect(result).toBe("foo/bar" as Path.Valid);
});

test("stripPrefix accepts Path instances", () => {
	const path = Path.from("foo/bar/baz");
	const prefix = Path.from("foo/bar");
	const result = Path.stripPrefix(prefix, path);
	expect(result).toBe("baz" as Path.Valid);
});

test("join paths with slashes", () => {
	const base = Path.from("foo");
	const joined = Path.join(base, Path.from("bar"));
	expect(joined).toBe("foo/bar" as Path.Valid);
});

test("join handles empty base", () => {
	const base = Path.empty();
	const joined = Path.join(base, Path.from("bar"));
	expect(joined).toBe("bar" as Path.Valid);
});

test("join handles empty suffix", () => {
	const base = Path.from("foo");
	const joined = Path.join(base, Path.empty());
	expect(joined).toBe("foo" as Path.Valid);
});

test("join accepts Path instances", () => {
	const base = Path.from("foo");
	const suffix = Path.from("bar");
	const joined = Path.join(base, suffix);
	expect(joined).toBe("foo/bar" as Path.Valid);
});

test("join handles multiple joins", () => {
	const path = Path.join(
		Path.join(Path.join(Path.from("api"), Path.from("v1")), Path.from("users")),
		Path.from("123"),
	);
	expect(path).toBe("api/v1/users/123" as Path.Valid);
});

test("handles special characters", () => {
	const path = Path.from("foo-bar_baz.txt");
	expect(path).toBe("foo-bar_baz.txt" as Path.Valid);
	expect(Path.hasPrefix(Path.from("foo-bar"), path)).toBe(false);
	expect(Path.hasPrefix(Path.from("foo-bar_baz.txt"), path)).toBe(true);
});

test("from accepts multiple arguments", () => {
	expect(Path.from("foo", "bar", "baz")).toBe("foo/bar/baz" as Path.Valid);
	expect(Path.from("api", "v1", "users")).toBe("api/v1/users" as Path.Valid);
});

test("from handles empty strings in arguments", () => {
	expect(Path.from("foo", "", "bar")).toBe("foo/bar" as Path.Valid);
	expect(Path.from("", "foo", "bar", "")).toBe("foo/bar" as Path.Valid);
});

test("resolve replaces the base name", () => {
	expect(Path.resolve(Path.from("a/b"), asRel("c"))).toBe(Path.from("a/c"));
	expect(Path.resolve(Path.from("a/b"), asRel("c/d"))).toBe(Path.from("a/c/d"));
	expect(Path.resolve(Path.from("foo.hang/catalog.pro"), asRel("./transcode.pro"))).toBe(
		Path.from("foo.hang/transcode.pro"),
	);
});

test("resolve with empty rel returns base", () => {
	expect(Path.resolve(Path.from("a/b"), asRel(""))).toBe(Path.from("a/b"));
});

test("resolve single dotdot pops one segment", () => {
	expect(Path.resolve(Path.from("a/b/c"), asRel("../d"))).toBe(Path.from("a/d"));
	expect(Path.resolve(Path.from("a/b/c"), asRel(".."))).toBe(Path.from("a"));
});

test("resolve multiple dotdot pops multiple segments", () => {
	expect(Path.resolve(Path.from("a/b/c"), asRel("../../x"))).toBe(Path.from("x"));
	expect(Path.resolve(Path.from("a/b/c"), asRel("../../../x"))).toBe(Path.from("x"));
});

test("resolve excess dotdot clamps at empty", () => {
	expect(Path.resolve(Path.from("a"), asRel("../../../foo"))).toBe(Path.from("foo"));
	expect(Path.resolve(Path.from("a"), asRel(".."))).toBe(Path.from(""));
});

test("relative inverts resolve", () => {
	// Nested under the base: the base's own last segment is replaced, so it repeats.
	expect(Path.relative(Path.from("foo/bar/baz"), Path.from("foo/bar"))).toBe(asRel("bar/baz"));
	// Sibling.
	expect(Path.relative(Path.from("foo/baz"), Path.from("foo/bar"))).toBe(asRel("baz"));
	// Different subtree.
	expect(Path.relative(Path.from("foo/baz/bar"), Path.from("foo/bar/baz"))).toBe(asRel("../baz/bar"));
	// The base's parent, which only `.` can name.
	expect(Path.relative(Path.from("a/b"), Path.from("a/b/transcode.hang"))).toBe(asRel("."));
	expect(Path.relative(Path.from("a/b"), Path.from("a/b/one/two/transcode.hang"))).toBe(asRel("../.."));
	// Roots.
	expect(Path.relative(Path.from("foo/bar"), Path.empty())).toBe(asRel("foo/bar"));
	expect(Path.relative(Path.empty(), Path.from("foo"))).toBe(asRel("."));
	// The base itself, which only the empty reference names.
	expect(Path.relative(Path.from("a/b"), Path.from("a/b"))).toBe(asRel(""));
	expect(Path.relative(Path.empty(), Path.empty())).toBe(asRel(""));
});

test("relative rejects unnameable targets", () => {
	// A segment literally named `.` or `..` is a legal path component, but resolution
	// would walk on it instead of naming it.
	expect(Path.relative(Path.from("a/../b"), Path.empty())).toBeUndefined();
	expect(Path.relative(Path.from("x/./y"), Path.from("x/z"))).toBeUndefined();
	expect(Path.relative(Path.from("a/.."), Path.from("a/b"))).toBeUndefined();

	// A base is always nameable by itself, however its last segment is spelled.
	expect(Path.relative(Path.from("a/.."), Path.from("a/.."))).toBe(asRel(""));

	// Dot segments inside the shared prefix are never emitted, so they are fine.
	const rel = Path.relative(Path.from("a/../b/x"), Path.from("a/../b/c"));
	expect(rel).toBe(asRel("x"));
	expect(Path.resolve(Path.from("a/../b/c"), rel as Path.Relative)).toBe(Path.from("a/../b/x"));
});

test("relative round trips through resolve", () => {
	const paths = ["", "a", "b", "a/b", "a/c", "a/b/c", "a/b/c/d", "x/y/z", "a/../b", "a/./b", "a/..", "a/."].map((p) =>
		Path.from(p),
	);

	for (const base of paths) {
		for (const target of paths) {
			const rel = Path.relative(target, base);
			if (rel === undefined) {
				// Only an unnameable target may be refused, and never the base itself.
				expect(target !== base && target.split("/").some((part) => part === "." || part === "..")).toBe(true);
				continue;
			}

			expect(Path.resolve(base, rel)).toBe(target);
			// The reference is derived from a real target, so it never escapes the root.
			expect(Path.tryResolve(base, rel)).toBe(target);
		}
	}
});

test("resolve with empty base", () => {
	expect(Path.resolve(Path.empty(), asRel("foo"))).toBe(Path.from("foo"));
	expect(Path.resolve(Path.empty(), asRel(".."))).toBe(Path.from(""));
});

test("resolve dot names the base parent", () => {
	expect(Path.resolve(Path.from("a/b"), asRel("."))).toBe(Path.from("a"));
	expect(Path.resolve(Path.from("a/b"), asRel("./c"))).toBe(Path.from("a/c"));
	expect(Path.resolve(Path.from("a/b"), asRel("./../c"))).toBe(Path.from("c"));
	expect(Path.resolve(Path.from("a/b"), asRel("foo/./bar"))).toBe(Path.from("a/foo/bar"));
});

test("resolve self-reference via sibling name equals base", () => {
	expect(Path.resolve(Path.from("a/b"), asRel("./b"))).toBe(Path.from("a/b"));
});

test("tryResolve distinguishes the root from an escape", () => {
	expect(Path.tryResolve(Path.from("top"), asRel("."))).toBe(Path.empty());
	expect(Path.tryResolve(Path.from("top"), asRel(".."))).toBeUndefined();
	expect(Path.tryResolve(Path.from("a/b"), asRel(".."))).toBe(Path.empty());
	expect(Path.tryResolve(Path.from("a/b"), asRel("../.."))).toBeUndefined();
});

test("normalizeRelative preserves an all-dot reference", () => {
	expect(Path.normalizeRelative("")).toBe(asRel(""));
	expect(Path.normalizeRelative(".")).toBe(asRel("."));
	expect(Path.normalizeRelative("./.")).toBe(asRel("."));
	expect(Path.normalizeRelative("./foo")).toBe(asRel("foo"));
	expect(Path.normalizeRelative("foo/./bar")).toBe(asRel("foo/bar"));
	expect(Path.normalizeRelative("../foo")).toBe(asRel("../foo"));
	// Dropping a `.` cannot strand the `\` of an escaped `/` before it.
	expect(() => Path.normalizeRelative("a\\/.")).toThrow("escapes neither");
	expect(Path.normalizeRelative("a\\\\/.")).toBe(asRel("a\\\\"));
});

test("normalizeRelative refuses empty segments", () => {
	for (const raw of ["/", "/foo", "foo/", "./", "foo//bar", "./foo/", "../"]) {
		expect(() => Path.normalizeRelative(raw)).toThrow("empty segment");
	}
});

test("parts splits a path into its components", () => {
	expect(Path.parts(Path.from(""))).toEqual([]);
	expect(Path.parts(Path.from("foo"))).toEqual(["foo"]);
	expect(Path.parts(Path.from("foo/bar"))).toEqual(["foo", "bar"]);
});

test("decode enforces the max part count", () => {
	const atLimit = Array.from({ length: Path.MAX_PARTS }, (_, i) => `${i}`).join("/");
	expect(Path.decode(atLimit)).toBe(atLimit as Path.Valid);
	expect(() => Path.decode(`${atLimit}/extra`)).toThrow();
});

test("decode never allocates per slash of a hostile path", () => {
	// Splitting a path would hold one entry per slash, so decode must count separators and stop at
	// the limit instead, even for the most slashes MAX_BYTES admits.
	const slashes = "/".repeat(Path.MAX_BYTES);
	const parts = `${"a/".repeat(Path.MAX_BYTES / 2 - 1)}a`;
	const split = spyOn(String.prototype, "split");
	const indexOf = spyOn(String.prototype, "indexOf");
	try {
		expect(() => Path.decode(slashes)).toThrow("empty segment");
		indexOf.mockClear();
		let error: unknown;
		try {
			Path.decode(parts);
		} catch (e) {
			error = e;
		}
		const scans = indexOf.mock.calls.length;
		expect(error).toBeInstanceOf(Error);
		expect(split).not.toHaveBeenCalled();
		// The scan stops at the limit rather than walking the whole input.
		expect(scans).toBeLessThanOrEqual(Path.MAX_PARTS + 1);
	} finally {
		split.mockRestore();
		indexOf.mockRestore();
	}
});

test("encode enforces the max part count", () => {
	const atLimit = Array.from({ length: Path.MAX_PARTS }, (_, i) => `${i}`).join("/") as Path.Valid;
	expect(Path.encode(atLimit)).toBe(atLimit);
	expect(() => Path.encode(`${atLimit}/extra` as Path.Valid)).toThrow();
});

test("encode and decode enforce the max byte length in UTF-8", () => {
	const atLimit = "a".repeat(Path.MAX_BYTES) as Path.Valid;
	expect(Path.encode(atLimit)).toBe(atLimit);
	expect(Path.decode(atLimit)).toBe(atLimit);
	expect(() => Path.encode(`${atLimit}b` as Path.Valid)).toThrow();
	expect(() => Path.decode(`${atLimit}b`)).toThrow();

	// One UTF-16 unit of "é" is two UTF-8 bytes, so half as many units reach the limit.
	const wide = "é".repeat(Math.floor(Path.MAX_BYTES / 2)) as Path.Valid;
	expect(Path.encode(wide)).toBe(wide);
	expect(() => Path.encode(`${wide}é` as Path.Valid)).toThrow();
	expect(() => Path.decode(`${wide}é`)).toThrow();
});

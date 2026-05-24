import { expect, test } from "bun:test";
import * as Path from "./path.ts";

test("Path constructor trims leading and trailing slashes", () => {
	expect(Path.from("/foo/bar/")).toBe("foo/bar" as Path.Valid);
	expect(Path.from("///foo/bar///")).toBe("foo/bar" as Path.Valid);
	expect(Path.from("foo/bar")).toBe("foo/bar" as Path.Valid);
});

test("Path constructor handles empty paths", () => {
	expect(Path.from("")).toBe("" as Path.Valid);
	expect(Path.from("/")).toBe("" as Path.Valid);
	expect(Path.from("///")).toBe("" as Path.Valid);
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

test("hasPrefix ignores trailing slashes in prefix", () => {
	const path = Path.from("foo/bar");
	expect(Path.hasPrefix(Path.from("foo/"), path)).toBe(true);
	expect(Path.hasPrefix(Path.from("foo/bar/"), path)).toBe(true);
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

test("isEmpty checks correctly", () => {
	expect(Path.from("") === "").toBe(true);
	expect(Path.from("foo") === "").toBe(false);
	expect(Path.empty() === "").toBe(true);
});

test("length property works correctly", () => {
	expect(Path.from("foo").length).toBe(3);
	expect(Path.from("foo/bar").length).toBe(7);
	expect(Path.empty().length).toBe(0);
});

test("equals checks correctly", () => {
	const path1 = Path.from("foo/bar");
	const path2 = Path.from("/foo/bar/");
	const path3 = Path.from("foo/baz");

	expect(path1 === path2).toBe(true);
	expect(path1 === path3).toBe(false);
});

test("JSON serialization works", () => {
	const path = Path.from("foo/bar");
	expect(JSON.stringify(path)).toBe('"foo/bar"');
});

test("handles paths with multiple consecutive slashes", () => {
	const path = Path.from("foo//bar///baz");
	// Multiple consecutive slashes are collapsed to single slashes
	expect(path).toBe("foo/bar/baz" as Path.Valid);
});

test("removes multiple slashes comprehensively", () => {
	// Test various multiple slash scenarios
	expect(Path.from("foo//bar")).toBe("foo/bar" as Path.Valid);
	expect(Path.from("foo///bar")).toBe("foo/bar" as Path.Valid);
	expect(Path.from("foo////bar")).toBe("foo/bar" as Path.Valid);

	// Multiple occurrences of double slashes
	expect(Path.from("foo//bar//baz")).toBe("foo/bar/baz" as Path.Valid);
	expect(Path.from("a//b//c//d")).toBe("a/b/c/d" as Path.Valid);

	// Mixed slash counts
	expect(Path.from("foo//bar///baz////qux")).toBe("foo/bar/baz/qux" as Path.Valid);

	// With leading and trailing slashes
	expect(Path.from("//foo//bar//")).toBe("foo/bar" as Path.Valid);
	expect(Path.from("///foo///bar///")).toBe("foo/bar" as Path.Valid);

	// Edge case: only slashes
	expect(Path.from("//")).toBe("" as Path.Valid);
	expect(Path.from("////")).toBe("" as Path.Valid);

	// Test that operations work correctly with normalized paths
	const pathWithSlashes = Path.from("foo//bar///baz");
	expect(Path.hasPrefix(Path.from("foo/bar"), pathWithSlashes)).toBe(true);
	expect(Path.stripPrefix(Path.from("foo"), pathWithSlashes)).toBe("bar/baz" as Path.Valid);
	expect(Path.join(pathWithSlashes, Path.from("qux"))).toBe("foo/bar/baz/qux" as Path.Valid);
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

test("from sanitizes multiple arguments with slashes", () => {
	expect(Path.from("/foo/", "/bar/", "/baz/")).toBe("foo/bar/baz" as Path.Valid);
	expect(Path.from("foo//", "//bar", "baz")).toBe("foo/bar/baz" as Path.Valid);
});

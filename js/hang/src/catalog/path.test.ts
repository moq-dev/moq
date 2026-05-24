import { expect, test } from "bun:test";
import { Path } from "@moq/lite";
import { normalizeRelativeBroadcast, resolveBroadcast } from "./path.ts";

test("resolveBroadcast appends named segments", () => {
	const base = Path.from("a/b");
	expect(resolveBroadcast(base, "c")).toBe(Path.from("a/b/c"));
	expect(resolveBroadcast(base, "c/d")).toBe(Path.from("a/b/c/d"));
});

test("resolveBroadcast with empty rel returns base", () => {
	expect(resolveBroadcast(Path.from("a/b"), "")).toBe(Path.from("a/b"));
});

test("resolveBroadcast single dotdot pops one segment", () => {
	const base = Path.from("a/b/c");
	expect(resolveBroadcast(base, "../d")).toBe(Path.from("a/b/d"));
	expect(resolveBroadcast(base, "..")).toBe(Path.from("a/b"));
});

test("resolveBroadcast multiple dotdot pops multiple segments", () => {
	const base = Path.from("a/b/c");
	expect(resolveBroadcast(base, "../../x")).toBe(Path.from("a/x"));
	expect(resolveBroadcast(base, "../../../x")).toBe(Path.from("x"));
});

test("resolveBroadcast excess dotdot clamps at empty", () => {
	const base = Path.from("a");
	expect(resolveBroadcast(base, "../../../foo")).toBe(Path.from("foo"));
	expect(resolveBroadcast(base, "..")).toBe(Path.from(""));
});

test("resolveBroadcast with empty base", () => {
	const base = Path.from("");
	expect(resolveBroadcast(base, "foo")).toBe(Path.from("foo"));
	expect(resolveBroadcast(base, "..")).toBe(Path.from(""));
});

test("resolveBroadcast treats dot as a no-op", () => {
	const base = Path.from("a/b");
	expect(resolveBroadcast(base, ".")).toBe(Path.from("a/b"));
	expect(resolveBroadcast(base, "./c")).toBe(Path.from("a/b/c"));
	expect(resolveBroadcast(base, "./../c")).toBe(Path.from("a/c"));
	expect(resolveBroadcast(base, "foo/./bar")).toBe(Path.from("a/b/foo/bar"));
});

test("resolveBroadcast self-reference via dotdot equals base", () => {
	const base = Path.from("a/b");
	expect(resolveBroadcast(base, "../b")).toBe(base);
});

test("normalizeRelativeBroadcast drops empty and dot segments", () => {
	expect(normalizeRelativeBroadcast("")).toBe("");
	expect(normalizeRelativeBroadcast(".")).toBe("");
	expect(normalizeRelativeBroadcast("./foo")).toBe("foo");
	expect(normalizeRelativeBroadcast("foo//bar")).toBe("foo/bar");
	expect(normalizeRelativeBroadcast("foo/./bar")).toBe("foo/bar");
	expect(normalizeRelativeBroadcast("/foo/")).toBe("foo");
	expect(normalizeRelativeBroadcast("../foo")).toBe("../foo");
});

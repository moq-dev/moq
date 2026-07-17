import { expect, test } from "bun:test";
import { authorize, type Claims } from "./claims.ts";

// These cases mirror the Rust moq-token crate's claims::tests one-for-one, so both
// sides stay pinned to the same authorization semantics.

function claims(root: string, get: string[], put: string[]): Claims {
	return { root, get, put };
}

test("authorize - path equals root", () => {
	const permissions = authorize(claims("room/123", [""], ["alice"]), "room/123");
	expect(permissions).toEqual({ subscribe: [""], publish: ["alice"] });
});

test("authorize - path extends root", () => {
	// Connecting below the root consumes the matching part of each grant.
	const permissions = authorize(claims("room/123", ["bob"], ["alice"]), "room/123/alice");
	expect(permissions).toEqual({ subscribe: [], publish: [""] });
});

test("authorize - path is parent of root", () => {
	// Connecting above the root prepends it, keeping the grants anchored.
	const permissions = authorize(claims("demo", [""], ["alice"]), "/");
	expect(permissions).toEqual({ subscribe: ["demo"], publish: ["demo/alice"] });
});

test("authorize - empty root", () => {
	// A root-scoped token grants everything it lists, wherever it connects.
	const permissions = authorize(claims("", ["demo"], []), "demo/room");
	expect(permissions).toEqual({ subscribe: [""], publish: [] });
});

test("authorize - slashes are implicit", () => {
	const permissions = authorize(claims("/room/123/", ["/bob/"], []), "//room/123//");
	expect(permissions.subscribe).toEqual(["bob"]);
});

test("authorize - accepts a single path as well as a list", () => {
	const permissions = authorize({ root: "demo", get: "bob", put: "alice" }, "demo");
	expect(permissions).toEqual({ subscribe: ["bob"], publish: ["alice"] });
});

test("authorize - respects segment boundaries", () => {
	// "foo" must not cover "foobar".
	expect(() => authorize(claims("foo", [""], [""]), "foobar")).toThrow(/does not overlap/);
});

test("authorize - unrelated path", () => {
	expect(() => authorize(claims("demo", [""], [""]), "other")).toThrow(/does not overlap/);
});

test("authorize - no access at path", () => {
	// The path overlaps the root, but every grant sits outside it.
	expect(() => authorize(claims("", ["demo"], []), "other")).toThrow(/grants no access/);
});

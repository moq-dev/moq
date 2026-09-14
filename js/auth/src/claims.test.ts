import { expect, test } from "bun:test";
import { authorize, type Claims, ClaimsSchema, ScopeSchema } from "./claims.ts";

// These cases mirror the Rust moq-auth crate's claims::tests one-for-one, so both
// sides stay pinned to the same authorization semantics.

function claims(root: string, subscribe: string[], publish: string[]): Claims {
	return { root, subscribe, publish };
}

function texts(permissions: { subscribe: { toJSON(): string[] }; publish: { toJSON(): string[] } }) {
	return { subscribe: permissions.subscribe.toJSON(), publish: permissions.publish.toJSON() };
}

test("claims granting nothing are rejected, matching Rust's useless-token rule", () => {
	// An empty list grants nothing, so the Rust crate refuses to verify such a token
	// ("no publish or subscribe allowed; token is useless"). Presence alone is not enough.
	expect(() => ClaimsSchema.parse({ root: "demo", publish: [] })).toThrow();
	expect(() => ClaimsSchema.parse({ root: "demo", publish: [], subscribe: [] })).toThrow();
	expect(() => ClaimsSchema.parse({ root: "demo" })).toThrow();

	// `**` is everything under the root, which is a real grant.
	expect(ClaimsSchema.parse({ root: "demo", publish: ["**"] }).publish).toEqual(["**"]);
});

test("claims refuse the retired put/get prefix fields", () => {
	expect(() => ClaimsSchema.parse({ root: "demo", put: ["alice"] })).toThrow();
	expect(() => ClaimsSchema.parse({ root: "demo", get: "" })).toThrow();
	expect(() => ClaimsSchema.parse({ root: "demo", publish: ["alice"], get: [""] })).toThrow();
	expect(() => ScopeSchema.parse({ root: "demo", put: ["alice"] })).toThrow();
});

test("claims refuse a bad pattern", () => {
	expect(() => ClaimsSchema.parse({ publish: ["a/**/b/**"] })).toThrow();
	expect(() => ClaimsSchema.parse({ publish: ["/leading"] })).toThrow();
});

test("authorize - path equals root", () => {
	const permissions = authorize(claims("room/123", ["**"], ["alice/**"]), "room/123");
	expect(texts(permissions)).toEqual({ subscribe: ["**"], publish: ["alice/**"] });
});

test("authorize - path extends root", () => {
	// Connecting below the root consumes the matching part of each grant.
	const permissions = authorize(claims("room/123", ["bob/**"], ["alice/**"]), "room/123/alice");
	expect(texts(permissions)).toEqual({ subscribe: [], publish: ["**"] });
});

test("authorize - a literal reached exactly is the path itself", () => {
	const permissions = authorize(claims("room", [], ["alice"]), "room/alice");
	expect(texts(permissions)).toEqual({ subscribe: [], publish: [""] });
});

test("authorize - path is parent of root", () => {
	// Connecting above the root prepends it, keeping the grants anchored.
	const permissions = authorize(claims("demo", ["**"], ["alice/**"]), "/");
	expect(texts(permissions)).toEqual({ subscribe: ["demo/**"], publish: ["demo/alice/**"] });
});

test("authorize - empty root", () => {
	// A root-scoped token grants everything it lists, wherever it connects.
	const permissions = authorize(claims("", ["demo/**"], []), "demo/room");
	expect(texts(permissions)).toEqual({ subscribe: ["**"], publish: [] });
});

test("authorize - slashes are implicit", () => {
	const permissions = authorize(claims("/room/123/", ["bob/**"], []), "//room/123//");
	expect(permissions.subscribe.toJSON()).toEqual(["bob/**"]);
});

test("authorize - respects segment boundaries", () => {
	// "foo" must not cover "foobar".
	expect(() => authorize(claims("foo", ["**"], ["**"]), "foobar")).toThrow(/does not overlap/);
});

test("authorize - unrelated path", () => {
	expect(() => authorize(claims("demo", ["**"], ["**"]), "other")).toThrow(/does not overlap/);
});

test("authorize - no access at path", () => {
	// The path overlaps the root, but every grant sits outside it.
	expect(() => authorize(claims("", ["demo/**"], []), "other")).toThrow(/grants no access/);
});

test("authorize - wildcards rebase as a set", () => {
	// `**/chat` reached at `chat` is both the path itself and deeper `**/chat`.
	const permissions = authorize(claims("", ["**/chat"], []), "chat");
	expect(permissions.subscribe.toJSON()).toEqual(["", "**/chat"]);
});

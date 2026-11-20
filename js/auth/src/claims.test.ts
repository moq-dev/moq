import { expect, test } from "bun:test";
import { Claims, ClaimsSchema, type Permissions, ScopeSchema } from "./claims.ts";
import { GrantSchema } from "./contract.ts";
import { encodeClaims, encodeGrants } from "./wire.ts";

// These cases mirror the Rust moq-auth crate's claims::tests one-for-one, so both
// sides stay pinned to the same authorization semantics.

function claims(root: string, subscribe: string[], publish: string[]) {
	return ClaimsSchema.parse({ root, subscribe, publish });
}

function texts(permissions: Permissions) {
	return { subscribe: permissions.subscribe.toJSON(), publish: permissions.publish.toJSON() };
}

test("claims granting nothing are rejected, matching Rust's useless-token rule", () => {
	// An empty list grants nothing, so the Rust crate refuses to verify such a token
	// ("no publish or subscribe allowed; token is useless"). Presence alone is not enough.
	expect(() => ClaimsSchema.parse({ root: "demo", publish: [] })).toThrow();
	expect(() => ClaimsSchema.parse({ root: "demo", publish: [], subscribe: [] })).toThrow();
	expect(() => ClaimsSchema.parse({ root: "demo" })).toThrow();

	// The empty prefix is everything under the root, which is a real grant.
	expect(ClaimsSchema.parse({ root: "demo", publish: [""] }).publish).toEqual([""]);
});

test("claims read legacy put/get prefixes as the same prefixes", () => {
	const claims = ClaimsSchema.parse({ root: "demo", put: ["alice", "/a//b/"], get: "" });
	expect(claims.publish).toEqual(["alice", "a/b"]);
	expect(claims.subscribe).toEqual([""]);
	expect(ScopeSchema.parse({ root: "demo", put: ["room"] }).publish).toEqual(["room"]);
});

// TEMPORARY MIGRATION (see `readPrefix` in wire.ts): released signers and auth servers spell a
// subtree as a pattern, which reads as its prefix.
test("every grant reads a subtree pattern as its prefix", () => {
	for (const [text, prefix] of [
		["**", ""],
		["alice/**", "alice"],
		["a\\/b/**", "a\\/b"],
	]) {
		expect(ClaimsSchema.parse({ publish: [text] }).publish, text).toEqual([prefix]);
		expect(ClaimsSchema.parse({ subscribe: { path: [text], rate: 8000 } }).subscribe, text).toEqual({
			path: [prefix],
			rate: 8000,
		});
		expect(ScopeSchema.parse({ subscribe: [text] }).subscribe, text).toEqual([prefix]);
		expect(GrantSchema.parse({ publish: [text] }).publish, text).toEqual([prefix ? `${prefix}/**` : "**"]);
	}
});

test("every grant refuses any other wildcard", () => {
	for (const text of ["*", "cam-*.hang", "a*", "room/*/cam", "room/*/**", "**/room", "/**", "a\\/**"]) {
		expect(ClaimsSchema.safeParse({ publish: [text] }).success, text).toBe(false);
		expect(ClaimsSchema.safeParse({ subscribe: { path: [text], rate: 8000 } }).success, text).toBe(false);
		expect(ScopeSchema.safeParse({ subscribe: [text] }).success, text).toBe(false);
		expect(GrantSchema.safeParse({ publish: [text] }).success, text).toBe(false);
	}
	expect(() => ClaimsSchema.parse({ publish: ["room/*/**"] })).toThrow(/wildcards are refused/);
});

test("claims refuse mixed encodings and wildcard legacy prefixes", () => {
	expect(() => ClaimsSchema.parse({ root: "demo", publish: ["alice"], get: [""] })).toThrow();
	expect(() => ClaimsSchema.parse({ root: "demo", put: [], subscribe: ["alice"] })).toThrow();
	expect(() => ClaimsSchema.parse({ root: "demo", put: ["a/*"] })).toThrow();
	expect(() => ScopeSchema.parse({ root: "demo", put: ["room"], publish: ["x"] })).toThrow();
	// A legacy scope held only lists.
	expect(() => ScopeSchema.parse({ root: "demo", put: "room" })).toThrow();
});

// TEMPORARY MIGRATION (see `readPrefix` in wire.ts): spelled as the subtree patterns a release
// before 0.22 reads.
test("grants are written as publish and subscribe, never legacy put and get", () => {
	expect(encodeGrants({ root: "live", publish: ["camera1"], subscribe: [""] })).toEqual({
		root: "live",
		publish: ["camera1/**"],
		subscribe: ["**"],
	});
	expect(encodeGrants({ root: "live", publish: ["camera1"], subscribe: [] })).toEqual({
		root: "live",
		publish: ["camera1/**"],
	});
	// Nothing lies beneath a prefix of the most segments a path holds, so it is written as its literal.
	const deepest = Array(32).fill("a").join("/");
	expect(encodeGrants({ publish: [deepest] })).toEqual({ publish: [deepest] });
	expect(ClaimsSchema.parse(encodeGrants({ publish: [deepest] })).publish).toEqual([deepest]);
});

test("claims read the registered names and write them back", () => {
	const wire = { root: "live", publish: ["**"], exp: 4102444800, iat: 1, nbf: 2, jti: "abc-123" };
	const claims = ClaimsSchema.parse(wire);
	expect(claims).toEqual({
		root: "live",
		publish: [""],
		expires: 4102444800,
		issued: 1,
		notBefore: 2,
		id: "abc-123",
	});
	expect(encodeClaims(claims)).toEqual(wire);
});

test("a token ID is a bounded printable string", () => {
	for (const jti of ["a", "550e8400-e29b-41d4-a716-446655440000", "user:42/a.b+c=", "x".repeat(128)]) {
		expect(ClaimsSchema.parse({ publish: [""], jti }).id).toBe(jti);
	}
	for (const jti of ["", "has space", "caf\u00e9", "x".repeat(129), 7, null, ["x"]]) {
		expect(ClaimsSchema.safeParse({ publish: [""], jti }).success).toBe(false);
	}
});

test("claims exp and iat are whole seconds", () => {
	expect(() => ClaimsSchema.parse({ publish: [""], exp: 1.5 })).toThrow();
	expect(() => ClaimsSchema.parse({ publish: [""], iat: 1.5 })).toThrow();
	expect(ClaimsSchema.parse({ publish: [""], exp: 4102444800 }).expires).toBe(4102444800);
});

test("claims refuse a malformed prefix", () => {
	for (const text of ["/leading", "trailing/", "a//b", "a\\b"]) {
		expect(ClaimsSchema.safeParse({ publish: [text] }).success, text).toBe(false);
	}
});

test("authorize - path equals root", () => {
	const permissions = Claims.authorize(claims("room/123", [""], ["alice"]), "room/123");
	expect(texts(permissions)).toEqual({ subscribe: [""], publish: ["alice"] });
});

test("authorize - path extends root", () => {
	// Connecting below the root consumes the matching part of each grant.
	const permissions = Claims.authorize(claims("room/123", ["bob"], ["alice"]), "room/123/alice");
	expect(texts(permissions)).toEqual({ subscribe: [], publish: [""] });
});

test("authorize - a path beneath a prefix keeps all of it", () => {
	// A prefix covers its subtree, so connecting anywhere beneath it grants everything there.
	const permissions = Claims.authorize(claims("room", [], ["alice"]), "room/alice/cam");
	expect(texts(permissions)).toEqual({ subscribe: [], publish: [""] });
});

test("authorize - path is parent of root", () => {
	// Connecting above the root prepends it, keeping the grants anchored.
	const permissions = Claims.authorize(claims("demo", [""], ["alice"]), "/");
	expect(texts(permissions)).toEqual({ subscribe: ["demo"], publish: ["demo/alice"] });
});

test("authorize - empty root", () => {
	// A root-scoped token grants everything it lists, wherever it connects.
	const permissions = Claims.authorize(claims("", ["demo"], []), "demo/room");
	expect(texts(permissions)).toEqual({ subscribe: [""], publish: [] });
});

test("authorize - slashes are implicit", () => {
	const permissions = Claims.authorize(claims("/room/123/", ["bob"], []), "//room/123//");
	expect(permissions.subscribe.toJSON()).toEqual(["bob"]);
});

test("authorize - respects segment boundaries", () => {
	// "foo" must not cover "foobar".
	expect(() => Claims.authorize(claims("foo", [""], [""]), "foobar")).toThrow(/does not overlap/);
	// Nor does a prefix `alice` cover `alice-b`.
	expect(Claims.authorize(claims("", [], ["alice"]), "").publish.matches("alice-b")).toBe(false);
});

test("authorize - unrelated path", () => {
	expect(() => Claims.authorize(claims("demo", [""], [""]), "other")).toThrow(/does not overlap/);
});

test("authorize - no access at path", () => {
	// The path overlaps the root, but every grant sits outside it.
	expect(() => Claims.authorize(claims("", ["demo"], []), "other")).toThrow(/grants no access/);
});

test("authorize - keeps only the prefixes beneath the path", () => {
	const permissions = Claims.authorize(claims("", ["a/x", "a/y/z", "b"], []), "a");
	expect(permissions.subscribe.toJSON()).toEqual(["x", "y/z"]);
});

test("authorize - reads an escaped separator as content", () => {
	// `a\/b` is one segment holding a `/`, the spelling of a moq-transport field.
	const grant = claims("room", ["a\\/b/x"], ["a\\/b"]);
	expect(texts(Claims.authorize(grant, "room/a\\/b"))).toEqual({ subscribe: ["x"], publish: [""] });
	expect(Claims.authorize(grant, "room").publish.matches("a\\/b")).toBe(true);
	expect(Claims.authorize(grant, "room").publish.matches("a/b")).toBe(false);
	expect(Claims.authorize(claims("room", [""], []), "//room/a\\///").subscribe.toJSON()).toEqual([""]);
	// `a\` ends inside that segment, so it is no path the root covers.
	expect(() => Claims.authorize(claims("a\\/b", [""], []), "a\\")).toThrow(/does not overlap/);
});

test("shared rate vectors parse and re-encode like Rust", async () => {
	const vectors = await Bun.file(new URL("../../../rs/moq-auth/rate.json", import.meta.url)).json();
	for (const row of vectors.valid) {
		expect(encodeGrants(ClaimsSchema.parse(row.input))).toEqual(row.claims);
		// A grant's type is its wire shape, so what parses is what a server answers with.
		expect(GrantSchema.parse(row.input)).toEqual(row.grant);
	}
	for (const input of vectors.invalid) {
		expect(ClaimsSchema.safeParse(input).success).toBe(false);
		expect(GrantSchema.safeParse(input).success).toBe(false);
	}
});

test("a capped role keeps its cap in the role and never encodes as a prefix", () => {
	for (const role of ["publish", "subscribe"] as const) {
		// Subtrees alone would be written as legacy prefixes, which a cap must not be.
		const claims = ClaimsSchema.parse({
			publish: ["alice"],
			subscribe: [""],
			[role]: { path: ["alice"], rate: 8000 },
		});
		expect(claims[role]).toEqual({ path: ["alice"], rate: 8000 });
		const encoded = encodeGrants(claims);
		expect(encoded).not.toHaveProperty("put");
		expect(encoded).not.toHaveProperty("get");
		expect(encoded[role]).toEqual({ path: ["alice/**"], rate: 8000 });
	}
	expect(() => ClaimsSchema.parse({ publish: { path: [""], rate: 0 } })).toThrow(/rate must be a positive integer/);
});

test("a key scope bounds paths, never bitrate", () => {
	expect(ScopeSchema.safeParse({ publish: { path: [""], rate: 8000 } }).success).toBe(false);
	expect(ScopeSchema.safeParse({ publish: [""], subscribe: { path: [""], rate: 8000 } }).success).toBe(false);
});

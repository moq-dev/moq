import { expect, test } from "bun:test";
import { claims } from "./token.ts";

test("claims root the token at the room and scope publish to the identity", () => {
	expect(claims("meet/demo", "alice")).toEqual({
		root: "meet/demo",
		subscribe: ["**"],
		publish: ["alice/**"],
	});
});

test("claims rejects an identity with an empty segment", () => {
	for (const identity of ["alice/", "/alice", "a//b"]) {
		expect(() => claims("room", identity)).toThrow("empty segment");
	}
});

test("claims accepts a multi-segment identity", () => {
	expect(claims("hang/room", "guest/uuid")).toEqual({
		root: "hang/room",
		subscribe: ["**"],
		publish: ["guest/uuid/**"],
	});
});

test("claims rejects an empty identity", () => {
	expect(() => claims("room", "")).toThrow("must not be empty");
});

test("claims rejects identities with wildcards", () => {
	for (const identity of ["*", "alice*", "a/*/b", "**"]) {
		expect(() => claims("room", identity)).toThrow();
	}
});

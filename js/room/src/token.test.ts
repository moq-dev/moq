import { expect, test } from "bun:test";
import { claims } from "./token.ts";

test("claims root the token at the room and scope publish to the identity", () => {
	expect(claims("meet/demo", "alice")).toEqual({
		root: "meet/demo",
		subscribe: ["**"],
		publish: ["alice/**"],
	});
});

test("claims does not double a trailing slash on publish", () => {
	expect(claims("meet/demo", "alice/").publish).toEqual(["alice/**"]);
});

test("claims accepts a multi-segment identity", () => {
	expect(claims("hang/room", "guest/uuid")).toEqual({
		root: "hang/room",
		subscribe: ["**"],
		publish: ["guest/uuid/**"],
	});
});

test("claims rejects empty normalized identities", () => {
	for (const identity of ["", "/", "///"]) {
		expect(() => claims("room", identity)).toThrow();
	}
});

test("claims rejects identities with wildcards", () => {
	for (const identity of ["*", "alice*", "a/*/b", "**"]) {
		expect(() => claims("room", identity)).toThrow();
	}
});

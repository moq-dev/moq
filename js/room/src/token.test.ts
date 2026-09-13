import { expect, test } from "bun:test";
import { claims } from "./token.ts";

test("claims root the token at the room and scope put to the identity", () => {
	expect(claims("meet/demo", "alice")).toEqual({
		root: "meet/demo",
		get: "",
		put: "alice/",
	});
});

test("claims does not double a trailing slash on put", () => {
	expect(claims("meet/demo", "alice/").put).toBe("alice/");
});

test("claims accepts a multi-segment identity", () => {
	expect(claims("hang/room", "guest/uuid")).toEqual({
		root: "hang/room",
		get: "",
		put: "guest/uuid/",
	});
});

test("claims rejects empty normalized identities", () => {
	for (const identity of ["", "/", "///"]) {
		expect(() => claims("room", identity)).toThrow();
	}
});

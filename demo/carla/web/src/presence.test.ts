import { describe, expect, test } from "bun:test";
import { presence } from "./presence.ts";

describe("presence", () => {
	test("a connected relay with nothing announced means the simulator is offline", () => {
		expect(presence("connected", 0)).toBe("offline");
	});

	test("clears as soon as one vehicle is announced", () => {
		expect(presence("connected", 1)).toBe("online");
	});

	test("does not call the simulator offline before the relay answers", () => {
		expect(presence("connecting", 0)).toBe("connecting");
		expect(presence("disconnected", 0)).toBe("connecting");
	});

	test("keeps vehicles shown regardless of the session", () => {
		expect(presence("disconnected", 2)).toBe("online");
	});
});

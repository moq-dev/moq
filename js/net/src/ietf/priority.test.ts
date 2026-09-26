import { expect, test } from "bun:test";
import { infoDefaults } from "../track.ts";
import { fromWire, toWire } from "./priority.ts";

test("IETF subscriber priority is lower first", () => {
	expect(fromWire(0)).toBe(0xff);
	expect(fromWire(0xff)).toBe(0);
	expect(toWire(0xff)).toBe(0);
	expect(toWire(0)).toBe(0xff);

	for (let priority = 0; priority < 0xff; priority++) {
		expect(fromWire(priority)).toBeGreaterThan(fromWire(priority + 1));
	}
});

test("subscriber priority round trips", () => {
	for (let priority = 0; priority <= 0xff; priority++) {
		expect(fromWire(toWire(priority))).toBe(priority);
	}
});

test("an unset track priority is the draft's usual publisher priority", () => {
	expect(toWire(infoDefaults().priority)).toBe(128);
});

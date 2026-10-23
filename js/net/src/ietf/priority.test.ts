import { expect, test } from "bun:test";
import * as Lite from "../lite/priority.ts";
import { infoDefaults } from "../track.ts";
import { ABSENT_WIRE, fromWire, toWire } from "./priority.ts";

test("IETF priority is lower first", () => {
	expect(fromWire(0)).toBe(127);
	expect(fromWire(0xff)).toBe(-128);
	expect(toWire(127)).toBe(0);
	expect(toWire(-128)).toBe(0xff);

	for (let priority = 0; priority < 0xff; priority++) {
		expect(fromWire(priority)).toBeGreaterThan(fromWire(priority + 1));
	}
});

test("priority round trips", () => {
	for (let priority = 0; priority <= 0xff; priority++) {
		expect(toWire(fromWire(priority))).toBe(priority);
	}
});

// Both wires carry one urgency: the IETF byte is the moq-lite byte flipped.
test("IETF mirrors moq-lite", () => {
	for (let priority = -128; priority <= 127; priority++) {
		expect(toWire(priority)).toBe(0xff - Lite.toWire(priority));
	}
});

test("an unset track priority goes out as the absent byte", () => {
	expect(toWire(infoDefaults().priority)).toBe(127);
	expect(fromWire(ABSENT_WIRE)).toBe(0);
});

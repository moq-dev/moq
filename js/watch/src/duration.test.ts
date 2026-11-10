import { describe, expect, it } from "bun:test";
import { Duration } from "@moq/clock";
import { formatDuration, parseDuration } from "./duration";

const ms = (value: number) => Duration.millis(value);

describe("parseDuration", () => {
	it("parses milliseconds", () => {
		expect(parseDuration("300ms")).toEqual(ms(300));
		expect(parseDuration("0ms")).toEqual(ms(0));
		expect(parseDuration("1.5ms")).toEqual(ms(1.5));
	});

	it("parses seconds", () => {
		expect(parseDuration("30s")).toEqual(ms(30_000));
		expect(parseDuration("0.25s")).toEqual(ms(250));
	});

	it("rejects a bare number, which would otherwise be a 1000x error", () => {
		expect(parseDuration("30")).toBeUndefined();
		expect(parseDuration("100")).toBeUndefined();
	});

	it("accepts a bare zero, where the unit can't matter", () => {
		expect(parseDuration("0")).toEqual(ms(0));
	});

	it("rejects anything else", () => {
		expect(parseDuration("")).toBeUndefined();
		expect(parseDuration("-5ms")).toBeUndefined();
		expect(parseDuration("auto")).toBeUndefined();
		expect(parseDuration("1e3ms")).toBeUndefined();
		expect(parseDuration("30 s")).toBeUndefined();
		expect(parseDuration("30sec")).toBeUndefined();
		expect(parseDuration("30m")).toBeUndefined();
	});

	it("tolerates surrounding whitespace", () => {
		expect(parseDuration("  30s ")).toEqual(ms(30_000));
	});
});

describe("formatDuration", () => {
	it("round-trips through parseDuration, which is what keeps attribute reflection settled", () => {
		expect(parseDuration(formatDuration(ms(300)))).toEqual(ms(300));
		expect(parseDuration(formatDuration(ms(30_000)))).toEqual(ms(30_000));
		expect(parseDuration(formatDuration(ms(0)))).toEqual(ms(0));
	});

	it("round-trips a fraction, since the reflected value is parsed straight back", () => {
		// Rounding here would rewrite the caller's value: 1.5 becoming 2, and 0.4 becoming 0,
		// which also switches buffered playback off.
		expect(parseDuration(formatDuration(ms(1.5)))).toEqual(ms(1.5));
		expect(parseDuration(formatDuration(ms(0.4)))).toEqual(ms(0.4));
	});
});

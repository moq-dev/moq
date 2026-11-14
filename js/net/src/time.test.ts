import { expect, test } from "bun:test";
import { floorAt, keyOf, Timescale, Timestamp } from "./time.ts";

test("Timescale accepts the safe integer range", () => {
	expect(Timescale(1)).toBe(Timescale.SECOND);
	expect(Timescale(Number.MAX_SAFE_INTEGER)).toBe(Number.MAX_SAFE_INTEGER as Timescale);
});

test("Timescale rejects values that cannot encode losslessly", () => {
	for (const value of [
		0,
		-1,
		1.5,
		Number.NaN,
		Number.POSITIVE_INFINITY,
		Number.NEGATIVE_INFINITY,
		Number.MAX_SAFE_INTEGER + 1,
		2 ** 62,
		2 ** 62 + 1,
		1e100,
	]) {
		expect(() => Timescale(value)).toThrow(RangeError);
	}
});

test("Timestamp rejects negative values", () => {
	expect(() => new Timestamp(-1, Timescale.MILLI)).toThrow();
	expect(() => Timestamp.fromMicros(-1)).toThrow();
});

test("Timestamp rejects non-finite values", () => {
	expect(() => new Timestamp(Number.NaN, Timescale.MILLI)).toThrow();
	expect(() => Timestamp.fromMillis(Number.POSITIVE_INFINITY)).toThrow();
});

test("Timestamp accepts fractional values in the safe range", () => {
	expect(new Timestamp(1.5, Timescale.MILLI).value).toBe(1.5);
	expect(new Timestamp(0, Timescale.MILLI).value).toBe(0);
	expect(new Timestamp(Number.MAX_SAFE_INTEGER, Timescale.MILLI).value).toBe(Number.MAX_SAFE_INTEGER);
});

test("Timestamp rejects values past the safe integer range", () => {
	expect(() => new Timestamp(Number.MAX_SAFE_INTEGER + 1, Timescale.MILLI)).toThrow(RangeError);
	expect(() => new Timestamp(2 ** 62, Timescale.MILLI)).toThrow(RangeError);
});

test("Timestamp.as converts within the safe range", () => {
	expect(Timestamp.fromMillis(2000).as(Timescale.MICRO)).toBe(2_000_000);
	expect(Timestamp.fromMillis(2000).as(Timescale.MILLI)).toBe(2000);
	expect(Timestamp.fromMicros(2_000_000).as(Timescale.MILLI)).toBe(2000);
});

test("Timestamp.as rejects conversions past the safe integer range", () => {
	// Source value and destination scale are each legal; the converted wire value is not.
	expect(() => Timestamp.fromMillis(2000).as(Timescale(Number.MAX_SAFE_INTEGER))).toThrow(RangeError);
});

test("Timestamp.isBefore orders across scales exactly", () => {
	expect(Timestamp.fromMillis(1).isBefore(Timestamp.fromMicros(1_001))).toBe(true);
	expect(Timestamp.fromMillis(1).isBefore(Timestamp.fromMicros(1_000))).toBe(false);
	expect(Timestamp.fromMicros(1_000).isBefore(Timestamp.fromMillis(1))).toBe(false);
	// Products past 2^53 still compare exactly.
	const micros = new Timestamp(Number.MAX_SAFE_INTEGER, Timescale.MICRO);
	const millis = new Timestamp(Number.MAX_SAFE_INTEGER - 1, Timescale.MILLI);
	expect(micros.isBefore(millis)).toBe(true);
	expect(millis.isBefore(micros)).toBe(false);
	expect(new Timestamp(0.5, Timescale.SECOND).isBefore(Timestamp.fromMillis(501))).toBe(true);
});

test("Timestamp.unscaled carries a group key with no scale", () => {
	const key = Timestamp.unscaled(7n);
	expect(key.value).toBe(7);
	expect(key.scale).toBeUndefined();
	expect(Timestamp.unscaled(7)).toEqual(key);
	expect(keyOf(key)).toBe(7n);
	expect(keyOf(Timestamp.fromMillis(7))).toBeUndefined();
	for (const bad of [-1, 1.5, Number.NaN, Number.MAX_SAFE_INTEGER + 1])
		expect(() => Timestamp.unscaled(bad)).toThrow(RangeError);
	expect(() => Timestamp.unscaled(-1n)).toThrow(RangeError);
	expect(() => Timestamp.unscaled(1n << 64n)).toThrow(RangeError);
});

test("Timestamp.unscaled takes any u64 key, refusing only to read one past 2^53 as a number", () => {
	// A received group can name any key, so building its timestamp never throws.
	const max = Timestamp.unscaled((1n << 64n) - 1n);
	expect(keyOf(max)).toBe((1n << 64n) - 1n);
	expect(() => max.value).toThrow(RangeError);
	const below = Timestamp.unscaled((1n << 64n) - 2n);
	expect(below.isBefore(max)).toBe(true);
	expect(max.isBefore(below)).toBe(false);
});

test("Timestamp conversions refuse an unscaled timestamp", () => {
	const key = Timestamp.unscaled(1000n);
	expect(() => key.as(Timescale.MILLI)).toThrow(RangeError);
	expect(() => key.asMillis()).toThrow(RangeError);
	expect(() => key.asMicros()).toThrow(RangeError);
	expect(() => floorAt(key, Timescale.MILLI)).toThrow(RangeError);
});

test("Timestamp.isBefore orders unscaled keys, and a scaled against an unscaled not at all", () => {
	expect(Timestamp.unscaled(1n).isBefore(Timestamp.unscaled(2n))).toBe(true);
	expect(Timestamp.unscaled(2n).isBefore(Timestamp.unscaled(1n))).toBe(false);
	expect(Timestamp.unscaled(2n).isBefore(Timestamp.unscaled(2n))).toBe(false);
	// Rust's `partial_cmp` is `None` here, so neither is before the other.
	expect(Timestamp.unscaled(1n).isBefore(Timestamp.fromMillis(2))).toBe(false);
	expect(Timestamp.fromMillis(2).isBefore(Timestamp.unscaled(1n))).toBe(false);
	expect(Timestamp.fromMillis(0).isBefore(Timestamp.unscaled(5n))).toBe(false);
});

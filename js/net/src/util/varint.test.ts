import { expect, test } from "bun:test";
import * as Varint from "../varint.ts";
import {
	lengthLeadingOnes,
	lengthQuic,
	parts,
	peekLeadingOnes,
	peekQuic,
	readLeadingOnes,
	readQuic,
	VarInt,
	writeLeadingOnes,
	writeQuic,
} from "./varint.ts";

test("VarInt converts numbers at the 32-bit and 53-bit boundaries", () => {
	for (const n of [0, 2 ** 30, 2 ** 32 - 1, 2 ** 32, Number.MAX_SAFE_INTEGER]) {
		const v = VarInt.fromNumber(n);
		expect(v.toNumber()).toBe(n);
		expect(v.toBigInt()).toBe(BigInt(n));
		expect(v.toString()).toBe(String(n));
	}
	expect(VarInt.fromNumber(2 ** 32 + 5)).toEqual(new VarInt(1, 5));
});

test("VarInt.fromNumber rejects anything but a non-negative safe integer", () => {
	for (const n of [-1, 1.5, Number.NaN, Number.POSITIVE_INFINITY, 2 ** 53]) {
		expect(() => VarInt.fromNumber(n)).toThrow(RangeError);
	}
});

test("VarInt holds 64 bits but converts to number only up to 2^53 - 1", () => {
	const max = VarInt.fromBigInt(2n ** 64n - 1n);
	expect(max).toEqual(VarInt.MAX);
	expect(max.toBigInt()).toBe(2n ** 64n - 1n);
	expect(max.toString()).toBe("18446744073709551615");
	expect(() => max.toNumber()).toThrow(/larger than 53-bits: 18446744073709551615/);
	expect(() => VarInt.fromBigInt(2n ** 53n).toNumber()).toThrow(RangeError);

	expect(() => VarInt.fromBigInt(2n ** 64n)).toThrow(RangeError);
	expect(() => VarInt.fromBigInt(-1n)).toThrow(RangeError);
	expect(() => new VarInt(2 ** 32, 0)).toThrow(RangeError);
	expect(() => new VarInt(0, 2 ** 32)).toThrow(RangeError);
	expect(() => new VarInt(0, -1)).toThrow(RangeError);
});

test("VarInt compares and adds without converting", () => {
	const a = VarInt.fromNumber(2 ** 32 - 1);
	const b = a.add(1);
	expect(b).toEqual(new VarInt(1, 0));
	expect(a.compare(b)).toBeLessThan(0);
	expect(b.compare(a)).toBeGreaterThan(0);
	expect(a.compare(VarInt.fromNumber(2 ** 32 - 1))).toBe(0);
	expect(VarInt.MAX.compare(VarInt.ZERO)).toBeGreaterThan(0);
	expect(a.equals(b)).toBe(false);
	expect(b.equals(new VarInt(1, 0))).toBe(true);

	expect(VarInt.ZERO.add(Number.MAX_SAFE_INTEGER).toNumber()).toBe(Number.MAX_SAFE_INTEGER);
	expect(VarInt.fromBigInt(2n ** 64n - 2n).add(1)).toEqual(VarInt.MAX);
	expect(() => VarInt.MAX.add(1)).toThrow(RangeError);
	expect(() => a.add(-1)).toThrow(RangeError);
});

// Each boundary, and the expected size in QUIC form (undefined where QUIC can't hold it) and in leading-ones form.
const boundaries: [bigint, number | undefined, number][] = [
	[2n ** 30n - 1n, 4, 5],
	[2n ** 30n, 8, 5],
	[2n ** 53n - 1n, 8, 8],
	[2n ** 53n, 8, 8],
	[2n ** 62n - 1n, 8, 9],
	[2n ** 62n, undefined, 9],
	[2n ** 64n - 1n, undefined, 9],
];

test("Both formats round-trip the 2^30, 2^53, 2^62, and 2^64 boundaries", () => {
	const buf = new Uint8Array(9);
	for (const [value, quic, leadingOnes] of boundaries) {
		const v = VarInt.fromBigInt(value);

		if (quic === undefined) {
			expect(() => lengthQuic(v.hi, v.lo)).toThrow(/larger than 62-bits/);
			expect(() => Varint.encode(value)).toThrow(/larger than 62-bits/);
		} else {
			expect(lengthQuic(v.hi, v.lo)).toBe(quic);
			writeQuic(buf, v.hi, v.lo, quic);
			expect(peekQuic(buf[0])).toBe(quic);
			const lo = readQuic(buf, 0, quic);
			expect(new VarInt(parts.hi, lo)).toEqual(v);
			expect(Varint.decodeBigInt(Varint.encode(value))[0]).toBe(value);
		}

		expect(lengthLeadingOnes(v.hi, v.lo)).toBe(leadingOnes);
		writeLeadingOnes(buf, v.hi, v.lo, leadingOnes);
		expect(peekLeadingOnes(buf[0])).toBe(leadingOnes);
		const lo = readLeadingOnes(buf, 0, leadingOnes);
		expect(new VarInt(parts.hi, lo)).toEqual(v);
		expect(Varint.decodeLeadingOnes(Varint.encodeLeadingOnes(value))[0]).toBe(value);
	}
});

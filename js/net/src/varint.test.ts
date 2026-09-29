import { expect, test } from "bun:test";
import * as Varint from "./varint.ts";

test("Varint encode/decode roundtrip - 1 byte values (0-63)", () => {
	const testValues = [0, 1, 32, 63];

	for (const value of testValues) {
		const encoded = Varint.encode(value);
		expect(encoded.byteLength).toBe(1);

		const [decoded, remaining] = Varint.decode(encoded);
		expect(decoded).toBe(value);
		expect(remaining.byteLength).toBe(0);
	}
});

test("Varint encode/decode roundtrip - 2 byte values (64-16383)", () => {
	const testValues = [64, 100, 1000, 16383];

	for (const value of testValues) {
		const encoded = Varint.encode(value);
		expect(encoded.byteLength).toBe(2);

		const [decoded, remaining] = Varint.decode(encoded);
		expect(decoded).toBe(value);
		expect(remaining.byteLength).toBe(0);
	}
});

test("Varint encode/decode roundtrip - 4 byte values (16384-1073741823)", () => {
	const testValues = [16384, 100000, 1073741823];

	for (const value of testValues) {
		const encoded = Varint.encode(value);
		expect(encoded.byteLength).toBe(4);

		const [decoded, remaining] = Varint.decode(encoded);
		expect(decoded).toBe(value);
		expect(remaining.byteLength).toBe(0);
	}
});

test("Varint encode/decode roundtrip - 8 byte values (1073741824+)", () => {
	const testValues = [1073741824, Number.MAX_SAFE_INTEGER];

	for (const value of testValues) {
		const encoded = Varint.encode(value);
		expect(encoded.byteLength).toBe(8);

		const [decoded, remaining] = Varint.decode(encoded);
		expect(decoded).toBe(value);
		expect(remaining.byteLength).toBe(0);
	}
});

test("Varint bigint roundtrip preserves 62-bit precision", () => {
	for (const value of [9007199254740993n, 2n ** 62n - 1n]) {
		const encoded = Varint.encode(value);
		const [decoded, remaining] = Varint.decodeBigInt(encoded);
		expect(decoded).toBe(value);
		expect(remaining.byteLength).toBe(0);
	}
});

test("Varint size calculation", () => {
	expect(Varint.size(0)).toBe(1);
	expect(Varint.size(63)).toBe(1);
	expect(Varint.size(64)).toBe(2);
	expect(Varint.size(16383)).toBe(2);
	expect(Varint.size(16384)).toBe(4);
	expect(Varint.size(1073741823)).toBe(4);
	expect(Varint.size(1073741824)).toBe(8);
	expect(Varint.size(Number.MAX_SAFE_INTEGER)).toBe(8);
});

test("Varint decode returns remaining buffer", () => {
	// Encode a value and append extra data
	const encoded = Varint.encode(42);
	const extra = new Uint8Array([0xde, 0xad, 0xbe, 0xef]);
	const combined = new Uint8Array(encoded.byteLength + extra.byteLength);
	combined.set(encoded, 0);
	combined.set(extra, encoded.byteLength);

	const [decoded, remaining] = Varint.decode(combined);
	expect(decoded).toBe(42);
	expect(remaining).toEqual(extra);
});

test("Varint decode handles buffer at non-zero offset", () => {
	// Create a buffer with padding before the varint
	const padding = new Uint8Array([0xff, 0xff]);
	const encoded = Varint.encode(1000); // 2-byte varint
	const combined = new Uint8Array(padding.byteLength + encoded.byteLength);
	combined.set(padding, 0);
	combined.set(encoded, padding.byteLength);

	// Create a subarray starting after the padding
	const subarray = combined.subarray(padding.byteLength);

	const [decoded, remaining] = Varint.decode(subarray);
	expect(decoded).toBe(1000);
	expect(remaining.byteLength).toBe(0);
});

test("Varint encode rejects negative values", () => {
	expect(() => Varint.encode(-1)).toThrow(/underflow/);
});

test("Varint decode throws on empty buffer", () => {
	expect(() => Varint.decode(new Uint8Array(0))).toThrow(/buffer is empty/);
});

test("Varint decode throws on truncated buffer", () => {
	// Create a 2-byte varint header but only provide 1 byte
	const truncated = new Uint8Array([0x40]); // 0x40 = 2-byte marker with value 0
	expect(() => Varint.decode(truncated)).toThrow(/buffer too short/);
});

test("Varint boundary values", () => {
	// Test exact boundary values
	const boundaries = [
		{ value: 63, expectedSize: 1 },
		{ value: 64, expectedSize: 2 },
		{ value: 16383, expectedSize: 2 },
		{ value: 16384, expectedSize: 4 },
		{ value: 1073741823, expectedSize: 4 },
		{ value: 1073741824, expectedSize: 8 },
	];

	for (const { value, expectedSize } of boundaries) {
		const encoded = Varint.encode(value);
		expect(encoded.byteLength).toBe(expectedSize);

		const [decoded] = Varint.decode(encoded);
		expect(decoded).toBe(value);
	}
});

const { VarInt } = Varint;

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

test("VarInt holds the full 62-bit range but converts to number only up to 2^53 - 1", () => {
	const max = VarInt.fromBigInt(2n ** 62n - 1n);
	expect(max).toEqual(VarInt.MAX);
	expect(max.toBigInt()).toBe(2n ** 62n - 1n);
	expect(max.toString()).toBe("4611686018427387903");
	expect(() => max.toNumber()).toThrow(/larger than 53-bits: 4611686018427387903/);
	expect(() => VarInt.fromBigInt(2n ** 53n).toNumber()).toThrow(RangeError);

	expect(() => VarInt.fromBigInt(2n ** 62n)).toThrow(RangeError);
	expect(() => VarInt.fromBigInt(-1n)).toThrow(RangeError);
	expect(() => new VarInt(2 ** 30, 0)).toThrow(RangeError);
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
	expect(a.equals(b)).toBe(false);
	expect(b.equals(new VarInt(1, 0))).toBe(true);

	expect(VarInt.ZERO.add(Number.MAX_SAFE_INTEGER).toNumber()).toBe(Number.MAX_SAFE_INTEGER);
	expect(VarInt.fromBigInt(2n ** 62n - 2n).add(1)).toEqual(VarInt.MAX);
	expect(() => VarInt.MAX.add(1)).toThrow(RangeError);
	expect(() => a.add(-1)).toThrow(RangeError);
});

test("VarInt encodes like the number and bigint it holds", () => {
	for (const n of [0n, 63n, 64n, 2n ** 30n - 1n, 2n ** 30n, 2n ** 53n - 1n, 2n ** 53n, 2n ** 62n - 1n]) {
		const v = VarInt.fromBigInt(n);
		expect(Varint.encode(v)).toEqual(Varint.encode(n));
		expect(Varint.encodeLeadingOnes(v)).toEqual(Varint.encodeLeadingOnes(n));
		expect(Varint.sizeLeadingOnes(v)).toBe(Varint.encodeLeadingOnes(n).byteLength);
		expect(Varint.decodeBigInt(Varint.encode(v))[0]).toBe(n);
	}
	expect(() => Varint.encode(2n ** 62n)).toThrow(/overflow/);
	expect(() => Varint.encode(1.5)).toThrow(RangeError);
});

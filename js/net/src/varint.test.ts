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

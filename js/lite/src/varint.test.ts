import assert from "node:assert";
import test from "node:test";
import * as Varint from "./varint.ts";

test("Varint encode/decode roundtrip - 1 byte values (0-63)", () => {
	const testValues = [0, 1, 32, 63];

	for (const value of testValues) {
		const encoded = Varint.encode(value);
		assert.strictEqual(encoded.byteLength, 1, `${value} should encode to 1 byte`);

		const [decoded, remaining] = Varint.decode(encoded);
		assert.strictEqual(decoded, value, `${value} should decode correctly`);
		assert.strictEqual(remaining.byteLength, 0, "remaining should be empty");
	}
});

test("Varint encode/decode roundtrip - 2 byte values (64-16383)", () => {
	const testValues = [64, 100, 1000, 16383];

	for (const value of testValues) {
		const encoded = Varint.encode(value);
		assert.strictEqual(encoded.byteLength, 2, `${value} should encode to 2 bytes`);

		const [decoded, remaining] = Varint.decode(encoded);
		assert.strictEqual(decoded, value, `${value} should decode correctly`);
		assert.strictEqual(remaining.byteLength, 0, "remaining should be empty");
	}
});

test("Varint encode/decode roundtrip - 4 byte values (16384-1073741823)", () => {
	const testValues = [16384, 100000, 1073741823];

	for (const value of testValues) {
		const encoded = Varint.encode(value);
		assert.strictEqual(encoded.byteLength, 4, `${value} should encode to 4 bytes`);

		const [decoded, remaining] = Varint.decode(encoded);
		assert.strictEqual(decoded, value, `${value} should decode correctly`);
		assert.strictEqual(remaining.byteLength, 0, "remaining should be empty");
	}
});

test("Varint encode/decode roundtrip - 8 byte values (1073741824+)", () => {
	const testValues = [1073741824, Number.MAX_SAFE_INTEGER];

	for (const value of testValues) {
		const encoded = Varint.encode(value);
		assert.strictEqual(encoded.byteLength, 8, `${value} should encode to 8 bytes`);

		const [decoded, remaining] = Varint.decode(encoded);
		assert.strictEqual(decoded, value, `${value} should decode correctly`);
		assert.strictEqual(remaining.byteLength, 0, "remaining should be empty");
	}
});

test("Varint size calculation", () => {
	assert.strictEqual(Varint.size(0), 1);
	assert.strictEqual(Varint.size(63), 1);
	assert.strictEqual(Varint.size(64), 2);
	assert.strictEqual(Varint.size(16383), 2);
	assert.strictEqual(Varint.size(16384), 4);
	assert.strictEqual(Varint.size(1073741823), 4);
	assert.strictEqual(Varint.size(1073741824), 8);
	assert.strictEqual(Varint.size(Number.MAX_SAFE_INTEGER), 8);
});

test("Varint decode returns remaining buffer", () => {
	// Encode a value and append extra data
	const encoded = Varint.encode(42);
	const extra = new Uint8Array([0xde, 0xad, 0xbe, 0xef]);
	const combined = new Uint8Array(encoded.byteLength + extra.byteLength);
	combined.set(encoded, 0);
	combined.set(extra, encoded.byteLength);

	const [decoded, remaining] = Varint.decode(combined);
	assert.strictEqual(decoded, 42);
	assert.deepEqual(remaining, extra);
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
	assert.strictEqual(decoded, 1000);
	assert.strictEqual(remaining.byteLength, 0);
});

test("Varint encode rejects negative values", () => {
	assert.throws(() => Varint.encode(-1), /underflow/);
});

test("Varint decode throws on empty buffer", () => {
	assert.throws(() => Varint.decode(new Uint8Array(0)), /buffer is empty/);
});

test("Varint decode throws on truncated buffer", () => {
	// Create a 2-byte varint header but only provide 1 byte
	const truncated = new Uint8Array([0x40]); // 0x40 = 2-byte marker with value 0
	assert.throws(() => Varint.decode(truncated), /buffer too short/);
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
		assert.strictEqual(encoded.byteLength, expectedSize, `${value} should encode to ${expectedSize} bytes`);

		const [decoded] = Varint.decode(encoded);
		assert.strictEqual(decoded, value, `${value} should roundtrip correctly`);
	}
});

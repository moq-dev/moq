import { expect, test } from "bun:test";
import { Reader, Writer } from "./stream.ts";

// Helper to create a writable stream that captures written data
function createTestWritableStream(): { stream: WritableStream<Uint8Array>; written: Uint8Array[] } {
	const written: Uint8Array[] = [];
	const stream = new WritableStream<Uint8Array>({
		write(chunk) {
			written.push(new Uint8Array(chunk));
		},
	});
	return { stream, written };
}

// Helper to concatenate written chunks
function concatChunks(chunks: Uint8Array[]): Uint8Array {
	const totalLength = chunks.reduce((sum, chunk) => sum + chunk.byteLength, 0);
	const result = new Uint8Array(totalLength);
	let offset = 0;
	for (const chunk of chunks) {
		result.set(chunk, offset);
		offset += chunk.byteLength;
	}
	return result;
}

test("Writer u8", async () => {
	const { stream, written } = createTestWritableStream();
	const writer = new Writer(stream);

	await writer.u8(42);
	await writer.u8(255);

	writer.close();
	await writer.closed;

	expect(written.length).toBe(2);
	expect(written[0]).toEqual(new Uint8Array([42]));
	expect(written[1]).toEqual(new Uint8Array([255]));
});

test("Writer i32", async () => {
	const { stream, written } = createTestWritableStream();
	const writer = new Writer(stream);

	await writer.i32(0);
	await writer.i32(-1);
	await writer.i32(1000);

	writer.close();
	await writer.closed;

	const result = concatChunks(written);
	expect(result.byteLength).toBe(12); // 3 * 4 bytes

	// Read back the values
	const view = new DataView(result.buffer, result.byteOffset, result.byteLength);
	expect(view.getInt32(0)).toBe(0);
	expect(view.getInt32(4)).toBe(-1);
	expect(view.getInt32(8)).toBe(1000);
});

test("Writer u53", async () => {
	const { stream, written } = createTestWritableStream();
	const writer = new Writer(stream);

	await writer.u53(0);
	await writer.u53(63); // MAX_U6
	await writer.u53(64); // MIN for 2-byte varint
	await writer.u53(16383); // MAX_U14
	await writer.u53(16384); // MIN for 4-byte varint

	writer.close();
	await writer.closed;

	// Verify the varint encoding sizes
	expect(written[0].byteLength).toBe(1); // 0 fits in 1 byte
	expect(written[1].byteLength).toBe(1); // 63 fits in 1 byte
	expect(written[2].byteLength).toBe(2); // 64 needs 2 bytes
	expect(written[3].byteLength).toBe(2); // 16383 fits in 2 bytes
	expect(written[4].byteLength).toBe(4); // 16384 needs 4 bytes
});

test("Writer string", async () => {
	const { stream, written } = createTestWritableStream();
	const writer = new Writer(stream);

	await writer.string("hello");
	await writer.string("🎉");

	writer.close();
	await writer.closed;

	const result = concatChunks(written);

	// Create a reader to parse the result
	const reader = new Reader(undefined, result);

	const str1 = await reader.string();
	const str2 = await reader.string();

	expect(str1).toBe("hello");
	expect(str2).toBe("🎉");
});

test("Reader u8", async () => {
	const data = new Uint8Array([42, 255, 0, 128]);
	const reader = new Reader(undefined, data);

	expect(await reader.u8()).toBe(42);
	expect(await reader.u8()).toBe(255);
	expect(await reader.u8()).toBe(0);
	expect(await reader.u8()).toBe(128);

	expect(await reader.done()).toBe(true);
});

test("Reader read with exact sizes", async () => {
	const data = new Uint8Array([1, 2, 3, 4, 5, 6, 7, 8]);
	const reader = new Reader(undefined, data);

	const chunk1 = await reader.read(3);
	expect(chunk1).toEqual(new Uint8Array([1, 2, 3]));

	const chunk2 = await reader.read(2);
	expect(chunk2).toEqual(new Uint8Array([4, 5]));

	const chunk3 = await reader.read(3);
	expect(chunk3).toEqual(new Uint8Array([6, 7, 8]));

	expect(await reader.done()).toBe(true);
});

test("Reader read with zero size", async () => {
	const data = new Uint8Array([1, 2, 3]);
	const reader = new Reader(undefined, data);

	const chunk = await reader.read(0);
	expect(chunk).toEqual(new Uint8Array([]));

	// Original data should still be available
	const remaining = await reader.read(3);
	expect(remaining).toEqual(new Uint8Array([1, 2, 3]));
});

test("Reader readAll", async () => {
	const data = new Uint8Array([1, 2, 3, 4, 5]);
	const reader = new Reader(undefined, data);

	// Read some data first
	await reader.read(2);

	// readAll should get the remaining data
	const remaining = await reader.readAll();
	expect(remaining).toEqual(new Uint8Array([3, 4, 5]));

	expect(await reader.done()).toBe(true);
});

test("Reader u53 varint decoding", async () => {
	// Test various varint sizes
	const testValues = [0, 63, 64, 16383, 16384, 1073741823, 1073741824];

	const { stream, written } = createTestWritableStream();
	const testWriter = new Writer(stream);

	for (const value of testValues) {
		await testWriter.u53(value);
	}

	testWriter.close();
	await testWriter.closed;

	const data = concatChunks(written);
	const reader = new Reader(undefined, data);

	for (const expectedValue of testValues) {
		const actualValue = await reader.u53();
		expect(actualValue).toBe(expectedValue);
	}

	expect(await reader.done()).toBe(true);
});

test("Reader u62 varint decoding", async () => {
	const testValues = [0n, 63n, 64n, 16383n, 16384n, 1073741823n, 1073741824n, 9007199254740991n]; // MAX_U53

	const { stream, written } = createTestWritableStream();
	const testWriter = new Writer(stream);

	for (const value of testValues) {
		await testWriter.u62(value);
	}

	testWriter.close();
	await testWriter.closed;

	const data = concatChunks(written);
	const reader = new Reader(undefined, data);

	for (const expectedValue of testValues) {
		const actualValue = await reader.u62();
		expect(actualValue).toBe(expectedValue);
	}

	expect(await reader.done()).toBe(true);
});

test("Reader string decoding", async () => {
	const testStrings = ["hello", "🎉", "", "world with spaces", "multi\nline\nstring"];

	const { stream, written } = createTestWritableStream();
	const writer = new Writer(stream);

	for (const str of testStrings) {
		await writer.string(str);
	}

	writer.close();
	await writer.closed;

	const data = concatChunks(written);
	const reader = new Reader(undefined, data);

	for (const expectedString of testStrings) {
		const actualString = await reader.string();
		expect(actualString).toBe(expectedString);
	}

	expect(await reader.done()).toBe(true);
});

test("Reader from stream", async () => {
	const chunks = [new Uint8Array([1, 2, 3]), new Uint8Array([4, 5]), new Uint8Array([6, 7, 8, 9])];

	const stream = new ReadableStream({
		start(controller) {
			for (const chunk of chunks) {
				controller.enqueue(chunk);
			}
			controller.close();
		},
	});

	const reader = new Reader(stream);

	// Read all data
	const result = await reader.readAll();
	expect(result).toEqual(new Uint8Array([1, 2, 3, 4, 5, 6, 7, 8, 9]));
});

test("Reader stream with partial reads", async () => {
	const chunks = [new Uint8Array([1, 2]), new Uint8Array([3, 4, 5]), new Uint8Array([6])];

	const stream = new ReadableStream({
		start(controller) {
			for (const chunk of chunks) {
				controller.enqueue(chunk);
			}
			controller.close();
		},
	});

	const reader = new Reader(stream);

	// Read specific amounts that cross chunk boundaries
	const first = await reader.read(3); // Should span first two chunks
	expect(first).toEqual(new Uint8Array([1, 2, 3]));

	const second = await reader.read(2);
	expect(second).toEqual(new Uint8Array([4, 5]));

	const third = await reader.read(1);
	expect(third).toEqual(new Uint8Array([6]));

	expect(await reader.done()).toBe(true);
});

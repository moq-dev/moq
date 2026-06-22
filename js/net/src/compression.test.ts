import { expect, test } from "bun:test";
import { Compression, compress, compressionFromCode, decompress } from "./compression.ts";

test("compression: deflate round-trips and shrinks repetitive data", async () => {
	const data = new Uint8Array(4096).fill(0x61); // "aaaa..." — highly compressible
	const packed = await compress(Compression.Deflate, data);
	expect(packed.byteLength).toBeLessThan(data.byteLength);
	expect(await decompress(Compression.Deflate, packed)).toEqual(data);
});

test("compression: deflate handles an empty payload", async () => {
	const packed = await compress(Compression.Deflate, new Uint8Array());
	expect(await decompress(Compression.Deflate, packed)).toEqual(new Uint8Array());
});

test("compression: decompress rejects garbage", async () => {
	await expect(decompress(Compression.Deflate, new TextEncoder().encode("not a deflate stream"))).rejects.toThrow();
});

test("compression: decompress enforces the max size", async () => {
	const data = new Uint8Array(4096).fill(0x61);
	const packed = await compress(Compression.Deflate, data);
	await expect(decompress(Compression.Deflate, packed, 512)).rejects.toThrow();
});

test("compression: wire-code round-trip", () => {
	expect(compressionFromCode(0)).toBeUndefined();
	expect(compressionFromCode(1)).toBe(Compression.Deflate);
	expect(() => compressionFromCode(99)).toThrow();
});

import { describe, expect, it } from "bun:test";
import * as Hex from "./hex.ts";

describe("hex", () => {
	it("round-trips", () => {
		const bytes = new Uint8Array([0x01, 0x42, 0x00, 0x2a, 0xff]);
		expect(Hex.fromBytes(bytes)).toBe("0142002aff");
		expect(Hex.toBytes("0x0142002AFF")).toEqual(bytes);
	});

	it("rejects an empty string", () => {
		expect(() => Hex.toBytes("")).toThrow("empty hex string");
		expect(() => Hex.toBytes("0x")).toThrow("empty hex string");
	});

	it("rejects an odd length", () => {
		expect(() => Hex.toBytes("0xabc")).toThrow("invalid hex string length");
		expect(() => Hex.toBytes("abc")).toThrow("invalid hex string length");
	});

	// parseInt("zz", 16) is NaN, which a Uint8Array stores as 0.
	it("rejects non-hex characters", () => {
		expect(() => Hex.toBytes("00zz")).toThrow("invalid hex character 'z' at position 2");
	});
});

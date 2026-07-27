import { describe, expect, it } from "bun:test";
import * as Hex from "./hex";

describe("hex", () => {
	it("round-trips", () => {
		const bytes = new Uint8Array([0x01, 0x42, 0x00, 0x2a, 0xff]);
		expect(Hex.fromBytes(bytes)).toBe("0142002aff");
		expect(Hex.toBytes("0142002aff")).toEqual(bytes);
	});

	it("accepts either case and an 0x prefix", () => {
		expect(Hex.toBytes("0xAbCd")).toEqual(new Uint8Array([0xab, 0xcd]));
	});

	it("rejects an odd length", () => {
		expect(() => Hex.toBytes("abc")).toThrow("invalid hex string length");
	});

	// A base64 avcC starts with "AU", which parseInt would silently truncate to 0x0a.
	it("rejects base64", () => {
		expect(() => Hex.toBytes("AUIAKv/hABtnQgAq")).toThrow("invalid hex character 'U' at position 1");
	});
});

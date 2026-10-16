import { describe, expect, test } from "bun:test";
import * as Varint from "../../../js/net/src/varint.ts";
import { integers, payload, verify } from "./fixture.ts";

describe("backend-independent fixtures", () => {
	test("payloads pin every byte and refuse corrupted samples", () => {
		for (const size of [160, 4096, 131072]) {
			const bytes = payload(size, 17);
			verify(bytes, size, 17);
			bytes[size - 1] ^= 1;
			expect(() => verify(bytes, size, 17)).toThrow();
		}
	});
	test("legal u64 values round-trip exactly and narrower versions refuse overflow", () => {
		for (const text of integers) {
			const value = BigInt(text);
			expect(Varint.decodeLeadingOnes(Varint.encodeLeadingOnes(value))[0]).toBe(value);
			if (value < 1n << 62n) expect(Varint.decodeBigInt(Varint.encode(value))[0]).toBe(value);
			else expect(() => Varint.encode(value)).toThrow();
		}
		for (const value of [-1n, 1n << 64n]) expect(() => Varint.encodeLeadingOnes(value)).toThrow();
	});
});

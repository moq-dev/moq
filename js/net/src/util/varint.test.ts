import { expect, test } from "bun:test";
import * as Varint from "../varint.ts";
import { U64 } from "./u64.ts";
import {
	lengthLeadingOnes,
	lengthQuic,
	parts,
	peekLeadingOnes,
	peekQuic,
	readLeadingOnes,
	readQuic,
	writeLeadingOnes,
	writeQuic,
} from "./varint.ts";

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
		const v = U64.fromBigInt(value);

		if (quic === undefined) {
			expect(() => lengthQuic(v.hi, v.lo)).toThrow(/larger than 62-bits/);
			expect(() => Varint.encode(value)).toThrow(/larger than 62-bits/);
		} else {
			expect(lengthQuic(v.hi, v.lo)).toBe(quic);
			writeQuic(buf, v.hi, v.lo, quic);
			expect(peekQuic(buf[0])).toBe(quic);
			const lo = readQuic(buf, 0, quic);
			expect(new U64(parts.hi, lo)).toEqual(v);
			expect(Varint.decodeBigInt(Varint.encode(value))[0]).toBe(value);
		}

		expect(lengthLeadingOnes(v.hi, v.lo)).toBe(leadingOnes);
		writeLeadingOnes(buf, v.hi, v.lo, leadingOnes);
		expect(peekLeadingOnes(buf[0])).toBe(leadingOnes);
		const lo = readLeadingOnes(buf, 0, leadingOnes);
		expect(new U64(parts.hi, lo)).toEqual(v);
		expect(Varint.decodeLeadingOnes(Varint.encodeLeadingOnes(value))[0]).toBe(value);
	}
});

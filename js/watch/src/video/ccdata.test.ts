import { describe, expect, test } from "bun:test";
import { extractCcData } from "./ccdata";

const GA94 = {
	valid: true,
	type: 0,
	data1: 0x14,
	data2: 0x2d,
} as const;

function ga94(pairs: Array<{ valid: boolean; type: number; data1: number; data2: number }> = [GA94]): Uint8Array {
	const out = [0xb5, 0x00, 0x31, 0x47, 0x41, 0x39, 0x34, 0x03, pairs.length & 0x1f];
	for (const pair of pairs) {
		out.push((pair.valid ? 0x04 : 0) | (pair.type & 0x03), pair.data1, pair.data2);
	}
	return Uint8Array.from(out);
}

function encodeLength(length: number): number[] {
	const out: number[] = [];
	let rest = length;
	while (rest >= 255) {
		out.push(0xff);
		rest -= 255;
	}
	out.push(rest);
	return out;
}

function insertEmulationPrevention(rbsp: Uint8Array): Uint8Array {
	const out: number[] = [];
	for (let i = 0; i < rbsp.length; i++) {
		if (out.length >= 2 && out[out.length - 2] === 0 && out[out.length - 1] === 0 && rbsp[i] <= 0x03) {
			out.push(0x03);
		}
		out.push(rbsp[i]);
	}
	return Uint8Array.from(out);
}

function seiRbsp(messages: Array<{ type: number; payload: Uint8Array }>): Uint8Array {
	const out: number[] = [];
	for (const message of messages) {
		let type = message.type;
		while (type >= 255) {
			out.push(0xff);
			type -= 255;
		}
		out.push(type);
		out.push(...encodeLength(message.payload.length));
		out.push(...message.payload);
	}
	return Uint8Array.from(out);
}

function avcSei(messages: Array<{ type: number; payload: Uint8Array }>): Uint8Array {
	return Uint8Array.of(0x06, ...insertEmulationPrevention(seiRbsp(messages)));
}

function hevcSei(nalType: 39 | 40, messages: Array<{ type: number; payload: Uint8Array }>): Uint8Array {
	const header0 = (nalType << 1) & 0x7e;
	return Uint8Array.of(header0, 0x01, ...insertEmulationPrevention(seiRbsp(messages)));
}

function annexB(...nals: Uint8Array[]): Uint8Array {
	const parts = nals.flatMap((nal) => [0, 0, 0, 1, ...nal]);
	return Uint8Array.from(parts);
}

function annexB3(...nals: Uint8Array[]): Uint8Array {
	const parts = nals.flatMap((nal) => [0, 0, 1, ...nal]);
	return Uint8Array.from(parts);
}

function lengthPrefixed(...nals: Uint8Array[]): Uint8Array {
	const size = nals.reduce((sum, nal) => sum + 4 + nal.length, 0);
	const out = new Uint8Array(size);
	let i = 0;
	for (const nal of nals) {
		out[i] = (nal.length >>> 24) & 0xff;
		out[i + 1] = (nal.length >>> 16) & 0xff;
		out[i + 2] = (nal.length >>> 8) & 0xff;
		out[i + 3] = nal.length & 0xff;
		out.set(nal, i + 4);
		i += 4 + nal.length;
	}
	return out;
}

describe("extractCcData", () => {
	test("returns nothing for an empty buffer", () => {
		expect(extractCcData(new Uint8Array(), "avc")).toEqual([]);
		expect(extractCcData(new Uint8Array(), "hevc")).toEqual([]);
	});

	test("ignores a non-SEI AVC NAL", () => {
		expect(extractCcData(annexB(Uint8Array.of(0x65, 0x88, 0x80)), "avc")).toEqual([]);
	});

	test("reads GA94 cc_data from a 4-byte Annex-B AVC SEI", () => {
		const buf = annexB(avcSei([{ type: 4, payload: ga94() }]));
		expect(extractCcData(buf, "avc")).toEqual([GA94]);
	});

	test("reads GA94 cc_data from a 3-byte Annex-B start code", () => {
		const buf = annexB3(avcSei([{ type: 4, payload: ga94() }]));
		expect(extractCcData(buf, "avc")).toEqual([GA94]);
	});

	test("reads the same SEI from a 4-byte length-prefixed AVCC unit", () => {
		const nal = avcSei([{ type: 4, payload: ga94() }]);
		expect(extractCcData(lengthPrefixed(nal), "avc")).toEqual([GA94]);
	});

	test("does not treat a 4-byte Annex-B start code as a length of 1", () => {
		// `00 00 00 01` would be a length-prefixed NAL of one byte if the start code were not preferred.
		const buf = annexB(avcSei([{ type: 4, payload: ga94() }]));
		expect(buf[0]).toBe(0);
		expect(buf[1]).toBe(0);
		expect(buf[2]).toBe(0);
		expect(buf[3]).toBe(1);
		expect(extractCcData(buf, "avc")).toEqual([GA94]);
	});

	test("reads prefix and suffix HEVC SEI NALs", () => {
		const payload = ga94();
		expect(extractCcData(annexB(hevcSei(39, [{ type: 4, payload }])), "hevc")).toEqual([GA94]);
		expect(extractCcData(annexB(hevcSei(40, [{ type: 4, payload }])), "hevc")).toEqual([GA94]);
		expect(extractCcData(lengthPrefixed(hevcSei(39, [{ type: 4, payload }])), "hevc")).toEqual([GA94]);
	});

	test("ignores ITU-T T.35 user data that is not GA94 type 0x03", () => {
		const other = Uint8Array.of(0xb5, 0x00, 0x31, 0x47, 0x41, 0x39, 0x34, 0x04, 0x01, 0x00, 0x00, 0x00);
		expect(extractCcData(annexB(avcSei([{ type: 4, payload: other }])), "avc")).toEqual([]);
	});

	test("returns every pair in the cc_data count", () => {
		const pairs = [
			{ valid: true, type: 0, data1: 0x14, data2: 0x2d },
			{ valid: true, type: 1, data1: 0x2c, data2: 0xad },
			{ valid: false, type: 0, data1: 0x80, data2: 0x80 },
		];
		expect(extractCcData(annexB(avcSei([{ type: 4, payload: ga94(pairs) }])), "avc")).toEqual(pairs);
	});

	test("strips emulation-prevention bytes before parsing", () => {
		// A type-5 message whose payload starts 00 00 01 forces a 0x03 into the NAL;
		// the following GA94 message must still parse after those bytes are removed.
		const forced = Uint8Array.of(0x00, 0x00, 0x01, 0xaa);
		const buf = annexB(
			avcSei([
				{ type: 5, payload: forced },
				{ type: 4, payload: ga94() },
			]),
		);
		expect(buf).toContain(0x03);
		expect(extractCcData(buf, "avc")).toEqual([GA94]);
	});

	test("reads SEI that shares an access unit with a slice NAL", () => {
		const buf = annexB(avcSei([{ type: 4, payload: ga94() }]), Uint8Array.of(0x65, 0x88, 0x80));
		expect(extractCcData(buf, "avc")).toEqual([GA94]);
	});

	test("does not throw on truncated or junk input", () => {
		expect(extractCcData(Uint8Array.of(0x06), "avc")).toEqual([]);
		expect(extractCcData(Uint8Array.of(0x00, 0x00, 0x00, 0x01, 0x06, 0x04, 0xff), "avc")).toEqual([]);
		expect(extractCcData(Uint8Array.of(0xff, 0xff, 0xff, 0xff), "avc")).toEqual([]);
		expect(extractCcData(lengthPrefixed(Uint8Array.of(0x06, 0x04)), "avc")).toEqual([]);
	});
});

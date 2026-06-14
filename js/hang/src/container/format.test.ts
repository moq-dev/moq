import { expect, test } from "bun:test";
import { type Time, Varint } from "@moq/net";
import type { InitSegment } from "./cmaf/decode.ts";
import { encodeDataSegment } from "./cmaf/encode.ts";
import { Format as CmafFormat } from "./cmaf/format.ts";
import { Format as LegacyFormat } from "./legacy.ts";

const TIMESCALE = 90_000;
const TEST_INIT: InitSegment = {
	timescale: TIMESCALE,
	trackId: 1,
	defaultSampleDuration: 0,
	defaultSampleSize: 0,
	defaultSampleFlags: 0,
};

function encodeLegacyFrame(timestamp: Time.Micro, payload: Uint8Array): Uint8Array {
	const tsBytes = Varint.encode(timestamp);
	const data = new Uint8Array(tsBytes.byteLength + payload.byteLength);
	data.set(tsBytes, 0);
	data.set(payload, tsBytes.byteLength);
	return data;
}

// --- LegacyFormat ---

test("LegacyFormat decodes a valid frame", () => {
	const format = new LegacyFormat();
	const payload = new Uint8Array([0xde, 0xad]);
	const timestamp = 1000 as Time.Micro;
	const frame = encodeLegacyFrame(timestamp, payload);

	const result = format.decode(frame);

	expect(result).toHaveLength(1);
	expect(result[0].timestamp).toBe(timestamp);
	expect(result[0].data).toEqual(payload);
	expect(result[0].keyframe).toBe(false);
});

test("LegacyFormat always returns keyframe: false", () => {
	const format = new LegacyFormat();
	const frame = encodeLegacyFrame(0 as Time.Micro, new Uint8Array([0x01]));

	const [decoded] = format.decode(frame);
	expect(decoded.keyframe).toBe(false);
});

test("LegacyFormat always returns exactly one frame", () => {
	const format = new LegacyFormat();
	const frame = encodeLegacyFrame(5000 as Time.Micro, new Uint8Array([0x01, 0x02, 0x03]));

	const result = format.decode(frame);
	expect(result).toHaveLength(1);
});

test("LegacyFormat throws on empty input", () => {
	const format = new LegacyFormat();
	expect(() => format.decode(new Uint8Array(0))).toThrow();
});

test("LegacyFormat throws on truncated input", () => {
	const format = new LegacyFormat();
	// A varint that indicates more bytes follow but is truncated
	expect(() => format.decode(new Uint8Array([0x80]))).toThrow();
});

// --- CmafFormat ---

test("CmafFormat decodes a valid keyframe segment", () => {
	const format = new CmafFormat(TEST_INIT);
	const segment = encodeDataSegment({
		data: new Uint8Array([0xca, 0xfe]),
		timestamp: 0,
		duration: 3000,
		keyframe: true,
		sequence: 0,
	});

	const result = format.decode(segment);

	expect(result).toHaveLength(1);
	expect(result[0].data).toEqual(new Uint8Array([0xca, 0xfe]));
	expect(result[0].timestamp).toBe(0 as Time.Micro);
	expect(result[0].keyframe).toBe(true);
});

test("CmafFormat decodes a delta frame segment", () => {
	const format = new CmafFormat(TEST_INIT);
	const segment = encodeDataSegment({
		data: new Uint8Array([0xbe, 0xef]),
		timestamp: 3000,
		duration: 3000,
		keyframe: false,
		sequence: 1,
	});

	const result = format.decode(segment);

	expect(result).toHaveLength(1);
	expect(result[0].keyframe).toBe(false);
});

test("CmafFormat converts timescale units to microseconds", () => {
	const format = new CmafFormat(TEST_INIT);
	// 90000 timescale units = 1 second = 1_000_000 microseconds
	const segment = encodeDataSegment({
		data: new Uint8Array([0x01]),
		timestamp: TIMESCALE,
		duration: 3000,
		keyframe: true,
		sequence: 0,
	});

	const result = format.decode(segment);
	expect(result[0].timestamp).toBe(1_000_000 as Time.Micro);
});

test("CmafFormat throws on corrupt segment", () => {
	const format = new CmafFormat(TEST_INIT);
	expect(() => format.decode(new Uint8Array([0x00, 0x01, 0x02]))).toThrow();
});

test("CmafFormat decodes the per-sample duration", () => {
	const format = new CmafFormat(TEST_INIT);
	const segment = encodeDataSegment({
		data: new Uint8Array([0xca, 0xfe]),
		timestamp: 0,
		duration: 3000,
		keyframe: true,
		sequence: 0,
	});

	const [frame] = format.decode(segment);
	// 3000 ticks / 90000 timescale * 1_000_000 = 33333µs
	expect(frame.duration).toBe(33_333 as Time.Micro);
});

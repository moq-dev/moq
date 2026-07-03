import { expect, test } from "bun:test";
import { Deflate, Inflate } from "fflate";
import { Decoder, Encoder } from "./index.ts";

const enc = new TextEncoder();
const dec = new TextDecoder();

function concatBytes(chunks: Uint8Array[]): Uint8Array {
	const out = new Uint8Array(chunks.reduce((n, c) => n + c.length, 0));
	let offset = 0;
	for (const chunk of chunks) {
		out.set(chunk, offset);
		offset += chunk.length;
	}
	return out;
}

// Round-trip frames through fflate's streaming `Deflate.flush(true)` + `Inflate`, the same
// shared-window scheme our pako codec uses. Returns true only if every frame survives unchanged.
function fflateRoundTrips(frames: Uint8Array[]): boolean {
	try {
		let captured: Uint8Array[] = [];
		const deflate = new Deflate({ level: 6 });
		deflate.ondata = (chunk) => captured.push(chunk);
		const slices = frames.map((frame) => {
			captured = [];
			deflate.push(frame, false);
			deflate.flush(true); // sync flush: byte-align and retain the window
			return concatBytes(captured);
		});

		let inflated: Uint8Array[] = [];
		const inflate = new Inflate();
		inflate.ondata = (chunk) => inflated.push(chunk);
		return slices.every((slice, i) => {
			inflated = [];
			inflate.push(slice, false);
			const got = concatBytes(inflated);
			return got.length === frames[i].length && got.every((b, j) => b === frames[i][j]);
		});
	} catch {
		return false;
	}
}

test("codec round-trips a stream of frames in order", () => {
	const frames = ["the quick brown fox", "the quick brown dog", "the lazy fox"];
	const encoder = new Encoder();
	const slices = frames.map((f) => encoder.frame(enc.encode(f)));

	const decoder = new Decoder();
	expect(slices.map((s) => dec.decode(decoder.frame(s)))).toEqual(frames);
});

test("codec round-trips an empty frame", () => {
	const encoder = new Encoder();
	const decoder = new Decoder();
	expect(encoder.frame(new Uint8Array()).length).toBe(0);
	expect(decoder.frame(new Uint8Array()).length).toBe(0);
});

test("codec rejects garbage", () => {
	const decoder = new Decoder();
	expect(() => decoder.frame(new Uint8Array(64).fill(0xff))).toThrow();
});

test("codec rejects frames that inflate past the default cap", () => {
	// A tiny slice can inflate enormously, so the decoder bounds the output as it is produced.
	const encoder = new Encoder();
	const decoder = new Decoder();
	const slice = encoder.frame(enc.encode("a".repeat(64 * 1024 * 1024 + 1)));
	expect(() => decoder.frame(slice)).toThrow(/exceeded/);
});

test("codec honors a custom maxFrameSize", () => {
	const slice = new Encoder().frame(new Uint8Array(1024));
	const decoder = new Decoder({ maxFrameSize: 512 });
	expect(() => decoder.frame(slice)).toThrow(/exceeded 512/);
});

test("a frame larger than pako's chunk size round-trips", () => {
	// High-entropy data barely compresses, so the slice spans multiple pako chunks (>16 KB), which
	// exercises the encoder's multi-chunk assembly and the decoder's multi-chunk concat.
	let state = 0x9e3779b9 >>> 0;
	const payload = new Uint8Array(64 * 1024);
	for (let i = 0; i < payload.length; i++) {
		state ^= state << 13;
		state ^= state >>> 17;
		state ^= state << 5;
		state >>>= 0;
		payload[i] = state & 0xff;
	}

	const slice = new Encoder().frame(payload);
	expect(slice.length).toBeGreaterThan(16 * 1024); // pako's default chunkSize
	expect(new Decoder().frame(slice)).toEqual(payload);
});

test("cross-frame context shrinks a repeated frame", () => {
	// A later frame identical to an earlier one compresses far smaller once the window holds it.
	const encoder = new Encoder();
	const payload = enc.encode("Media over QUIC delivers real-time latency at massive scale.".repeat(6));
	const first = encoder.frame(payload);
	const second = encoder.frame(payload);
	expect(second.length).toBeLessThan(first.length);
});

test("a custom level round-trips", () => {
	const payload = enc.encode("compress me at maximum effort".repeat(8));
	const slice = new Encoder({ level: 9 }).frame(payload);
	expect(new Decoder().frame(slice)).toEqual(payload);
});

test("pako round-trips a stream that fflate's flush corrupts", () => {
	// A catalog snapshot + 3 deltas that fflate's streaming flush mis-encodes: even fflate's own
	// Inflate can't round-trip its output here. This pins why we depend on pako, not the smaller
	// fflate. If this ever fails (fflateRoundTrips returns true), fflate may have fixed its sync-flush
	// encoder, and dropping the pako dependency could be reconsidered.
	const stream = [
		{
			video: {
				renditions: {
					v0: { codec: "avc1.64001f", bitrate: 6000000 },
					v1: { codec: "avc1.640015", bitrate: 3000000 },
				},
			},
			audio: { renditions: { a0: { codec: "opus", bitrate: 128000 } } },
		},
		{ video: { renditions: { v0: { bitrate: 6200000 } } } },
		{ video: { renditions: { v0: { bitrate: 5800000 } } } },
		{ audio: { renditions: { a0: { bitrate: 96000 } } } },
	];
	const frames = stream.map((value) => enc.encode(JSON.stringify(value)));

	// Our pako codec round-trips every frame of the stream exactly.
	const encoder = new Encoder();
	const decoder = new Decoder();
	for (const frame of frames) {
		expect(decoder.frame(encoder.frame(frame))).toEqual(frame);
	}

	// Positive control: fflate's flush works on simpler frames, so the helper is sound and fflate is
	// only selectively broken, not failing for some unrelated reason.
	expect(fflateRoundTrips(["the quick brown fox", "the quick brown dog"].map((s) => enc.encode(s)))).toBe(true);

	// fflate's streaming flush does not round-trip the same stream our pako codec handles.
	expect(fflateRoundTrips(frames)).toBe(false);
});

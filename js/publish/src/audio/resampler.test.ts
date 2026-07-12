import { describe, expect, it } from "bun:test";
import { StreamResampler } from "./resampler";

// Safari captures Opus at the hardware rate (~44.1 kHz) and we resample to the canonical 48 kHz before
// encoding. These pin the sample bookkeeping the encoder relies on to synthesize contiguous timestamps.
const CAPTURE_RATE = 44100;
const ENCODE_RATE = 48000;
const QUANTUM = 128; // one AudioWorklet render quantum

// Mirror the encoder's timestamp synthesis: anchor + (output samples emitted before this chunk) / rate.
function synthTimestamp(anchor: number, emittedBefore: number): number {
	return Math.round(anchor + (emittedBefore / ENCODE_RATE) * 1e6);
}

// Mirror the watcher ring's index: round(timestamp_seconds * rate).
function ringIndex(timestampMicro: number): number {
	return Math.round((timestampMicro / 1e6) * ENCODE_RATE);
}

describe("StreamResampler", () => {
	it("tracks a running output-sample count", () => {
		const r = new StreamResampler(CAPTURE_RATE, ENCODE_RATE);
		let total = 0;
		for (let i = 0; i < 200; i++) {
			const out = r.resample([new Float32Array(QUANTUM)]);
			total += out[0].length;
			expect(r.emitted).toBe(total);
			// 128 input samples at 44.1 kHz become ~139.3 at 48 kHz, so 139 or 140 per chunk.
			expect(out[0].length === 139 || out[0].length === 140).toBe(true);
		}
		// Over many quanta the running count tracks the rate ratio to within a sample.
		const expected = Math.round((200 * QUANTUM * ENCODE_RATE) / CAPTURE_RATE);
		expect(Math.abs(total - expected)).toBeLessThanOrEqual(1);
	});

	it("synthesized timestamps index the ring back-to-back (no gap, no overlap)", () => {
		const r = new StreamResampler(CAPTURE_RATE, ENCODE_RATE);
		const anchor = 123_456; // arbitrary capture-clock epoch (us)
		let prevStart: number | undefined;
		let prevLen = 0;
		for (let i = 0; i < 500; i++) {
			const emittedBefore = r.emitted;
			const len = r.resample([new Float32Array(QUANTUM)])[0].length;
			if (len === 0) continue;
			const start = ringIndex(synthTimestamp(anchor, emittedBefore));
			if (prevStart !== undefined) {
				// The whole point: each chunk starts exactly where the previous one ended.
				expect(start).toBe(prevStart + prevLen);
			}
			prevStart = start;
			prevLen = len;
		}
	});

	it("raw capture-clock timestamps would NOT be contiguous (why the encoder synthesizes timestamps)", () => {
		// Stamp each chunk with the capture-clock time of its first input sample and index the ring with
		// it: consecutive chunks land off-by-a-sample, forcing zero-fill/overwrite. That is why the
		// encoder instead synthesizes timestamps from the sample counter (see the previous test).
		const r = new StreamResampler(CAPTURE_RATE, ENCODE_RATE);
		let captureSamples = 0;
		let prevStart: number | undefined;
		let prevLen = 0;
		let discontinuities = 0;
		for (let i = 0; i < 500; i++) {
			const captureTs = Math.round((captureSamples / CAPTURE_RATE) * 1e6);
			const len = r.resample([new Float32Array(QUANTUM)])[0].length;
			captureSamples += QUANTUM;
			if (len === 0) continue;
			const start = ringIndex(captureTs);
			if (prevStart !== undefined && start !== prevStart + prevLen) discontinuities++;
			prevStart = start;
			prevLen = len;
		}
		expect(discontinuities).toBeGreaterThan(0);
	});
});

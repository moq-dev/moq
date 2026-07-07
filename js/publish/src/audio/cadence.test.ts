import { describe, expect, it } from "bun:test";
import { snapCadence } from "./cadence";

// Nominal 20 ms Opus frame.
const NOMINAL = 20000;

// Run a sequence of encoder-reported timestamps through the cadence snap, returning the emitted
// container timestamps (what goes on the wire). The window defaults to half a frame, matching the
// encoder's `max(nominal/2, quantum)` when the quantum is small (48 kHz / 20 ms).
function run(actuals: number[], nominal = NOMINAL, window = nominal / 2): number[] {
	let cadence: number | undefined;
	return actuals.map((actual) => {
		const { ts, next } = snapCadence(cadence, actual, nominal, window);
		cadence = next;
		return ts;
	});
}

function deltas(ts: number[]): number[] {
	return ts.slice(1).map((t, i) => t - ts[i]);
}

describe("snapCadence", () => {
	it("flattens Safari's 18667/21333 quantized cadence to a constant 20000", () => {
		// The exact alternation from the QA logs: frame starts land on the 128-sample capture grid.
		const actuals = [0];
		for (let i = 1; i < 40; i++) actuals.push(actuals[i - 1] + (i % 2 === 1 ? 18667 : 21333));
		const out = run(actuals);
		expect(out[0]).toBe(0);
		expect(deltas(out).every((d) => d === NOMINAL)).toBe(true);
	});

	it("is an identity on an already-exact cadence (Chrome/Firefox)", () => {
		const actuals = Array.from({ length: 20 }, (_, i) => i * NOMINAL);
		expect(run(actuals)).toEqual(actuals);
	});

	it("re-anchors on a real gap larger than the window (mute / silence)", () => {
		const out = run([0, 18667, 40000, 440000, 458667, 480000]);
		expect(out).toEqual([0, 20000, 40000, 440000, 460000, 480000]);
		expect(deltas(out)).toEqual([20000, 20000, 400000, 20000, 20000]);
	});

	it("keeps a fractional-nominal cadence contiguous at the ring index", () => {
		// AAC: 1024 samples at 48 kHz = 21333.333.. us per frame; container timestamps must still land on
		// exact 1024-sample boundaries once indexed by the watcher ring.
		const nominal = (1024 / 48000) * 1_000_000;
		const actuals = Array.from({ length: 30 }, (_, i) => Math.round(i * nominal));
		const out = run(actuals, nominal);
		const idx = out.map((t) => Math.round((t / 1_000_000) * 48000));
		expect(
			idx
				.slice(1)
				.map((v, i) => v - idx[i])
				.every((d) => d === 1024),
		).toBe(true);
	});

	it("widens the window to a full quantum at a low sample rate (8 kHz override)", () => {
		// 8 kHz capture, 48 kHz encode: input chunks are 768 samples = 16000 us, so frame starts quantize
		// to 16000 us and the encoder sets window = max(nominal/2, 16000) = 16000. Feed the actual
		// floor(960N/768)*16000 stamping and assert it flattens to a constant 20000 (nominal/2 = 10000
		// would NOT, since the max deviation is 12000 us).
		const window = Math.max(NOMINAL / 2, 16000);
		expect(window).toBe(16000);
		const actuals = Array.from({ length: 40 }, (_, N) => Math.floor((960 * N) / 768) * 16000);
		const out = run(actuals, NOMINAL, window);
		expect(out[0]).toBe(0);
		expect(deltas(out).every((d) => d === NOMINAL)).toBe(true);
	});
});

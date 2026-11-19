import { describe, expect, test } from "bun:test";
import { Drift, Stamper } from "./drift";

// One 20 ms audio frame, in milliseconds: how far audio may drift from its video over a session.
const FRAME = 20;

// A simulated capture: a context `ppm` fast of the media clock, 10 ms quanta each rendered late by
// up to `jitter` seconds (a fixed LCG, so every run is the same), `stalls` of media time no sample
// covers, as `[at, length]`, and `misreads` of one quantum's capture time, as `[at, error]`, all in
// media seconds.
interface Device {
	ppm: number;
	jitter: number;
	stalls?: [number, number][];
	misreads?: [number, number][];
}

// What the loop did over a run: the error, timestamp less capture time, of every stamped quantum as
// `[media seconds, error seconds]`, and when it re-anchored or how often it dropped.
interface Run {
	errors: [number, number][];
	resyncs: number[];
	drops: number;
}

// The worst error from `after` media seconds on, in milliseconds.
function worst(run: Run, after: number): number {
	return run.errors.filter(([at]) => at >= after).reduce((max, [, error]) => Math.max(max, Math.abs(error) * 1e3), 0);
}

// Every quantum's capture time in media seconds, and how late it reaches the main thread.
function* quanta(device: Device, seconds: number, block: number): Generator<{ host: number; late: number }> {
	let seed = 0x2545f491n;
	device = { ...device, misreads: [...(device.misreads ?? [])] };
	const stalls = [...(device.stalls ?? [])];
	let host = 0;
	while (host < seconds) {
		const stall = stalls[0];
		if (stall && host >= stall[0]) {
			host += stall[1];
			stalls.shift();
		}
		seed = (seed * 6364136223846793005n + 1442695040888963407n) & 0xffffffffffffffffn;
		let late = (device.jitter * Number(seed >> 11n)) / 2 ** 53;
		const misread = device.misreads?.findIndex(([at]) => host >= at) ?? -1;
		if (misread >= 0) late += device.misreads?.splice(misread, 1)[0][1] ?? 0;
		yield { host, late };
		// The context's 10 ms covers less media time when it runs fast.
		host += block / (1 + device.ppm * 1e-6);
	}
}

// Drive the loop alone, with the timeline it steers modelled rather than resampled.
function run(device: Device, seconds: number): Run {
	const block = 0.01;
	const drift = new Drift();
	let anchor: number | undefined;
	let position = 0;
	let relative = 1;
	let ahead = false;
	const out: Run = { errors: [], resyncs: [], drops: 0 };
	for (const { host, late } of quanta(device, seconds, block)) {
		const captured = 1 + host + late;
		const at = anchor ?? captured;
		anchor = at;
		const steer =
			position === 0
				? { kind: "hold" as const }
				: drift.observe(captured * 1e6, (at + position) * 1e6, block * 1e6);
		ahead = false;
		if (steer.kind === "ratio") relative = steer.ratio;
		if (steer.kind === "resync") {
			out.resyncs.push(host);
			anchor = captured;
			position = 0;
		}
		if (steer.kind === "drop") {
			out.drops++;
			ahead = true;
		}
		if (!ahead) {
			out.errors.push([host, (anchor ?? 0) + position - (1 + host)]);
			position += block * relative;
		}
	}
	return out;
}

describe("Drift", () => {
	// Over a simulated hour, every clock within the bound stays within a frame of its capture time.
	// Uncorrected, 1000 ppm drifts 3.6 s.
	test("follows every clock within the bound", () => {
		const failures: string[] = [];
		for (const ppm of [-1000, -300, -50, 0, 12, 100, 1000]) {
			// Quanta delivered on time, or each late by up to a busy page's 50 ms. The first quantum
			// anchors the timeline late by its own delay, which the loop then removes, so a late source
			// is judged once that has settled.
			for (const [jitter, settled] of [
				[0, 0],
				[0.05, 60],
			]) {
				const result = run({ ppm, jitter }, 3600);
				const error = worst(result, settled);
				if (result.resyncs.length || result.drops || error >= FRAME) {
					failures.push(
						`${ppm} ppm, ${jitter} s late: ${result.resyncs.length} resyncs, ${result.drops} drops, ${error} ms`,
					);
				}
			}
		}
		expect(failures).toEqual([]);
	});

	// A host whose monotonic clock counted a five second sleep the context did not, which no
	// suspend jump reports, re-anchors once rather than slewing for hours.
	test("a stall re-anchors once", () => {
		const result = run({ ppm: 300, jitter: 0.002, stalls: [[600, 5]] }, 1200);
		expect([result.resyncs.length, result.drops]).toEqual([1, 0]);
		// Within two windows of the stall, the first of which still held errors from before it.
		expect(result.resyncs[0]).toBeLessThan(607);
		expect(worst(result, result.resyncs[0])).toBeLessThan(FRAME);
	});

	// One quantum read two seconds early, as when the wall clock steps while it is in flight, is
	// ignored rather than learned as a rate the loop then spends minutes unwinding.
	test("a misread quantum is ignored", () => {
		const result = run({ ppm: 50, jitter: 0.002, misreads: [[600, -2]] }, 1200);
		expect([result.resyncs.length, result.drops]).toEqual([0, 0]);
		// Settled long before it, and still on time to within the 2 ms jitter after it.
		expect(worst(result, 300)).toBeLessThan(2);
	});

	// Past the bound the loop runs at it and corrects the rest in steps: a slow clock re-anchors on
	// the gap it leaves, and a fast one drops what runs ahead. Either way the error stays within the
	// resync threshold rather than growing for the whole session.
	test("a clock past the bound stays bounded", () => {
		for (const ppm of [-3000, 3000]) {
			const result = run({ ppm, jitter: 0 }, 600);
			expect(result.resyncs.length + result.drops).toBeGreaterThan(0);
			expect(worst(result, 0)).toBeLessThan(100 + 15);
		}
	});
});

// Drive a stamper as the capture does, through its resampler: `rate` Hz quanta of `samples`, each
// with the media clock's suspend jumps so far.
function stamp(
	device: Device,
	seconds: number,
	options: { rate?: number; samples?: number; jumps?: [number, number][] } = {},
): { errors: [number, number][]; stamps: number[]; ends: number[] } {
	const rate = options.rate ?? 8_000;
	const samples = options.samples ?? 80;
	const stamper = new Stamper(rate);
	const errors: [number, number][] = [];
	const stamps: number[] = [];
	const ends: number[] = [];
	const jumps = [...(options.jumps ?? [])];
	let jumped = 0;
	let shift = 0;
	for (const { host, late } of quanta(device, seconds, samples / rate)) {
		// A suspend the media clock jumped: nothing is captured across it, and the capture times after
		// it read later by the jump.
		const jump = jumps[0];
		if (jump && host >= jump[0]) {
			jumped += jump[1];
			shift += jump[1] / 1e6;
			jumps.shift();
		}
		const captured = 1 + host + shift;
		const channels = [Float32Array.from({ length: samples }, (_, i) => Math.sin(i / 7))];
		const frame = stamper.push(channels, (captured + late) * 1e6, jumped);
		if (!frame) continue;
		const timestamp = frame.timestamp.asMicros();
		stamps.push(timestamp);
		ends.push(timestamp + (frame.channels[0].length / rate) * 1e6);
		// The first sample out sits the resampler's 12-sample lookahead back in the previous quantum.
		errors.push([host, timestamp / 1e6 - captured]);
	}
	return { errors, stamps, ends };
}

describe("Stamper", () => {
	// Through the real resampler, a 1000 ppm context, uncorrected 120 ms off after two minutes,
	// stays within a frame of when each quantum was captured.
	test("keeps a drifting context on the media clock", () => {
		for (const ppm of [-1000, 1000]) {
			const { errors } = stamp({ ppm, jitter: 0.005 }, 120);
			expect(worst({ errors, resyncs: [], drops: 0 }, 30)).toBeLessThan(FRAME);
		}
	});

	// The media clock's suspend jumps land in the timestamps at once, so the timeline neither lags
	// behind video for the windows a re-anchor needs, nor re-anchors at all.
	test("takes the media clock's suspend jumps", () => {
		const { errors, stamps } = stamp({ ppm: 100, jitter: 0.002 }, 60, { jumps: [[30, 3_600_000_000]] });
		expect(worst({ errors, resyncs: [], drops: 0 }, 10)).toBeLessThan(FRAME);
		for (let i = 1; i < stamps.length; i++) expect(stamps[i]).toBeGreaterThan(stamps[i - 1]);
	});

	// Never back: every frame starts at or after where the last one ended.
	test("stamps only forward", () => {
		for (const ppm of [-3000, 3000]) {
			const { stamps, ends } = stamp({ ppm, jitter: 0.01 }, 60);
			expect(stamps.length).toBeGreaterThan(1);
			for (let i = 1; i < stamps.length; i++) expect(stamps[i]).toBeGreaterThanOrEqual(ends[i - 1] - 1e-6);
		}
	});
});

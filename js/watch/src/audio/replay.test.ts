import { afterEach, beforeEach, describe, expect, it, type Mock, spyOn } from "bun:test";
import { Duration } from "@moq/clock";
import type * as Catalog from "@moq/hang/catalog";
import { type Arrival, type Options, QUANTUM, type Quantum, replay } from "./replay";

const RATE = 48000;
const CONFIG = { codec: "opus", sampleRate: RATE, numberOfChannels: 1 } as Catalog.AudioConfig;

/** Twenty ms frames for `seconds`, one per group, each arriving `lateness(i)` ms after it was captured. */
function paced(seconds: number, lateness: (i: number) => number): Arrival[] {
	return Array.from({ length: seconds * 50 }, (_, i) => ({
		at: i * 20 + lateness(i),
		timestamp: i * 20,
		group: i,
		duration: 20,
	}));
}

/** How long {@link paced} observes `seconds` of frames: the frames, plus one more. */
const observed = (seconds: number) => seconds * 1000 + 20;

/**
 * Quanta that came back short or empty once the ring had first played. A ring that runs dry
 * re-stalls until it refills, and that refill is silence too.
 */
async function underruns(trace: Arrival[], options: Options): Promise<number> {
	let started = false;
	let short = 0;
	for await (const { output } of replay(trace, options)) {
		const filled = output.findLastIndex((v) => v !== 0) + 1;
		if (started && filled < QUANTUM) short++;
		if (filled > 0) started = true;
	}
	return short;
}

describe("replay", () => {
	// The consumer warns on every group it skips.
	let warn: Mock<typeof console.warn>;
	beforeEach(() => {
		warn = spyOn(console, "warn").mockImplementation(() => {});
	});
	afterEach(() => warn.mockRestore());

	it("plays an evenly paced sender without a gap", async () => {
		expect(
			await underruns(
				paced(10, () => 30),
				{ rate: RATE, config: CONFIG, delay: Duration.millis(100), duration: observed(10) },
			),
		).toBe(0);
	});

	// Short of the spread, audio about to run out slows down and trails the clock instead, which leaves
	// the underruns the cap cannot bridge.
	it("underruns when a flush span outlasts the target, and not when it is covered", async () => {
		// Five frames held and flushed at once: 80 ms of arrival spread.
		const trace = paced(10, (i) => 30 + (4 - (i % 5)) * 20);
		expect(
			await underruns(trace, { rate: RATE, config: CONFIG, delay: Duration.millis(40), duration: observed(10) }),
		).toBeGreaterThan(0);
		expect(
			await underruns(trace, {
				rate: RATE,
				config: CONFIG,
				delay: Duration.millis(150),
				duration: observed(10),
			}),
		).toBe(0);
	});

	it("counts each run-dry the way the player's stats do", async () => {
		// One frame 200 ms late against a 100 ms target: the ring runs dry once and re-stalls to refill,
		// which counts the short quantum it ran dry in and the stall after it.
		const trace = paced(10, (i) => (i === 250 ? 230 : 30)).sort((a, b) => a.at - b.at);
		const counted = async (arrivals: Arrival[]) => {
			let last = 0;
			for await (const { underruns } of replay(arrivals, {
				rate: RATE,
				config: CONFIG,
				delay: Duration.millis(100),
				duration: observed(10),
			}))
				last = underruns;
			return last;
		};
		expect(await counted(paced(10, () => 30))).toBe(0);
		expect(await counted(trace)).toBeGreaterThan(0);
		expect(await counted(trace)).toBeLessThanOrEqual(2);
	});

	it("holds newer groups behind a missing one until the max delay gives up on it", async () => {
		const trace = paced(10, () => 30).filter((arrival) => arrival.group !== 250);
		// The consumer delivers in group order, so the ring runs dry for longer than the missing frame
		// while the groups behind it wait. Writing arrivals straight into the ring plays straight through.
		const frame = Math.ceil((20 / 1000) * (RATE / QUANTUM));
		expect(
			await underruns(trace, {
				rate: RATE,
				config: CONFIG,
				delay: Duration.millis(100),
				duration: observed(10),
			}),
		).toBeGreaterThan(frame);
		expect(warn).toHaveBeenCalled();
	});

	it("leaves a frame missing inside a group as missing audio", async () => {
		// Five frames a group, delivered on time, with one from the middle of a group never sent.
		const trace = paced(10, () => 30)
			.map((arrival, i) => ({ ...arrival, group: Math.floor(i / 5) }))
			.filter((_, i) => i !== 252);
		expect(
			await underruns(trace, {
				rate: RATE,
				config: CONFIG,
				delay: Duration.millis(100),
				duration: observed(10),
			}),
		).toBeGreaterThan(0);
	});

	it("renders through the end of the observation, past the last arrival", async () => {
		let last = 0;
		let heard = 0;
		for await (const { at, output } of replay(
			paced(10, () => 30),
			{
				rate: RATE,
				config: CONFIG,
				delay: Duration.millis(100),
				duration: 15_000,
			},
		)) {
			last = at;
			if (at > 11_000 && output.some((v) => v !== 0)) heard++;
		}
		expect(last).toBeGreaterThanOrEqual(15_000);
		expect(heard).toBe(0);
	});
});

/**
 * How far the playhead trails the shared clock video plays from, in ms, for each quantum played once
 * the first `settle` ms have passed, with the underruns the ring counted by the end.
 */
async function lag(trace: Arrival[], options: Options, settle: number) {
	const lags: number[] = [];
	let underruns = 0;
	let trimmed = 0;
	let accelerated = 0;
	for await (const quantum of replay(trace, options)) {
		if (quantum.at >= settle && !quantum.stalled && quantum.clock !== undefined) {
			lags.push(quantum.clock - quantum.timestamp);
		}
		underruns = quantum.underruns;
		trimmed = quantum.trimmed;
		accelerated = quantum.accelerated;
	}
	lags.sort((a, b) => a - b);
	return { lags, median: lags[lags.length >> 1], underruns, trimmed, accelerated };
}

/**
 * The longest pitch period of the replay's speech, at its 90 Hz floor: a seam moves by one period, so
 * a lag shorter than that may rest uncorrected.
 */
const PERIOD = 1000 / 90;

// Audio and video keep separate clocks that align once the delay covers the arrivals. The ring plays
// onto the shared clock: in steady state the playhead rests within a pitch period of it, either way.
// A buffer that runs low slows playback and leaves it behind the clock, only as far as the stretch
// allows, which a zero stretch leaves none of.
describe("replay against the shared clock", () => {
	let warn: Mock<typeof console.warn>;
	beforeEach(() => {
		warn = spyOn(console, "warn").mockImplementation(() => {});
	});
	afterEach(() => warn.mockRestore());

	for (const [name, lateness] of [
		["evenly paced", () => 30],
		["flushed five frames at a time", (i: number) => 30 + (4 - (i % 5)) * 20],
	] as const) {
		it(`plays ${name} frames on the clock at auto`, async () => {
			const options: Options = { rate: RATE, config: CONFIG, delay: "auto", duration: observed(60) };
			const { lags, underruns } = await lag(paced(60, lateness), options, 30_000);
			expect(lags.length).toBeGreaterThan(0);
			expect(lags[0]).toBeGreaterThanOrEqual(-PERIOD);
			expect(lags.at(-1)).toBeLessThanOrEqual(PERIOD);
			expect(underruns).toBe(0);
		});
	}

	// 80 ms of spread against a fixed 40 ms target: every flush runs a ring dry that cannot fall behind.
	const flushed = paced(60, (i) => 30 + (4 - (i % 5)) * 20);
	const fixed = { rate: RATE, config: CONFIG, delay: Duration.millis(40), duration: observed(60) };

	it("underruns a fixed target short of the spread with the stretch off, rather than trailing the clock", async () => {
		const { lags, median, underruns, accelerated } = await lag(flushed, { ...fixed, stretch: 0 }, 30_000);
		expect(underruns).toBeGreaterThanOrEqual(flushed.length / 5);
		// Each refill skips back onto the clock within the frame after it, never by a whole flush.
		expect(median).toBeLessThanOrEqual(20);
		expect(lags.at(-1)).toBeLessThan(40);
		expect(accelerated).toBe(0);
	});

	// The stretch raises no target, so the flushes still underrun, but what arrives late plays and is
	// caught up by speeding up, where a zero stretch skips it.
	it("keeps the audio a fixed target short of the spread receives late, where a zero stretch skips it", async () => {
		const off = await lag(flushed, { ...fixed, stretch: 0 }, 30_000);
		const on = await lag(flushed, fixed, 30_000);
		// A zero stretch skips most of every flush; the stretch skips only the backlog it joins on.
		expect(off.trimmed).toBeGreaterThan(20_000);
		expect(on.trimmed).toBeLessThanOrEqual(40);
		expect(on.accelerated).toBeGreaterThan(0);
	});
});

describe("replay at auto", () => {
	/** The delay the replay resolved once `seconds` of {@link paced} frames played out. */
	async function settled(seconds: number, lateness: (i: number) => number): Promise<number> {
		let delay = 0;
		const options: Options = { rate: RATE, config: CONFIG, delay: "auto", duration: observed(seconds) };
		for await (const quantum of replay(paced(seconds, lateness), options)) delay = quantum.delay;
		return delay;
	}

	// A minute, so the estimator has long since spent its prior and settled.
	it("sizes the target from the arrival spread, not a round trip", async () => {
		// Evenly paced, the target is the estimator's 20 ms floor plus one frame.
		expect(await settled(60, () => 30)).toBe(40);
		// Five frames flushed at once: 80 ms of spread to cover, plus one frame.
		expect(await settled(60, (i) => 30 + (4 - (i % 5)) * 20)).toBeGreaterThanOrEqual(100);
	});
});

describe("recorded frame durations", () => {
	it("keeps missing packets out of the codec term even when most packets are absent", async () => {
		const trace = Array.from({ length: 600 }, (_, i) => ({
			at: i * 100,
			timestamp: i * 100,
			group: 0,
			duration: 20,
		}));
		let delay = 0;
		for await (const quantum of replay(trace, {
			rate: RATE,
			config: CONFIG,
			delay: "auto",
			duration: 60_000,
		}))
			delay = quantum.delay;
		expect(delay).toBe(40);
	});

	it("updates the codec term when the packet duration changes", async () => {
		const trace = Array.from({ length: 600 }, (_, i) => ({
			at: i * 100,
			timestamp: i * 100,
			group: 0,
			duration: i < 300 ? 20 : 10,
		}));
		const targets: number[] = [];
		for await (const quantum of replay(trace, {
			rate: RATE,
			config: CONFIG,
			delay: "auto",
			duration: 60_000,
		})) {
			if (quantum.at >= 29_000 && quantum.at <= 29_100) targets.push(quantum.delay);
			if (quantum.at >= 59_000) targets.push(quantum.delay);
		}
		expect(new Set(targets)).toEqual(new Set([40, 30]));
	});

	it("plays a single packet for its recorded duration", async () => {
		const trace = [{ at: 0, timestamp: 0, group: 0, duration: 20 }];
		let heard = 0;
		for await (const { output } of replay(trace, {
			rate: RATE,
			config: CONFIG,
			delay: Duration.millis(20),
			// Without the stretch, which would slow a lone packet down as it runs out.
			stretch: 0,
			duration: 100,
		}))
			heard += output.filter((sample) => sample !== 0).length;
		expect(heard).toBe(960);
	});
});

it("replay refuses missing, non-finite or non-positive recorded durations", async () => {
	for (const duration of [undefined, Number.NaN, Number.POSITIVE_INFINITY, 0, -20]) {
		const trace = [{ at: 0, timestamp: 0, group: 0, duration }] as Arrival[];
		const reader = replay(trace, {
			rate: RATE,
			config: CONFIG,
			delay: "auto",
			duration: 100,
		});
		await expect(reader.next()).rejects.toThrow("invalid recorded frame duration");
	}
});

// Five seconds advertised is ten times what the ring holds decoded, so the rest waits encoded in the
// consumer until the received media covers it, then plays at the advertised delay.
it("plays an advertised flush span past the decoded floor at its delay without a gap", async () => {
	const config = { ...CONFIG, jitter: 5000 } as Catalog.AudioConfig;
	let start: number | undefined;
	let short = 0;
	let lag = 0;
	let delay = 0;
	for await (const quantum of replay(
		paced(12, () => 30),
		{
			rate: RATE,
			config,
			delay: "auto",
			duration: observed(12),
		},
	)) {
		const filled = quantum.output.findLastIndex((v) => v !== 0) + 1;
		if (start === undefined && filled > 0) start = quantum.at;
		if (start !== undefined && filled < QUANTUM) short++;
		lag = (quantum.clock ?? 0) - quantum.timestamp;
		delay = quantum.delay;
	}
	expect(delay).toBe(5020);
	// The first frame arrived 30 ms late, so it plays the target after its own capture plus that.
	expect(start).toBeGreaterThanOrEqual(5020);
	expect(start).toBeLessThan(5040);
	expect(short).toBe(0);
	// Then onto the shared clock, which counts that lateness too.
	expect(Math.abs(lag)).toBeLessThanOrEqual(PERIOD);
});

// The same rule plays a buffered player onto the clock: its lookahead keeps the level high, so it plays
// faster whenever it trails the clock, and the container keeps every group, since audio on the clock
// stays within the max delay of the newest arrival.
describe("buffered replay against the shared clock", () => {
	let warn: Mock<typeof console.warn>;
	beforeEach(() => {
		warn = spyOn(console, "warn").mockImplementation(() => {});
	});
	afterEach(() => warn.mockRestore());

	/** Plays `trace`, returning how far audio trails the clock once settled, and what was dropped. */
	async function play(trace: Arrival[], options: Options, settled: (at: number) => boolean) {
		const lags: number[] = [];
		let heard = 0;
		let last: Quantum | undefined;
		for await (const quantum of replay(trace, options)) {
			heard += quantum.output.filter((v) => v !== 0).length;
			if (settled(quantum.at) && !quantum.stalled && quantum.clock !== undefined) {
				lags.push(quantum.clock - quantum.timestamp);
			}
			last = quantum;
		}
		if (!last) throw new Error("nothing played");
		return {
			min: Math.min(...lags),
			max: Math.max(...lags),
			skips: last.skips,
			trimmed: last.trimmed,
			accelerated: last.accelerated,
			// Frames the consumer passed over rather than delivered.
			lost: trace.length - last.delivered,
			heard: (heard * 1000) / RATE,
		};
	}

	it("catches up after a stall", async () => {
		// Five seconds in, 400 ms of frames are held and land at once.
		const trace = paced(30, (i) => 30 + (i >= 250 && i < 270 ? (270 - i) * 20 : 0)).sort((a, b) => a.at - b.at);
		const options: Options = {
			rate: RATE,
			config: CONFIG,
			delay: Duration.millis(100),
			lookahead: 500,
			duration: observed(30),
		};
		// Speeding up at the cap gains 100 ms a second, so the stall's lag is gone well before 20 s.
		const result = await play(trace, options, (at) => (at >= 2000 && at < 5000) || at >= 20_000);
		expect(result.min).toBeGreaterThanOrEqual(-PERIOD);
		expect(result.max).toBeLessThanOrEqual(PERIOD);
		expect(result).toMatchObject({ skips: 0, trimmed: 0, lost: 0 });
		expect(warn).not.toHaveBeenCalled();
	});

	// The rate follows the lag, so a clock running 1% fast leaves audio trailing it by about 1% of the
	// 500 ms lag that reaches the cap, and a splice every second or so: within a 20 ms frame.
	it("follows a publisher clock running 1% fast at auto", async () => {
		const trace = paced(60, (i) => 30 - (i * 20) / 101);
		const options: Options = {
			rate: RATE,
			config: CONFIG,
			delay: "auto",
			lookahead: 500,
			duration: observed(60),
		};
		const result = await play(trace, options, (at) => at >= 10_000);
		expect(result.min).toBeGreaterThanOrEqual(-PERIOD);
		expect(result.max).toBeLessThan(20);
		expect(result).toMatchObject({ skips: 0, trimmed: 0, lost: 0 });
		expect(warn).not.toHaveBeenCalled();
	});

	it("plays every word of an utterance that stalls mid-burst, catching up after", async () => {
		// Forty seconds of speech generated four times faster than real time, held for 3.5 s after its
		// first two seconds, so playback runs dry and resumes about two seconds behind the clock, inside
		// the lookahead: at the cap that takes some 20 s to catch up. The utterance's last moments slow
		// down as it runs out, which no declared end explains.
		const trace = paced(40, (i) => (i < 100 ? 30 : 3500) - i * 15);
		const options: Options = {
			rate: RATE,
			config: CONFIG,
			delay: Duration.millis(100),
			lookahead: 60_000,
			duration: 42_000,
		};
		const result = await play(trace, options, (at) => at >= 30_000 && at < 39_000);
		expect(result.min).toBeGreaterThanOrEqual(-PERIOD);
		expect(result.max).toBeLessThanOrEqual(PERIOD);
		expect(result).toMatchObject({ skips: 0, trimmed: 0, lost: 0 });
		// Every frame played: what was heard and what the stretch shortened cover the utterance. A
		// slow-down only adds to what is heard.
		expect(Math.round(result.heard + result.accelerated)).toBeGreaterThanOrEqual(40_000);
		expect(warn).not.toHaveBeenCalled();
	});
});

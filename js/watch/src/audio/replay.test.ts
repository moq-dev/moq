import { afterEach, beforeEach, describe, expect, it, type Mock, spyOn } from "bun:test";
import type * as Catalog from "@moq/hang/catalog";
import { Time } from "@moq/net";
import { type Arrival, type Options, QUANTUM, replay, target } from "./replay";

const RATE = 48000;

/** Twenty ms frames for `seconds`, one per group, each arriving `lateness(i)` ms after it was captured. */
function paced(seconds: number, lateness: (i: number) => number): Arrival[] {
	return Array.from({ length: seconds * 50 }, (_, i) => ({ at: i * 20 + lateness(i), timestamp: i * 20, group: i }));
}

/** Quanta that came back short or empty while playing, once the ring had first played. */
async function underruns(trace: Arrival[], options: Options): Promise<number> {
	let started = false;
	let short = 0;
	for await (const { output, stalled } of replay(trace, options)) {
		const filled = output.findLastIndex((v) => v !== 0) + 1;
		if (started && !stalled && filled < QUANTUM) short++;
		if (filled > 0) started = true;
	}
	return short;
}

describe.each(["shared", "post"] as const)("%s ring", (ring) => {
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
				{ ring, rate: RATE, delay: 100 },
			),
		).toBe(0);
	});

	it("underruns when a flush span outlasts the target, and not when it is covered", async () => {
		// Five frames held and flushed at once: 80 ms of arrival spread.
		const trace = paced(10, (i) => 30 + (4 - (i % 5)) * 20);
		expect(await underruns(trace, { ring, rate: RATE, delay: 40 })).toBeGreaterThan(50);
		expect(await underruns(trace, { ring, rate: RATE, delay: 150 })).toBe(0);
	});

	it("holds newer groups behind a missing one until the max age gives up on it", async () => {
		const trace = paced(10, () => 30).filter((arrival) => arrival.group !== 250);
		// The consumer delivers in group order, so the ring runs dry for longer than the missing frame
		// while the groups behind it wait. Writing arrivals straight into the ring plays straight through.
		const frame = Math.ceil((20 / 1000) * (RATE / QUANTUM));
		expect(await underruns(trace, { ring, rate: RATE, delay: 100 })).toBeGreaterThan(frame);
		expect(warn).toHaveBeenCalled();
	});
});

describe("target", () => {
	const config = { codec: "opus", sampleRate: RATE, numberOfChannels: 1 } as Catalog.AudioConfig;

	it("sizes auto from the RTT and the rendition's jitter", async () => {
		// 1.25 x 40 ms of RTT, plus the Opus frame and a render quantum.
		expect(await target({ delay: "auto", config, rtt: 40 })).toBe(Time.Milli(50 + 20 + 3));
	});

	it("adds the rendition's jitter to a fixed delay", async () => {
		expect(await target({ delay: Time.Milli(250), config })).toBe(Time.Milli(250 + 20 + 3));
	});
});

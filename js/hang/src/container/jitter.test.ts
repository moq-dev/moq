import { describe, expect, it } from "bun:test";
import { Time } from "@moq/net";
import { Jitter } from "./jitter";

const CHUNK_MS = 20;

/** Feed one arrival, in milliseconds on both axes. */
function observe(jitter: Jitter, media: number, arrival: number): void {
	jitter.observe(Time.Micro.fromMilli(media as Time.Milli), arrival as Time.Milli);
}

/**
 * A sender emitting one chunk every `CHUNK_MS`, holding `burst - 1` of them back and flushing the
 * whole run at once. `burst` of 1 is an evenly paced sender.
 */
function flush(jitter: Jitter, chunks: number, burst: number, base = 50): void {
	let held: number[] = [];
	for (let i = 0; i < chunks; i++) {
		const media = i * CHUNK_MS;
		held.push(media);
		if (held.length < burst) continue;
		const arrival = media + base;
		for (const m of held) observe(jitter, m, arrival);
		held = [];
	}
}

describe("arrival jitter", () => {
	it("is one frame for an evenly paced sender", () => {
		// Nothing arrives late, so all the buffer needs to hold is the frame being played.
		const jitter = new Jitter();
		flush(jitter, 500, 1);
		expect(jitter.value.peek()).toBe(CHUNK_MS as Time.Milli);
	});

	it("does not follow the absolute delay", () => {
		// A sender on the other side of the planet is not a jittery sender: only the spread of
		// arrivals matters, which is what makes the round trip the wrong term to size a buffer from.
		const near = new Jitter();
		flush(near, 500, 1, 10);
		const far = new Jitter();
		flush(far, 500, 1, 2000);
		expect(far.value.peek()).toBe(near.value.peek());
	});

	it("converges on the flush span of a bursty sender", () => {
		// Three chunks land at once, so the oldest is two chunk durations late.
		const jitter = new Jitter();
		flush(jitter, 600, 3);
		expect(jitter.value.peek()).toBeGreaterThanOrEqual(3 * CHUNK_MS);
		expect(jitter.value.peek()).toBeLessThanOrEqual(4 * CHUNK_MS);
	});

	it("rises as soon as one arrival proves the buffer is too shallow", () => {
		const jitter = new Jitter();
		flush(jitter, 100, 1);
		expect(jitter.value.peek()).toBe(CHUNK_MS as Time.Milli);

		// A single 250ms straggler is only 1% of the samples, so the percentile ignores it. It costs
		// one bucket of histogram resolution, not a quarter second of buffer.
		observe(jitter, 100 * CHUNK_MS, 100 * CHUNK_MS + 250);
		expect(jitter.value.peek()).toBeLessThan(2 * CHUNK_MS);

		// A run of them is the network, not an outlier, and the estimate follows within a second.
		for (let i = 0; i < 20; i++) {
			observe(jitter, (101 + i) * CHUNK_MS, (101 + i) * CHUNK_MS + 250);
		}
		expect(jitter.value.peek()).toBeGreaterThan(150 as Time.Milli);
	});

	it("lowers by at most one chunk per interval", () => {
		const jitter = new Jitter();
		flush(jitter, 600, 3);
		const peak = jitter.value.peek();
		expect(peak).toBeGreaterThan(0 as Time.Milli);

		// Arrivals settle. Sample the estimate every second of the recovery: a step larger than one
		// chunk would drop a viewer's playhead onto a cushion that isn't there yet.
		let previous = peak;
		let last = 0;
		const base = 600 * CHUNK_MS;
		for (let i = 0; i < 6000; i++) {
			const media = base + i * CHUNK_MS;
			observe(jitter, media, media + 50);
			const now = media + 50;
			if (now - last < 1000) continue;
			last = now;
			const current = jitter.value.peek();
			expect(previous - current).toBeLessThanOrEqual(CHUNK_MS);
			previous = current;
		}

		// And it does come back down to a frame plus the histogram's resolution once arrivals have
		// been even for a while.
		expect(jitter.value.peek()).toBeLessThan(2 * CHUNK_MS);
	});

	it("expires the baseline after a long gap", () => {
		const jitter = new Jitter();
		flush(jitter, 200, 1);

		// Nothing arrives for over a minute, and the path delay is higher when the stream resumes.
		// Frames still land evenly, so they are punctual, not late: a minimum from before the gap
		// would read the whole difference as jitter.
		const base = 200 * CHUNK_MS + 90_000;
		for (let i = 0; i < 200; i++) {
			observe(jitter, base + i * CHUNK_MS, base + i * CHUNK_MS + 400);
		}
		expect(jitter.value.peek()).toBe(CHUNK_MS as Time.Milli);
	});

	it("forgets the baseline across a discontinuity", () => {
		const jitter = new Jitter();
		flush(jitter, 200, 1);

		// The publisher restarts its timeline an hour in the past. Without a re-anchor every arrival
		// after it reads as an hour late and the estimate saturates.
		jitter.reanchor();
		const base = 3_600_000;
		for (let i = 0; i < 200; i++) {
			observe(jitter, base + i * CHUNK_MS, i * CHUNK_MS + 50);
		}
		expect(jitter.value.peek()).toBe(CHUNK_MS as Time.Milli);
	});
});

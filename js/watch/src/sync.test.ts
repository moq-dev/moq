import { describe, expect, it } from "bun:test";
import type { Time } from "@moq/net";
import { Signal } from "@moq/signals";
import { Sync } from "./sync";

// Effects in @moq/signals flush on a microtask, so let pending updates drain before asserting.
const flush = () => new Promise((resolve) => setTimeout(resolve, 0));

describe("delay and buffer", () => {
	it("holds no lookahead by default", async () => {
		const sync = new Sync();
		await flush();
		expect(sync.out.buffered.peek()).toBe(false);
		sync.close();
	});

	it("caps maxAge at the delay when no buffer is configured", async () => {
		const sync = new Sync({ delay: 100 as Time.Milli });
		await flush();
		expect(sync.out.buffered.peek()).toBe(false);
		expect(sync.out.delay.peek()).toBe(100 as Time.Milli);
		expect(sync.out.maxAge.peek()).toBe(100 as Time.Milli);
		sync.close();
	});

	it("adds the buffer on top of the delay", async () => {
		// The buffer is measured from the live edge, so it does not swallow the delay: a frame may
		// sit `delay + buffer` ahead of the playhead before playback skips forward.
		const sync = new Sync({ delay: 100 as Time.Milli, buffer: 30_000 as Time.Milli });
		await flush();
		expect(sync.out.buffered.peek()).toBe(true);
		expect(sync.out.maxAge.peek()).toBe(30_100 as Time.Milli);
		sync.close();
	});

	it("stays unbuffered for a zero buffer", async () => {
		const sync = new Sync({ delay: 200 as Time.Milli, buffer: 0 as Time.Milli });
		await flush();
		expect(sync.out.buffered.peek()).toBe(false);
		expect(sync.out.maxAge.peek()).toBe(200 as Time.Milli);
		sync.close();
	});

	it("reacts to a buffer set after construction", async () => {
		const buffer = new Signal<Time.Milli>(0 as Time.Milli);
		const sync = new Sync({ delay: 100 as Time.Milli, buffer });
		await flush();
		expect(sync.out.buffered.peek()).toBe(false);

		buffer.set(30_000 as Time.Milli);
		await flush();
		expect(sync.out.buffered.peek()).toBe(true);
		expect(sync.out.maxAge.peek()).toBe(30_100 as Time.Milli);
		sync.close();
	});

	it("holds nothing when instant, whatever the buffer says", async () => {
		const sync = new Sync({ delay: "instant", buffer: 30_000 as Time.Milli });
		await flush();
		expect(sync.out.buffered.peek()).toBe(false);
		expect(sync.out.delay.peek()).toBe(0 as Time.Milli);
		expect(sync.out.maxAge.peek()).toBe(0 as Time.Milli);
		sync.close();
	});
});

describe("auto delay", () => {
	it("starts at the advertised delay and follows the measured arrivals", async () => {
		const audioSpread = new Signal<Time.Milli | undefined>(undefined);
		const sync = new Sync({ audio: 20 as Time.Milli, video: 33 as Time.Milli, audioSpread });
		await flush();

		// Nothing measured yet, so the delay is what the catalog advertises.
		expect(sync.out.jitter.peek()).toBe(0 as Time.Milli);
		expect(sync.out.delay.peek()).toBe(33 as Time.Milli);

		// A publisher flushing 250ms of audio at once needs 250ms of buffer, whatever the RTT is.
		// The measured spread already covers what the catalog advertises, so it replaces the
		// advertised delay rather than stacking on it.
		audioSpread.set(250 as Time.Milli);
		await flush();
		expect(sync.out.jitter.peek()).toBe(250 as Time.Milli);
		expect(sync.out.delay.peek()).toBe(250 as Time.Milli);

		sync.close();
	});

	it("takes the largest spread across tracks", async () => {
		const audioSpread = new Signal<Time.Milli | undefined>(40 as Time.Milli);
		const videoSpread = new Signal<Time.Milli | undefined>(120 as Time.Milli);
		const sync = new Sync({ audioSpread, videoSpread });
		await flush();
		expect(sync.out.jitter.peek()).toBe(120 as Time.Milli);

		audioSpread.set(200 as Time.Milli);
		await flush();
		expect(sync.out.jitter.peek()).toBe(200 as Time.Milli);

		sync.close();
	});

	it("ignores the measured spread when the delay is a fixed number", async () => {
		const audioSpread = new Signal<Time.Milli | undefined>(250 as Time.Milli);
		const sync = new Sync({ delay: 500 as Time.Milli, audio: 20 as Time.Milli, audioSpread });
		await flush();
		expect(sync.out.jitter.peek()).toBe(500 as Time.Milli);
		expect(sync.out.delay.peek()).toBe(520 as Time.Milli);
		sync.close();
	});
});

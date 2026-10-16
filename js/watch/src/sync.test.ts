import { describe, expect, it } from "bun:test";
import { Time } from "@moq/net";
import { Signal } from "@moq/signals";
import { type Delay, Sync } from "./sync";

// Effects in @moq/signals flush on a microtask, so let pending updates drain before asserting.
const flush = () => new Promise((resolve) => setTimeout(resolve, 0));

describe("delay and buffer", () => {
	it("holds no lookahead by default", async () => {
		const sync = new Sync();
		await flush();
		expect(sync.out.buffered.peek()).toBe(false);
		sync.close();
	});

	it("caps maxDelay at the delay when no buffer is configured", async () => {
		const sync = new Sync({ delay: 100 as Time.Milli });
		await flush();
		expect(sync.out.buffered.peek()).toBe(false);
		expect(sync.out.delay.peek()).toBe(100 as Time.Milli);
		expect(sync.out.maxDelay.peek()).toBe(100 as Time.Milli);
		sync.close();
	});

	it("adds the buffer on top of the delay", async () => {
		// The buffer is measured from the live edge, so it does not swallow the delay: a frame may
		// sit `delay + buffer` ahead of the playhead before playback skips forward.
		const sync = new Sync({ delay: 100 as Time.Milli, buffer: 30_000 as Time.Milli });
		await flush();
		expect(sync.out.buffered.peek()).toBe(true);
		expect(sync.out.maxDelay.peek()).toBe(30_100 as Time.Milli);
		sync.close();
	});

	it("stays unbuffered for a zero buffer", async () => {
		const sync = new Sync({ delay: 200 as Time.Milli, buffer: 0 as Time.Milli });
		await flush();
		expect(sync.out.buffered.peek()).toBe(false);
		expect(sync.out.maxDelay.peek()).toBe(200 as Time.Milli);
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
		expect(sync.out.maxDelay.peek()).toBe(30_100 as Time.Milli);
		sync.close();
	});

	it("holds nothing when instant, whatever the buffer says", async () => {
		const sync = new Sync({ delay: "instant", buffer: 30_000 as Time.Milli });
		await flush();
		expect(sync.out.instant.peek()).toBe(true);
		expect(sync.out.buffered.peek()).toBe(false);
		expect(sync.out.delay.peek()).toBe(0 as Time.Milli);
		expect(sync.out.maxDelay.peek()).toBe(0 as Time.Milli);
		sync.close();
	});
});

describe("auto delay", () => {
	it("holds nothing until a decoder registers", async () => {
		const sync = new Sync();
		await flush();
		expect(sync.out.delay.peek()).toBe(0 as Time.Milli);
		sync.close();
	});

	it("follows the deepest registered target until it unregisters", async () => {
		const audio = new Signal<Time.Milli | undefined>(120 as Time.Milli);
		const video = new Signal<Time.Milli | undefined>(40 as Time.Milli);
		const sync = new Sync();
		const unregisterAudio = sync.register(audio);
		sync.register(video);
		await flush();
		expect(sync.out.delay.peek()).toBe(120 as Time.Milli);
		expect(sync.out.jitter.peek()).toBe(120 as Time.Milli);

		// A publisher flushing 250ms at once needs 250ms of buffer, whatever the round trip is.
		audio.set(270 as Time.Milli);
		await flush();
		expect(sync.out.delay.peek()).toBe(270 as Time.Milli);

		unregisterAudio();
		await flush();
		expect(sync.out.delay.peek()).toBe(40 as Time.Milli);
		sync.close();
	});

	it("unregisters duplicate targets independently", async () => {
		const media = new Signal<Time.Milli | undefined>(20 as Time.Milli);
		const sync = new Sync();
		const unregisterFirst = sync.register(media);
		const unregisterSecond = sync.register(media);

		unregisterFirst();
		await flush();
		expect(sync.out.delay.peek()).toBe(20 as Time.Milli);

		unregisterSecond();
		await flush();
		expect(sync.out.delay.peek()).toBe(0 as Time.Milli);
		sync.close();
	});

	it("takes a fixed delay literally, ignoring the measured targets", async () => {
		const media = new Signal<Time.Milli | undefined>(250 as Time.Milli);
		const sync = new Sync({ delay: 100 as Time.Milli });
		sync.register(media);
		await flush();
		expect(sync.out.delay.peek()).toBe(100 as Time.Milli);
		expect(sync.out.jitter.peek()).toBe(100 as Time.Milli);
		sync.close();
	});
});

describe("wait", () => {
	it("returns at once when constructed instant, before effects flush", async () => {
		const sync = new Sync({ delay: "instant" });
		await sync.wait(Time.Milli.zero);
		sync.close();
	});

	it("wakes a sleeping wait when the delay switches to instant", async () => {
		const delay = new Signal<Delay>(10_000 as Time.Milli);
		const sync = new Sync({ delay });
		await flush();
		sync.received(Time.Milli.now());

		let woke = false;
		const waiting = sync.wait(Time.Milli.now()).then(() => {
			woke = true;
		});
		await flush();
		expect(woke).toBe(false);

		delay.set("instant");
		await waiting;
		expect(woke).toBe(true);
		sync.close();
	});

	it("holds a sleeping wait across a reset until the next frame re-anchors", async () => {
		const sync = new Sync({ delay: 10_000 as Time.Milli });
		await flush();
		const timestamp = Time.Milli.now();
		sync.received(timestamp);

		let woke = false;
		const waiting = sync.wait(timestamp).then(() => {
			woke = true;
		});
		await flush();
		sync.reset();
		await flush();
		expect(woke).toBe(false);

		// The next frame anchors far enough ahead that this one is overdue.
		sync.received(Time.Milli.add(timestamp, Time.Milli(20_000)));
		await waiting;
		expect(woke).toBe(true);
		sync.close();
	});

	it("releases a wait parked across a reset when its signal aborts", async () => {
		const sync = new Sync({ delay: 10_000 as Time.Milli });
		await flush();
		const timestamp = Time.Milli.now();
		sync.received(timestamp);

		const controller = new AbortController();
		const waiting = sync.wait(timestamp, controller.signal);
		await flush();
		sync.reset();
		await flush();
		controller.abort();
		await waiting;
		sync.close();
	});

	it("releases a wait parked across a reset when closed", async () => {
		const sync = new Sync({ delay: 10_000 as Time.Milli });
		await flush();
		const timestamp = Time.Milli.now();
		sync.received(timestamp);

		const waiting = sync.wait(timestamp);
		await flush();
		sync.reset();
		await flush();
		sync.close();
		await waiting;
	});
});

describe("recorded ingress", () => {
	it("anchors and classifies using arrival rather than delayed delivery", async () => {
		const sync = new Sync({ delay: Time.Milli(100) });
		const original = performance.now;
		performance.now = () => 10000;
		try {
			for (let i = 0; i < 20; i++) await Promise.resolve();
			expect(sync.received(Time.Milli(0), "audio", Time.Milli(1000))).toBe(false);
			expect(sync.out.reference.peek()).toBe(Time.Milli(1000));
			expect(sync.received(Time.Milli(20), "audio", Time.Milli(1020))).toBe(false);
			expect(sync.received(Time.Milli(40), "audio", Time.Milli(1200))).toBe(true);
		} finally {
			performance.now = original;
			sync.close();
		}
	});
});

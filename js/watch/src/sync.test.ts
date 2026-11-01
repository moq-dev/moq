import { afterEach, beforeEach, describe, expect, it } from "bun:test";
import { type Installed, Manual, Milli, Nano, now, yieldNow } from "@moq/clock";
import { Signal } from "@moq/signals";
import { type Delay, Sync } from "./sync";

// Effects in @moq/signals flush on a microtask, so let pending updates drain before asserting.
const flush = () => yieldNow();

describe("delay and buffer", () => {
	it("holds no lookahead by default", async () => {
		const sync = new Sync();
		await flush();
		expect(sync.out.buffered.peek()).toBe(false);
		sync.close();
	});

	it("caps maxDelay at the delay when no buffer is configured", async () => {
		const sync = new Sync({ delay: 100 as Milli });
		await flush();
		expect(sync.out.buffered.peek()).toBe(false);
		expect(sync.out.delay.peek()).toBe(100 as Milli);
		expect(sync.out.maxDelay.peek()).toBe(100 as Milli);
		sync.close();
	});

	it("adds the buffer on top of the delay", async () => {
		// The buffer is measured from the live edge, so it does not swallow the delay: a frame may
		// sit `delay + buffer` ahead of the playhead before playback skips forward.
		const sync = new Sync({ delay: 100 as Milli, buffer: 30_000 as Milli });
		await flush();
		expect(sync.out.buffered.peek()).toBe(true);
		expect(sync.out.maxDelay.peek()).toBe(30_100 as Milli);
		sync.close();
	});

	it("stays unbuffered for a zero buffer", async () => {
		const sync = new Sync({ delay: 200 as Milli, buffer: 0 as Milli });
		await flush();
		expect(sync.out.buffered.peek()).toBe(false);
		expect(sync.out.maxDelay.peek()).toBe(200 as Milli);
		sync.close();
	});

	it("reacts to a buffer set after construction", async () => {
		const buffer = new Signal<Milli>(0 as Milli);
		const sync = new Sync({ delay: 100 as Milli, buffer });
		await flush();
		expect(sync.out.buffered.peek()).toBe(false);

		buffer.set(30_000 as Milli);
		await flush();
		expect(sync.out.buffered.peek()).toBe(true);
		expect(sync.out.maxDelay.peek()).toBe(30_100 as Milli);
		sync.close();
	});

	it("holds nothing when instant, whatever the buffer says", async () => {
		const sync = new Sync({ delay: "instant", buffer: 30_000 as Milli });
		await flush();
		expect(sync.out.instant.peek()).toBe(true);
		expect(sync.out.buffered.peek()).toBe(false);
		expect(sync.out.delay.peek()).toBe(0 as Milli);
		expect(sync.out.maxDelay.peek()).toBe(0 as Milli);
		sync.close();
	});
});

describe("auto delay", () => {
	it("holds nothing until a decoder registers", async () => {
		const sync = new Sync();
		await flush();
		expect(sync.out.delay.peek()).toBe(0 as Milli);
		sync.close();
	});

	it("follows the deepest registered target until it unregisters", async () => {
		const audio = new Signal<Milli | undefined>(120 as Milli);
		const video = new Signal<Milli | undefined>(40 as Milli);
		const sync = new Sync();
		const unregisterAudio = sync.register(audio);
		sync.register(video);
		await flush();
		expect(sync.out.delay.peek()).toBe(120 as Milli);

		// A publisher flushing 250ms at once needs 250ms of buffer, whatever the round trip is.
		audio.set(270 as Milli);
		await flush();
		expect(sync.out.delay.peek()).toBe(270 as Milli);

		unregisterAudio();
		await flush();
		expect(sync.out.delay.peek()).toBe(40 as Milli);
		sync.close();
	});

	it("unregisters duplicate targets independently", async () => {
		const media = new Signal<Milli | undefined>(20 as Milli);
		const sync = new Sync();
		const unregisterFirst = sync.register(media);
		const unregisterSecond = sync.register(media);

		unregisterFirst();
		await flush();
		expect(sync.out.delay.peek()).toBe(20 as Milli);

		unregisterSecond();
		await flush();
		expect(sync.out.delay.peek()).toBe(0 as Milli);
		sync.close();
	});

	it("takes a fixed delay literally, ignoring the measured targets", async () => {
		const media = new Signal<Milli | undefined>(250 as Milli);
		const sync = new Sync({ delay: 100 as Milli });
		sync.register(media);
		await flush();
		expect(sync.out.delay.peek()).toBe(100 as Milli);
		sync.close();
	});
});

describe("wait", () => {
	// Sleeps run on a manual clock, so a parked wait only wakes on what the test changes.
	let clock: Manual;
	let installed: Installed;
	beforeEach(() => {
		clock = new Manual();
		clock.jump(Nano.fromMilli(Milli(1000)));
		installed = clock.install();
	});
	afterEach(() => installed.close());
	const nowMs = () => Nano.toMilli(Nano(now()));

	it("presents at once when constructed instant, before effects flush", async () => {
		const sync = new Sync({ delay: "instant" });
		sync.received(nowMs());
		expect(await sync.wait(Milli(nowMs() + 10_000))).toBe(true);
		sync.close();
	});

	it("drops an unanchored frame", async () => {
		const sync = new Sync({ delay: "instant" });
		expect(await sync.wait(nowMs())).toBe(false);
		sync.close();
	});

	it("presents a frame once its deadline comes due", async () => {
		const sync = new Sync({ delay: 100 as Milli });
		await flush();
		const timestamp = nowMs();
		sync.received(timestamp);

		const waiting = sync.wait(timestamp);
		await flush();
		expect(Bun.peek.status(waiting)).toBe("pending");

		await clock.advance(Nano.fromMilli(Milli(100)));
		expect(await waiting).toBe(true);
		sync.close();
	});

	it("wakes a sleeping wait to present when the delay switches to instant", async () => {
		const delay = new Signal<Delay>(10_000 as Milli);
		const sync = new Sync({ delay });
		await flush();
		sync.received(nowMs());

		const waiting = sync.wait(nowMs());
		await flush();
		expect(Bun.peek.status(waiting)).toBe("pending");

		delay.set("instant");
		expect(await waiting).toBe(true);
		sync.close();
	});

	it("drops a frame a skip passed over, before and after it parks", async () => {
		const sync = new Sync({ delay: 100 as Milli });
		await flush();
		const timestamp = nowMs();
		sync.received(timestamp);

		const parked = sync.wait(timestamp);
		await flush();
		expect(Bun.peek.status(parked)).toBe("pending");

		// A frame 10s newer, with no buffer, skips the playhead past the parked one.
		sync.received(Milli(timestamp + 10_000));
		expect(await parked).toBe(false);
		expect(await sync.wait(timestamp)).toBe(false);
		sync.close();
	});

	it("drops a sleeping wait at a reset, even when the next anchor restores the reference", async () => {
		const sync = new Sync({ delay: 10_000 as Milli });
		await flush();
		const timestamp = nowMs();
		sync.received(timestamp);

		const waiting = sync.wait(timestamp);
		await flush();

		// In one turn, so the reference never visibly changes.
		sync.reset();
		sync.received(timestamp);
		expect(await waiting).toBe(false);
		sync.close();
	});

	it("drops a sleeping wait when its signal aborts", async () => {
		const sync = new Sync({ delay: 10_000 as Milli });
		await flush();
		const timestamp = nowMs();
		sync.received(timestamp);

		const controller = new AbortController();
		const waiting = sync.wait(timestamp, controller.signal);
		await flush();
		controller.abort();
		expect(await waiting).toBe(false);
		sync.close();
	});

	it("drops a sleeping wait when closed", async () => {
		const sync = new Sync({ delay: 10_000 as Milli });
		await flush();
		const timestamp = nowMs();
		sync.received(timestamp);

		const waiting = sync.wait(timestamp);
		await flush();
		sync.close();
		expect(await waiting).toBe(false);
	});
});

describe("recorded ingress", () => {
	it("anchors and classifies using arrival rather than delayed delivery", async () => {
		const clock = new Manual();
		clock.jump(Nano.fromMilli(Milli(10_000)));
		const installed = clock.install();
		const sync = new Sync({ delay: Milli(100) });
		try {
			for (let i = 0; i < 20; i++) await Promise.resolve();
			expect(sync.received(Milli(0), Milli(1000))).toBe(false);
			expect(sync.out.reference.peek()).toBe(1000);
			expect(sync.received(Milli(20), Milli(1020))).toBe(false);
			expect(sync.received(Milli(40), Milli(1200))).toBe(true);
		} finally {
			installed.close();
			sync.close();
		}
	});
});

describe("skip floor", () => {
	// Arrivals are on the local clock, pinned here so a skip's playhead does not depend on wall time.
	let clock: Manual;
	let installed: Installed;
	beforeEach(() => {
		clock = new Manual();
		clock.jump(Nano.fromMilli(Milli(5000)));
		installed = clock.install();
	});
	afterEach(() => installed.close());

	it("a skip passes over only frames behind the playhead it jumped to", async () => {
		const sync = new Sync({ delay: Milli(100) });
		await flush();
		sync.received(Milli(1000), Milli(5000));
		expect(sync.presentable(Milli(0))).toBe(true);

		// A startup burst with no buffer: audio 50ms newer skips the playhead to 950ms.
		sync.received(Milli(1050), Milli(5000));
		expect(sync.presentable(Milli(1000))).toBe(true);
		expect(sync.presentable(Milli(949))).toBe(false);

		// A large skip passes over the earlier frame.
		sync.received(Milli(10_000), Milli(5000));
		expect(sync.presentable(Milli(1000))).toBe(false);
		expect(sync.presentable(Milli(9900))).toBe(true);
		sync.close();
	});

	it("a skip never lowers the floor and a reset clears it", async () => {
		const delay = new Signal<Delay>(Milli(100));
		const sync = new Sync({ delay });
		await flush();
		sync.received(Milli(0), Milli(5000));
		sync.received(Milli(10_000), Milli(5000));
		expect(sync.presentable(Milli(9000))).toBe(false);

		// A larger cap re-anchors to a lower playhead, but cannot revive what was passed over.
		delay.set(Milli(5000));
		await flush();
		sync.received(Milli(11_000), Milli(5000));
		expect(sync.presentable(Milli(9000))).toBe(false);

		// A new timeline may start its timestamps over, once it is anchored.
		sync.reset();
		expect(sync.presentable(Milli(9000))).toBe(false);
		sync.received(Milli(0), Milli(5000));
		expect(sync.presentable(Milli(9000))).toBe(true);
		sync.close();
	});

	it("a skip counts how long the skipping frame waited after ingress", async () => {
		const sync = new Sync({ delay: Milli(100) });
		await flush();
		sync.received(Milli(0), Milli(5000));

		// Reordering held this audio for 200ms, so the new reference puts the playhead at 10.1s now.
		clock.jump(Nano.fromMilli(Milli(200)));
		sync.received(Milli(10_000), Milli(5000));
		expect(sync.presentable(Milli(10_050))).toBe(false);
		expect(sync.presentable(Milli(10_100))).toBe(true);
		sync.close();
	});

	it("instant skips pass over nothing", async () => {
		const sync = new Sync({ delay: "instant" });
		await flush();
		sync.received(Milli(0), Milli(5000));
		sync.received(Milli(10_000), Milli(5000));
		expect(sync.presentable(Milli(0))).toBe(true);
		sync.close();
	});
});

describe("buffered startup", () => {
	for (const first of ["audio", "video"] as const) {
		it(`waits for both spans when ${first} fills first`, async () => {
			const clock = new Manual();
			const installed = clock.install();
			const sync = new Sync({ delay: Milli(100), buffer: Milli(1000) });
			const spans = {
				audio: new Signal<{ start: Milli; end: Milli } | null>(null),
				video: new Signal<{ start: Milli; end: Milli } | null>(null),
			};
			const dispose = Object.values(spans).map((span) => sync.register(new Signal(Milli(100)), span));
			try {
				await clock.advance(Nano.zero);
				sync.received(Milli(500));
				spans[first].set({ start: Milli(500), end: Milli(600) });
				await clock.advance(Nano.zero);
				let delivered = false;
				const waiting = sync.wait(Milli(500)).then((ok) => {
					delivered = ok;
				});
				await clock.advance(Nano.fromMilli(Milli(1000)));
				expect(delivered).toBe(false);
				expect(sync.now()).toBeUndefined();
				spans[first === "audio" ? "video" : "audio"].set({ start: Milli(500), end: Milli(599) });
				await clock.advance(Nano.zero);
				expect(delivered).toBe(false);
				spans[first === "audio" ? "video" : "audio"].set({ start: Milli(500), end: Milli(600) });
				await clock.advance(Nano.zero);
				await waiting;
				expect(delivered).toBe(true);
				expect(sync.now()).toBe(Milli(500));
			} finally {
				for (const fn of dispose) fn();
				sync.close();
				installed.close();
			}
		});
	}
	it("releases startup waits on abort, reset, and close without starting playback", async () => {
		const clock = new Manual();
		const installed = clock.install();
		const sync = new Sync({ delay: Milli(100), buffer: Milli(1000) });
		const dispose = sync.register(new Signal(Milli(100)), new Signal(null));
		try {
			await clock.advance(Nano.zero);
			sync.received(Milli(0));
			await clock.advance(Nano.zero);
			const abort = new AbortController();
			const cancelled = sync.wait(Milli(0), abort.signal);
			abort.abort();
			expect(await cancelled).toBe(false);
			const reset = sync.wait(Milli(0));
			sync.reset();
			sync.received(Milli(0));
			expect(await reset).toBe(false);
			const closed = sync.wait(Milli(0));
			sync.close();
			expect(await closed).toBe(false);
		} finally {
			dispose();
			sync.close();
			installed.close();
		}
	});
	it("removes a disabled track from the floor and leaves live pacing unchanged", async () => {
		const clock = new Manual();
		const installed = clock.install();
		const buffer = new Signal(Milli(1000));
		const sync = new Sync({ delay: Milli(100), buffer });
		const absent = new Signal<{ start: Milli; end: Milli } | null | undefined>(null);
		const remove = sync.register(new Signal(Milli(100)), absent);
		const video = sync.register(new Signal(Milli(100)), new Signal({ start: Milli(0), end: Milli(100) }));
		try {
			await clock.advance(Nano.zero);
			sync.received(Milli(0));
			await clock.advance(Nano.zero);
			expect(sync.playing.peek()).toBe(false);
			absent.set(undefined);
			await clock.advance(Nano.zero);
			expect(await sync.wait(Milli(0))).toBe(true);
			buffer.set(Milli.zero);
			sync.reset();
			absent.set(null);
			await clock.advance(Nano.zero);
			sync.received(Milli(0));
			const waiting = sync.wait(Milli(0));
			await clock.advance(Nano.fromMilli(Milli(100)));
			expect(await waiting).toBe(true);
		} finally {
			remove();
			video();
			sync.close();
			installed.close();
		}
	});
});

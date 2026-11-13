import { afterEach, beforeEach, describe, expect, it } from "bun:test";
import { Duration, type Installed, Instant, Manual, now, yieldNow } from "@moq/clock";

// A monotonic instant `ms` after the clock origin.
const at = (ms: number) => Instant.ZERO.saturatingAdd(Duration.millis(ms));

import { Signal } from "@moq/signals";
import { target } from "./audio/latency";
import { type Delay, Sync } from "./sync";

// Effects in @moq/signals flush on a microtask, so let pending updates drain before asserting.
const flush = () => yieldNow();

describe("delay and lookahead", () => {
	it("refuses the renamed buffer input", () => {
		expect(() => new Sync({ buffer: 1000 } as never)).toThrow("`buffer` was renamed to `lookahead`");
	});

	it("holds no lookahead by default", async () => {
		const sync = new Sync();
		await flush();
		expect(sync.out.buffered.peek()).toBe(false);
		sync.close();
	});

	it("caps maxDelay at the delay when no lookahead is configured", async () => {
		const sync = new Sync({ delay: Duration.millis(100) });
		await flush();
		expect(sync.out.buffered.peek()).toBe(false);
		expect(sync.out.delay.peek()).toEqual(Duration.millis(100));
		expect(sync.out.maxDelay.peek()).toEqual(Duration.millis(100));
		sync.close();
	});

	it("adds the lookahead on top of the delay", async () => {
		// The lookahead is measured from the live edge, so it does not swallow the delay: a frame may
		// sit `delay + lookahead` ahead of the playhead before playback skips forward.
		const sync = new Sync({ delay: Duration.millis(100), lookahead: Duration.millis(30_000) });
		await flush();
		expect(sync.out.buffered.peek()).toBe(true);
		expect(sync.out.maxDelay.peek()).toEqual(Duration.millis(30_100));
		sync.close();
	});

	it("stays unbuffered for a zero lookahead", async () => {
		const sync = new Sync({ delay: Duration.millis(200), lookahead: Duration.millis(0) });
		await flush();
		expect(sync.out.buffered.peek()).toBe(false);
		expect(sync.out.maxDelay.peek()).toEqual(Duration.millis(200));
		sync.close();
	});

	it("reacts to a lookahead set after construction", async () => {
		const lookahead = new Signal<Duration>(Duration.millis(0));
		const sync = new Sync({ delay: Duration.millis(100), lookahead });
		await flush();
		expect(sync.out.buffered.peek()).toBe(false);

		lookahead.set(Duration.millis(30_000));
		await flush();
		expect(sync.out.buffered.peek()).toBe(true);
		expect(sync.out.maxDelay.peek()).toEqual(Duration.millis(30_100));
		sync.close();
	});

	it("holds nothing when instant, whatever the lookahead says", async () => {
		const sync = new Sync({ delay: "instant", lookahead: Duration.millis(30_000) });
		await flush();
		expect(sync.out.instant.peek()).toBe(true);
		expect(sync.out.buffered.peek()).toBe(false);
		expect(sync.out.delay.peek()).toEqual(Duration.millis(0));
		expect(sync.out.maxDelay.peek()).toEqual(Duration.millis(0));
		sync.close();
	});
});

describe("auto delay", () => {
	it("holds nothing until a decoder registers", async () => {
		const sync = new Sync();
		await flush();
		expect(sync.out.delay.peek()).toEqual(Duration.millis(0));
		sync.close();
	});

	it("follows the deepest registered target until it unregisters", async () => {
		const audio = new Signal<Duration | undefined>(Duration.millis(120));
		const video = new Signal<Duration | undefined>(Duration.millis(40));
		const sync = new Sync();
		const unregisterAudio = sync.track({ target: audio });
		sync.track({ target: video });
		await flush();
		expect(sync.out.delay.peek()).toEqual(Duration.millis(120));

		// A publisher flushing 250ms at once needs 250ms of buffer, whatever the round trip is.
		audio.set(Duration.millis(270));
		await flush();
		expect(sync.out.delay.peek()).toEqual(Duration.millis(270));

		unregisterAudio.close();
		await flush();
		expect(sync.out.delay.peek()).toEqual(Duration.millis(40));
		sync.close();
	});

	it("unregisters duplicate targets independently", async () => {
		const media = new Signal<Duration | undefined>(Duration.millis(20));
		const sync = new Sync();
		const unregisterFirst = sync.track({ target: media });
		const unregisterSecond = sync.track({ target: media });

		unregisterFirst.close();
		await flush();
		expect(sync.out.delay.peek()).toEqual(Duration.millis(20));

		unregisterSecond.close();
		await flush();
		expect(sync.out.delay.peek()).toEqual(Duration.millis(0));
		sync.close();
	});

	it("caps an auto delay that a catalog advertises past the ceiling", async () => {
		// An hour of advertised jitter and catalog delay, through the audio target the decoder builds.
		const hour = Duration.secs(3600);
		const audio = new Signal<Duration | undefined>(
			target({ measured: Duration.millis(0), advertised: hour, frame: Duration.millis(20), delay: hour }),
		);
		const sync = new Sync();
		sync.track({ target: audio });
		await flush();
		expect(sync.out.delay.peek()).toEqual(Duration.secs(10));
		expect(sync.out.maxDelay.peek()).toEqual(Duration.secs(10));
		sync.close();
	});

	it("takes a fixed delay past the ceiling literally, since the viewer chose it", async () => {
		const sync = new Sync({ delay: Duration.secs(20) });
		await flush();
		expect(sync.out.delay.peek()).toEqual(Duration.secs(20));
		sync.close();
	});

	it("takes a fixed delay literally, ignoring the measured targets", async () => {
		const media = new Signal<Duration | undefined>(Duration.millis(250));
		const sync = new Sync({ delay: Duration.millis(100) });
		sync.track({ target: media });
		await flush();
		expect(sync.out.delay.peek()).toEqual(Duration.millis(100));
		sync.close();
	});
});

describe("wait", () => {
	// Sleeps run on a manual clock, so a parked wait only wakes on what the test changes.
	let clock: Manual;
	let installed: Installed;
	beforeEach(() => {
		clock = new Manual();
		clock.jump(Duration.millis(1000));
		installed = clock.install();
	});
	afterEach(() => installed.close());
	const nowMs = () => now().since(Instant.ZERO).asMillis();

	it("presents at once when constructed instant, before effects flush", async () => {
		const sync = new Sync({ delay: "instant" });
		const video = sync.track({ target: new Signal<Duration | undefined>(undefined) });
		video.received(Duration.millis(nowMs()));
		expect(await video.wait(Duration.millis(nowMs() + 10_000))).toBe(true);
		sync.close();
	});

	it("drops an unanchored frame", async () => {
		const sync = new Sync({ delay: "instant" });
		const video = sync.track({ target: new Signal<Duration | undefined>(undefined) });
		expect(await video.wait(Duration.millis(nowMs()))).toBe(false);
		sync.close();
	});

	it("presents a frame once its deadline comes due", async () => {
		const sync = new Sync({ delay: Duration.millis(100) });
		const video = sync.track({ target: new Signal<Duration | undefined>(undefined) });
		await flush();
		const timestamp = nowMs();
		video.received(Duration.millis(timestamp));

		const waiting = video.wait(Duration.millis(timestamp));
		await flush();
		expect(Bun.peek.status(waiting)).toBe("pending");

		await clock.advance(Duration.millis(100));
		expect(await waiting).toBe(true);
		sync.close();
	});

	it("wakes a sleeping wait to present when the delay switches to instant", async () => {
		const delay = new Signal<Delay>(Duration.millis(10_000));
		const sync = new Sync({ delay });
		const video = sync.track({ target: new Signal<Duration | undefined>(undefined) });
		await flush();
		video.received(Duration.millis(nowMs()));

		const waiting = video.wait(Duration.millis(nowMs()));
		await flush();
		expect(Bun.peek.status(waiting)).toBe("pending");

		delay.set("instant");
		expect(await waiting).toBe(true);
		sync.close();
	});

	it("drops a frame a skip passed over, before and after it parks", async () => {
		const sync = new Sync({ delay: Duration.millis(100) });
		const video = sync.track({ target: new Signal<Duration | undefined>(undefined) });
		await flush();
		const timestamp = nowMs();
		video.received(Duration.millis(timestamp));

		const parked = video.wait(Duration.millis(timestamp));
		await flush();
		expect(Bun.peek.status(parked)).toBe("pending");

		// A frame 10s newer, with no lookahead, skips the playhead past the parked one.
		video.received(Duration.millis(timestamp + 10_000));
		expect(await parked).toBe(false);
		expect(await video.wait(Duration.millis(timestamp))).toBe(false);
		sync.close();
	});

	it("drops a sleeping wait at a reset, even when the next anchor restores the reference", async () => {
		const sync = new Sync({ delay: Duration.millis(10_000) });
		const video = sync.track({ target: new Signal<Duration | undefined>(undefined) });
		await flush();
		const timestamp = nowMs();
		video.received(Duration.millis(timestamp));

		const waiting = video.wait(Duration.millis(timestamp));
		await flush();

		// In one turn, so the reference never visibly changes.
		sync.reset();
		video.received(Duration.millis(timestamp));
		expect(await waiting).toBe(false);
		sync.close();
	});

	it("drops a sleeping wait when its signal aborts", async () => {
		const sync = new Sync({ delay: Duration.millis(10_000) });
		const video = sync.track({ target: new Signal<Duration | undefined>(undefined) });
		await flush();
		const timestamp = nowMs();
		video.received(Duration.millis(timestamp));

		const controller = new AbortController();
		const waiting = video.wait(Duration.millis(timestamp), controller.signal);
		await flush();
		controller.abort();
		expect(await waiting).toBe(false);
		sync.close();
	});

	it("drops a sleeping wait when closed", async () => {
		const sync = new Sync({ delay: Duration.millis(10_000) });
		const video = sync.track({ target: new Signal<Duration | undefined>(undefined) });
		await flush();
		const timestamp = nowMs();
		video.received(Duration.millis(timestamp));

		const waiting = video.wait(Duration.millis(timestamp));
		await flush();
		sync.close();
		expect(await waiting).toBe(false);
	});
});

describe("recorded ingress", () => {
	it("anchors and classifies using arrival rather than delayed delivery", async () => {
		const clock = new Manual();
		clock.jump(Duration.millis(10_000));
		const installed = clock.install();
		const sync = new Sync({ delay: Duration.millis(100) });
		const video = sync.track({ target: new Signal<Duration | undefined>(undefined) });
		try {
			for (let i = 0; i < 20; i++) await Promise.resolve();
			expect(video.received(Duration.millis(0), at(1000))).toBe(false);
			expect(sync.out.reference.peek()).toBe(1000);
			expect(video.received(Duration.millis(20), at(1020))).toBe(false);
			expect(video.received(Duration.millis(40), at(1200))).toBe(true);
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
		clock.jump(Duration.millis(5000));
		installed = clock.install();
	});
	afterEach(() => installed.close());

	it("a skip passes over only frames behind the playhead it jumped to", async () => {
		const sync = new Sync({ delay: Duration.millis(100) });
		const video = sync.track({ target: new Signal<Duration | undefined>(undefined) });
		await flush();
		video.received(Duration.millis(1000), at(5000));
		expect(video.presentable(Duration.millis(0))).toBe(true);

		// A startup burst with no lookahead: audio 50ms newer skips the playhead to 950ms.
		video.received(Duration.millis(1050), at(5000));
		expect(video.presentable(Duration.millis(1000))).toBe(true);
		expect(video.presentable(Duration.millis(949))).toBe(false);

		// A large skip passes over the earlier frame.
		video.received(Duration.millis(10_000), at(5000));
		expect(video.presentable(Duration.millis(1000))).toBe(false);
		expect(video.presentable(Duration.millis(9900))).toBe(true);
		sync.close();
	});

	it("a skip never lowers the floor and a reset clears it", async () => {
		const delay = new Signal<Delay>(Duration.millis(100));
		const sync = new Sync({ delay });
		const video = sync.track({ target: new Signal<Duration | undefined>(undefined) });
		await flush();
		video.received(Duration.millis(0), at(5000));
		video.received(Duration.millis(10_000), at(5000));
		expect(video.presentable(Duration.millis(9000))).toBe(false);

		// A larger cap re-anchors to a lower playhead, but cannot revive what was passed over.
		delay.set(Duration.millis(5000));
		await flush();
		video.received(Duration.millis(11_000), at(5000));
		expect(video.presentable(Duration.millis(9000))).toBe(false);

		// A new timeline may start its timestamps over, once it is anchored.
		sync.reset();
		expect(video.presentable(Duration.millis(9000))).toBe(false);
		video.received(Duration.millis(0), at(5000));
		expect(video.presentable(Duration.millis(9000))).toBe(true);
		sync.close();
	});

	it("a skip counts how long the skipping frame waited after ingress", async () => {
		const sync = new Sync({ delay: Duration.millis(100) });
		const video = sync.track({ target: new Signal<Duration | undefined>(undefined) });
		await flush();
		video.received(Duration.millis(0), at(5000));

		// Reordering held this audio for 200ms, so the new reference puts the playhead at 10.1s now.
		clock.jump(Duration.millis(200));
		video.received(Duration.millis(10_000), at(5000));
		expect(video.presentable(Duration.millis(10_050))).toBe(false);
		expect(video.presentable(Duration.millis(10_100))).toBe(true);
		sync.close();
	});

	it("instant skips pass over nothing", async () => {
		const sync = new Sync({ delay: "instant" });
		const video = sync.track({ target: new Signal<Duration | undefined>(undefined) });
		await flush();
		video.received(Duration.millis(0), at(5000));
		video.received(Duration.millis(10_000), at(5000));
		expect(video.presentable(Duration.millis(0))).toBe(true);
		sync.close();
	});
});

describe("buffered startup", () => {
	for (const first of ["audio", "video"] as const) {
		it(`waits for both spans when ${first} fills first`, async () => {
			const clock = new Manual();
			const installed = clock.install();
			const sync = new Sync({ delay: Duration.millis(100), lookahead: Duration.millis(1000) });
			const spans = {
				audio: new Signal<{ start: Duration; end: Duration } | null>(null),
				video: new Signal<{ start: Duration; end: Duration } | null>(null),
			};
			const tracks = Object.values(spans).map((startup) =>
				sync.track({ target: new Signal(Duration.millis(100)), startup }),
			);
			const [video] = tracks;
			try {
				await clock.advance(Duration.ZERO);
				video.received(Duration.millis(500));
				spans[first].set({ start: Duration.millis(500), end: Duration.millis(600) });
				await clock.advance(Duration.ZERO);
				let delivered = false;
				const waiting = video.wait(Duration.millis(500)).then((ok) => {
					delivered = ok;
				});
				await clock.advance(Duration.millis(1000));
				expect(delivered).toBe(false);
				expect(sync.now()).toBeUndefined();
				spans[first === "audio" ? "video" : "audio"].set({
					start: Duration.millis(500),
					end: Duration.millis(599),
				});
				await clock.advance(Duration.ZERO);
				expect(delivered).toBe(false);
				spans[first === "audio" ? "video" : "audio"].set({
					start: Duration.millis(500),
					end: Duration.millis(600),
				});
				await clock.advance(Duration.ZERO);
				await waiting;
				expect(delivered).toBe(true);
				expect(sync.now()).toEqual(Duration.millis(500));
			} finally {
				for (const track of tracks) track.close();
				sync.close();
				installed.close();
			}
		});
	}
	// Audio's output sounds its opening a while after it renders, so that wait counts toward the
	// delay, and the opening, even at media zero, comes due as it first sounds instead of being skipped.
	it("holds the opening's turn back by the time its output takes to sound it", async () => {
		const clock = new Manual();
		const installed = clock.install();
		const sync = new Sync({ delay: Duration.millis(300), lookahead: Duration.millis(1000) });
		const span = new Signal<{ start: Duration; end: Duration; lead?: Duration } | null>(null);
		const audio = sync.track({ target: new Signal(Duration.millis(300)), startup: span });
		try {
			await clock.advance(Duration.ZERO);
			audio.received(Duration.ZERO);
			span.set({ start: Duration.ZERO, end: Duration.millis(170), lead: Duration.millis(120) });
			await clock.advance(Duration.ZERO);
			expect(sync.now()).toBeUndefined();
			span.set({ start: Duration.ZERO, end: Duration.millis(180), lead: Duration.millis(120) });
			await clock.advance(Duration.ZERO);
			// Short of media zero, which the clock reads as zero until the opening's turn.
			expect(sync.now()).toEqual(Duration.ZERO);
			await clock.advance(Duration.millis(150));
			expect(sync.now()).toEqual(Duration.millis(30));
		} finally {
			audio.close();
			sync.close();
			installed.close();
		}
	});

	it("releases startup waits on abort, reset, and close without starting playback", async () => {
		const clock = new Manual();
		const installed = clock.install();
		const sync = new Sync({ delay: Duration.millis(100), lookahead: Duration.millis(1000) });
		const video = sync.track({ target: new Signal(Duration.millis(100)), startup: new Signal(null) });
		try {
			await clock.advance(Duration.ZERO);
			video.received(Duration.millis(0));
			await clock.advance(Duration.ZERO);
			const abort = new AbortController();
			const cancelled = video.wait(Duration.millis(0), abort.signal);
			abort.abort();
			expect(await cancelled).toBe(false);
			const reset = video.wait(Duration.millis(0));
			sync.reset();
			video.received(Duration.millis(0));
			expect(await reset).toBe(false);
			const closed = video.wait(Duration.millis(0));
			sync.close();
			expect(await closed).toBe(false);
		} finally {
			video.close();
			sync.close();
			installed.close();
		}
	});
	// A video track's startup span opens at the first picture of its timeline, which a live stream
	// played long ago. Turning lookahead on mid-stream must not anchor playback back there.
	it("keeps playing when lookahead turns on mid-stream", async () => {
		const clock = new Manual();
		const installed = clock.install();
		const lookahead = new Signal(Duration.ZERO);
		const sync = new Sync({ delay: Duration.millis(100), lookahead });
		const span = new Signal<{ start: Duration; end: Duration } | null>({
			start: Duration.millis(0),
			end: Duration.millis(200),
		});
		const video = sync.track({ target: new Signal(Duration.millis(100)), startup: span });
		try {
			await clock.advance(Duration.ZERO);
			video.received(Duration.millis(0));
			await clock.advance(Duration.millis(10_000));
			video.received(Duration.millis(10_000));
			expect(sync.now()).toEqual(Duration.millis(9900));

			lookahead.set(Duration.millis(1000));
			await clock.advance(Duration.ZERO);
			expect(sync.playing.peek()).toBe(true);
			expect(sync.now()).toEqual(Duration.millis(9900));
		} finally {
			video.close();
			sync.close();
			installed.close();
		}
	});
	it("removes a disabled track from the floor and leaves live pacing unchanged", async () => {
		const clock = new Manual();
		const installed = clock.install();
		const lookahead = new Signal(Duration.millis(1000));
		const sync = new Sync({ delay: Duration.millis(100), lookahead });
		const absent = new Signal<{ start: Duration; end: Duration } | null | undefined>(null);
		const remove = sync.track({ target: new Signal(Duration.millis(100)), startup: absent });
		const video = sync.track({
			target: new Signal(Duration.millis(100)),
			startup: new Signal({ start: Duration.millis(0), end: Duration.millis(100) }),
		});
		try {
			await clock.advance(Duration.ZERO);
			video.received(Duration.millis(0));
			await clock.advance(Duration.ZERO);
			expect(sync.playing.peek()).toBe(false);
			absent.set(undefined);
			await clock.advance(Duration.ZERO);
			expect(await video.wait(Duration.millis(0))).toBe(true);
			lookahead.set(Duration.ZERO);
			sync.reset();
			absent.set(null);
			await clock.advance(Duration.ZERO);
			video.received(Duration.millis(0));
			const waiting = video.wait(Duration.millis(0));
			await clock.advance(Duration.millis(100));
			expect(await waiting).toBe(true);
		} finally {
			remove.close();
			video.close();
			sync.close();
			installed.close();
		}
	});
});

describe("tracks", () => {
	// Local time is pinned, so every playhead and deadline below is exact.
	let clock: Manual;
	let installed: Installed;
	beforeEach(() => {
		clock = new Manual();
		clock.jump(Duration.millis(5000));
		installed = clock.install();
	});
	afterEach(() => installed.close());

	const target = () => new Signal<Duration | undefined>(undefined);

	it("a video-only discontinuity leaves audio's timeline alone", async () => {
		const sync = new Sync({ delay: Duration.millis(100) });
		const audio = sync.track({ target: target() });
		const video = sync.track({ target: target() });
		await flush();
		// Video arrived earliest and anchors the clock.
		audio.received(Duration.millis(1000), at(5000));
		video.received(Duration.millis(1010), at(5000));
		expect(sync.now()).toEqual(Duration.millis(910));

		// Video's parked frame belongs to the timeline that ended; audio's arrivals take the reference
		// and its parked frame stays on its own timeline.
		const parked = video.wait(Duration.millis(950));
		const playing = audio.wait(Duration.millis(950));
		video.reset();
		expect(await parked).toBe(false);
		expect(sync.now()).toEqual(Duration.millis(900));
		await clock.advance(Duration.millis(50));
		expect(await playing).toBe(true);

		// An audio break likewise leaves video's parked frames alone.
		video.received(Duration.millis(1100), at(5050));
		const picture = video.wait(Duration.millis(1040));
		audio.reset();
		await clock.advance(Duration.millis(50));
		expect(await picture).toBe(true);
		sync.close();
	});

	it("releases the reference of the earliest track when it leaves", async () => {
		const sync = new Sync({ delay: Duration.millis(100) });
		const audio = sync.track({ target: target() });
		const video = sync.track({ target: target() });
		await flush();
		// Video trails audio by 300ms, so audio anchors the clock and video is late against it.
		audio.received(Duration.millis(1000), at(5000));
		expect(video.received(Duration.millis(1000), at(5300))).toBe(true);

		// Once audio stops arriving, video plays against its own arrivals instead.
		audio.reset();
		expect(sync.out.reference.peek()).toBe(4300);
		expect(video.received(Duration.millis(1033), at(5333))).toBe(false);

		// Closing the last track leaves nothing anchored.
		video.close();
		expect(sync.now()).toBeUndefined();
		sync.close();
	});

	it("forgets a departed track's skip once the reference moves back to the survivor's", async () => {
		const sync = new Sync({ delay: Duration.millis(100) });
		const audio = sync.track({ target: target() });
		const video = sync.track({ target: target() });
		await flush();
		// Audio's burst skips the playhead to 1900; video's own arrivals put it at 900.
		video.received(Duration.millis(1000), at(5000));
		audio.received(Duration.millis(2000), at(5000));
		expect(video.presentable(Duration.millis(1100))).toBe(false);

		audio.reset();
		expect(sync.now()).toEqual(Duration.millis(900));
		const waiting = video.wait(Duration.millis(1100));
		await clock.advance(Duration.millis(200));
		expect(await waiting).toBe(true);
		sync.close();
	});

	it("keeps the survivor's own skip when the reference moves back to it", async () => {
		const sync = new Sync({ delay: Duration.millis(100) });
		const audio = sync.track({ target: target() });
		const video = sync.track({ target: target() });
		await flush();
		// Video skips its own playhead from 900 to 1400, then audio's burst skips it to 2900.
		video.received(Duration.millis(1000), at(5000));
		video.received(Duration.millis(1500), at(5000));
		audio.received(Duration.millis(3000), at(5000));

		audio.reset();
		expect(sync.now()).toEqual(Duration.millis(1400));
		expect(video.presentable(Duration.millis(1300))).toBe(false);
		expect(video.presentable(Duration.millis(1450))).toBe(true);
		sync.close();
	});

	it("keeps the playhead where buffered startup put it when the owner leaves", async () => {
		const sync = new Sync({ delay: Duration.millis(100), lookahead: Duration.millis(1000) });
		const audioSpan = new Signal<{ start: Duration; end: Duration } | null>(null);
		const videoSpan = new Signal<{ start: Duration; end: Duration } | null>(null);
		const audio = sync.track({ target: target(), startup: audioSpan });
		const video = sync.track({ target: target(), startup: videoSpan });
		await flush();
		audio.received(Duration.millis(500), at(5000));
		video.received(Duration.millis(500), at(5000));

		// Startup releases a second later, anchoring at the retained opening rather than its arrival.
		await clock.advance(Duration.millis(1000));
		audioSpan.set({ start: Duration.millis(500), end: Duration.millis(700) });
		videoSpan.set({ start: Duration.millis(500), end: Duration.millis(700) });
		await flush();
		expect(sync.now()).toEqual(Duration.millis(500));

		audio.reset();
		expect(sync.now()).toEqual(Duration.millis(500));
		sync.close();
	});

	it("drops every track's parked frames at a reset", async () => {
		const sync = new Sync({ delay: Duration.millis(100) });
		const audio = sync.track({ target: target() });
		const video = sync.track({ target: target() });
		await flush();
		audio.received(Duration.millis(1000), at(5000));
		const sound = audio.wait(Duration.millis(1500));
		const picture = video.wait(Duration.millis(1500));
		sync.reset();
		expect(await sound).toBe(false);
		expect(await picture).toBe(false);
		expect(sync.now()).toBeUndefined();

		// The next frame anchors a new timeline, ignoring the anchors before the reset.
		video.received(Duration.millis(3000), at(5000));
		expect(sync.now()).toEqual(Duration.millis(2900));
		sync.close();
	});

	it("reads zero until media time zero's turn", async () => {
		const sync = new Sync({ delay: Duration.millis(100) });
		const video = sync.track({ target: target() });
		await flush();
		video.received(Duration.millis(0), at(5000));
		expect(sync.now()).toEqual(Duration.ZERO);
		await clock.advance(Duration.millis(110));
		expect(sync.now()).toEqual(Duration.millis(10));
		sync.close();
	});
});

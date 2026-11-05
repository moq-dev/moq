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
		const unregisterAudio = sync.track("audio", { target: audio });
		sync.track("video", { target: video });
		await flush();
		expect(sync.out.delay.peek()).toBe(120 as Milli);

		// A publisher flushing 250ms at once needs 250ms of buffer, whatever the round trip is.
		audio.set(270 as Milli);
		await flush();
		expect(sync.out.delay.peek()).toBe(270 as Milli);

		unregisterAudio.close();
		await flush();
		expect(sync.out.delay.peek()).toBe(40 as Milli);
		sync.close();
	});

	it("unregisters duplicate targets independently", async () => {
		const media = new Signal<Milli | undefined>(20 as Milli);
		const sync = new Sync();
		const unregisterFirst = sync.track("video", { target: media });
		const unregisterSecond = sync.track("video", { target: media });

		unregisterFirst.close();
		await flush();
		expect(sync.out.delay.peek()).toBe(20 as Milli);

		unregisterSecond.close();
		await flush();
		expect(sync.out.delay.peek()).toBe(0 as Milli);
		sync.close();
	});

	it("takes a fixed delay literally, ignoring the measured targets", async () => {
		const media = new Signal<Milli | undefined>(250 as Milli);
		const sync = new Sync({ delay: 100 as Milli });
		sync.track("video", { target: media });
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
		const video = sync.track("video", { target: new Signal<Milli | undefined>(undefined) });
		video.received(nowMs());
		expect(await video.wait(Milli(nowMs() + 10_000))).toBe(true);
		sync.close();
	});

	it("drops an unanchored frame", async () => {
		const sync = new Sync({ delay: "instant" });
		const video = sync.track("video", { target: new Signal<Milli | undefined>(undefined) });
		expect(await video.wait(nowMs())).toBe(false);
		sync.close();
	});

	it("presents a frame once its deadline comes due", async () => {
		const sync = new Sync({ delay: 100 as Milli });
		const video = sync.track("video", { target: new Signal<Milli | undefined>(undefined) });
		await flush();
		const timestamp = nowMs();
		video.received(timestamp);

		const waiting = video.wait(timestamp);
		await flush();
		expect(Bun.peek.status(waiting)).toBe("pending");

		await clock.advance(Nano.fromMilli(Milli(100)));
		expect(await waiting).toBe(true);
		sync.close();
	});

	it("wakes a sleeping wait to present when the delay switches to instant", async () => {
		const delay = new Signal<Delay>(10_000 as Milli);
		const sync = new Sync({ delay });
		const video = sync.track("video", { target: new Signal<Milli | undefined>(undefined) });
		await flush();
		video.received(nowMs());

		const waiting = video.wait(nowMs());
		await flush();
		expect(Bun.peek.status(waiting)).toBe("pending");

		delay.set("instant");
		expect(await waiting).toBe(true);
		sync.close();
	});

	it("drops a frame a skip passed over, before and after it parks", async () => {
		const sync = new Sync({ delay: 100 as Milli });
		const video = sync.track("video", { target: new Signal<Milli | undefined>(undefined) });
		await flush();
		const timestamp = nowMs();
		video.received(timestamp);

		const parked = video.wait(timestamp);
		await flush();
		expect(Bun.peek.status(parked)).toBe("pending");

		// A frame 10s newer, with no buffer, skips the playhead past the parked one.
		video.received(Milli(timestamp + 10_000));
		expect(await parked).toBe(false);
		expect(await video.wait(timestamp)).toBe(false);
		sync.close();
	});

	it("drops a sleeping wait at a reset, even when the next anchor restores the reference", async () => {
		const sync = new Sync({ delay: 10_000 as Milli });
		const video = sync.track("video", { target: new Signal<Milli | undefined>(undefined) });
		await flush();
		const timestamp = nowMs();
		video.received(timestamp);

		const waiting = video.wait(timestamp);
		await flush();

		// In one turn, so the reference never visibly changes.
		sync.reset();
		video.received(timestamp);
		expect(await waiting).toBe(false);
		sync.close();
	});

	it("drops a sleeping wait when its signal aborts", async () => {
		const sync = new Sync({ delay: 10_000 as Milli });
		const video = sync.track("video", { target: new Signal<Milli | undefined>(undefined) });
		await flush();
		const timestamp = nowMs();
		video.received(timestamp);

		const controller = new AbortController();
		const waiting = video.wait(timestamp, controller.signal);
		await flush();
		controller.abort();
		expect(await waiting).toBe(false);
		sync.close();
	});

	it("drops a sleeping wait when closed", async () => {
		const sync = new Sync({ delay: 10_000 as Milli });
		const video = sync.track("video", { target: new Signal<Milli | undefined>(undefined) });
		await flush();
		const timestamp = nowMs();
		video.received(timestamp);

		const waiting = video.wait(timestamp);
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
		const video = sync.track("video", { target: new Signal<Milli | undefined>(undefined) });
		try {
			for (let i = 0; i < 20; i++) await Promise.resolve();
			expect(video.received(Milli(0), Milli(1000))).toBe(false);
			expect(sync.out.reference.peek()).toBe(1000);
			expect(video.received(Milli(20), Milli(1020))).toBe(false);
			expect(video.received(Milli(40), Milli(1200))).toBe(true);
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
		const video = sync.track("video", { target: new Signal<Milli | undefined>(undefined) });
		await flush();
		video.received(Milli(1000), Milli(5000));
		expect(video.presentable(Milli(0))).toBe(true);

		// A startup burst with no buffer: audio 50ms newer skips the playhead to 950ms.
		video.received(Milli(1050), Milli(5000));
		expect(video.presentable(Milli(1000))).toBe(true);
		expect(video.presentable(Milli(949))).toBe(false);

		// A large skip passes over the earlier frame.
		video.received(Milli(10_000), Milli(5000));
		expect(video.presentable(Milli(1000))).toBe(false);
		expect(video.presentable(Milli(9900))).toBe(true);
		sync.close();
	});

	it("a skip never lowers the floor and a reset clears it", async () => {
		const delay = new Signal<Delay>(Milli(100));
		const sync = new Sync({ delay });
		const video = sync.track("video", { target: new Signal<Milli | undefined>(undefined) });
		await flush();
		video.received(Milli(0), Milli(5000));
		video.received(Milli(10_000), Milli(5000));
		expect(video.presentable(Milli(9000))).toBe(false);

		// A larger cap re-anchors to a lower playhead, but cannot revive what was passed over.
		delay.set(Milli(5000));
		await flush();
		video.received(Milli(11_000), Milli(5000));
		expect(video.presentable(Milli(9000))).toBe(false);

		// A new timeline may start its timestamps over, once it is anchored.
		sync.reset();
		expect(video.presentable(Milli(9000))).toBe(false);
		video.received(Milli(0), Milli(5000));
		expect(video.presentable(Milli(9000))).toBe(true);
		sync.close();
	});

	it("a skip counts how long the skipping frame waited after ingress", async () => {
		const sync = new Sync({ delay: Milli(100) });
		const video = sync.track("video", { target: new Signal<Milli | undefined>(undefined) });
		await flush();
		video.received(Milli(0), Milli(5000));

		// Reordering held this audio for 200ms, so the new reference puts the playhead at 10.1s now.
		clock.jump(Nano.fromMilli(Milli(200)));
		video.received(Milli(10_000), Milli(5000));
		expect(video.presentable(Milli(10_050))).toBe(false);
		expect(video.presentable(Milli(10_100))).toBe(true);
		sync.close();
	});

	it("instant skips pass over nothing", async () => {
		const sync = new Sync({ delay: "instant" });
		const video = sync.track("video", { target: new Signal<Milli | undefined>(undefined) });
		await flush();
		video.received(Milli(0), Milli(5000));
		video.received(Milli(10_000), Milli(5000));
		expect(video.presentable(Milli(0))).toBe(true);
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
			const tracks = Object.values(spans).map((startup) =>
				sync.track("video", { target: new Signal(Milli(100)), startup }),
			);
			const [video] = tracks;
			try {
				await clock.advance(Nano.zero);
				video.received(Milli(500));
				spans[first].set({ start: Milli(500), end: Milli(600) });
				await clock.advance(Nano.zero);
				let delivered = false;
				const waiting = video.wait(Milli(500)).then((ok) => {
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
				for (const track of tracks) track.close();
				sync.close();
				installed.close();
			}
		});
	}
	it("releases startup waits on abort, reset, and close without starting playback", async () => {
		const clock = new Manual();
		const installed = clock.install();
		const sync = new Sync({ delay: Milli(100), buffer: Milli(1000) });
		const video = sync.track("video", { target: new Signal(Milli(100)), startup: new Signal(null) });
		try {
			await clock.advance(Nano.zero);
			video.received(Milli(0));
			await clock.advance(Nano.zero);
			const abort = new AbortController();
			const cancelled = video.wait(Milli(0), abort.signal);
			abort.abort();
			expect(await cancelled).toBe(false);
			const reset = video.wait(Milli(0));
			sync.reset();
			video.received(Milli(0));
			expect(await reset).toBe(false);
			const closed = video.wait(Milli(0));
			sync.close();
			expect(await closed).toBe(false);
		} finally {
			video.close();
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
		const remove = sync.track("audio", { target: new Signal(Milli(100)), startup: absent });
		const video = sync.track("video", {
			target: new Signal(Milli(100)),
			startup: new Signal({ start: Milli(0), end: Milli(100) }),
		});
		try {
			await clock.advance(Nano.zero);
			video.received(Milli(0));
			await clock.advance(Nano.zero);
			expect(sync.playing.peek()).toBe(false);
			absent.set(undefined);
			await clock.advance(Nano.zero);
			expect(await video.wait(Milli(0))).toBe(true);
			buffer.set(Milli.zero);
			sync.reset();
			absent.set(null);
			await clock.advance(Nano.zero);
			video.received(Milli(0));
			const waiting = video.wait(Milli(0));
			await clock.advance(Nano.fromMilli(Milli(100)));
			expect(await waiting).toBe(true);
		} finally {
			remove.close();
			video.close();
			sync.close();
			installed.close();
		}
	});
});

describe("audio clock", () => {
	// Local time is pinned, so every playhead and deadline below is exact.
	let clock: Manual;
	let installed: Installed;
	beforeEach(() => {
		clock = new Manual();
		clock.jump(Nano.fromMilli(Milli(5000)));
		installed = clock.install();
	});
	afterEach(() => installed.close());

	const target = () => new Signal<Milli | undefined>(undefined);
	const local = () => Nano.toMilli(Nano(now()));

	it("follows the audio playhead while it rolls, not the earliest arrival", async () => {
		const sync = new Sync({ delay: Milli(100) });
		const audio = sync.track("audio", { target: target() });
		const video = sync.track("video", { target: target() });
		await flush();
		video.received(Milli(1000), Milli(5000));
		expect(sync.now()).toBe(Milli(900));

		// The ring re-buffered and plays 40ms behind where the wall clock would be.
		audio.playhead({ rolling: true, timestamp: Milli(860), wall: Milli(5000) });
		expect(sync.now()).toBe(Milli(860));
		await clock.advance(Nano.fromMilli(Milli(10)));
		expect(sync.now()).toBe(Milli(870));

		// Video waits for the audio it accompanies, however early it arrived.
		let presented: boolean | undefined;
		void video.wait(Milli(900)).then((ok) => {
			presented = ok;
		});
		await clock.advance(Nano.fromMilli(Milli(29)));
		expect(presented).toBeUndefined();
		await clock.advance(Nano.fromMilli(Milli(1)));
		expect(presented).toBe(true);

		// An earlier video arrival no longer moves the playhead audio owns.
		video.received(Milli(5000), local());
		expect(sync.now()).toBe(Milli(900));
		sync.close();
	});

	it("holds the playhead through a re-stall and resumes with it", async () => {
		const sync = new Sync({ delay: Milli(100) });
		const audio = sync.track("audio", { target: target() });
		const video = sync.track("video", { target: target() });
		await flush();
		audio.playhead({ rolling: true, timestamp: Milli(1000), wall: local() });
		audio.playhead({ rolling: false, timestamp: Milli(1010) });
		await clock.advance(Nano.fromMilli(Milli(500)));
		expect(sync.now()).toBe(Milli(1010));

		// A frame already due presents; the next waits for audio to resume.
		expect(await video.wait(Milli(1010))).toBe(true);
		let presented: boolean | undefined;
		void video.wait(Milli(1020)).then((ok) => {
			presented = ok;
		});
		await clock.advance(Nano.fromMilli(Milli(500)));
		expect(presented).toBeUndefined();
		audio.playhead({ rolling: true, timestamp: Milli(1015), wall: local() });
		await clock.advance(Nano.fromMilli(Milli(5)));
		expect(presented).toBe(true);
		sync.close();
	});

	it("holds a re-stall where the output stops, not where the ring did", async () => {
		const sync = new Sync({ delay: Milli(100) });
		const audio = sync.track("audio", { target: target() });
		await flush();
		// 30ms of output latency: the ring has read to 1030 while the output plays 1000.
		audio.playhead({ rolling: true, timestamp: Milli(1000), wall: local() });
		audio.playhead({ rolling: false, timestamp: Milli(1030) });
		expect(sync.now()).toBe(Milli(1000));
		await clock.advance(Nano.fromMilli(Milli(20)));
		expect(sync.now()).toBe(Milli(1020));
		await clock.advance(Nano.fromMilli(Milli(100)));
		expect(sync.now()).toBe(Milli(1030));

		// Refilled, the output plays the latency's silence first: the clock never steps back.
		audio.playhead({ rolling: true, timestamp: Milli(1000), wall: local() });
		expect(sync.now()).toBe(Milli(1030));
		await clock.advance(Nano.fromMilli(Milli(40)));
		expect(sync.now()).toBe(Milli(1040));
		audio.playhead({ rolling: true, timestamp: Milli(1040), wall: local() });
		await clock.advance(Nano.fromMilli(Milli(10)));
		expect(sync.now()).toBe(Milli(1050));
		sync.close();
	});

	it("forgets the wall clock's skips once audio takes over", async () => {
		const sync = new Sync({ delay: Milli(100) });
		const audio = sync.track("audio", { target: target() });
		const video = sync.track("video", { target: target() });
		await flush();
		// A burst skips the wall-clock playhead to 900, passing over everything before it.
		video.received(Milli(0), local());
		video.received(Milli(1000), local());
		expect(video.presentable(Milli(100))).toBe(false);

		// Audio plays from 0, so a frame at 100 is still ahead of it.
		audio.playhead({ rolling: true, timestamp: Milli(0), wall: local() });
		expect(video.presentable(Milli(100))).toBe(true);
		const waiting = video.wait(Milli(100));
		await clock.advance(Nano.fromMilli(Milli(100)));
		expect(await waiting).toBe(true);
		sync.close();
	});

	it("falls back to the wall clock where audio left it when audio stops", async () => {
		for (const stalled of [false, true]) {
			const sync = new Sync({ delay: Milli(100) });
			const audio = sync.track("audio", { target: target() });
			const video = sync.track("video", { target: target() });
			await flush();
			video.received(Milli(1000), Milli(5000));
			audio.playhead({ rolling: true, timestamp: Milli(800), wall: local() });
			if (stalled) audio.playhead({ rolling: false, timestamp: Milli(800) });
			await clock.advance(Nano.fromMilli(Milli(50)));
			const before = sync.now();

			// Muted: no jump back to video's arrival anchor, and time runs on.
			audio.playhead(undefined);
			expect(sync.now()).toBe(before);
			await clock.advance(Nano.fromMilli(Milli(10)));
			expect(sync.now()).toBe(Milli((before ?? 0) + 10));

			// Video's own arrivals hold the clock now, so ending its timeline re-anchors it.
			video.reset();
			expect(sync.now()).toBeUndefined();
			video.received(Milli(3000), local());
			expect(sync.now()).toBe(Milli(2900));
			sync.close();
		}
	});

	it("a buffered ring that drains hands the clock back instead of holding video", async () => {
		const sync = new Sync({ delay: Milli(100), buffer: Milli(1000) });
		const audio = sync.track("audio", { target: target() });
		await flush();
		audio.playhead({ rolling: true, timestamp: Milli(1000), wall: local() });
		audio.playhead({ rolling: false, timestamp: Milli(1000) });
		await clock.advance(Nano.fromMilli(Milli(20)));
		expect(sync.now()).toBe(Milli(1020));
		sync.close();
	});

	it("a video-only discontinuity leaves audio's timeline alone", async () => {
		const sync = new Sync({ delay: Milli(100) });
		const audio = sync.track("audio", { target: target() });
		const video = sync.track("video", { target: target() });
		await flush();
		audio.received(Milli(1000), Milli(5000));
		video.received(Milli(1000), Milli(5000));
		audio.playhead({ rolling: true, timestamp: Milli(900), wall: local() });

		// Video's parked frame belongs to the timeline that ended; audio keeps playing on its own.
		const parked = video.wait(Milli(950));
		video.reset();
		expect(await parked).toBe(false);
		expect(sync.now()).toBe(Milli(900));
		await clock.advance(Nano.fromMilli(Milli(10)));
		expect(sync.now()).toBe(Milli(910));

		// The same holds before audio rolls: audio's arrivals keep the reference.
		const idle = new Sync({ delay: Milli(100) });
		const quiet = idle.track("audio", { target: target() });
		const picture = idle.track("video", { target: target() });
		await flush();
		quiet.received(Milli(1000), Milli(5000));
		picture.received(Milli(990), Milli(5000));
		const anchored = idle.out.reference.peek();
		picture.reset();
		expect(idle.out.reference.peek()).toBe(anchored);
		sync.close();
		idle.close();
	});

	it("releases the reference of the earliest track when it leaves", async () => {
		const sync = new Sync({ delay: Milli(100) });
		const audio = sync.track("audio", { target: target() });
		const video = sync.track("video", { target: target() });
		await flush();
		// Video trails audio by 300ms, so audio anchors the clock and video is late against it.
		audio.received(Milli(1000), Milli(5000));
		expect(video.received(Milli(1000), Milli(5300))).toBe(true);

		// Once audio stops arriving, video plays against its own arrivals instead.
		audio.reset();
		expect(sync.out.reference.peek()).toBe(4300);
		expect(video.received(Milli(1033), Milli(5333))).toBe(false);

		// Closing the last track leaves nothing anchored.
		video.close();
		expect(sync.now()).toBeUndefined();
		sync.close();
	});

	it("keeps the playhead still when the delay moves under it", async () => {
		const delay = new Signal<Delay>(Milli(100));
		const sync = new Sync({ delay });
		const audio = sync.track("audio", { target: target() });
		await flush();
		audio.playhead({ rolling: true, timestamp: Milli(1000), wall: local() });
		delay.set(Milli(300));
		await flush();
		expect(sync.now()).toBe(Milli(1000));
		sync.close();
	});

	it("drops parked frames and stops following audio at a reset", async () => {
		const sync = new Sync({ delay: Milli(100) });
		const audio = sync.track("audio", { target: target() });
		const video = sync.track("video", { target: target() });
		await flush();
		audio.playhead({ rolling: true, timestamp: Milli(1000), wall: local() });
		const parked = video.wait(Milli(1500));
		sync.reset();
		expect(await parked).toBe(false);
		expect(sync.now()).toBeUndefined();

		// A stall reported after the reset is not a re-stall of the old timeline.
		audio.playhead({ rolling: false, timestamp: Milli(1000) });
		video.received(Milli(3000), local());
		expect(sync.now()).toBe(Milli(2900));
		sync.close();
	});

	it("reads zero until media time zero's turn, from either source", async () => {
		const sync = new Sync({ delay: Milli(100) });
		const audio = sync.track("audio", { target: target() });
		const video = sync.track("video", { target: target() });
		await flush();
		video.received(Milli(0), local());
		expect(sync.now()).toBe(Milli.zero);
		// Output latency still covers the first sample.
		audio.playhead({ rolling: true, timestamp: -30, wall: local() });
		expect(sync.now()).toBe(Milli.zero);
		await clock.advance(Nano.fromMilli(Milli(40)));
		expect(sync.now()).toBe(Milli(10));
		sync.close();
	});

	it("allows one audio track at a time", () => {
		const sync = new Sync();
		const audio = sync.track("audio", { target: target() });
		expect(() => sync.track("audio", { target: target() })).toThrow();
		audio.close();
		sync.track("audio", { target: target() }).close();
		sync.close();
	});
});

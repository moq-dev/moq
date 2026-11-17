import { expect, it } from "bun:test";
import { Duration } from "@moq/clock";
import { Manual } from "@moq/clock/test";
import { Signal } from "@moq/signals";
import { Sync } from "../sync";
import { playable } from "./playhead";

it("reads the clock the ring renders at, less the stretch it plays late audio within", async () => {
	const clock = new Manual();
	const installed = clock.install();
	const sync = new Sync({ delay: Duration.millis(300), stretch: Duration.millis(200) });
	const span = new Signal<{ start: Duration; end: Duration } | null>(null);
	const audio = sync.track({ target: new Signal(Duration.millis(300)), startup: span });
	const output = new Signal(Duration.millis(40));
	const playhead = playable(sync, output);
	try {
		await clock.advance(Duration.ZERO);
		audio.received(Duration.ZERO);
		span.set({ start: Duration.ZERO, end: Duration.millis(300) });
		await clock.advance(Duration.millis(1000));
		const now = sync.now();
		if (now === undefined) throw new Error("the clock never started");
		expect(playhead.now()).toEqual(now.add(Duration.millis(40)).saturatingSub(Duration.millis(200)));

		// The trail moving is a jump, which wakes a consumer waiting on a hole.
		let jumped = false;
		const dispose = playhead.anchor.subscribe(() => {
			jumped = true;
		});
		await clock.advance(Duration.ZERO);
		jumped = false;
		output.set(Duration.millis(60));
		await clock.advance(Duration.ZERO);
		expect(jumped).toBe(true);
		expect(playhead.now()).toEqual(now.add(Duration.millis(60)).saturatingSub(Duration.millis(200)));
		dispose();
	} finally {
		audio.close();
		sync.close();
		installed.close();
	}
});

it("applies the output latency before media zero's turn, when the ring already renders it", async () => {
	const clock = new Manual();
	const installed = clock.install();
	const sync = new Sync({ delay: Duration.millis(300), lookahead: Duration.millis(1000), stretch: Duration.ZERO });
	const span = new Signal<{ start: Duration; end: Duration; lead?: Duration } | null>(null);
	const audio = sync.track({ target: new Signal(Duration.millis(300)), startup: span });
	const playhead = playable(sync, new Signal(Duration.millis(120)));
	try {
		await clock.advance(Duration.ZERO);
		audio.received(Duration.ZERO);
		span.set({ start: Duration.ZERO, end: Duration.millis(180), lead: Duration.millis(120) });
		await clock.advance(Duration.millis(50));
		// 70 ms short of media zero's turn, which the clock reads as zero, the ring renders 50 ms, and
		// plays audio trailing it by its 30 ms window even with the stretch off.
		expect(sync.now()).toEqual(Duration.ZERO);
		expect(sync.position()).toBe(-70);
		expect(playhead.now()).toEqual(Duration.millis(20));
	} finally {
		audio.close();
		sync.close();
		installed.close();
	}
});

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

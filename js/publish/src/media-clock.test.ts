import { expect, test } from "bun:test";
import { Duration, Instant } from "@moq/clock";
import { Manual } from "@moq/clock/test";
import * as MediaClock from "./media-clock.ts";

test("under a manual clock, media timestamps are its instants", () => {
	const clock = new Manual();
	const installed = clock.install();
	try {
		clock.jump(Duration.secs(5));
		expect(MediaClock.now()).toEqual(Duration.secs(5));

		// A wall step moves a manual clock's origin with it, so its instants still stamp as they are.
		clock.stepWall(clock.wall().add(Duration.secs(3600)));
		clock.jump(Duration.secs(1));
		expect(MediaClock.now()).toEqual(Duration.secs(6));

		// A capture maps an instant by its distance from now, either way.
		const at = clock.now();
		clock.jump(Duration.secs(2));
		expect(MediaClock.capture(at)).toEqual(Duration.secs(6));
		expect(MediaClock.capture(at.saturatingAdd(Duration.secs(5)))).toEqual(Duration.secs(11));
		expect(MediaClock.capture(Instant.ZERO)).toEqual(Duration.ZERO);
	} finally {
		installed.close();
	}
});

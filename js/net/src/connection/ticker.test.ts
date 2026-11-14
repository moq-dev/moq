import { expect, test } from "bun:test";
import { Duration } from "@moq/clock";
import { Manual } from "@moq/clock/test";
import { tick } from "./ticker.ts";

test("subscribers share one tick and the loop stops with the last", async () => {
	const clock = new Manual();
	const installed = clock.install();
	try {
		const period = Duration.millis(100);
		const ticks: string[] = [];
		const a = tick(period, () => ticks.push("a"));
		await clock.advance(Duration.millis(50));
		// Joins the running loop rather than starting its own out of phase.
		const b = tick(period, () => ticks.push("b"));
		await clock.advance(Duration.millis(50));
		expect(ticks).toEqual(["a", "b"]);

		a();
		await clock.advance(period);
		expect(ticks).toEqual(["a", "b", "b"]);

		b();
		b();
		await clock.advance(Duration.millis(500));
		expect(ticks).toEqual(["a", "b", "b"]);

		// A stopped loop restarts for a later subscriber, rather than leaving the page unsampled.
		const c = tick(period, () => ticks.push("c"));
		await clock.advance(period);
		expect(ticks).toEqual(["a", "b", "b", "c"]);
		// Stop the page-wide loop, so a later test file's subscriber starts its own on that file's clock.
		c();
	} finally {
		installed.close();
	}
});

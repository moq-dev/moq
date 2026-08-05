import { expect, test } from "bun:test";
import { Backoff } from "./retry.ts";

test("the window escalates to the cap, each delay inside its jitter band", () => {
	const backoff = new Backoff({ initial: 1000, multiplier: 2, max: 8000, timeout: 0 });

	for (const window of [1000, 2000, 4000, 8000, 8000, 8000]) {
		const delay = backoff.delay();
		expect(delay).toBeGreaterThanOrEqual(window / 2);
		expect(delay).toBeLessThanOrEqual(window);
	}
});

test("jitter separates identical schedules", () => {
	const props = { initial: 1000, multiplier: 2, max: 8000, timeout: 0 };
	const a = new Backoff(props);
	const b = new Backoff(props);

	// One shared draw could collide by chance; a run of them colliding means no jitter at all.
	const differs = Array.from({ length: 8 }).some(() => a.delay() !== b.delay());
	expect(differs).toBe(true);
});

test("reset returns to the initial window", () => {
	const backoff = new Backoff({ initial: 1000, multiplier: 2, max: 8000, timeout: 0 });
	for (let i = 0; i < 4; i++) backoff.delay();

	backoff.reset();
	expect(backoff.delay()).toBeLessThanOrEqual(1000);
});

test("a zero timeout never gives up", () => {
	const backoff = new Backoff({ initial: 1, multiplier: 2, max: 8, timeout: 0 });
	for (let i = 0; i < 64; i++) expect(backoff.delay()).toBeDefined();
});

test("a delay never outlives the budget", () => {
	// An initial delay longer than the whole budget must not sleep past it: the budget is the
	// promise, and one oversized window would blow through it before a single retry lands.
	const backoff = new Backoff({ initial: 60000, multiplier: 2, max: 60000, timeout: 50 });

	const delay = backoff.delay();
	expect(delay).toBeDefined();
	expect(delay).toBeLessThanOrEqual(50);
});

test("the budget is a deadline over the whole sequence", async () => {
	const backoff = new Backoff({ initial: 1, multiplier: 2, max: 8, timeout: 5 });

	// The first delay starts the clock; outliving the budget is what stops the sequence. Sleeping
	// past it rather than shrinking the timeout keeps the test off `performance.now()`'s resolution.
	expect(backoff.delay()).toBeDefined();
	await Bun.sleep(15);
	expect(backoff.delay()).toBeUndefined();

	// A reset says the earlier failures no longer describe reality, so the budget refills.
	backoff.reset();
	expect(backoff.delay()).toBeDefined();
});

import { expect, test } from "bun:test";
import { Backoff, isRetryable, Terminal } from "./retry.ts";

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

test("the budget is a deadline over the whole sequence", () => {
	// Already expired by the time the second call reads the clock.
	const backoff = new Backoff({ initial: 1, multiplier: 2, max: 8, timeout: 0.0001 });

	expect(backoff.delay()).toBeDefined();
	expect(backoff.delay()).toBeUndefined();

	// A reset says the earlier failures no longer describe reality, so the budget refills.
	backoff.reset();
	expect(backoff.delay()).toBeDefined();
});

test("only a Terminal failure stops the retry", () => {
	expect(isRetryable(new Terminal("unsupported WebTransport protocol: moq-99"))).toBe(false);

	// The browser hands back untyped failures, and those are overwhelmingly the network.
	expect(isRetryable(new Error("connection lost"))).toBe(true);
	expect(isRetryable(new DOMException("closed", "AbortError"))).toBe(true);

	// A lost transport race: worth repeating if any half of it was.
	expect(isRetryable(new AggregateError([new Terminal("no WebSocket"), new Error("timed out")]))).toBe(true);
	expect(isRetryable(new AggregateError([new Terminal("no WebSocket"), new Terminal("no WebTransport")]))).toBe(
		false,
	);
});

import { afterEach, expect, test } from "bun:test";
import { setTimeout as hostTimeout } from "node:timers/promises";
import {
	Elapsed,
	Instant,
	interval,
	Manual,
	Micro,
	Milli,
	MissedTickBehavior,
	Nano,
	now,
	origin,
	Second,
	sleep,
	sleepUntil,
	timeout,
	Wall,
	wall,
} from "./index.ts";

const restores: (() => void)[] = [];
afterEach(() => {
	for (const restore of restores.splice(0).reverse()) restore();
});
function manual(): Manual {
	const clock = new Manual();
	restores.push(clock.install());
	return clock;
}
const ns = Nano.fromMilli;

test("units convert without permitting instant or wall time as durations", () => {
	expect<number>(Nano.fromSecond(Second(1))).toBe(1_000_000_000);
	expect<number>(Micro.fromMilli(Milli(2))).toBe(2000);
	expect<number>(Milli.fromNano(Nano(1_500_000))).toBe(1.5);
	expect<number>(Second.fromMicro(Micro(2_000_000))).toBe(2);
	expect(() => Nano(-1)).toThrow(RangeError);
	expect(() => Milli(Number.POSITIVE_INFINITY)).toThrow(RangeError);
	expect(() => Instant(Number.NaN)).toThrow(RangeError);
	if (false as boolean) {
		// @ts-expect-error instants are not durations
		void sleep(Instant.zero);
		// @ts-expect-error wall time is not monotonic time
		void sleepUntil(Wall.UNIX_EPOCH);
		// @ts-expect-error milliseconds must be converted to nanoseconds
		void sleep(Milli(1));
	}
});

test("manual reads and wall corrections share a clock without inventing elapsed time", () => {
	const clock = manual();
	expect<number>(now()).toBe(0);
	expect<number>(wall()).toBe(Manual.WALL_START);
	clock.advance(ns(Milli(5)));
	expect<number>(now()).toBe(5_000_000);
	expect<number>(wall()).toBe(Manual.WALL_START + 5);
	clock.stepWall(Wall(Manual.WALL_START - 1000));
	expect<number>(now()).toBe(5_000_000);
	clock.stepWall(Wall(Manual.WALL_START + 1000));
	clock.advance(ns(Milli(1)));
	expect<number>(clock.now()).toBe(6_000_000);
	expect<number>(clock.wall()).toBe(Manual.WALL_START + 1001);
	expect<number>(origin()).toBe(Manual.WALL_START + 1000 - 5);
});

test("many small advances move wall time by their combined duration", () => {
	const clock = new Manual();
	for (let i = 0; i < 10_000; i++) clock.advance(Nano(100));
	expect<number>(clock.now()).toBe(1_000_000);
	expect<number>(clock.wall()).toBe(Manual.WALL_START + 1);
});

test("installation restores nested and out-of-order scopes without reviving them", () => {
	const outer = manual();
	outer.advance(Nano(4));
	const inner = manual();
	inner.advance(Nano(8));
	expect<number>(now()).toBe(8);
	restores[0]();
	expect<number>(now()).toBe(8);
	restores[1]();
	const restored = now();
	restores[1]();
	expect<number>(now()).toBeGreaterThanOrEqual(restored);
	expect<number>(now()).not.toBe(4);
});

test("deadlines fire in time then arming order and reached deadlines fire without advance", async () => {
	const clock = manual();
	const order: number[] = [];
	void sleepUntil(Instant(10)).then(() => order.push(2));
	void sleepUntil(Instant(5)).then(() => order.push(1));
	void sleepUntil(Instant(10)).then(() => order.push(3));
	await clock.advanceAsync(Nano(10));
	expect(order).toEqual([1, 2, 3]);
	await sleepUntil(Instant(1));
	await sleep(Nano.zero);
});

test("advanceAsync drains arbitrarily deep microtasks and newly due timers", async () => {
	const clock = manual();
	let done = false;
	void (async () => {
		await sleep(Nano(5));
		for (let i = 0; i < 1000; i++) await Promise.resolve();
		await sleep(Nano.zero);
		done = true;
	})();
	await clock.advanceAsync(Nano(5));
	expect(done).toBe(true);
});

test("a sleep stays on its creation clock and abort releases it", async () => {
	const first = manual();
	let done = false;
	const pending = sleep(Nano(10)).then(() => {
		done = true;
	});
	const second = manual();
	await second.advanceAsync(Nano(10));
	expect(done).toBe(false);
	await first.advanceAsync(Nano(10));
	await pending;
	const abort = new AbortController();
	const cancelled = sleep(Nano(100), { signal: abort.signal }).catch((error: unknown) => error);
	abort.abort();
	expect(await cancelled).toHaveProperty("name", "AbortError");
	await second.advanceAsync(Nano(100));
	await expect(sleep(Nano.zero, { signal: abort.signal })).rejects.toHaveProperty("name", "AbortError");
});

test("timeouts prefer ready work, propagate rejection, and expire on the manual deadline", async () => {
	const clock = manual();
	expect(await timeout(Nano.zero, Promise.resolve(7))).toBe(7);
	const failure = new Error("operation failed");
	await expect(timeout(Nano(10), Promise.reject(failure))).rejects.toBe(failure);
	const pending = timeout(Nano(10), new Promise<never>(() => {})).catch((error: unknown) => error);
	await clock.advanceAsync(Nano(9));
	await clock.advanceAsync(Nano(1));
	expect(await pending).toBeInstanceOf(Elapsed);
});

test("interval burst delivers every missed scheduled tick", async () => {
	const clock = manual();
	const ticks = interval(Nano(10));
	try {
		expect<number>(await ticks.tick()).toBe(0);
		clock.advance(Nano(35));
		expect<number>(await ticks.tick()).toBe(10);
		expect<number>(await ticks.tick()).toBe(20);
		expect<number>(await ticks.tick()).toBe(30);
		const next = ticks.tick();
		await clock.advanceAsync(Nano(5));
		expect<number>(await next).toBe(40);
		expect<number>(ticks.period()).toBe(10);
	} finally {
		ticks.close();
	}
});

for (const behavior of [MissedTickBehavior.Delay, MissedTickBehavior.Skip]) {
	test(`interval ${behavior} resumes after a missed tick`, async () => {
		const clock = manual();
		const ticks = interval(Nano(10));
		ticks.setMissedTickBehavior(behavior);
		try {
			expect<number>(await ticks.tick()).toBe(0);
			clock.advance(Nano(35));
			expect<number>(await ticks.tick()).toBe(10);
			const next = ticks.tick();
			await clock.advanceAsync(Nano(behavior === MissedTickBehavior.Delay ? 10 : 5));
			expect<number>(await next).toBe(behavior === MissedTickBehavior.Delay ? 45 : 40);
		} finally {
			ticks.close();
		}
	});
}

test("an interval cancels pending ticks, refuses concurrent ticks and stays closed", async () => {
	manual();
	expect(() => interval(Nano.zero)).toThrow(RangeError);
	const ticks = interval(Nano(10));
	await ticks.tick();
	const pending = ticks.tick().catch((error: unknown) => error);
	await expect(ticks.tick()).rejects.toThrow("already pending");
	ticks.close();
	ticks.close();
	expect(await pending).toHaveProperty("name", "AbortError");
	await expect(ticks.tick()).rejects.toHaveProperty("name", "AbortError");
});

test("the host clock counts from the performance time origin", () => {
	const host = performance;
	const before = host.now();
	const at = Nano.toMilli(Instant.durationSince(now(), Instant.zero));
	expect(at).toBeGreaterThanOrEqual(before);
	expect(at).toBeLessThanOrEqual(host.now());
	expect<number>(origin()).toBe(host.timeOrigin);
});

test("an unreferenced sleep still completes on the host", async () => {
	await sleep(Nano.fromMilli(Milli(1)), { ref: false });
});

test("manual installation leaves the host's own timers real", async () => {
	manual();
	await hostTimeout(0);
	expect<number>(now()).toBe(0);
});

test("many cancelled and live timers retain deadline and arming order", async () => {
	const clock = manual();
	const fired: number[] = [];
	const expected: { at: number; index: number }[] = [];
	const aborts: AbortController[] = [];
	const pending = Array.from({ length: 257 }, (_, index) => {
		const abort = new AbortController();
		aborts.push(abort);
		const at = 1 + ((index * 73) % 17);
		if (index % 3 !== 0) expected.push({ at, index });
		return sleepUntil(Instant(at), { signal: abort.signal }).then(
			() => {
				fired.push(index);
			},
			() => {},
		);
	});
	for (let index = aborts.length - 1; index >= 0; index--) if (index % 3 === 0) aborts[index].abort();
	await clock.advanceAsync(Nano(17));
	await Promise.all(pending);
	expect(fired).toEqual(expected.sort((a, b) => a.at - b.at || a.index - b.index).map(({ index }) => index));
});

test("fire advances exactly to the earliest deadline and runs only what is due", async () => {
	const clock = manual();
	const order: number[] = [];
	void sleepUntil(Instant(30)).then(() => order.push(30));
	void sleepUntil(Instant(10)).then(() => order.push(1));
	void sleepUntil(Instant(20)).then(() => order.push(20));
	void sleepUntil(Instant(10)).then(() => order.push(2));
	clock.advance(Nano(3));
	expect<number>(await clock.fire()).toBe(10);
	expect<number>(now()).toBe(10);
	expect(order).toEqual([1, 2]);
	expect<number>(await clock.fire()).toBe(20);
	expect<number>(await clock.fire()).toBe(30);
	expect(order).toEqual([1, 2, 20, 30]);
});

test("fire picks an earlier timer that a pending microtask chain arms", async () => {
	const clock = manual();
	void sleepUntil(Instant(100));
	void Promise.resolve().then(() => sleepUntil(Instant(10)));
	expect<number>(await clock.fire()).toBe(10);
});

test("fire waits, without a timer, until a later host task arms one", async () => {
	const clock = manual();
	const abort = new AbortController();
	void sleep(Nano(5), { signal: abort.signal }).catch(() => {});
	abort.abort();
	let fired: number | undefined;
	const pending = clock.fire().then((at) => {
		fired = at;
	});
	await hostTimeout(0);
	await hostTimeout(0);
	expect(fired).toBeUndefined();
	await expect(clock.fire()).rejects.toThrow("already pending");
	void sleep(Nano(7));
	await pending;
	expect(fired).toBe(7);
});

test("fire resolves when a later host task arms an already-due timer", async () => {
	const clock = manual();
	clock.advance(Nano(4));
	let woke = false;
	const pending = clock.fire();
	await hostTimeout(0);
	void sleep(Nano(0)).then(() => {
		woke = true;
	});
	expect<number>(await pending).toBe(4);
	expect(woke).toBe(true);
});

test("fire stops at an already-due timer a pending microtask chain arms", async () => {
	const clock = manual();
	void sleepUntil(Instant(100));
	void Promise.resolve().then(() => sleep(Nano(0)));
	expect<number>(await clock.fire()).toBe(0);
});

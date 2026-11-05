import { afterEach, expect, test } from "bun:test";
import { setTimeout as hostTimeout } from "node:timers/promises";
import {
	Elapsed,
	type Installed,
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
	yieldNow,
} from "./index.ts";

const installs: Installed[] = [];
afterEach(() => {
	for (const installed of installs.splice(0).reverse()) installed.close();
});
function manual(): Manual {
	const clock = new Manual();
	installs.push(clock.install());
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
	clock.jump(ns(Milli(5)));
	expect<number>(now()).toBe(5_000_000);
	expect<number>(wall()).toBe(Manual.WALL_START + 5);
	clock.stepWall(Wall(Manual.WALL_START - 1000));
	expect<number>(now()).toBe(5_000_000);
	clock.stepWall(Wall(Manual.WALL_START + 1000));
	clock.jump(ns(Milli(1)));
	expect<number>(clock.now()).toBe(6_000_000);
	expect<number>(clock.wall()).toBe(Manual.WALL_START + 1001);
	expect<number>(origin()).toBe(Manual.WALL_START + 1000 - 5);
});

test("many small advances move wall time by their combined duration", () => {
	const clock = new Manual();
	for (let i = 0; i < 10_000; i++) clock.jump(Nano(100));
	expect<number>(clock.now()).toBe(1_000_000);
	expect<number>(clock.wall()).toBe(Manual.WALL_START + 1);
});

test("installation restores nested and out-of-order scopes without reviving them", () => {
	const outer = manual();
	outer.jump(Nano(4));
	const inner = manual();
	inner.jump(Nano(8));
	expect<number>(now()).toBe(8);
	installs[0].close();
	expect<number>(now()).toBe(8);
	installs[1].close();
	const restored = now();
	installs[1].close();
	expect<number>(now()).toBeGreaterThanOrEqual(restored);
	expect<number>(now()).not.toBe(4);
});

test("deadlines fire in time then arming order and reached deadlines fire without advance", async () => {
	const clock = manual();
	const order: number[] = [];
	void sleepUntil(Instant(10)).then(() => order.push(2));
	void sleepUntil(Instant(5)).then(() => order.push(1));
	void sleepUntil(Instant(10)).then(() => order.push(3));
	await clock.advance(Nano(10));
	expect(order).toEqual([1, 2, 3]);
	await sleepUntil(Instant(1));
	await sleep(Nano.zero);
});

test("advance drains arbitrarily deep microtasks and newly due timers", async () => {
	const clock = manual();
	let done = false;
	void (async () => {
		await sleep(Nano(5));
		for (let i = 0; i < 1000; i++) await Promise.resolve();
		await sleep(Nano.zero);
		done = true;
	})();
	await clock.advance(Nano(5));
	expect(done).toBe(true);
});

for (const driver of ["advance", "fire"] as const) {
	test(`${driver} waits out a chain of host-task yields before moving time`, async () => {
		const clock = manual();
		void sleep(Nano(10));
		const seen: number[] = [];
		void (async () => {
			for (let i = 0; i < 3; i++) await yieldNow();
			seen.push(now());
		})();
		if (driver === "advance") await clock.advance(Nano(10));
		else expect<number>(await clock.fire()).toBe(10);
		expect(seen).toEqual([0]);
	});
}

test("a sleep stays on its creation clock and abort releases it", async () => {
	const first = manual();
	let done = false;
	const pending = sleep(Nano(10)).then(() => {
		done = true;
	});
	const second = manual();
	await second.advance(Nano(10));
	expect(done).toBe(false);
	await first.advance(Nano(10));
	await pending;
	const abort = new AbortController();
	const cancelled = sleep(Nano(100), { signal: abort.signal }).catch((error: unknown) => error);
	abort.abort();
	expect(await cancelled).toHaveProperty("name", "AbortError");
	await second.advance(Nano(100));
	await expect(sleep(Nano.zero, { signal: abort.signal })).rejects.toHaveProperty("name", "AbortError");
});

test("timeouts prefer ready work, propagate rejection, and expire on the manual deadline", async () => {
	const clock = manual();
	expect(await timeout(Nano.zero, Promise.resolve(7))).toBe(7);
	const failure = new Error("operation failed");
	await expect(timeout(Nano(10), Promise.reject(failure))).rejects.toBe(failure);
	const pending = timeout(Nano(10), new Promise<never>(() => {})).catch((error: unknown) => error);
	await clock.advance(Nano(9));
	await clock.advance(Nano(1));
	expect(await pending).toBeInstanceOf(Elapsed);
});

test("interval burst delivers every missed scheduled tick", async () => {
	const clock = manual();
	const ticks = interval(Nano(10));
	try {
		expect<number>(await ticks.tick()).toBe(0);
		clock.jump(Nano(35));
		expect<number>(await ticks.tick()).toBe(10);
		expect<number>(await ticks.tick()).toBe(20);
		expect<number>(await ticks.tick()).toBe(30);
		const next = ticks.tick();
		await clock.advance(Nano(5));
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
			clock.jump(Nano(35));
			expect<number>(await ticks.tick()).toBe(10);
			const next = ticks.tick();
			await clock.advance(Nano(behavior === MissedTickBehavior.Delay ? 10 : 5));
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
	// biome-ignore lint/style/noRestrictedGlobals: checks the host backend against the global it wraps.
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
	await clock.advance(Nano(17));
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
	clock.jump(Nano(3));
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
	clock.jump(Nano(4));
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

test("advance steps through deadlines armed by earlier timers and stops at its target", async () => {
	const clock = manual();
	const at: number[] = [];
	void (async () => {
		for (let i = 0; i < 4; i++) {
			await sleep(Nano(10));
			at.push(now());
		}
	})();
	await clock.advance(Nano(30));
	expect(at).toEqual([10, 20, 30]);
	expect<number>(now()).toBe(30);
	expect<number>(await clock.fire()).toBe(40);
	expect(at).toEqual([10, 20, 30, 40]);
});

test("advance drains one timer's tasks before firing the next at the same deadline", async () => {
	const clock = manual();
	const abort = new AbortController();
	const order: string[] = [];
	void sleep(Nano(10)).then(() => {
		order.push("first");
		abort.abort();
	});
	void sleep(Nano(10), { signal: abort.signal }).then(
		() => order.push("second"),
		() => order.push("cancelled"),
	);
	await clock.advance(Nano(10));
	expect(order).toEqual(["first", "cancelled"]);
});

test("a timer armed while draining joins existing timers at the same deadline", async () => {
	const clock = manual();
	const order: string[] = [];
	void sleep(Nano(10)).then(() => {
		order.push("first");
		void sleepUntil(Instant(10)).then(() => order.push("new"));
	});
	void sleep(Nano(10)).then(() => order.push("second"));
	await clock.advance(Nano(10));
	expect(order).toEqual(["first", "second", "new"]);
});

test("a past deadline armed while draining observes the current instant", async () => {
	const clock = manual();
	const at: number[] = [];
	void sleep(Nano(10)).then(async () => {
		at.push(now());
		await sleepUntil(Instant(3));
		at.push(now());
	});
	await clock.advance(Nano(10));
	expect(at).toEqual([10, 10]);
	expect<number>(clock.now()).toBe(10);
});

test("drive fires each deadline its work waits on and stops when the work settles", async () => {
	const clock = manual();
	let late = false;
	void sleepUntil(Instant(100)).then(() => {
		late = true;
	});
	const work = (async () => {
		await sleep(Nano(10));
		await sleep(Nano(20));
		return now();
	})();
	expect<number>(await clock.drive(work)).toBe(30);
	expect<number>(clock.now()).toBe(30);
	await hostTimeout(0);
	expect(late).toBe(false);
});

test("drive settles with its work's rejection at the deadline that caused it", async () => {
	const clock = manual();
	await expect(
		clock.drive(
			sleep(Nano(10)).then(() => {
				throw new Error("boom");
			}),
		),
	).rejects.toThrow("boom");
	expect<number>(clock.now()).toBe(10);
});

for (const settles of ["before any timer", "on a later host task"] as const) {
	test(`drive leaves no driver behind when its work settles ${settles}`, async () => {
		const clock = manual();
		const work = settles === "before any timer" ? Promise.resolve(1) : hostTimeout(0).then(() => 1);
		expect(await clock.drive(work)).toBe(1);
		const fired: number[] = [];
		void sleep(ns(Milli(60_000))).then(() => fired.push(now()));
		void sleep(ns(Milli(70_000))).then(() => fired.push(now()));
		await hostTimeout(0);
		expect(fired).toEqual([]);
		expect<number>(clock.now()).toBe(0);
		expect<number>(await clock.fire()).toBe(ns(Milli(60_000)));
		expect(fired).toEqual([ns(Milli(60_000))]);
	});
}

for (const driver of ["advance", "fire", "drive"] as const) {
	test(`${driver} rejects overlapping time drivers without releasing its ownership`, async () => {
		const clock = manual();
		let release = () => {};
		const work = new Promise<void>((resolve) => {
			release = resolve;
		});
		const pending =
			driver === "advance" ? clock.advance(Nano(10)) : driver === "fire" ? clock.fire() : clock.drive(work);
		try {
			await expect(clock.advance(Nano(100))).rejects.toThrow("already pending");
			expect(() => clock.jump(Nano(100))).toThrow("already pending");
			await expect(clock.fire()).rejects.toThrow("already pending");
			await expect(clock.drive(Promise.resolve())).rejects.toThrow("already pending");
			expect<number>(clock.now()).toBe(0);
		} finally {
			// Let fire and drive finish even if an assertion fails before they have a timer.
			void sleepUntil(Instant(10)).then(release);
			await pending;
		}
		expect<number>(clock.now()).toBe(10);
		clock.jump(Nano(1));
		await clock.advance(Nano(1));
		void sleep(Nano(1));
		expect<number>(await clock.fire()).toBe(13);
	});
}

test("a timer callback cannot move time beyond the active advance target", async () => {
	const clock = manual();
	const at: number[] = [];
	const callback = sleep(Nano(5)).then(async () => {
		await expect(clock.advance(Nano(100))).rejects.toThrow("already pending");
		expect(() => clock.jump(Nano(100))).toThrow("already pending");
		await expect(clock.fire()).rejects.toThrow("already pending");
		await sleepUntil(Instant(7));
		at.push(now());
	});
	await clock.advance(Nano(10));
	await callback;
	expect(at).toEqual([7]);
	expect<number>(clock.now()).toBe(10);
});

test("fire returns the first deadline reached when a host task arms an earlier timer", async () => {
	const clock = manual();
	const at: number[] = [];
	void sleepUntil(Instant(10)).then(() => at.push(now()));
	const pending = clock.fire();
	const channel = new MessageChannel();
	try {
		channel.port1.onmessage = () => {
			channel.port1.close();
			channel.port2.close();
			void sleepUntil(Instant(5)).then(() => at.push(now()));
		};
		channel.port2.postMessage(null);
		const fired = await pending;
		// A host may run the task before selection or during the drain after firing.
		// Either way fire must stop where it first observed a timer becoming ready.
		expect<number>(fired).toBe(at[0]);
		expect(at.every((value) => value === fired)).toBe(true);
		if (fired < 10) {
			expect<number>(await clock.fire()).toBe(10);
			expect(at).toEqual([5, 10]);
		}
	} finally {
		channel.port1.close();
		channel.port2.close();
	}
});

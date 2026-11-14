import { afterEach, expect, test } from "bun:test";
// biome-ignore lint/style/noRestrictedImports: a host task that bypasses the installed Manual clock.
import { setTimeout as hostTimeout } from "node:timers/promises";
import {
	Duration,
	Elapsed,
	type Installed,
	Instant,
	interval,
	MissedTickBehavior,
	now,
	origin,
	sleep,
	sleepUntil,
	timeout,
	timeoutSignal,
	Wall,
	wall,
	yieldNow,
} from "./index.ts";
import { hostTime, Manual } from "./test.ts";

const installs: Installed[] = [];
afterEach(() => {
	for (const installed of installs.splice(0).reverse()) installed.close();
});
function manual(): Manual {
	const clock = new Manual();
	installs.push(clock.install());
	return clock;
}
// Read an instant or wall time as a plain number for assertions.
const ns = (at: Instant): number => at.since(Instant.ZERO).asNanos();
const ms = (at: Wall): number => at.asUnixMillis();
const instant = (nanos: number): Instant => Instant.ZERO.saturatingAdd(Duration.nanos(nanos));
const unix = (millis: number): Wall => Wall.unixMillis(millis);
const START = ms(Manual.WALL_START);

test("durations convert units and refuse to become numbers", () => {
	expect(Duration.secs(1).asNanos()).toBe(1_000_000_000);
	expect(Duration.millis(2).asMicros()).toBe(2000);
	expect(Duration.nanos(1_500_000).asMillis()).toBe(1.5);
	expect(Duration.micros(2_000_000).asSecs()).toBe(2);
	expect(() => Duration.nanos(-1)).toThrow(RangeError);
	expect(() => Duration.millis(Number.POSITIVE_INFINITY)).toThrow(RangeError);
	expect(() => instant(Number.NaN)).toThrow(RangeError);
	expect(() => Duration.millis(1).sub(Duration.millis(2))).toThrow(RangeError);
	expect(Duration.millis(1).saturatingSub(Duration.millis(2)).isZero()).toBe(true);
	expect(instant(5).since(instant(9)).isZero()).toBe(true);
	expect(`${Duration.micros(1500)}`).toBe("1.5ms");
	const loose: unknown = Duration.millis(1);
	expect(() => (loose as number) + 1).toThrow(TypeError);
	expect(() => (loose as number) < 2).toThrow(TypeError);
	expect(() => JSON.stringify({ loose })).toThrow(TypeError);
	expect(() => (instant(1) as unknown as number) * 2).toThrow(TypeError);
	expect(() => JSON.stringify(Wall.UNIX_EPOCH)).toThrow(TypeError);
	if (false as boolean) {
		// @ts-expect-error instants are not durations
		void sleep(Instant.ZERO);
		// @ts-expect-error wall time is not monotonic time
		void sleepUntil(Wall.UNIX_EPOCH);
		// @ts-expect-error a duration is not a number
		void sleep(5);
		// @ts-expect-error a duration does not convert to a number
		void (Duration.millis(1) as number);
	}
});

test("moving past either end of a range is checked or saturates", () => {
	// Past the longest duration, a nanosecond count is no longer finite.
	const longest = Duration.secs(Number.MAX_VALUE);
	expect(Instant.ZERO.checkedSub(Duration.nanos(1))).toBeUndefined();
	expect(instant(5).saturatingSub(Duration.nanos(9))).toEqual(Instant.ZERO);
	expect(instant(5).checkedSub(Duration.nanos(5))).toEqual(Instant.ZERO);
	expect(instant(5).checkedAdd(Duration.nanos(4))).toEqual(instant(9));
	expect(Instant.ZERO.checkedAdd(longest)).toBeUndefined();
	const last = Instant.ZERO.saturatingAdd(longest);
	expect(last.gt(instant(Number.MAX_SAFE_INTEGER))).toBe(true);
	expect(last.saturatingAdd(Duration.nanos(1))).toEqual(last);
	expect(longest.checkedAdd(longest)).toBeUndefined();
	expect(longest.saturatingAdd(longest).gt(longest)).toBe(true);
	const max = longest.saturatingAdd(longest);
	expect(max.checkedAdd(Duration.nanos(1))).toBeUndefined();
	expect(max.saturatingAdd(Duration.nanos(1))).toEqual(max);
	expect(Duration.secs(1).checkedAdd(Duration.millis(1))).toEqual(Duration.millis(1001));
	expect(Duration.secs(1).saturatingAdd(Duration.millis(1))).toEqual(Duration.millis(1001));
});

test("durations keep a whole count of any unit exact past 2^53 ns", () => {
	// Microseconds since the Unix epoch, as a legacy container timestamp carries them.
	for (const micros of [1_735_689_600_000_016, 4_102_444_800_123_457, Number.MAX_SAFE_INTEGER]) {
		expect(Duration.micros(micros).asMicros()).toBe(micros);
	}
	for (const millis of [1_735_689_600_017, Number.MAX_SAFE_INTEGER]) {
		expect(Duration.millis(millis).asMillis()).toBe(millis);
	}
	const epoch = Duration.micros(1_735_689_600_000_016);
	expect(epoch.add(Duration.micros(1)).asMicros()).toBe(1_735_689_600_000_017);
	expect(epoch.sub(Duration.micros(17)).asMicros()).toBe(1_735_689_599_999_999);
	expect(epoch.add(Duration.nanos(1)).gt(epoch)).toBe(true);
	expect(Duration.micros(1500)).toEqual(Duration.millis(1.5));
	expect(Duration.secs(1).sub(Duration.nanos(1)).asNanos()).toBe(999_999_999);
	expect(Duration.millis(250).mul(4)).toEqual(Duration.secs(1));
	expect(Duration.secs(1).div(4)).toEqual(Duration.millis(250));
	expect(() => Duration.micros(1).sub(Duration.micros(2))).toThrow(RangeError);
	// A unit with no exact binary form rounds to whole nanoseconds, so it orders as it converts.
	expect(Duration.secs(1.1)).toEqual(Duration.millis(1100));
	expect(Duration.millis(3300).divDuration(Duration.secs(1.1))).toBe(3);
	expect(Duration.millis(3300).sub(Duration.secs(1.1).mul(3)).isZero()).toBe(true);
	expect(Duration.nanos(0.4).isZero()).toBe(true);
});

test("wall time round-trips Unix milliseconds exactly", () => {
	// Past 2^53 ns, a detour through a Duration since the epoch would round.
	for (const millis of [0, 1_735_689_600_001, 4_102_444_800_123, Number.MAX_SAFE_INTEGER]) {
		expect(Wall.unixMillis(millis).asUnixMillis()).toBe(millis);
	}
	expect(() => Wall.unixMillis(-1)).toThrow(RangeError);
	expect(() => Wall.unixMillis(Number.NaN)).toThrow(RangeError);
});

test("manual reads and wall corrections share a clock without inventing elapsed time", () => {
	const clock = manual();
	expect(ns(now())).toBe(0);
	expect(ms(wall())).toBe(START);
	clock.jump(Duration.millis(5));
	expect(ns(now())).toBe(5_000_000);
	expect(ms(wall())).toBe(START + 5);
	clock.stepWall(unix(START - 1000));
	expect(ns(now())).toBe(5_000_000);
	clock.stepWall(unix(START + 1000));
	clock.jump(Duration.millis(1));
	expect(ns(clock.now())).toBe(6_000_000);
	expect(ms(clock.wall())).toBe(START + 1001);
	expect(ms(origin())).toBe(START + 1000 - 5);
});

test("many small advances move wall time by their combined duration", () => {
	const clock = new Manual();
	for (let i = 0; i < 10_000; i++) clock.jump(Duration.nanos(100));
	expect(ns(clock.now())).toBe(1_000_000);
	expect(ms(clock.wall())).toBe(START + 1);
});

test("installation restores nested and out-of-order scopes without reviving them", () => {
	const outer = manual();
	outer.jump(Duration.nanos(4));
	const inner = manual();
	inner.jump(Duration.nanos(8));
	expect(ns(now())).toBe(8);
	installs[0].close();
	expect(ns(now())).toBe(8);
	installs[1].close();
	const restored = ns(now());
	installs[1].close();
	expect(ns(now())).toBeGreaterThanOrEqual(restored);
	expect(ns(now())).not.toBe(4);
});

test("deadlines fire in time then arming order and reached deadlines fire without advance", async () => {
	const clock = manual();
	const order: number[] = [];
	void sleepUntil(instant(10)).then(() => order.push(2));
	void sleepUntil(instant(5)).then(() => order.push(1));
	void sleepUntil(instant(10)).then(() => order.push(3));
	await clock.advance(Duration.nanos(10));
	expect(order).toEqual([1, 2, 3]);
	await sleepUntil(instant(1));
	await sleep(Duration.ZERO);
});

test("advance drains arbitrarily deep microtasks and newly due timers", async () => {
	const clock = manual();
	let done = false;
	void (async () => {
		await sleep(Duration.nanos(5));
		for (let i = 0; i < 1000; i++) await Promise.resolve();
		await sleep(Duration.ZERO);
		done = true;
	})();
	await clock.advance(Duration.nanos(5));
	expect(done).toBe(true);
});

for (const driver of ["advance", "fire"] as const) {
	test(`${driver} waits out a chain of host-task yields before moving time`, async () => {
		const clock = manual();
		void sleep(Duration.nanos(10));
		const seen: number[] = [];
		void (async () => {
			for (let i = 0; i < 3; i++) await yieldNow();
			seen.push(ns(now()));
		})();
		if (driver === "advance") await clock.advance(Duration.nanos(10));
		else expect(ns(await clock.fire())).toBe(10);
		expect(seen).toEqual([0]);
	});
}

test("a sleep stays on its creation clock and abort releases it", async () => {
	const first = manual();
	let done = false;
	const pending = sleep(Duration.nanos(10)).then(() => {
		done = true;
	});
	const second = manual();
	await second.advance(Duration.nanos(10));
	expect(done).toBe(false);
	await first.advance(Duration.nanos(10));
	await pending;
	const abort = new AbortController();
	const cancelled = sleep(Duration.nanos(100), { signal: abort.signal }).catch((error: unknown) => error);
	abort.abort();
	expect(await cancelled).toHaveProperty("name", "AbortError");
	await second.advance(Duration.nanos(100));
	await expect(sleep(Duration.ZERO, { signal: abort.signal })).rejects.toHaveProperty("name", "AbortError");
});

test("timeouts prefer ready work, propagate rejection, and expire on the manual deadline", async () => {
	const clock = manual();
	expect(await timeout(Duration.ZERO, Promise.resolve(7))).toBe(7);
	const failure = new Error("operation failed");
	await expect(timeout(Duration.nanos(10), Promise.reject(failure))).rejects.toBe(failure);
	const pending = timeout(Duration.nanos(10), new Promise<never>(() => {})).catch((error: unknown) => error);
	await clock.advance(Duration.nanos(9));
	await clock.advance(Duration.nanos(1));
	expect(await pending).toBeInstanceOf(Elapsed);
});

test("a timeout signal aborts at the manual deadline unless its parent aborts first", async () => {
	const clock = manual();
	const signal = timeoutSignal(Duration.nanos(10));
	await clock.advance(Duration.nanos(9));
	expect(signal.aborted).toBe(false);
	await clock.advance(Duration.nanos(1));
	expect(signal.reason).toHaveProperty("name", "TimeoutError");
	const parent = new AbortController();
	const child = timeoutSignal(Duration.nanos(10), { signal: parent.signal });
	parent.abort("closed");
	expect(child.reason).toBe("closed");
	await clock.advance(Duration.nanos(10));
	expect(child.reason).toBe("closed");
	const late = new AbortController();
	const fired = timeoutSignal(Duration.nanos(10), { signal: late.signal });
	clock.jump(Duration.nanos(10));
	late.abort("closed");
	expect(fired.reason).toHaveProperty("name", "TimeoutError");
});

test("interval burst delivers every missed scheduled tick", async () => {
	const clock = manual();
	const ticks = interval(Duration.nanos(10));
	try {
		expect(ns(await ticks.tick())).toBe(0);
		clock.jump(Duration.nanos(35));
		expect(ns(await ticks.tick())).toBe(10);
		expect(ns(await ticks.tick())).toBe(20);
		expect(ns(await ticks.tick())).toBe(30);
		const next = ticks.tick();
		await clock.advance(Duration.nanos(5));
		expect(ns(await next)).toBe(40);
		expect(ticks.period().asNanos()).toBe(10);
	} finally {
		ticks.close();
	}
});

for (const behavior of [MissedTickBehavior.Delay, MissedTickBehavior.Skip]) {
	test(`interval ${behavior} resumes after a missed tick`, async () => {
		const clock = manual();
		const ticks = interval(Duration.nanos(10));
		ticks.setMissedTickBehavior(behavior);
		try {
			expect(ns(await ticks.tick())).toBe(0);
			clock.jump(Duration.nanos(35));
			expect(ns(await ticks.tick())).toBe(10);
			const next = ticks.tick();
			await clock.advance(Duration.nanos(behavior === MissedTickBehavior.Delay ? 10 : 5));
			expect(ns(await next)).toBe(behavior === MissedTickBehavior.Delay ? 45 : 40);
		} finally {
			ticks.close();
		}
	});
}

test("an interval cancels pending ticks, refuses concurrent ticks and stays closed", async () => {
	manual();
	expect(() => interval(Duration.ZERO)).toThrow(RangeError);
	const ticks = interval(Duration.nanos(10));
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
	const at = now().since(Instant.ZERO).asMillis();
	expect(at).toBeGreaterThanOrEqual(before);
	expect(at).toBeLessThanOrEqual(host.now());
	expect(ms(origin())).toBe(host.timeOrigin);
});

test("an unreferenced sleep still completes on the host", async () => {
	hostTime(); // tests the host backend itself
	await sleep(Duration.millis(1), { ref: false });
});

test("manual installation leaves the host's own timers real", async () => {
	manual();
	await hostTimeout(0);
	expect(ns(now())).toBe(0);
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
		return sleepUntil(instant(at), { signal: abort.signal }).then(
			() => {
				fired.push(index);
			},
			() => {},
		);
	});
	for (let index = aborts.length - 1; index >= 0; index--) if (index % 3 === 0) aborts[index].abort();
	await clock.advance(Duration.nanos(17));
	await Promise.all(pending);
	expect(fired).toEqual(expected.sort((a, b) => a.at - b.at || a.index - b.index).map(({ index }) => index));
});

test("fire advances exactly to the earliest deadline and runs only what is due", async () => {
	const clock = manual();
	const order: number[] = [];
	void sleepUntil(instant(30)).then(() => order.push(30));
	void sleepUntil(instant(10)).then(() => order.push(1));
	void sleepUntil(instant(20)).then(() => order.push(20));
	void sleepUntil(instant(10)).then(() => order.push(2));
	clock.jump(Duration.nanos(3));
	expect(ns(await clock.fire())).toBe(10);
	expect(ns(now())).toBe(10);
	expect(order).toEqual([1, 2]);
	expect(ns(await clock.fire())).toBe(20);
	expect(ns(await clock.fire())).toBe(30);
	expect(order).toEqual([1, 2, 20, 30]);
});

test("fire picks an earlier timer that a pending microtask chain arms", async () => {
	const clock = manual();
	void sleepUntil(instant(100));
	void Promise.resolve().then(() => sleepUntil(instant(10)));
	expect(ns(await clock.fire())).toBe(10);
});

test("fire waits, without a timer, until a later host task arms one", async () => {
	const clock = manual();
	const abort = new AbortController();
	void sleep(Duration.nanos(5), { signal: abort.signal }).catch(() => {});
	abort.abort();
	let fired: number | undefined;
	const pending = clock.fire().then((at) => {
		fired = ns(at);
	});
	await hostTimeout(0);
	await hostTimeout(0);
	expect(fired).toBeUndefined();
	await expect(clock.fire()).rejects.toThrow("already pending");
	void sleep(Duration.nanos(7));
	await pending;
	expect(fired).toBe(7);
});

test("fire resolves when a later host task arms an already-due timer", async () => {
	const clock = manual();
	clock.jump(Duration.nanos(4));
	let woke = false;
	const pending = clock.fire();
	await hostTimeout(0);
	void sleep(Duration.nanos(0)).then(() => {
		woke = true;
	});
	expect(ns(await pending)).toBe(4);
	expect(woke).toBe(true);
});

test("fire stops at an already-due timer a pending microtask chain arms", async () => {
	const clock = manual();
	void sleepUntil(instant(100));
	void Promise.resolve().then(() => sleep(Duration.nanos(0)));
	expect(ns(await clock.fire())).toBe(0);
});

test("advance steps through deadlines armed by earlier timers and stops at its target", async () => {
	const clock = manual();
	const at: number[] = [];
	void (async () => {
		for (let i = 0; i < 4; i++) {
			await sleep(Duration.nanos(10));
			at.push(ns(now()));
		}
	})();
	await clock.advance(Duration.nanos(30));
	expect(at).toEqual([10, 20, 30]);
	expect(ns(now())).toBe(30);
	expect(ns(await clock.fire())).toBe(40);
	expect(at).toEqual([10, 20, 30, 40]);
});

test("advance drains one timer's tasks before firing the next at the same deadline", async () => {
	const clock = manual();
	const abort = new AbortController();
	const order: string[] = [];
	void sleep(Duration.nanos(10)).then(() => {
		order.push("first");
		abort.abort();
	});
	void sleep(Duration.nanos(10), { signal: abort.signal }).then(
		() => order.push("second"),
		() => order.push("cancelled"),
	);
	await clock.advance(Duration.nanos(10));
	expect(order).toEqual(["first", "cancelled"]);
});

test("a timer armed while draining joins existing timers at the same deadline", async () => {
	const clock = manual();
	const order: string[] = [];
	void sleep(Duration.nanos(10)).then(() => {
		order.push("first");
		void sleepUntil(instant(10)).then(() => order.push("new"));
	});
	void sleep(Duration.nanos(10)).then(() => order.push("second"));
	await clock.advance(Duration.nanos(10));
	expect(order).toEqual(["first", "second", "new"]);
});

test("a past deadline armed while draining observes the current instant", async () => {
	const clock = manual();
	const at: number[] = [];
	void sleep(Duration.nanos(10)).then(async () => {
		at.push(ns(now()));
		await sleepUntil(instant(3));
		at.push(ns(now()));
	});
	await clock.advance(Duration.nanos(10));
	expect(at).toEqual([10, 10]);
	expect(ns(clock.now())).toBe(10);
});

test("drive fires each deadline its work waits on and stops when the work settles", async () => {
	const clock = manual();
	let late = false;
	void sleepUntil(instant(100)).then(() => {
		late = true;
	});
	const work = (async () => {
		await sleep(Duration.nanos(10));
		await sleep(Duration.nanos(20));
		return now();
	})();
	expect(ns(await clock.drive(work))).toBe(30);
	expect(ns(clock.now())).toBe(30);
	await hostTimeout(0);
	expect(late).toBe(false);
});

test("drive settles with its work's rejection at the deadline that caused it", async () => {
	const clock = manual();
	const err = await clock
		.drive(
			sleep(Duration.nanos(10)).then(() => {
				throw new Error("boom");
			}),
		)
		.catch((e: unknown) => e);
	expect(err).toEqual(new Error("boom"));
	expect(ns(clock.now())).toBe(10);
});

for (const settles of ["before any timer", "on a later host task"] as const) {
	test(`drive leaves no driver behind when its work settles ${settles}`, async () => {
		const clock = manual();
		const work = settles === "before any timer" ? Promise.resolve(1) : hostTimeout(0).then(() => 1);
		expect(await clock.drive(work)).toBe(1);
		const fired: number[] = [];
		void sleep(Duration.millis(60_000)).then(() => fired.push(ns(now())));
		void sleep(Duration.millis(70_000)).then(() => fired.push(ns(now())));
		await hostTimeout(0);
		expect(fired).toEqual([]);
		expect(ns(clock.now())).toBe(0);
		expect(ns(await clock.fire())).toBe(60_000_000_000);
		expect(fired).toEqual([60_000_000_000]);
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
			driver === "advance"
				? clock.advance(Duration.nanos(10))
				: driver === "fire"
					? clock.fire()
					: clock.drive(work);
		try {
			await expect(clock.advance(Duration.nanos(100))).rejects.toThrow("already pending");
			expect(() => clock.jump(Duration.nanos(100))).toThrow("already pending");
			await expect(clock.fire()).rejects.toThrow("already pending");
			expect(await clock.drive(Promise.resolve()).catch((e: unknown) => e)).toEqual(
				new Error("time driver already pending"),
			);
			expect(ns(clock.now())).toBe(0);
		} finally {
			// Let fire and drive finish even if an assertion fails before they have a timer.
			void sleepUntil(instant(10)).then(release);
			await pending;
		}
		expect(ns(clock.now())).toBe(10);
		clock.jump(Duration.nanos(1));
		await clock.advance(Duration.nanos(1));
		void sleep(Duration.nanos(1));
		expect(ns(await clock.fire())).toBe(13);
	});
}

test("a timer callback cannot move time beyond the active advance target", async () => {
	const clock = manual();
	const at: number[] = [];
	const callback = sleep(Duration.nanos(5)).then(async () => {
		await expect(clock.advance(Duration.nanos(100))).rejects.toThrow("already pending");
		expect(() => clock.jump(Duration.nanos(100))).toThrow("already pending");
		await expect(clock.fire()).rejects.toThrow("already pending");
		await sleepUntil(instant(7));
		at.push(ns(now()));
	});
	await clock.advance(Duration.nanos(10));
	await callback;
	expect(at).toEqual([7]);
	expect(ns(clock.now())).toBe(10);
});

test("fire returns the first deadline reached when a host task arms an earlier timer", async () => {
	const clock = manual();
	const at: number[] = [];
	void sleepUntil(instant(10)).then(() => at.push(ns(now())));
	const pending = clock.fire();
	const channel = new MessageChannel();
	try {
		channel.port1.onmessage = () => {
			channel.port1.close();
			channel.port2.close();
			void sleepUntil(instant(5)).then(() => at.push(ns(now())));
		};
		channel.port2.postMessage(null);
		const fired = ns(await pending);
		// A host may run the task before selection or during the drain after firing.
		// Either way fire must stop where it first observed a timer becoming ready.
		expect(fired).toBe(at[0]);
		expect(at.every((value) => value === fired)).toBe(true);
		if (fired < 10) {
			expect(ns(await clock.fire())).toBe(10);
			expect(at).toEqual([5, 10]);
		}
	} finally {
		channel.port1.close();
		channel.port2.close();
	}
});

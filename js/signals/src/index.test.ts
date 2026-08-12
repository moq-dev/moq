import { describe, expect, spyOn, test } from "bun:test";
import { Computed, Effect, Once, Signal } from "./index.ts";

const NO_SUBSCRIPTION_WARNING = "Effect did not subscribe to any signals; it will never rerun.";

// Flush pending microtasks. Signal notifications and effect/computed reruns are
// coalesced onto microtasks, so a chain of A -> B -> effect needs several flushes.
const flush = () => new Promise<void>((resolve) => queueMicrotask(resolve));
async function settle(times = 5): Promise<void> {
	for (let i = 0; i < times; i++) await flush();
}

describe("Signal", () => {
	test("peek returns the current value synchronously after set", () => {
		const count = new Signal(0);
		expect(count.peek()).toBe(0);
		count.set(1);
		expect(count.peek()).toBe(1); // value is synchronous; only notification is deferred
	});

	test("set stores a function as the value; update transforms", () => {
		const fn = () => 1;
		const held = new Signal<() => number>(fn);
		expect(held.peek()).toBe(fn);

		const other = () => 2;
		held.set(other); // stored as-is, never invoked as a transform
		expect(held.peek()).toBe(other);

		held.update(() => fn);
		expect(held.peek()).toBe(fn);
	});

	test("subscribers are notified asynchronously", async () => {
		const count = new Signal(0);
		const seen: number[] = [];
		const dispose = count.subscribe((n) => seen.push(n));

		count.set(1);
		expect(seen).toEqual([]); // not yet
		await settle();
		expect(seen).toEqual([1]);
		dispose();
	});

	test("coalesces multiple sets into one notification", async () => {
		const count = new Signal(0);
		const seen: number[] = [];
		const dispose = count.subscribe((n) => seen.push(n));

		count.set(1);
		count.set(2);
		count.set(3);
		await settle();
		expect(seen).toEqual([3]);
		dispose();
	});

	test("no notification when the net value is unchanged", async () => {
		const count = new Signal(0);
		const seen: number[] = [];
		const dispose = count.subscribe((n) => seen.push(n));

		count.set(1);
		count.set(0); // back to the original within the same tick
		await settle();
		expect(seen).toEqual([]);
		dispose();
	});

	test("deep equality avoids notifications for equal plain objects", async () => {
		const obj = new Signal<{ a: number }>({ a: 1 });
		const seen: { a: number }[] = [];
		const dispose = obj.subscribe((v) => seen.push(v));

		obj.set({ a: 1 }); // structurally equal
		await settle();
		expect(seen).toEqual([]);

		obj.set({ a: 2 });
		await settle();
		expect(seen).toEqual([{ a: 2 }]);
		dispose();
	});
});

describe("Effect", () => {
	test("runs once on creation, tracking via get", async () => {
		const name = new Signal("world");
		const seen: string[] = [];
		const effect = new Effect((e) => {
			seen.push(e.get(name));
		});

		await settle();
		expect(seen).toEqual(["world"]);
		effect.close();
	});

	test("reruns when a tracked signal changes", async () => {
		const name = new Signal("world");
		const seen: string[] = [];
		const effect = new Effect((e) => {
			seen.push(e.get(name));
		});
		await settle();

		name.set("signals");
		await settle();
		expect(seen).toEqual(["world", "signals"]);
		effect.close();
	});

	test("does not rerun after close", async () => {
		const name = new Signal("world");
		const seen: string[] = [];
		const effect = new Effect((e) => {
			seen.push(e.get(name));
		});
		await settle();
		effect.close();

		name.set("signals");
		await settle();
		expect(seen).toEqual(["world"]);
	});

	test("cleanup runs on rerun and close", async () => {
		const toggle = new Signal(0);
		const log: string[] = [];
		const effect = new Effect((e) => {
			const v = e.get(toggle);
			log.push(`run ${v}`);
			e.cleanup(() => log.push(`cleanup ${v}`));
		});
		await settle();

		toggle.set(1);
		await settle();
		effect.close();

		expect(log).toEqual(["run 0", "cleanup 0", "run 1", "cleanup 1"]);
	});

	test("run() returns a disposer that closes the child and releases it from the parent", async () => {
		const parent = new Effect();
		const trigger = new Signal(0);
		const runs: number[] = [];
		const log: string[] = [];

		const dispose = parent.run((e) => {
			runs.push(e.get(trigger));
			e.cleanup(() => log.push("cleanup"));
		});
		await settle();
		expect(runs).toEqual([0]);

		// Disposing runs the child's cleanup and stops it from rerunning.
		dispose();
		expect(log).toEqual(["cleanup"]);
		trigger.set(1);
		await settle();
		expect(runs).toEqual([0]);

		// Idempotent, and the child was released so closing the parent doesn't re-run its cleanup.
		dispose();
		parent.close();
		expect(log).toEqual(["cleanup"]);
	});

	test("run() children left undisposed are still closed with the parent", async () => {
		const parent = new Effect();
		const log: string[] = [];

		parent.run((e) => e.cleanup(() => log.push("a")));
		parent.run((e) => e.cleanup(() => log.push("b")));
		await settle();

		parent.close();
		expect(log.sort()).toEqual(["a", "b"]);
	});

	test("event-only effects do not warn and remove listeners on close", async () => {
		const warn = spyOn(console, "warn").mockImplementation(() => {});
		const target = new EventTarget();
		let events = 0;
		const effect = new Effect((e) => e.event(target, "ping", () => events++));

		try {
			await settle();
			expect(warn).not.toHaveBeenCalledWith(NO_SUBSCRIPTION_WARNING, expect.anything());

			target.dispatchEvent(new Event("ping"));
			expect(events).toBe(1);

			effect.close();
			target.dispatchEvent(new Event("ping"));
			expect(events).toBe(1);
		} finally {
			effect.close();
			warn.mockRestore();
		}
	});

	test("abort-only effects do not warn and abort scoped work on close", async () => {
		const warn = spyOn(console, "warn").mockImplementation(() => {});
		const target = new EventTarget();
		let events = 0;
		const effect = new Effect((e) => {
			target.addEventListener("ping", () => events++, { signal: e.abort });
		});

		try {
			await settle();
			expect(warn).not.toHaveBeenCalledWith(NO_SUBSCRIPTION_WARNING, expect.anything());

			target.dispatchEvent(new Event("ping"));
			expect(events).toBe(1);

			effect.close();
			target.dispatchEvent(new Event("ping"));
			expect(events).toBe(1);
		} finally {
			effect.close();
			warn.mockRestore();
		}
	});

	test("cleanup from a spawn that outlived its run fires immediately", async () => {
		// A rerun drains the dispose list before it awaits in-flight spawns, so a task resuming
		// after that point used to register teardown against the NEXT run: the resource it owned
		// stayed alive until some unrelated later teardown, or forever if there wasn't one.
		const tick = new Signal(0);
		const closed: string[] = [];
		let runs = 0;

		// Held open so the task is guaranteed to resume inside the teardown window rather
		// than racing a timer, which would let a slow runner pass this for the wrong reason.
		const gate = Promise.withResolvers<void>();

		const effect = new Effect((e) => {
			e.get(tick);
			const run = ++runs;
			if (run > 1) return;

			e.spawn(async () => {
				await gate.promise;
				e.cleanup(() => closed.push(`run ${run}`));
			});
		});

		try {
			await settle();
			tick.set(1);
			await settle();

			// The rerun has torn run 1 down and is parked awaiting its task, so run 2 has
			// not started: whatever the task does now belongs to a run that is already over.
			expect(runs).toBe(1);

			gate.resolve();
			await settle();

			expect(closed).toEqual(["run 1"]);
			expect(runs).toBe(2);
		} finally {
			effect.close();
		}
	});

	test("the 5s warning does not open the next run while a spawn is in flight", async () => {
		// The 5s timer is a diagnostic, not a cutoff. It used to win a Promise.race and open the
		// next run while the task was still in flight, which reset #stale and swapped in a fresh
		// AbortController: the task's cleanup then landed on a run it never belonged to, and its
		// `abort` guard read that run's un-aborted signal and silently passed.
		const tick = new Signal(0);
		const gate = Promise.withResolvers<void>();
		const closed: string[] = [];
		let runs = 0;
		let lateAbort: boolean | undefined;
		let immediate: boolean | undefined;

		// Fire the 5s timer by hand instead of waiting five real seconds for it.
		let elapsed5s: (() => void) | undefined;
		const realSetTimeout = globalThis.setTimeout;
		const timer = spyOn(globalThis, "setTimeout").mockImplementation(((fn: () => void, ms?: number) => {
			if (ms === 5000) {
				elapsed5s = fn;
				return 0 as unknown as ReturnType<typeof setTimeout>;
			}
			return realSetTimeout(fn, ms);
		}) as typeof setTimeout);
		const warn = spyOn(console, "warn").mockImplementation(() => {});

		const effect = new Effect((e) => {
			e.get(tick);
			const run = ++runs;
			if (run > 1) return;

			e.spawn(async () => {
				await gate.promise;

				// Read off the effect, not off a signal captured during the run: the point is
				// that the effect still exposes this run's scope while the task is in flight.
				lateAbort = e.abort.aborted;

				let ran = false;
				e.cleanup(() => {
					ran = true;
					closed.push(`run ${run}`);
				});
				immediate = ran;
			});
		});

		try {
			await settle();
			tick.set(1);
			await settle();
			expect(runs).toBe(1); // parked awaiting the task

			// Five seconds pass. The warning fires, but the next run must stay shut.
			expect(elapsed5s).toBeDefined();
			elapsed5s?.();
			await settle();
			expect(runs).toBe(1);
			expect(warn).toHaveBeenCalled();

			gate.resolve();
			await settle();

			// The task still owned its run, so teardown was immediate rather than deferred.
			expect(immediate).toBe(true);
			expect(closed).toEqual(["run 1"]);
			expect(lateAbort).toBe(true);

			// Only once the task settled does the rerun proceed.
			expect(runs).toBe(2);
		} finally {
			effect.close();
			timer.mockRestore();
			warn.mockRestore();
		}
	});

	test("close() during the wait releases it and drops the warning", async () => {
		// A task that never settles pins the drain. close() has to cut it loose: it already ran
		// every dispose function and there is no next run to protect, so continuing to wait would
		// keep the effect's own promise pending and fire the 5s warning at an effect that is
		// already gone.
		const tick = new Signal(0);
		const never = Promise.withResolvers<void>(); // deliberately never resolved

		let elapsed5s: (() => void) | undefined;
		let cleared = false;
		const realSetTimeout = globalThis.setTimeout;
		const realClearTimeout = globalThis.clearTimeout;
		const TIMER = 987 as unknown as ReturnType<typeof setTimeout>;

		const timer = spyOn(globalThis, "setTimeout").mockImplementation(((fn: () => void, ms?: number) => {
			if (ms === 5000) {
				elapsed5s = fn;
				return TIMER;
			}
			return realSetTimeout(fn, ms);
		}) as typeof setTimeout);
		const clear = spyOn(globalThis, "clearTimeout").mockImplementation(((id?: unknown) => {
			if (id === TIMER) cleared = true;
			else realClearTimeout(id as never);
		}) as typeof clearTimeout);
		const warn = spyOn(console, "warn").mockImplementation(() => {});

		const effect = new Effect((e) => {
			e.get(tick);
			e.spawn(async () => {
				await never.promise;
			});
		});

		try {
			await settle();
			tick.set(1);
			await settle();
			expect(elapsed5s).toBeDefined(); // parked on the task, timer armed

			effect.close();
			await settle();

			// The wait let go, so the diagnostic timer was disarmed on the way out.
			expect(cleared).toBe(true);

			// Even if the timer had already been queued, a closed effect must not be warned about.
			elapsed5s?.();
			const warned = warn.mock.calls.some((call) => String(call[0]).includes("spawn is still running"));
			expect(warned).toBe(false);
		} finally {
			effect.close();
			timer.mockRestore();
			clear.mockRestore();
			warn.mockRestore();
		}
	});

	test("a task that never settles stalls the rerun instead of leaking", async () => {
		// The tradeoff the unbounded wait buys: a task ignoring both abort and cancel holds the
		// next run shut rather than handing its cleanup to a run it never belonged to. Teardown
		// of the run it did belong to still happened, and close() still recovers everything.
		const tick = new Signal(0);
		const never = Promise.withResolvers<void>();
		const closed: string[] = [];
		let runs = 0;

		const effect = new Effect((e) => {
			e.get(tick);
			if (++runs > 1) return;

			e.cleanup(() => closed.push("run 1"));
			e.spawn(async () => {
				await never.promise;
			});
		});

		try {
			await settle();
			tick.set(1);
			await settle(20);

			expect(runs).toBe(1); // stalled, not advanced
			expect(closed).toEqual(["run 1"]); // teardown still ran, before the wait

			// Further signal changes cannot sneak a run open while the task is outstanding.
			tick.set(2);
			await settle(20);
			expect(runs).toBe(1);
		} finally {
			effect.close();
		}

		// close() is never blocked, and a late registration from the wedged task fires at once.
		let late = false;
		effect.cleanup(() => {
			late = true;
		});
		expect(late).toBe(true);
	});

	test("a rerun waits for a task spawned while the previous one unwinds", async () => {
		// A task that spawns another as it unwinds (a read loop queueing one last handler) has to
		// keep the run open too, or the nested task inherits the same broken ownership.
		const tick = new Signal(0);
		const outer = Promise.withResolvers<void>();
		const inner = Promise.withResolvers<void>();
		const closed: string[] = [];
		let runs = 0;
		let immediate: boolean | undefined;

		const effect = new Effect((e) => {
			e.get(tick);
			if (++runs > 1) return;

			e.spawn(async () => {
				await outer.promise;

				e.spawn(async () => {
					await inner.promise;

					let ran = false;
					e.cleanup(() => {
						ran = true;
						closed.push("nested");
					});
					immediate = ran;
				});
			});
		});

		try {
			await settle();
			tick.set(1);
			await settle();
			expect(runs).toBe(1);

			// The outer task settles, but the one it spawned is still in flight.
			outer.resolve();
			await settle();
			expect(runs).toBe(1);

			inner.resolve();
			await settle();

			expect(immediate).toBe(true);
			expect(closed).toEqual(["nested"]);
			expect(runs).toBe(2);
		} finally {
			effect.close();
		}
	});

	test("a spawn that outlived its run sees that run aborted and cancelled", async () => {
		// The scope a stale task reads has to be its own, not the incoming run's: an abort
		// signal that never fires would scope its listeners to the next run, and a cancel
		// promise that never resolves would park it forever.
		const tick = new Signal(0);
		const target = new EventTarget();
		let events = 0;
		let aborted: boolean | undefined;
		let cancelled = false;
		let runs = 0;

		const gate = Promise.withResolvers<void>();

		const effect = new Effect((e) => {
			e.get(tick);
			if (++runs > 1) return;

			e.spawn(async () => {
				await gate.promise;
				aborted = e.abort.aborted;
				e.event(target, "ping", () => events++);
				await e.cancel;
				cancelled = true;
			});
		});

		try {
			await settle();
			tick.set(1);
			await settle();
			expect(runs).toBe(1);

			gate.resolve();
			await settle();

			expect(aborted).toBe(true);
			expect(cancelled).toBe(true);

			// The listener belonged to a dead run, so it must not survive into the next one.
			target.dispatchEvent(new Event("ping"));
			expect(events).toBe(0);
			expect(runs).toBe(2);
		} finally {
			effect.close();
		}
	});

	test("run() from a stale spawn defers its child to the next teardown", async () => {
		// Deliberately unlike cleanup(): closing the child right away would cancel its first run
		// before `fn` executes, so the teardown `fn` registers would never fire at all. Landing on
		// the next teardown is late, but it still runs and still releases.
		const tick = new Signal(0);
		const gate = Promise.withResolvers<void>();
		const log: string[] = [];
		let runs = 0;

		const effect = new Effect((e) => {
			e.get(tick);
			if (++runs > 1) return;

			e.spawn(async () => {
				await gate.promise;
				e.run((child) => {
					log.push("served");
					child.cleanup(() => log.push("released"));
				});
			});
		});

		try {
			await settle();
			tick.set(1);
			await settle();
			expect(runs).toBe(1); // stale window

			gate.resolve();
			await settle();

			// The child ran rather than being cancelled before it could serve.
			expect(log).toEqual(["served"]);
		} finally {
			effect.close();
		}

		expect(log).toEqual(["served", "released"]);
	});

	test("cleanup after close fires immediately", async () => {
		const effect = new Effect((e) => {
			e.get(new Signal(0));
		});
		await settle();

		let closed = false;
		effect.close();
		effect.cleanup(() => {
			closed = true;
		});
		expect(closed).toBe(true);
	});

	test("empty effects still warn", async () => {
		const warn = spyOn(console, "warn").mockImplementation(() => {});
		const effect = new Effect(() => {});

		try {
			await settle();
			expect(warn).toHaveBeenCalledWith(NO_SUBSCRIPTION_WARNING, expect.anything());
		} finally {
			effect.close();
			warn.mockRestore();
		}
	});
});

describe("Computed", () => {
	test("is undefined until the first run completes, then resolves", async () => {
		const a = new Signal(2);
		const b = new Signal(3);
		const sum = new Computed((e) => e.get(a) + e.get(b));

		// Like any signal, the value isn't available synchronously.
		expect(sum.peek()).toBeUndefined();
		await settle();
		expect(sum.peek()).toBe(5);
		sum.close();
	});

	test("recomputes asynchronously when a dependency changes", async () => {
		const a = new Signal(2);
		const tenfold = new Computed((e) => e.get(a) * 10);
		await settle();
		expect(tenfold.peek()).toBe(20);

		a.set(5);
		expect(tenfold.peek()).toBe(20); // not yet: a set never reruns readers synchronously
		await settle();
		expect(tenfold.peek()).toBe(50);
		tenfold.close();
	});

	test("a downstream effect reruns when the computed value changes", async () => {
		const a = new Signal(1);
		const doubled = new Computed((e) => e.get(a) * 2);
		const seen: (number | undefined)[] = [];
		const effect = new Effect((e) => {
			seen.push(e.get(doubled));
		});
		await settle();
		expect(seen.at(-1)).toBe(2);

		a.set(4);
		await settle();
		expect(seen.at(-1)).toBe(8);

		effect.close();
		doubled.close();
	});

	test("equality filtering: no downstream rerun when the output is unchanged", async () => {
		const a = new Signal(1);
		const positive = new Computed((e) => e.get(a) > 0);
		const seen: (boolean | undefined)[] = [];
		const effect = new Effect((e) => {
			seen.push(e.get(positive));
		});
		await settle();
		const base = seen.length;

		a.set(5); // still positive: computed output is unchanged
		await settle();
		expect(seen.length).toBe(base);

		a.set(-1); // now the output flips
		await settle();
		expect(seen.length).toBe(base + 1);
		expect(seen.at(-1)).toBe(false);

		effect.close();
		positive.close();
	});

	test("coalesces multiple dependency changes into a single recompute", async () => {
		const a = new Signal(1);
		const b = new Signal(1);
		let computes = 0;
		const sum = new Computed((e) => {
			computes++;
			return e.get(a) + e.get(b);
		});
		await settle();
		expect(sum.peek()).toBe(2);
		expect(computes).toBe(1);

		a.set(2);
		b.set(3);
		await settle();
		expect(computes).toBe(2);
		expect(sum.peek()).toBe(5);
		sum.close();
	});

	test("computeds nest", async () => {
		const a = new Signal(2);
		const plusOne = new Computed((e) => e.get(a) + 1);
		const timesTen = new Computed((e) => (e.get(plusOne) ?? 0) * 10);
		await settle();
		expect(timesTen.peek()).toBe(30);

		a.set(9);
		await settle();
		expect(timesTen.peek()).toBe(100);

		plusOne.close();
		timesTen.close();
	});

	test("close stops recomputing", async () => {
		const a = new Signal(1);
		let computes = 0;
		const derived = new Computed((e) => {
			computes++;
			return e.get(a);
		});
		await settle();
		const before = computes;
		derived.close();

		a.set(2);
		await settle();
		expect(computes).toBe(before);
	});
});

describe("effect.computed", () => {
	// Create the computed once on a container effect, observe it from another effect.
	// (Creating and observing it in the same rerunning body would loop: the async
	// first value reschedules the body, which rebuilds the computed, and so on.)
	test("derives a value tied to the container effect", async () => {
		const a = new Signal(1);
		const b = new Signal(2);
		const container = new Effect();
		const sum = container.computed((e) => e.get(a) + e.get(b));

		const seen: (number | undefined)[] = [];
		const observer = new Effect((e) => {
			seen.push(e.get(sum));
		});
		await settle();
		expect(seen.at(-1)).toBe(3);

		a.set(10);
		await settle();
		expect(seen.at(-1)).toBe(12);

		observer.close();
		container.close();
	});

	test("is closed with its container effect", async () => {
		const a = new Signal(1);
		let computes = 0;
		const container = new Effect();
		const derived = container.computed((e) => {
			computes++;
			return e.get(a) * 2;
		});
		const observer = new Effect((e) => {
			e.get(derived);
		});
		await settle();
		const before = computes;

		container.close(); // closes derived
		a.set(5);
		await settle();
		expect(computes).toBe(before);
		observer.close();
	});
});

describe("Once", () => {
	test("awaits the settled value, immediately if already settled", async () => {
		const { Once } = await import("./index.ts");
		const once = new Once<string>();

		let awaited: string | undefined;
		void once.then((v) => {
			awaited = v;
		});
		expect(awaited).toBeUndefined();

		once.set("done");
		await once; // resolves now
		expect(await once).toBe("done"); // still resolves after the fact
		expect(awaited).toBe("done");
	});

	test("peek returns undefined while pending, the value once settled", () => {
		const once = new Once<number>();
		expect(once.peek()).toBeUndefined();
		once.set(7);
		expect(once.peek()).toBe(7);
	});

	test("set throws if called twice", () => {
		const once = new Once<boolean>();
		once.set(true);
		expect(() => once.set(true)).toThrow();
	});

	test("notifies subscribers once when it settles", async () => {
		const once = new Once<string>();
		const seen: (string | undefined)[] = [];
		once.subscribe((v) => seen.push(v));
		once.set("x");
		await flush();
		expect(seen).toEqual(["x"]);
	});
});

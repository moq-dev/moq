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

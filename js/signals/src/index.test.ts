import { describe, expect, test } from "bun:test";
import { Computed, Effect, Signal } from "./index.ts";

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
});

describe("Computed", () => {
	test("computes lazily on first read; never undefined", () => {
		const a = new Signal(2);
		const b = new Signal(3);
		const sum = new Computed((e) => e.get(a) + e.get(b));
		expect(sum.peek()).toBe(5);
		sum.close();
	});

	test("get() is an alias for peek()", () => {
		const a = new Signal(10);
		const doubled = new Computed((e) => e.get(a) * 2);
		expect(doubled.get()).toBe(20);
		doubled.close();
	});

	test("bare read after a dependency set is stale-but-defined, fresh next tick", async () => {
		const a = new Signal(2);
		const tenfold = new Computed((e) => e.get(a) * 10);
		expect(tenfold.peek()).toBe(20);

		a.set(5);
		// Consistent with the rest of the library: a set does not synchronously
		// rerun readers, so the cached value lags by a microtask.
		expect(tenfold.peek()).toBe(20);
		await settle();
		expect(tenfold.peek()).toBe(50);
		tenfold.close();
	});

	test("a downstream effect reruns when the computed value changes", async () => {
		const a = new Signal(1);
		const doubled = new Computed((e) => e.get(a) * 2);
		const seen: number[] = [];
		const effect = new Effect((e) => {
			seen.push(e.get(doubled));
		});
		await settle();
		expect(seen).toEqual([2]);

		a.set(4);
		await settle();
		expect(seen).toEqual([2, 8]);

		effect.close();
		doubled.close();
	});

	test("equality filtering: no downstream rerun when the output is unchanged", async () => {
		const a = new Signal(1);
		const positive = new Computed((e) => e.get(a) > 0);
		const seen: boolean[] = [];
		const effect = new Effect((e) => {
			seen.push(e.get(positive));
		});
		await settle();
		expect(seen).toEqual([true]);

		a.set(5); // still positive: computed output is unchanged
		await settle();
		expect(seen).toEqual([true]);

		a.set(-1); // now the output flips
		await settle();
		expect(seen).toEqual([true, false]);

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
		const timesTen = new Computed((e) => e.get(plusOne) * 10);
		expect(timesTen.peek()).toBe(30);

		a.set(9);
		await settle();
		expect(timesTen.peek()).toBe(100);

		plusOne.close();
		timesTen.close();
	});

	test("close stops tracking dependencies", async () => {
		const a = new Signal(1);
		let computes = 0;
		const derived = new Computed((e) => {
			computes++;
			return e.get(a);
		});
		derived.peek();
		const before = computes;
		derived.close();

		a.set(2);
		await settle();
		expect(computes).toBe(before);
	});

	test("a constant computed (no dependencies) works", () => {
		const k = new Computed(() => 42);
		expect(k.peek()).toBe(42);
		k.close();
	});

	test("detects a direct self-reference cycle", () => {
		const c: Computed<number> = new Computed((e) => e.get(c) + 1);
		expect(() => c.peek()).toThrow("Computed cycle detected");
		c.close();
	});

	test("detects a transitive cycle (a -> b -> a)", () => {
		const a: Computed<number> = new Computed((e) => e.get(b) + 1);
		const b: Computed<number> = new Computed((e) => e.get(a) + 1);
		expect(() => a.peek()).toThrow("Computed cycle detected");
		a.close();
		b.close();
	});

	test("a cycle does not poison unrelated computeds", () => {
		const a: Computed<number> = new Computed((e) => e.get(a));
		expect(() => a.peek()).toThrow();
		const ok = new Computed(() => 7);
		expect(ok.peek()).toBe(7);
		a.close();
		ok.close();
	});
});

describe("effect.computed", () => {
	test("derives a value tied to the effect", async () => {
		const a = new Signal(1);
		const b = new Signal(2);
		const seen: number[] = [];
		const effect = new Effect((e) => {
			const sum = e.computed((c) => c.get(a) + c.get(b));
			seen.push(e.get(sum));
		});
		await settle();
		expect(seen).toEqual([3]);

		a.set(10);
		await settle();
		expect(seen.at(-1)).toBe(12);
		effect.close();
	});

	test("is closed with its parent effect", async () => {
		const a = new Signal(1);
		let computes = 0;
		const effect = new Effect((e) => {
			const derived = e.computed((c) => {
				computes++;
				return c.get(a) * 2;
			});
			e.get(derived); // observe so it stays hot
		});
		await settle();
		const before = computes;

		effect.close();
		a.set(5);
		await settle();
		expect(computes).toBe(before);
	});
});

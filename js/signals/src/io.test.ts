import { expect, test } from "bun:test";
import {
	Computed,
	Derived,
	Effect,
	type Getter,
	type GetterInit,
	getter,
	type Inputs,
	Once,
	readonlys,
	Signal,
} from "./index.ts";

// Lets the microtask flush and any timer-based follow-up run.
const settle = () => new Promise((resolve) => setTimeout(resolve, 0));

// A conforming Getter this package did not create: no brand, just the three methods.
function adapter<T>(source: Getter<T>): Getter<T> {
	return {
		peek: () => source.peek(),
		subscribe: (fn) => source.subscribe(fn),
		changed: ((fn?: (value: T) => void) => (fn ? source.changed(fn) : source.changed())) as Getter<T>["changed"],
	};
}

test("getter wraps a raw value in a fresh Signal", () => {
	const g = getter(5);
	expect(g.peek()).toBe(5);
});

test("getter reuses an existing Signal instead of wrapping it", () => {
	const s = new Signal(1);
	expect(getter(s)).toBe(s);
});

test("getter reuses a readonlys() result (so out wires into in)", () => {
	const s = new Signal("hello");
	const view = readonlys({ value: s }).value;
	// The read-only view is the same branded Signal, so getter() passes it through.
	expect(getter(view)).toBe(s);
});

test("readonlys exposes live reads without a writable handle", () => {
	const s = new Signal(1);
	const out = readonlys({ count: s });
	expect(out.count.peek()).toBe(1);
	s.set(2);
	expect(out.count.peek()).toBe(2);
});

test("getter reuses a Computed instead of wrapping it as a constant", async () => {
	const source = new Signal(1);
	const computed = new Computed((effect) => effect.get(source) * 2);

	const input = getter(computed);
	expect(input).toBe(computed);

	await Promise.resolve();
	expect(input.peek()).toBe(2);

	source.set(5);
	await computed.changed();
	expect(input.peek()).toBe(10);

	computed.close();
});

test("getter reuses a Once instead of wrapping it as a constant", async () => {
	const once = new Once<string>();

	const input = getter(once);
	expect(input).toBe(once);
	expect(input.peek()).toBeUndefined();

	const settled = once.changed();
	once.set("settled");

	expect(await settled).toBe("settled");
	expect(input.peek()).toBe("settled");
});

test("a Computed wired into an in stays live under an effect", async () => {
	const source = new Signal(1);
	const computed = new Computed((effect) => effect.get(source) * 2);
	const input = getter(computed);

	const seen: (number | undefined)[] = [];
	const effect = new Effect((e) => {
		seen.push(e.get(input));
	});

	await Promise.resolve();
	source.set(4);
	await computed.changed();
	await Promise.resolve();

	expect(seen).toEqual([2, 8]);

	effect.close();
	computed.close();
});

test("getter passes through a Signal from an older package version", () => {
	// Older versions brand Signal but not the readable, so getter() must still accept the
	// signal brand alone. Symbol.for shares the brand across copies of the package.
	const old = {
		[Symbol.for("@moq/signals")]: true,
		peek: () => 3,
		changed: (() => {}) as Getter<number>["changed"],
		subscribe: () => () => {},
	};

	expect(getter(old as unknown as Getter<number>)).toBe(old);
});

test("getter reuses a foreign readable instead of freezing it", async () => {
	const source = new Signal(0);
	const foreign = adapter(source);

	const input = getter(foreign);
	expect(input).toBe(foreign);

	const other = source.subscribe(() => {}); // so set() has subscribers and queues a flush
	source.set(1);

	const raw: number[] = [];
	const adapted: number[] = [];
	const cancelRaw = source.changed((value) => raw.push(value));
	const cancelAdapted = input.changed((value) => adapted.push(value));

	await settle();
	expect(adapted).toEqual(raw);
	expect(adapted).toEqual([1]);
	cancelRaw();
	cancelAdapted();
	other();
});

test("a foreign readable wired as an input notifies until unsubscribed", async () => {
	const source = new Signal(0);
	const input = getter(adapter(source));

	const seen: number[] = [];
	const dispose = input.subscribe((value) => seen.push(value));

	source.set(1);
	await settle();
	source.set(2);
	await settle();
	expect(seen).toEqual([1, 2]);

	dispose();
	source.set(3);
	await settle();
	expect(seen).toEqual([1, 2]);
});

test("a foreign readable wired into an in stays live under an effect", async () => {
	const source = new Signal(1);
	const input = getter(adapter(source));

	const seen: number[] = [];
	const effect = new Effect((e) => {
		seen.push(e.get(input));
	});

	await Promise.resolve();
	source.set(4);
	await settle();

	expect(seen).toEqual([1, 4]);

	effect.close();
});

test("getter does not subscribe to a readable it reuses", () => {
	let subscribed = 0;
	const foreign: Getter<number> = {
		peek: () => 1,
		changed: (() => {}) as Getter<number>["changed"],
		subscribe: () => {
			subscribed++;
			return () => {};
		},
	};

	expect(getter(foreign)).toBe(foreign);
	expect(subscribed).toBe(0);
});

test("GetterInit and Inputs accept every conforming readable and reject the rest", () => {
	const source = new Signal(1);
	const computed = new Computed((effect) => effect.get(source) * 2);
	const once = new Once<number>();
	const derived = new Derived([source], (value) => value);
	const output = readonlys({ count: source }).count;
	const foreign = adapter(source);

	function read(value: GetterInit<number>): Getter<number> {
		return getter(value);
	}
	function readMaybe(value: GetterInit<number | undefined>): Getter<number | undefined> {
		return getter(value);
	}
	function construct(props: Inputs<{ count: Getter<number> }>): Getter<number> {
		return getter(props.count ?? 0);
	}

	expect(read(1).peek()).toBe(1);
	expect(read(source)).toBe(source);
	expect(read(derived)).toBe(derived);
	expect(read(output)).toBe(source);
	expect(read(foreign)).toBe(foreign);
	expect(readMaybe(computed)).toBe(computed);
	expect(readMaybe(once)).toBe(once);

	expect(construct({}).peek()).toBe(0);
	expect(construct({ count: 2 }).peek()).toBe(2);
	expect(construct({ count: source })).toBe(source);
	expect(construct({ count: output })).toBe(source);
	expect(construct({ count: derived })).toBe(derived);
	expect(construct({ count: foreign })).toBe(foreign);

	// @ts-expect-error a string is not a number or Getter<number>
	read("nope");
	// @ts-expect-error a data object is not a number or Getter<number>
	read({ peek: 1 });
	// @ts-expect-error an incomplete readable is not a Getter
	read({ peek: () => 1 });
	// @ts-expect-error a Getter of the wrong value type
	read(new Signal("nope"));
	// @ts-expect-error a string is not a number or Getter<number>
	construct({ count: "nope" });

	computed.close();
});

test("getter still wraps plain objects that are not readables", () => {
	const value = { peek: 1 };
	const g = getter(value);
	expect(g.peek()).toBe(value);
});

test("getter accepts a Derived, so a mapped view can be wired as an input", () => {
	const source = new Signal({ total: 0 });
	const view = new Derived([source], ({ total }) => total > 0);

	expect(getter(view)).toBe(view);
});

test("Derived reads through on every peek, with no first-run gap", () => {
	const a = new Signal(1);
	const b = new Signal(2);
	const sum = new Derived([a, b], (x, y) => x + y);

	expect(sum.peek()).toBe(3);
	a.set(10);
	expect(sum.peek()).toBe(12);
});

test("Derived relays every source notification, redundant or not", async () => {
	const source = new Signal({ total: 0, discovery: 0 });
	const view = new Derived([source], ({ total, discovery }) => (total === 0 ? undefined : discovery > 0));

	const seen: (boolean | undefined)[] = [];
	const dispose = view.subscribe((value) => seen.push(value));

	source.set({ total: 1, discovery: 1 });
	await Promise.resolve();
	// A second session moves the counts but not the answer: relayed anyway, because the
	// alternative drops real edges (see the two cases below).
	source.set({ total: 2, discovery: 2 });
	await Promise.resolve();
	source.set({ total: 0, discovery: 0 });
	await Promise.resolve();

	expect(seen).toEqual([true, true, undefined]);
	dispose();
});

test("Derived delivers a change whose flush was already queued when we subscribed", async () => {
	const source = new Signal(0);
	const other = source.subscribe(() => {}); // so set() has subscribers and queues a flush
	const view = new Derived([source], (value) => value);

	// The value is already 1 here; only its notification is still queued. Comparing against
	// peek() at subscribe time would treat this edge as already seen.
	source.set(1);

	const raw: number[] = [];
	const derived: number[] = [];
	const cancelRaw = source.changed((value) => raw.push(value));
	const cancelDerived = view.changed((value) => derived.push(value));

	await settle();
	expect(derived).toEqual(raw);
	expect(derived).toEqual([1]);
	cancelRaw();
	cancelDerived();
	other();
});

test("Derived delivers an in-place mutation of a value it returns as-is", async () => {
	const source = new Signal({ count: 0 });
	const view = new Derived([source], (value) => value);

	const seen: number[] = [];
	const dispose = view.subscribe((value) => seen.push(value.count));

	// mutate() force-notifies precisely because the object identity cannot change.
	source.mutate((value) => {
		value.count++;
	});
	await settle();

	expect(seen).toEqual([1]);
	dispose();
});

test("Derived changed() fires once and unsubscribes itself", async () => {
	const a = new Signal(1);
	const b = new Signal(1);
	const max = new Derived([a, b], (x, y) => Math.max(x, y));

	const seen: number[] = [];
	const cancel = max.changed((value) => seen.push(value));

	b.set(5);
	await Promise.resolve();
	a.set(9);
	await Promise.resolve();

	expect(seen).toEqual([5]);
	cancel();

	const next = max.changed();
	a.set(11);
	expect(await next).toBe(11);
});

test("an out Getter feeds another component's in end to end", () => {
	// Mimic: produced.out.value -> consumed input via getter().
	const produced = new Signal(0);
	const output: Getter<number> = readonlys({ value: produced }).value;

	const consumedInput = getter(output);
	produced.set(42);
	expect(consumedInput.peek()).toBe(42);
});

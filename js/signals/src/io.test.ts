import { expect, test } from "bun:test";
import { Computed, Effect, type Getter, getter, Once, readonlys, Signal } from "./index.ts";

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

	expect(seen).toContain(8);

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

test("getter throws on a foreign readable instead of freezing it", () => {
	// Quacks like a Getter but carries no brand: wrapping it would silently never update.
	const foreign: Getter<number> = {
		peek: () => 1,
		changed: (() => {}) as Getter<number>["changed"],
		subscribe: () => () => {},
	};

	expect(() => getter(foreign)).toThrow();
});

test("getter still wraps plain objects that are not readables", () => {
	const value = { peek: 1 };
	const g = getter(value);
	expect(g.peek()).toBe(value);
});

test("an out Getter feeds another component's in end to end", () => {
	// Mimic: produced.out.value -> consumed input via getter().
	const produced = new Signal(0);
	const output: Getter<number> = readonlys({ value: produced }).value;

	const consumedInput = getter(output);
	produced.set(42);
	expect(consumedInput.peek()).toBe(42);
});

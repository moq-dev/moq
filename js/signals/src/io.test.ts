import { expect, test } from "bun:test";
import { type Getter, getter, readonlys, Signal } from "./index.ts";

test("getter wraps a raw value in a fresh Signal", () => {
	const g = getter(5);
	expect(g.peek()).toBe(5);
});

test("getter reuses an existing Signal instead of wrapping it", () => {
	const s = new Signal(1);
	expect(getter(s)).toBe(s);
});

test("getter reuses a readonlys() output (so outputs wire into inputs)", () => {
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

test("an output Getter feeds another component's input end to end", () => {
	// Mimic: produced.output.value -> consumed input via getter().
	const produced = new Signal(0);
	const output: Getter<number> = readonlys({ value: produced }).value;

	const consumedInput = getter(output);
	produced.set(42);
	expect(consumedInput.peek()).toBe(42);
});

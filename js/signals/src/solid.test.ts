import { expect, test } from "bun:test";
import { createRoot } from "solid-js";
import { Signal } from "./index.ts";
import { createPair } from "./solid.ts";

// Signal notifications are coalesced onto a microtask.
const flush = () => new Promise<void>((resolve) => queueMicrotask(resolve));

test("a Solid pair follows its signal until its owner is disposed", async () => {
	const signal = new Signal(1);
	const [dispose, get, set] = createRoot((dispose) => [dispose, ...createPair(signal)] as const);
	expect(get()).toBe(1);

	signal.set(2);
	await flush();
	expect(get()).toBe(2);

	expect(set((prev) => prev + 1)).toBe(3);
	expect(signal.peek()).toBe(3);
	await flush();
	expect(get()).toBe(3);

	dispose();
	signal.set(4);
	await flush();
	expect(get()).toBe(3);
});

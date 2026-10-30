import { afterAll, beforeAll, expect, test } from "bun:test";
import { GlobalRegistrator } from "@happy-dom/global-registrator";
import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { type Dispose, Signal, type Subscriber } from "./index.ts";
import { useSignal, useValue } from "./react.ts";

// Signal notifications are coalesced onto a microtask.
const flush = () => new Promise<void>((resolve) => queueMicrotask(resolve));

beforeAll(() => {
	GlobalRegistrator.register();
	(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
});

afterAll(async () => {
	delete (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT;
	await GlobalRegistrator.unregister();
});

/** A signal that counts its live subscriptions. */
class Counted<T> extends Signal<T> {
	active = 0;

	override subscribe(fn: Subscriber<T>): Dispose {
		this.active++;
		const dispose = super.subscribe(fn);
		return () => {
			this.active--;
			dispose();
		};
	}
}

async function mount(element: Parameters<Root["render"]>[0]): Promise<Root> {
	const root = createRoot(document.createElement("div"));
	await act(async () => root.render(element));
	return root;
}

async function write(fn: () => void) {
	await act(async () => {
		fn();
		await flush();
	});
}

test("useValue renders the current value and follows changes until unmount", async () => {
	const signal = new Counted(1);
	const seen: number[] = [];
	const View = ({ source }: { source: Signal<number> }) => {
		seen.push(useValue(source));
		return null;
	};

	const root = await mount(createElement(View, { source: signal }));
	expect(seen.at(-1)).toBe(1);
	expect(signal.active).toBe(1);

	await write(() => signal.set(2));
	expect(seen.at(-1)).toBe(2);

	await act(async () => root.unmount());
	expect(signal.active).toBe(0);

	const renders = seen.length;
	await write(() => signal.set(3));
	expect(seen.length).toBe(renders);
});

test("useValue moves its subscription when the component switches signals", async () => {
	const a = new Counted("a");
	const b = new Counted("b");
	const seen: string[] = [];
	const View = ({ source }: { source: Signal<string> }) => {
		seen.push(useValue(source));
		return null;
	};

	const root = await mount(createElement(View, { source: a }));
	expect(seen.at(-1)).toBe("a");

	await act(async () => root.render(createElement(View, { source: b })));
	expect(seen.at(-1)).toBe("b");
	expect(a.active).toBe(0);
	expect(b.active).toBe(1);

	// The old signal no longer renders; the new one does.
	const renders = seen.length;
	await write(() => a.set("a2"));
	expect(seen.length).toBe(renders);
	await write(() => b.set("b2"));
	expect(seen.at(-1)).toBe("b2");

	await act(async () => root.unmount());
	expect(b.active).toBe(0);
});

test("useSignal's setter takes values and functional updates", async () => {
	const signal = new Counted(1);
	const seen: number[] = [];
	let setter: ((value: number | ((prev: number) => number)) => void) | undefined;
	const View = ({ source }: { source: Signal<number> }) => {
		const [value, set] = useSignal(source);
		seen.push(value);
		setter = set;
		return null;
	};

	const root = await mount(createElement(View, { source: signal }));
	const set = setter;
	if (!set) throw new Error("the component never rendered");
	expect(seen.at(-1)).toBe(1);

	await write(() => set(5));
	expect(signal.peek()).toBe(5);
	expect(seen.at(-1)).toBe(5);

	// Both updates read the value the previous one wrote, not the rendered one.
	await write(() => {
		set((prev) => prev + 1);
		set((prev) => prev * 2);
	});
	expect(signal.peek()).toBe(12);
	expect(seen.at(-1)).toBe(12);

	// The setter is stable across renders of the same signal.
	expect(setter).toBe(set);

	await act(async () => root.unmount());
	expect(signal.active).toBe(0);
});

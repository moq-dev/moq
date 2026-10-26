import { expect, mock, spyOn, test } from "bun:test";

mock.module("./audio/capture-worklet.ts?worklet", () => ({ default: async () => "blob:capture" }));

mock.module("./video/capture-worker.ts?worklet", () => ({ default: async () => "blob:worker" }));

test("an invalid URL attribute warns and clears the publish URL", async () => {
	const originalElement = globalThis.HTMLElement;
	const originalRegistry = globalThis.customElements;
	const registry = new Map<string, CustomElementConstructor>();
	globalThis.HTMLElement = class {} as unknown as typeof HTMLElement;
	globalThis.customElements = {
		define: (name: string, ctor: CustomElementConstructor) => {
			registry.set(name, ctor);
		},
		get: (name: string) => registry.get(name),
	} as unknown as CustomElementRegistry;

	const set = mock(() => {});
	const warn = spyOn(console, "warn").mockImplementation(() => {});
	try {
		const { default: Element } = await import("./element");
		expect(() =>
			Element.prototype.attributeChangedCallback.call({ connection: { url: { set } } }, "url", null, "http://["),
		).not.toThrow();
		expect(set).toHaveBeenCalledWith(undefined);
		expect(warn).toHaveBeenCalledTimes(1);
	} finally {
		warn.mockRestore();
		globalThis.HTMLElement = originalElement;
		globalThis.customElements = originalRegistry;
	}
});

test("an invalid announce attribute throws and stops announcing", async () => {
	const originalElement = globalThis.HTMLElement;
	const originalRegistry = globalThis.customElements;
	const registry = new Map<string, CustomElementConstructor>();
	globalThis.HTMLElement = class {} as unknown as typeof HTMLElement;
	globalThis.customElements = {
		define: (name: string, ctor: CustomElementConstructor) => {
			registry.set(name, ctor);
		},
		get: (name: string) => registry.get(name),
	} as unknown as CustomElementRegistry;

	const set = mock(() => {});
	try {
		const { default: Element } = await import("./element");
		expect(() =>
			Element.prototype.attributeChangedCallback.call(
				{ controls: { announce: { set } } },
				"announce",
				"source",
				"never",
			),
		).toThrow('invalid announce="never"');
		expect(set).toHaveBeenCalledWith(undefined);
	} finally {
		globalThis.HTMLElement = originalElement;
		globalThis.customElements = originalRegistry;
	}
});

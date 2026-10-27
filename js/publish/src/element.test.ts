import { expect, mock, test } from "bun:test";

mock.module("./audio/capture-worklet.ts?worklet", () => ({ default: async () => "blob:capture" }));

mock.module("./video/capture-worker.ts?worklet", () => ({ default: async () => "blob:worker" }));

// Native attribute reactions and their errors are covered by publish-ui.browser.ts.
test("importing the publish entrypoint registers the element", async () => {
	const originalElement = globalThis.HTMLElement;
	const originalRegistry = globalThis.customElements;
	const registry = new Map<string, CustomElementConstructor>();
	globalThis.HTMLElement = class {} as unknown as typeof HTMLElement;
	globalThis.customElements = {
		define: (name: string, ctor: CustomElementConstructor) => registry.set(name, ctor),
	} as unknown as CustomElementRegistry;
	try {
		await import("./element");
		expect(registry.has("moq-publish")).toBe(true);
	} finally {
		globalThis.HTMLElement = originalElement;
		globalThis.customElements = originalRegistry;
	}
});

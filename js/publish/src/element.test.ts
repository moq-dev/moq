import { afterAll, expect, mock, test } from "bun:test";
import { yieldNow } from "@moq/clock";

mock.module("./audio/capture-worklet.ts?worklet", () => ({ default: async () => "blob:capture" }));

mock.module("./video/capture-worker.ts?worklet", () => ({ default: async () => "blob:worker" }));

// Just enough of the DOM to import the element, construct it and drive its callbacks. Native attribute
// reactions and their errors are covered by publish-ui.browser.ts.
const originalElement = globalThis.HTMLElement;
const originalRegistry = globalThis.customElements;
const originalObserver = globalThis.MutationObserver;
const originalVideo = globalThis.HTMLVideoElement;
const registry = new Map<string, CustomElementConstructor>();
globalThis.HTMLElement = class {
	getRootNode() {
		return {};
	}
	getAttribute() {
		return null;
	}
	setAttribute() {}
	removeAttribute() {}
	querySelector() {
		return null;
	}
} as unknown as typeof HTMLElement;
globalThis.customElements = {
	define: (name: string, ctor: CustomElementConstructor) => registry.set(name, ctor),
} as unknown as CustomElementRegistry;
globalThis.MutationObserver = class {
	observe() {}
	disconnect() {}
} as unknown as typeof MutationObserver;
globalThis.HTMLVideoElement = class {} as unknown as typeof HTMLVideoElement;
afterAll(() => {
	globalThis.HTMLElement = originalElement;
	globalThis.customElements = originalRegistry;
	globalThis.MutationObserver = originalObserver;
	globalThis.HTMLVideoElement = originalVideo;
});

const { default: MoqPublish } = await import("./element");

// Effects rerun in a later task, so each check waits one out.
const settle = () => yieldNow();

test("importing the publish entrypoint registers the element", () => {
	expect(registry.get("moq-publish")).toBe(MoqPublish);
});

test("a corrected attribute resumes the element", async () => {
	const el = new MoqPublish();
	try {
		el.connectedCallback();
		await settle();
		expect(el.connection.enabled.peek()).toBe(true);

		expect(() => el.attributeChangedCallback("backend", null, "bad")).toThrow();
		await settle();
		expect(el.connection.enabled.peek()).toBe(false);

		el.attributeChangedCallback("backend", "bad", null);
		await settle();
		expect(el.connection.enabled.peek()).toBe(true);
	} finally {
		el.signals.close();
	}
});

// A page's explicit disable wins: correcting a refused attribute must not turn the element back on.
test("an explicit disable survives an invalid attribute being corrected", async () => {
	const el = new MoqPublish();
	try {
		el.connectedCallback();
		await settle();

		el.connection.enabled.set(false);
		expect(() => el.attributeChangedCallback("backend", null, "bad")).toThrow();
		await settle();
		el.attributeChangedCallback("backend", "bad", null);
		await settle();
		expect(el.connection.enabled.peek()).toBe(false);

		// Nor does moving the element to another place in the document.
		el.disconnectedCallback();
		el.connectedCallback();
		await settle();
		expect(el.connection.enabled.peek()).toBe(false);
	} finally {
		el.signals.close();
	}
});

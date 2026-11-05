import { expect, mock, test } from "bun:test";
import { yieldNow } from "@moq/clock";

// The UI renders into a shadow root, which the test runner has no DOM for.
mock.module("solid-js/web", () => ({ render: () => () => {} }));
mock.module("./ui/element.tsx", () => ({ BoyUI: () => null }));
// Bun cannot load the blob-URL worklet import used by the audio decoder. Mock it here rather than
// relying on game.test.ts, since that mock only applies when its file happens to run first.
mock.module("../../watch/src/audio/render-worklet.ts?worklet", () => ({ default: async () => "blob:fake-render" }));

const registry = new Map<string, CustomElementConstructor>();
globalThis.HTMLElement = class {
	attachShadow() {
		return {};
	}
} as unknown as typeof HTMLElement;
globalThis.customElements = {
	define: (name: string, ctor: CustomElementConstructor) => {
		registry.set(name, ctor);
	},
	get: (name: string) => registry.get(name),
} as unknown as CustomElementRegistry;

const { default: MoqBoy } = await import("./element");

// Effects rerun in a later task, so each check waits one out.
const settle = () => yieldNow();

// A refused prefix idles the element rather than discovering games under a default prefix.
for (const attribute of ["prefix", "prefix-game", "prefix-viewer"] as const) {
	test(`an invalid ${attribute} idles the element until a valid one arrives`, async () => {
		const el = new MoqBoy();
		try {
			// Invalid markup, parsed before the element connects.
			expect(() => el.attributeChangedCallback(attribute, null, "/boy")).toThrow();
			el.connectedCallback();
			await settle();
			expect(el.connection.enabled.peek()).toBe(false);

			el.attributeChangedCallback(attribute, "/boy", "boy");
			await settle();
			expect(el.connection.enabled.peek()).toBe(true);

			expect(() => el.attributeChangedCallback(attribute, "boy", "boy//x")).toThrow();
			await settle();
			expect(el.connection.enabled.peek()).toBe(false);

			// Removing the attribute restores the default, which is valid.
			el.attributeChangedCallback(attribute, "boy//x", null);
			await settle();
			expect(el.connection.enabled.peek()).toBe(true);
		} finally {
			el.disconnectedCallback();
		}
	});
}

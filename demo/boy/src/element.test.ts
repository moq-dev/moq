import { expect, mock, test } from "bun:test";
import { yieldNow } from "@moq/clock";

// The UI renders into a shadow root, which the test runner has no DOM for.
mock.module("./ui/element.tsx", () => ({ mount: () => () => {} }));
// Bun cannot load the blob-URL worklet import used by the audio decoder. Mock it here rather than
// relying on game.test.ts, since that mock only applies when its file happens to run first.
mock.module("../../../js/watch/src/audio/render-worklet.ts?worklet", () => ({
	default: async () => "blob:fake-render",
}));

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
for (const attribute of ["prefix", "prefix-game"] as const) {
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

// A page's explicit disable wins: correcting a refused path must not turn the element back on.
test("an explicit disable survives an invalid path being corrected", async () => {
	const el = new MoqBoy();
	try {
		el.connectedCallback();
		await settle();
		expect(el.connection.enabled.peek()).toBe(true);

		el.connection.enabled.set(false);
		expect(() => el.attributeChangedCallback("prefix", null, "/boy")).toThrow();
		await settle();
		el.attributeChangedCallback("prefix", "/boy", "boy");
		await settle();
		expect(el.connection.enabled.peek()).toBe(false);

		// A valid edit leaves the page's choice alone too.
		el.attributeChangedCallback("prefix", "boy", "other");
		await settle();
		expect(el.connection.enabled.peek()).toBe(false);
	} finally {
		el.disconnectedCallback();
	}
});

// The element is already off while a path is refused, so the page's disable there must still count.
test("an explicit disable while a path is refused survives the fix", async () => {
	const el = new MoqBoy();
	try {
		el.connectedCallback();
		expect(() => el.attributeChangedCallback("prefix", null, "/boy")).toThrow();
		el.connection.enabled.set(false);
		el.attributeChangedCallback("prefix", "/boy", "boy");
		await settle();
		expect(el.connection.enabled.peek()).toBe(false);

		el.connection.enabled.set(true);
		await settle();
		expect(el.connection.enabled.peek()).toBe(true);
	} finally {
		el.disconnectedCallback();
	}
});

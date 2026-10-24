import { expect, mock, spyOn, test } from "bun:test";

// Stand-ins for the only DOM globals the module body touches: the base class and the registry it
// writes to. No element is constructed here, so nothing else is reached.
const registry = new Map<string, CustomElementConstructor>();
globalThis.HTMLElement = class {} as unknown as typeof HTMLElement;
globalThis.customElements = {
	define: (name: string, ctor: CustomElementConstructor) => {
		registry.set(name, ctor);
	},
	get: (name: string) => registry.get(name),
} as unknown as CustomElementRegistry;

// The audio worklet is inlined by a bundler plugin, which the test runner has no equivalent for.
mock.module("./audio/render-worklet.ts?worklet", () => ({ default: async () => "blob:worklet" }));

// A framework binds the raw node (Svelte's `bind:this`, React's ref) and reads properties from it.
// Until the browser upgrades that node, every field the class sets is absent, so `el.broadcast` is
// undefined. Registering while the entrypoint evaluates is what makes the upgrade land first: any
// page that imports it before mounting can only ever see an upgraded element.
//
// `require` finishes the module graph synchronously, so a registration deferred to a microtask, an
// event, or a nested dynamic import leaves the registry empty here.
test("importing the entrypoint registers <moq-watch> before the import finishes", () => {
	require("./element");
	expect(registry.get("moq-watch")).toBeDefined();
});

for (const [attribute, value, expected] of [
	["url", "http://[", undefined],
	["volume", "foo", 0.5],
	["volume", "Infinity", 0.5],
	["volume", "1oops", 0.5],
	["volume", "0x10", 0.5],
	["volume", "-0.1", 0.5],
] as const) {
	test(`invalid ${attribute}=${value} warns and uses the default`, () => {
		const Element = require("./element").default;
		const set = mock(() => {});
		const target = { connection: { url: { set } }, controls: { audio: { volume: { set } } } };
		const warn = spyOn(console, "warn").mockImplementation(() => {});
		try {
			expect(() => Element.prototype.attributeChangedCallback.call(target, attribute, null, value)).not.toThrow();
			expect(set).toHaveBeenCalledWith(expected);
			expect(warn).toHaveBeenCalledTimes(1);
		} finally {
			warn.mockRestore();
		}
	});
}

for (const [value, expected] of [
	["moq-transport-14", ["moq-transport-14"]],
	[" moq-lite-06 , moq-transport-22 ", ["moq-lite-06", "moq-transport-22"]],
	["", undefined],
	[" , ", undefined],
	[null, undefined],
] as const) {
	test(`versions=${JSON.stringify(value)} offers ${JSON.stringify(expected)}`, () => {
		const Element = require("./element").default;
		const set = mock(() => {});
		Element.prototype.attributeChangedCallback.call({ connection: { versions: { set } } }, "versions", "x", value);
		expect(set).toHaveBeenCalledWith(expected);
	});
}

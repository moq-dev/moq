import { expect, mock, test } from "bun:test";

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

test("catalog formats refuse unknown values rather than selecting detection", () => {
	const { parseCatalogFormat } = require("./element");
	for (const format of ["hang", "msf", "manual"]) expect(parseCatalogFormat(format)).toBe(format);
	expect(parseCatalogFormat(null)).toBeUndefined();
	for (const invalid of ["", "guess"]) expect(() => parseCatalogFormat(invalid)).toThrow("catalog-format");
});

test("importing the entrypoint registers <moq-sync>, which refuses an instant delay", () => {
	require("./element");
	const MoqSync = registry.get("moq-sync") as unknown as new () => {
		attributeChangedCallback(name: string, oldValue: string | null, newValue: string | null): void;
		delay: unknown;
		cap: { asMillis(): number };
		sync: { close(): void };
	};
	expect(MoqSync).toBeDefined();
	const group = new MoqSync();
	expect(group.delay).toBe("auto");
	expect(() => group.attributeChangedCallback("delay", null, "instant")).toThrow('delay="instant"');
	expect(() => group.attributeChangedCallback("delay", null, "soon")).toThrow('moq-sync: invalid delay="soon"');
	group.attributeChangedCallback("cap", null, "1s");
	expect(group.cap.asMillis()).toBe(1000);
	expect(() => group.attributeChangedCallback("cap", null, "soon")).toThrow('cap="soon"');
	group.sync.close();
});

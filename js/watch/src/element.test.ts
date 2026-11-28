import { expect, mock, test } from "bun:test";

// Stand-ins for the DOM globals the module touches: the registry it writes to, and a base class
// with just enough of an element for one to be constructed and (dis)connected.
const registry = new Map<string, CustomElementConstructor>();
globalThis.HTMLElement = class {
	style = {};
	parentElement: { closest(selector: string): unknown } | null = null;
	#attributes = new Map<string, string>();
	getAttribute(name: string) {
		return this.#attributes.get(name) ?? null;
	}
	setAttribute(name: string, value: string) {
		this.#attributes.set(name, value);
	}
	removeAttribute(name: string) {
		this.#attributes.delete(name);
	}
	toggleAttribute(name: string, force: boolean) {
		if (force) this.setAttribute(name, "");
		else this.removeAttribute(name);
	}
	appendChild() {}
	querySelector() {
		return null;
	}
	getBoundingClientRect() {
		return { width: 0, height: 0 };
	}
} as unknown as typeof HTMLElement;
globalThis.ResizeObserver = class {
	observe() {}
	disconnect() {}
} as unknown as typeof ResizeObserver;
globalThis.MutationObserver = class {
	observe() {}
	disconnect() {}
} as unknown as typeof MutationObserver;
globalThis.document = { createElement: () => ({ style: {} }) } as unknown as Document;
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
	group.attributeChangedCallback("cap", null, "2s");
	expect(group.cap.asMillis()).toBe(2000);
	expect(() => group.attributeChangedCallback("cap", null, "soon")).toThrow('cap="soon"');
	group.sync.close();
});

test("a delay refused inside <moq-sync> plays again once the element leaves it", () => {
	require("./element");
	const MoqSync = registry.get("moq-sync") as unknown as new () => { sync: { close(): void } };
	const MoqWatch = registry.get("moq-watch") as unknown as new () => {
		parentElement: { closest(selector: string): unknown } | null;
		connection: { enabled: { peek(): boolean } };
		delay: unknown;
		setAttribute(name: string, value: string): void;
		attributeChangedCallback(name: string, oldValue: string | null, newValue: string | null): void;
		connectedCallback(): void;
		disconnectedCallback(): void;
		signals: { close(): void };
	};
	const group = new MoqSync();
	const watch = new MoqWatch();
	watch.parentElement = { closest: () => group };
	watch.connectedCallback();
	expect(watch.connection.enabled.peek()).toBe(true);

	// Refused inside the group: the element stops, as for any invalid attribute.
	watch.setAttribute("delay", "instant");
	expect(() => watch.attributeChangedCallback("delay", null, "instant")).toThrow("inside <moq-sync>");
	expect(watch.connection.enabled.peek()).toBe(false);

	// Moved out of the group, "instant" is valid again.
	watch.disconnectedCallback();
	watch.parentElement = null;
	watch.connectedCallback();
	expect(watch.delay).toBe("instant");
	expect(watch.connection.enabled.peek()).toBe(true);

	watch.signals.close();
	group.sync.close();
});

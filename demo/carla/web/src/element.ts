/**
 * The `<moq-carla>` custom element: discovers simulated vehicles on a relay and
 * lets a viewer watch and steer them.
 *
 * Side-effectful: importing this registers the element.
 *
 * @module
 */

import * as Moq from "@moq/net";
import { mount, PREVIEW } from "./player.ts";

/** The attributes the element observes. */
const OBSERVED = ["url"] as const;

// Closes the connection of an element collected while still open. The held value
// must not reach the element, or the registry would keep it alive.
const cleanup = new FinalizationRegistry<Moq.Connection>((connection) => connection.close());

/**
 * `<moq-carla>` web component: renders the CARLA player in a Shadow DOM.
 *
 * Attributes:
 *   - `url` - MoQ relay URL
 */
export default class MoqCarla extends HTMLElement {
	/** The attributes the browser reports changes to. */
	static observedAttributes: typeof OBSERVED = OBSERVED;

	/** The relay connection, open while the element is in a document. */
	readonly connection: Moq.Connection;

	// Set when the element is connected to the DOM.
	readonly #connected = new Moq.Signals.Signal(false);
	#dispose?: () => void;

	constructor() {
		super();
		this.connection = new Moq.Connection({ enabled: PREVIEW ? false : this.#connected });
		cleanup.register(this, this.connection);
	}

	/** Called by the browser when the element joins a document. */
	connectedCallback() {
		this.#connected.set(true);

		// Render into a Shadow DOM, reusing the existing one on reconnect.
		const shadow = this.shadowRoot ?? this.attachShadow({ mode: "open" });
		this.#dispose = mount(shadow, this.connection);
	}

	/** Called by the browser when the element leaves its document. */
	disconnectedCallback() {
		this.#connected.set(false);
		this.#dispose?.();
		this.#dispose = undefined;
	}

	/** Called by the browser when an observed attribute changes. */
	attributeChangedCallback(_name: (typeof OBSERVED)[number], _oldValue: string | null, newValue: string | null) {
		this.url = newValue ?? undefined;
	}

	/** The relay URL, mirrored by the `url` attribute. */
	get url(): URL | undefined {
		return this.connection.url.peek();
	}

	set url(value: string | URL | undefined) {
		this.connection.url.set(value ? new URL(value) : undefined);
	}
}

customElements.define("moq-carla", MoqCarla);

declare global {
	interface HTMLElementTagNameMap {
		"moq-carla": MoqCarla;
	}
}

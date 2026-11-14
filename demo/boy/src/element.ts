/**
 * The `<moq-boy>` custom element: discovers Game Boy sessions on a relay and renders them.
 *
 * Side-effectful: importing this registers the element.
 *
 * @module
 */

import * as Moq from "@moq/net";
import type { GameConfig } from "./game.ts";
import { Game } from "./game.ts";
import { mount } from "./ui/element.tsx";

/** The attributes the element observes. */
const OBSERVED = ["url", "prefix", "prefix-game"] as const;
type Observed = (typeof OBSERVED)[number];

const DEFAULT_PREFIX = Moq.Path.from("boy");

// Sockets need moq-lite-07 or moq-transport on every hop, and the relay offers moq-lite-07 only when asked.
const VERSIONS = ["moq-lite-07-wip"];

// A viewer's client endpoint id: random, so no other viewer can claim it. A viewer only dials, so it
// announces a client endpoint, which can never pose as a game's server endpoint.
function viewerId(): string {
	const bytes = crypto.getRandomValues(new Uint8Array(16));
	return Array.from(bytes, (byte) => byte.toString(16).padStart(2, "0")).join("");
}

const cleanup = new FinalizationRegistry<Moq.Signals.Effect>((signals) => signals.close());

/**
 * The element's `connection.enabled`, which the page may also write. The page's writes set what it
 * wants and the element sets whether it can run (connected to a document, every path valid); it
 * reads as on only while both hold. Kept apart, a page's disable survives the element suspending and
 * resuming, whenever the page writes it: an explicit page choice wins.
 */
class Enabled extends Moq.Signals.Signal<boolean> {
	#wanted = true;
	#running = false;

	constructor() {
		super(false);
	}

	override set(value: boolean, notify?: boolean): void {
		this.#wanted = value;
		super.set(value && this.#running, notify);
	}

	run(running: boolean): void {
		this.#running = running;
		super.set(this.#wanted && running);
	}
}

/**
 * `<moq-boy>` web component - discovers and manages Game Boy streaming sessions,
 * and renders the interactive UI in a Shadow DOM.
 *
 * Attributes:
 *   - `url` - MoQ relay URL
 *   - `prefix` - Base path prefix (default: "boy"). Derives prefix-game.
 *   - `prefix-game` - Path prefix for game broadcasts and the socket endpoints of games and viewers
 *     (default: `{prefix}/game`)
 *
 * Each viewer runs one client socket endpoint with a random id and dials each game it plays.
 */
export default class MoqBoy extends HTMLElement {
	/** The attributes the browser reports changes to. */
	static observedAttributes: typeof OBSERVED = OBSERVED;

	/** The relay connection, shared by every game. */
	readonly connection: Moq.Connection;
	/** The origin this viewer's endpoint is published into, served across reconnects. */
	readonly origin = new Moq.Origin.Producer();
	/** The session currently expanded, or undefined for the grid. */
	readonly expanded = new Moq.Signals.Signal<string | undefined>(undefined);

	/** Reactive map of active game sessions, read by the overlay. Emits on add/remove. @internal */
	readonly games = new Moq.Signals.Signal<ReadonlyMap<string, Game>>(new Map());

	readonly #signals = new Moq.Signals.Effect();
	// Set when the element is connected to the DOM.
	#connected = false;
	// The path attributes holding an invalid path, which idle the element until each is fixed.
	readonly #refused = new Set<Observed>();
	// Whether to run: `#update` gates it on the element's state, and the page may turn it off.
	readonly #enabled = new Enabled();
	readonly #prefix = new Moq.Signals.Signal<Moq.Path.Valid>(DEFAULT_PREFIX);
	readonly #gamePrefixOverride = new Moq.Signals.Signal<Moq.Path.Valid | undefined>(undefined);
	readonly #sessions = new Map<string, Game>();
	#dispose?: () => void;

	constructor() {
		super();
		cleanup.register(this, this.#signals);

		// One origin, both directions: the viewer's endpoint is published into it and the
		// relay's announced games and endpoints arrive in it, with no risk of echoing either back.
		this.connection = new Moq.Connection({
			enabled: this.#enabled,
			versions: VERSIONS,
			publish: this.origin.consume(),
			consume: this.origin,
			share: false,
		});
		this.#signals.cleanup(() => this.connection.close());
		this.#signals.cleanup(() => this.origin.close());

		// Discover game sessions via announcements.
		this.#signals.run(this.#runDiscovery.bind(this));
	}

	/** Called by the browser when the element joins a document. */
	connectedCallback() {
		this.#connected = true;
		this.#update();

		// Render the UI into a Shadow DOM (reuse existing on reconnect).
		const shadow = this.shadowRoot ?? this.attachShadow({ mode: "open" });
		this.#dispose = mount(this, shadow);
	}

	/** Called by the browser when the element leaves its document. */
	disconnectedCallback() {
		this.#connected = false;
		this.#update();
		this.#dispose?.();
		this.#dispose = undefined;
	}

	/** Called by the browser when an observed attribute changes. */
	attributeChangedCallback(name: (typeof OBSERVED)[number], _oldValue: string | null, newValue: string | null) {
		if (name === "url") {
			this.connection.url.set(newValue ? new URL(newValue) : undefined);
			return;
		}

		// An invalid path leaves the element idle until fixed, with the error rethrown.
		let path: Moq.Path.Valid | undefined;
		try {
			path = newValue === null ? undefined : Moq.Path.from(newValue);
		} catch (error) {
			this.#refuse(name, true);
			throw error;
		}
		this.#refuse(name, false);

		if (name === "prefix") {
			this.#prefix.set(path ?? DEFAULT_PREFIX);
		} else {
			this.#gamePrefixOverride.set(path);
		}
	}

	#refuse(name: Observed, refused: boolean) {
		if (refused) this.#refused.add(name);
		else this.#refused.delete(name);
		this.#update();
	}

	#update() {
		this.#enabled.run(this.#connected && this.#refused.size === 0);
	}

	/** The relay URL, mirrored by the `url` attribute. */
	get url(): URL | undefined {
		return this.connection.url.peek();
	}

	set url(value: string | URL | undefined) {
		this.connection.url.set(value ? new URL(value) : undefined);
	}

	/** The base path prefix, mirrored by the `prefix` attribute. */
	get prefixPath(): string {
		return this.#prefix.peek();
	}

	set prefixPath(value: string) {
		this.#prefix.set(Moq.Path.from(value));
		this.#refuse("prefix", false);
	}

	/** The path prefix for game broadcasts, mirrored by the `prefix-game` attribute. */
	get prefixGame(): string {
		return this.#gamePrefixOverride.peek() ?? Moq.Path.from(this.#prefix.peek(), "game");
	}

	set prefixGame(value: string) {
		this.#gamePrefixOverride.set(Moq.Path.from(value));
		this.#refuse("prefix-game", false);
	}

	#runDiscovery(effect: Moq.Signals.Effect) {
		const base = effect.get(this.#prefix);
		const gamePrefix = effect.get(this.#gamePrefixOverride) ?? Moq.Path.from(base, "game");

		// The endpoint every game under this prefix is dialed from.
		const endpoint = new Moq.Socket.Endpoint(this.origin, gamePrefix, viewerId());
		effect.cleanup(() => endpoint.close());

		// Games under the old prefix dial from the old endpoint, so they go with it.
		effect.cleanup(() => {
			for (const game of this.#sessions.values()) game.close();
			this.#sessions.clear();
			this.games.set(new Map());
		});

		// The origin's stream spans reconnects: entries retract when the session dies and
		// return when the next one re-announces them, so this loop never needs to restart.
		const announced = this.origin.consume().announced(Moq.Path.Pattern.parse("*").rooted(gamePrefix));
		effect.cleanup(() => announced.close());

		effect.spawn(async () => {
			for (;;) {
				const entry = await effect.race(announced.next());
				if (!entry) break;

				// A broad route that cannot pin the game id names nothing to open.
				const capture = entry.captures?.[0];
				if (!capture?.isLiteral) continue;

				const id = capture.text;
				const gone = entry.kind === "absent" || entry.kind === "offline";
				if (entry.kind === "present" && !this.#sessions.has(id)) {
					const config: GameConfig = {
						sessionId: id,
						connection: this.connection,
						origin: this.origin,
						endpoint,
						expanded: this.expanded,
						gamePrefix,
					};
					const game = new Game(config);
					this.#sessions.set(id, game);
					this.games.set(new Map(this.#sessions));
				} else if (gone) {
					const game = this.#sessions.get(id);
					if (game) {
						game.close();
						this.#sessions.delete(id);
						this.games.set(new Map(this.#sessions));
					}
				}
			}
		});
	}
}

customElements.define("moq-boy", MoqBoy);

declare global {
	interface HTMLElementTagNameMap {
		"moq-boy": MoqBoy;
	}
}

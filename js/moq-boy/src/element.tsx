/**
 * The `<moq-boy>` custom element: discovers Game Boy sessions on a relay and renders them.
 *
 * Side-effectful: importing this registers the element.
 *
 * @module
 */

import * as Moq from "@moq/net";
import { render } from "solid-js/web";
import type { GameConfig } from "./game.ts";
import { Game } from "./game.ts";
import { BoyUI } from "./ui/element.tsx";

/** The attributes the element observes. */
const OBSERVED = ["url", "prefix", "prefix-game", "prefix-viewer"] as const;
type Observed = (typeof OBSERVED)[number];

const DEFAULT_PREFIX = Moq.Path.from("boy");

const cleanup = new FinalizationRegistry<Moq.Signals.Effect>((signals) => signals.close());

/**
 * `<moq-boy>` web component — discovers and manages Game Boy streaming sessions,
 * and renders the interactive UI in a Shadow DOM.
 *
 * Attributes:
 *   - `url` — MoQ relay URL
 *   - `prefix` — Base path prefix (default: "boy"). Derives prefix-game and prefix-viewer.
 *   - `prefix-game` — Path prefix for game broadcasts (default: `{prefix}/game`)
 *   - `prefix-viewer` — Path prefix for viewer broadcasts (default: `{prefix}/viewer`)
 */
export default class MoqBoy extends HTMLElement {
	/** The attributes the browser reports changes to. */
	static observedAttributes: typeof OBSERVED = OBSERVED;

	/** The relay connection, shared by every game. */
	readonly connection: Moq.Connection;
	/** The origin viewer broadcasts are published into, served across reconnects. */
	readonly origin = new Moq.Origin.Producer();
	/** The session currently expanded, or undefined for the grid. */
	readonly expanded = new Moq.Signals.Signal<string | undefined>(undefined);

	/** Reactive map of active game sessions. Emits on add/remove. */
	readonly games = new Moq.Signals.Signal<ReadonlyMap<string, Game>>(new Map());

	readonly #signals = new Moq.Signals.Effect();
	// Set when the element is connected to the DOM.
	readonly #connected = new Moq.Signals.Signal(false);
	// The path attributes holding an invalid path, which idle the element until each is fixed.
	readonly #refused = new Moq.Signals.Signal<ReadonlySet<Observed>>(new Set());
	// Whether to run: connected, with nothing refused.
	readonly #enabled = new Moq.Signals.Signal(false);
	readonly #prefix = new Moq.Signals.Signal<Moq.Path.Valid>(DEFAULT_PREFIX);
	readonly #gamePrefixOverride = new Moq.Signals.Signal<Moq.Path.Valid | undefined>(undefined);
	readonly #viewerPrefixOverride = new Moq.Signals.Signal<Moq.Path.Valid | undefined>(undefined);
	readonly #sessions = new Map<string, Game>();
	#dispose?: () => void;

	constructor() {
		super();
		cleanup.register(this, this.#signals);

		this.#signals.run((effect) => {
			this.#enabled.set(effect.get(this.#connected) && effect.get(this.#refused).size === 0);
		});

		// One origin, both directions: viewer broadcasts are published into it and the
		// relay's announced games arrive in it, with no risk of echoing either back.
		this.connection = new Moq.Connection({
			enabled: this.#enabled,
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
		this.#connected.set(true);

		// Render the UI into a Shadow DOM (reuse existing on reconnect).
		const shadow = this.shadowRoot ?? this.attachShadow({ mode: "open" });
		this.#dispose = render(() => <BoyUI boy={this} />, shadow);
	}

	/** Called by the browser when the element leaves its document. */
	disconnectedCallback() {
		this.#connected.set(false);
		this.#dispose?.();
		this.#dispose = undefined;
	}

	/** Called by the browser when an observed attribute changes. */
	attributeChangedCallback(name: (typeof OBSERVED)[number], _oldValue: string | null, newValue: string | null) {
		if (name === "url") {
			this.connection.url.set(newValue ? new URL(newValue) : undefined);
			return;
		}

		// Refused until parsed, so an invalid path leaves the element idle and the error rethrown.
		this.#refuse(name, true);
		const path = newValue === null ? undefined : Moq.Path.from(newValue);
		this.#refuse(name, false);

		if (name === "prefix") {
			this.#prefix.set(path ?? DEFAULT_PREFIX);
		} else if (name === "prefix-game") {
			this.#gamePrefixOverride.set(path);
		} else {
			this.#viewerPrefixOverride.set(path);
		}
	}

	#refuse(name: Observed, refused: boolean) {
		const next = new Set(this.#refused.peek());
		if (refused) next.add(name);
		else next.delete(name);
		this.#refused.set(next);
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

	/** The path prefix for viewer broadcasts, mirrored by the `prefix-viewer` attribute. */
	get prefixViewer(): string {
		return this.#viewerPrefixOverride.peek() ?? Moq.Path.from(this.#prefix.peek(), "viewer");
	}

	set prefixViewer(value: string) {
		this.#viewerPrefixOverride.set(Moq.Path.from(value));
		this.#refuse("prefix-viewer", false);
	}

	#runDiscovery(effect: Moq.Signals.Effect) {
		const base = effect.get(this.#prefix);
		const gamePrefix = effect.get(this.#gamePrefixOverride) ?? Moq.Path.from(base, "game");
		const viewerPrefix = effect.get(this.#viewerPrefixOverride) ?? Moq.Path.from(base, "viewer");

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
				if (entry.kind !== "end" && !this.#sessions.has(id)) {
					const config: GameConfig = {
						sessionId: id,
						connection: this.connection,
						origin: this.origin,
						expanded: this.expanded,
						gamePrefix,
						viewerPrefix,
					};
					const game = new Game(config);
					this.#sessions.set(id, game);
					this.games.set(new Map(this.#sessions));
				} else if (entry.kind === "end") {
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

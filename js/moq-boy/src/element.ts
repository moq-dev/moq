import * as Moq from "@moq/lite";
import type { GameConfig } from "./index.ts";
import { Game } from "./index.ts";

const OBSERVED = ["url", "game-prefix", "viewer-prefix"] as const;
type Observed = (typeof OBSERVED)[number];

const DEFAULT_GAME_PREFIX = "anon/boy/game";
const DEFAULT_VIEWER_PREFIX = "anon/boy/viewer";

const cleanup = new FinalizationRegistry<Moq.Signals.Effect>((signals) => signals.close());

/**
 * `<moq-boy>` web component — discovers and manages Game Boy streaming sessions.
 *
 * Connects to a MoQ relay, discovers game sessions via announcements,
 * and creates Game instances for each. The UI layer (moq-boy-ui) renders
 * the visual interface by reading signals from this element.
 *
 * Attributes:
 *   - `url` — MoQ relay URL
 *   - `game-prefix` — Path prefix for game broadcasts (default: "anon/boy/game")
 *   - `viewer-prefix` — Path prefix for viewer broadcasts (default: "anon/boy/viewer")
 */
export default class MoqBoy extends HTMLElement {
	static observedAttributes = OBSERVED;

	readonly connection: Moq.Connection.Reload;
	readonly expanded = new Moq.Signals.Signal<string | undefined>(undefined);

	/** Reactive map of active game sessions. Emits on add/remove. */
	readonly games = new Moq.Signals.Signal<ReadonlyMap<string, Game>>(new Map());

	readonly #signals = new Moq.Signals.Effect();
	readonly #enabled = new Moq.Signals.Signal(false);
	readonly #gamePrefix = new Moq.Signals.Signal(DEFAULT_GAME_PREFIX);
	readonly #viewerPrefix = new Moq.Signals.Signal(DEFAULT_VIEWER_PREFIX);
	readonly #sessions = new Map<string, Game>();

	constructor() {
		super();
		cleanup.register(this, this.#signals);

		this.connection = new Moq.Connection.Reload({ enabled: this.#enabled });
		this.#signals.cleanup(() => this.connection.close());

		// Discover game sessions via announcements.
		this.#signals.run(this.#runDiscovery.bind(this));
	}

	connectedCallback() {
		this.#enabled.set(true);
	}

	disconnectedCallback() {
		this.#enabled.set(false);
	}

	attributeChangedCallback(name: Observed, _oldValue: string | null, newValue: string | null) {
		switch (name) {
			case "url":
				this.connection.url.set(newValue ? new URL(newValue) : undefined);
				break;
			case "game-prefix":
				this.#gamePrefix.set(newValue ?? DEFAULT_GAME_PREFIX);
				break;
			case "viewer-prefix":
				this.#viewerPrefix.set(newValue ?? DEFAULT_VIEWER_PREFIX);
				break;
		}
	}

	get url(): URL | undefined {
		return this.connection.url.peek();
	}

	set url(value: string | URL | undefined) {
		this.connection.url.set(value ? new URL(value) : undefined);
	}

	get gamePrefix(): string {
		return this.#gamePrefix.peek();
	}

	set gamePrefix(value: string) {
		this.#gamePrefix.set(value);
	}

	get viewerPrefix(): string {
		return this.#viewerPrefix.peek();
	}

	set viewerPrefix(value: string) {
		this.#viewerPrefix.set(value);
	}

	#runDiscovery(effect: Moq.Signals.Effect) {
		const conn = effect.get(this.connection.established);
		if (!conn) return;

		const gamePrefix = effect.get(this.#gamePrefix);
		const viewerPrefix = effect.get(this.#viewerPrefix);
		const prefix = Moq.Path.from(gamePrefix);

		const announced = conn.announced(prefix);
		effect.cleanup(() => announced.close());

		effect.spawn(async () => {
			for (;;) {
				const entry = await Promise.race([effect.cancel, announced.next()]);
				if (!entry) break;

				// Strip prefix, skip nested paths (e.g. "viewer/..." sub-broadcasts).
				const suffix = Moq.Path.stripPrefix(prefix, entry.path);
				if (!suffix || suffix.includes("/")) continue;

				const id = suffix;
				if (entry.active && !this.#sessions.has(id)) {
					const config: GameConfig = {
						sessionId: id,
						connection: this.connection,
						expanded: this.expanded,
						gamePrefix,
						viewerPrefix,
					};
					const game = new Game(config);
					this.#sessions.set(id, game);
					this.games.set(new Map(this.#sessions));
				} else if (!entry.active) {
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

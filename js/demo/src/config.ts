import { Moq, Signals } from "@moq/hang";
import type MoqWatch from "@moq/watch/element";

type Effect = Signals.Effect;
const { Effect, Signal } = Signals;

/**
 * A simple web component for configuring the relay URL and broadcast name.
 * Uses the watch element's connection for discovery instead of creating its own.
 */
export default class MoqWatchConfig extends HTMLElement {
	#urlInput: HTMLInputElement;
	#pathInput: HTMLInputElement;
	#suggestions: HTMLDivElement;
	#signals = new Effect();

	// The watch element to use for connection and broadcast name.
	#watch: MoqWatch | undefined;

	// The list of discovered broadcast names, updated reactively.
	#broadcasts = new Signal<string[]>([]);

	constructor() {
		super();

		// Create URL input
		const urlLabel = document.createElement("label");
		urlLabel.textContent = "Relay URL";
		urlLabel.style.cssText = "display: block; font-size: 0.85rem; color: #888; margin-bottom: 0.25rem;";

		this.#urlInput = document.createElement("input");
		this.#urlInput.type = "url";
		this.#urlInput.placeholder = "http://localhost:4443/anon";
		this.#urlInput.style.cssText = `
			width: 100%; padding: 0.5rem; margin-bottom: 0.75rem;
			background: #111; border: 1px solid #333; border-radius: 4px;
			color: #fff; font-family: monospace; font-size: 0.9rem;
		`;

		// Create path input
		const pathLabel = document.createElement("label");
		pathLabel.textContent = "Broadcast";
		pathLabel.style.cssText = "display: block; font-size: 0.85rem; color: #888; margin-bottom: 0.25rem;";

		this.#pathInput = document.createElement("input");
		this.#pathInput.type = "text";
		this.#pathInput.placeholder = "bbb";
		this.#pathInput.style.cssText = `
			width: 100%; padding: 0.5rem;
			background: #111; border: 1px solid #333; border-radius: 4px;
			color: #fff; font-family: monospace; font-size: 0.9rem;
		`;

		// Create suggestions container
		this.#suggestions = document.createElement("div");
		this.#suggestions.style.cssText = "margin-top: 0.5rem; font-size: 0.85rem;";

		// Append elements
		this.appendChild(urlLabel);
		this.appendChild(this.#urlInput);
		this.appendChild(pathLabel);
		this.appendChild(this.#pathInput);
		this.appendChild(this.#suggestions);

		// Event listeners
		this.#urlInput.addEventListener("input", () => this.#onUrlChange());
		this.#pathInput.addEventListener("input", () => this.#onPathChange());

		// Reactively discover broadcasts when the connection changes.
		this.#signals.run(this.#runDiscovery.bind(this));

		// Reactively render suggestions when broadcasts or selected name changes.
		this.#signals.run(this.#runRender.bind(this));
	}

	set watch(watch: MoqWatch) {
		this.#watch = watch;

		// Sync the URL input with the watch element's URL.
		this.#signals.run((effect) => {
			const url = effect.get(watch.connection.url);
			this.#urlInput.value = url?.toString() ?? "";
		});

		// Sync the name input with the watch element's broadcast name.
		this.#signals.run((effect) => {
			const name = effect.get(watch.broadcast.name);
			this.#pathInput.value = name.toString();
		});
	}

	get watch(): MoqWatch | undefined {
		return this.#watch;
	}

	connectedCallback() {
		this.style.cssText = "display: block; margin: 1rem 0;";
	}

	disconnectedCallback() {
		this.#signals.close();
	}

	static get observedAttributes() {
		return ["url", "name"];
	}

	attributeChangedCallback(name: string, _oldValue: string | null, newValue: string | null) {
		if (!this.#watch) return;

		if (name === "url") {
			this.#watch.connection.url.set(newValue ? new URL(newValue) : undefined);
		} else if (name === "name") {
			this.#watch.broadcast.name.set(Moq.Path.from(newValue ?? ""));
		}
	}

	get url(): string {
		return this.#urlInput.value;
	}

	get name(): string {
		return this.#pathInput.value;
	}

	#onUrlChange() {
		if (this.#watch) {
			this.#watch.connection.url.set(this.#urlInput.value ? new URL(this.#urlInput.value) : undefined);
		}
	}

	#onPathChange() {
		if (this.#watch) {
			this.#watch.broadcast.name.set(Moq.Path.from(this.#pathInput.value));
		}
	}

	#runDiscovery(effect: Effect) {
		const watch = this.#watch;
		if (!watch) return;

		const connection = effect.get(watch.connection.established);
		if (!connection) {
			this.#broadcasts.set([]);
			return;
		}

		const announced = connection.announced(Moq.Path.empty());
		effect.cleanup(() => announced.close());

		const active = new Map<string, boolean>();

		effect.spawn(async () => {
			try {
				for (;;) {
					const entry = await Promise.race([effect.cancel, announced.next()]);
					if (!entry) break;

					if (entry.active) {
						active.set(entry.path, true);
					} else {
						active.delete(entry.path);
					}

					this.#broadcasts.set([...active.keys()]);
				}
			} catch {
				// Connection closed or effect cancelled
			}
		});
	}

	#runRender(effect: Effect) {
		const broadcasts = effect.get(this.#broadcasts);

		// Also react to the selected name changing.
		const selected = this.#watch ? effect.get(this.#watch.broadcast.name).toString() : "";

		this.#clearSuggestions();

		if (broadcasts.length === 0) return;

		const label = document.createElement("span");
		label.textContent = "Available: ";
		label.style.color = "#666";
		this.#suggestions.appendChild(label);

		for (const name of broadcasts) {
			const isSelected = name === selected;
			const tag = document.createElement("button");
			tag.type = "button";
			tag.textContent = name;

			const defaultBg = isSelected ? "#2d4a2d" : "#1a2e1a";
			const defaultBorder = isSelected ? "#4ade80" : "#2d4a2d";

			tag.style.cssText = `
				background: ${defaultBg}; color: #4ade80; border: 1px solid ${defaultBorder};
				padding: 0.2rem 0.5rem; margin: 0 0.25rem; border-radius: 4px;
				font-size: 0.8rem; font-family: monospace; cursor: pointer;
				font-weight: ${isSelected ? "bold" : "normal"};
				transition: background 0.15s, border-color 0.15s;
			`;
			if (!isSelected) {
				tag.addEventListener("mouseenter", () => {
					tag.style.background = "#2d4a2d";
					tag.style.borderColor = "#4ade80";
				});
				tag.addEventListener("mouseleave", () => {
					tag.style.background = defaultBg;
					tag.style.borderColor = defaultBorder;
				});
			}
			tag.addEventListener("click", () => {
				if (this.#watch) {
					this.#watch.broadcast.name.set(Moq.Path.from(name));
				}
			});
			this.#suggestions.appendChild(tag);
		}
	}

	#clearSuggestions() {
		while (this.#suggestions.firstChild) {
			this.#suggestions.removeChild(this.#suggestions.firstChild);
		}
	}
}

customElements.define("moq-watch-config", MoqWatchConfig);

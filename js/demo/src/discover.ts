import { Moq, Signals } from "@moq/hang";
import type MoqWatch from "@moq/watch/element";

/**
 * Wraps a <moq-watch> element and live discovers new broadcasts available at the given URL.
 * Displays clickable broadcast names below the player.
 */
export default class MoqDiscover extends HTMLElement {
	#pathInput: HTMLInputElement;
	#suggestions: HTMLDivElement;
	#signals = new Signals.Effect();

	constructor() {
		super();

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

		// Event listeners
		this.#pathInput.addEventListener("input", () => this.#onPathChange());
	}

	async connectedCallback() {
		this.style.cssText = "display: block; margin: 1rem 0;";

		// Discover the inner moq-watch element.
		await customElements.whenDefined("moq-watch");
		const watch = this.querySelector("moq-watch") as MoqWatch | null;
		if (!watch) return;

		// Append the discovery UI after the existing children.
		this.appendChild(this.#pathInput);
		this.appendChild(this.#suggestions);

		// Sync the name input with the watch element's broadcast name.
		this.#signals.run((effect) => {
			const name = effect.get(watch.broadcast.name);
			this.#pathInput.value = name.toString();
		});

		// Reactively render suggestions when broadcasts or selected name changes.
		this.#signals.run((effect) => {
			const broadcasts = effect.get(watch.connection.announced);
			const selected = effect.get(watch.broadcast.name).toString();

			this.#clearSuggestions();

			if (broadcasts.size === 0) return;

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
					watch.broadcast.name.set(Moq.Path.from(name));
				});
				this.#suggestions.appendChild(tag);
			}
		});
	}

	disconnectedCallback() {
		this.#signals.close();
	}

	#onPathChange() {
		const watch = this.querySelector("moq-watch") as MoqWatch | null;
		if (watch) {
			watch.broadcast.name.set(Moq.Path.from(this.#pathInput.value));
		}
	}

	#clearSuggestions() {
		while (this.#suggestions.firstChild) {
			this.#suggestions.removeChild(this.#suggestions.firstChild);
		}
	}
}

customElements.define("moq-discover", MoqDiscover);

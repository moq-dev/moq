import { Moq } from "@moq/hang";

/**
 * A simple web component for configuring the relay URL and broadcast name.
 * Auto-discovers available broadcasts and shows them as clickable suggestions.
 */
export default class MoqWatchConfig extends HTMLElement {
	#urlInput: HTMLInputElement;
	#pathInput: HTMLInputElement;
	#suggestions: HTMLDivElement;
	#discoveryConnection: Moq.Connection.Established | null = null;
	#discoveryTimeout: ReturnType<typeof setTimeout> | null = null;
	#discoveryAbort: AbortController | null = null;

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
	}

	connectedCallback() {
		this.style.cssText = "display: block; margin: 1rem 0;";

		// Initialize from attributes
		const url = this.getAttribute("url");
		const name = this.getAttribute("name");

		if (url) this.#urlInput.value = url;
		if (name) this.#pathInput.value = name;

		// Start discovery if URL is set
		if (url) this.#scheduleDiscovery();
	}

	disconnectedCallback() {
		this.#closeDiscovery();
	}

	static get observedAttributes() {
		return ["url", "name"];
	}

	attributeChangedCallback(name: string, _oldValue: string | null, newValue: string | null) {
		if (name === "url" && newValue !== this.#urlInput.value) {
			this.#urlInput.value = newValue || "";
			this.#scheduleDiscovery();
		} else if (name === "name" && newValue !== this.#pathInput.value) {
			this.#pathInput.value = newValue || "";
		}
	}

	get url(): string {
		return this.#urlInput.value;
	}

	get name(): string {
		return this.#pathInput.value;
	}

	#onUrlChange() {
		this.setAttribute("url", this.#urlInput.value);
		this.#dispatchChange();
		this.#scheduleDiscovery();
	}

	#onPathChange() {
		this.setAttribute("name", this.#pathInput.value);
		this.#dispatchChange();
	}

	#dispatchChange() {
		this.dispatchEvent(
			new CustomEvent("change", {
				detail: { url: this.url, name: this.name },
				bubbles: true,
			}),
		);
	}

	#scheduleDiscovery() {
		// Debounce discovery
		if (this.#discoveryTimeout) {
			clearTimeout(this.#discoveryTimeout);
		}
		this.#discoveryTimeout = setTimeout(() => this.#discover(), 500);
	}

	async #discover() {
		const relayUrl = this.#urlInput.value.trim();
		if (!relayUrl) {
			this.#suggestions.innerHTML = "";
			return;
		}

		// Abort any in-flight discovery to prevent orphaned connections
		this.#discoveryAbort?.abort();
		this.#discoveryAbort = new AbortController();
		const signal = this.#discoveryAbort.signal;

		this.#closeDiscovery();
		this.#suggestions.innerHTML = '<span style="color: #666;">Discovering...</span>';

		try {
			const url = new URL(relayUrl);
			const connection = await Moq.Connection.connect(url);

			// Check if this discovery was aborted while connecting
			if (signal.aborted) {
				connection.close();
				return;
			}

			this.#discoveryConnection = connection;

			const announced = connection.announced(Moq.Path.empty());
			const broadcasts: string[] = [];
			const timeout = 2000;
			const startTime = Date.now();

			while (Date.now() - startTime < timeout) {
				const remaining = Math.max(0, timeout - (Date.now() - startTime));
				const timeoutPromise = new Promise<undefined>((resolve) =>
					setTimeout(() => resolve(undefined), remaining),
				);

				const entry = await Promise.race([announced.next(), timeoutPromise]);
				if (entry === undefined) break;
				if (entry.active) {
					broadcasts.push(entry.path);
					this.#renderSuggestions(broadcasts);
				}
			}

			announced.close();

			if (broadcasts.length === 0) {
				this.#suggestions.innerHTML = '<span style="color: #666;">No broadcasts found</span>';
			}
		} catch (err) {
			console.error("Discovery error:", err);
			this.#suggestions.innerHTML = '<span style="color: #666;">Discovery unavailable</span>';
		} finally {
			// Close the connection after discovery to avoid resource leaks
			this.#closeDiscovery();
		}
	}

	#renderSuggestions(broadcasts: string[]) {
		this.#suggestions.innerHTML = "";

		const label = document.createElement("span");
		label.textContent = "Available: ";
		label.style.color = "#666";
		this.#suggestions.appendChild(label);

		broadcasts.forEach((name) => {
			const tag = document.createElement("button");
			tag.type = "button";
			tag.textContent = name;
			tag.style.cssText = `
				background: #1a2e1a; color: #4ade80; border: 1px solid #2d4a2d;
				padding: 0.2rem 0.5rem; margin: 0 0.25rem; border-radius: 4px;
				font-size: 0.8rem; font-family: monospace; cursor: pointer;
				transition: background 0.15s, border-color 0.15s;
			`;
			tag.addEventListener("mouseenter", () => {
				tag.style.background = "#2d4a2d";
				tag.style.borderColor = "#4ade80";
			});
			tag.addEventListener("mouseleave", () => {
				tag.style.background = "#1a2e1a";
				tag.style.borderColor = "#2d4a2d";
			});
			tag.addEventListener("click", () => {
				this.#pathInput.value = name;
				this.#onPathChange();
			});
			this.#suggestions.appendChild(tag);
		});
	}

	#closeDiscovery() {
		if (this.#discoveryConnection) {
			this.#discoveryConnection.close();
			this.#discoveryConnection = null;
		}
	}
}

customElements.define("moq-watch-config", MoqWatchConfig);

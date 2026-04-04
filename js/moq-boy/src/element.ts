import * as Moq from "@moq/lite";
import { GameCard } from "./index.ts";
import { gridStyles } from "./styles.ts";

const OBSERVED = ["url"] as const;
type Observed = (typeof OBSERVED)[number];

const cleanup = new FinalizationRegistry<Moq.Signals.Effect>((signals) => signals.close());

export default class MoqBoy extends HTMLElement {
	static observedAttributes = OBSERVED;

	connection: Moq.Connection.Reload;
	#signals = new Moq.Signals.Effect();
	#enabled = new Moq.Signals.Signal(false);
	#sessions = new Map<string, GameCard>();
	#expanded = new Moq.Signals.Signal<string | undefined>(undefined);
	#gridEl: HTMLDivElement;
	#emptyState: HTMLDivElement;
	#statusEl: HTMLSpanElement;

	constructor() {
		super();

		cleanup.register(this, this.#signals);

		const shadow = this.attachShadow({ mode: "open" });

		// Inject styles.
		const style = document.createElement("style");
		style.textContent = gridStyles;
		shadow.appendChild(style);

		// Header.
		const header = document.createElement("header");
		const h1 = document.createElement("h1");
		h1.textContent = "MoQ Boy";
		this.#statusEl = document.createElement("span");
		this.#statusEl.className = "status";
		this.#statusEl.textContent = "Disconnected";
		header.appendChild(h1);
		header.appendChild(this.#statusEl);
		shadow.appendChild(header);

		// Grid.
		this.#gridEl = document.createElement("div");
		this.#gridEl.className = "grid";
		shadow.appendChild(this.#gridEl);

		// Empty state.
		this.#emptyState = document.createElement("div");
		this.#emptyState.className = "empty-state";
		this.#emptyState.style.display = "block";

		const emptyIcon = document.createElement("div");
		emptyIcon.className = "icon";
		emptyIcon.textContent = "\u{1F3AE}";
		this.#emptyState.appendChild(emptyIcon);

		const emptyMsg = document.createElement("div");
		emptyMsg.className = "msg";
		emptyMsg.textContent = "No games online";
		this.#emptyState.appendChild(emptyMsg);

		const emptyHint = document.createElement("div");
		emptyHint.className = "hint";
		emptyHint.textContent = "Waiting for Game Boy sessions to connect...";
		this.#emptyState.appendChild(emptyHint);

		this.#gridEl.appendChild(this.#emptyState);

		// About section.
		const about = document.createElement("div");
		about.className = "about";

		const aboutP1 = document.createElement("p");
		aboutP1.textContent = "Click a game to play. Everyone controls the same game (anarchy mode).";
		about.appendChild(aboutP1);

		const aboutP2 = document.createElement("p");
		aboutP2.textContent = "A generic ";
		const moqLink = document.createElement("a");
		moqLink.href = "https://moq.dev";
		moqLink.textContent = "MoQ";
		aboutP2.appendChild(moqLink);
		aboutP2.appendChild(document.createTextNode(" relay is used for everything:"));
		about.appendChild(aboutP2);

		const aboutUl = document.createElement("ul");
		for (const text of [
			"Discovering online games and players.",
			"Transmitting audio/video tracks, metadata, and (multiple) player controls.",
			"Subscribing to audio/video on-demand.",
			"Pausing emulation/encoding when there are no subscribers.",
		]) {
			const li = document.createElement("li");
			li.textContent = text;
			aboutUl.appendChild(li);
		}
		about.appendChild(aboutUl);
		shadow.appendChild(about);

		// Connection.
		this.connection = new Moq.Connection.Reload({ enabled: this.#enabled });
		this.#signals.cleanup(() => this.connection.close());

		// Track connection status.
		this.#signals.run((e) => {
			const status = e.get(this.connection.status);
			this.#statusEl.textContent = status.charAt(0).toUpperCase() + status.slice(1);
			this.#statusEl.style.color =
				status === "connected" ? "#8bac0f" : status === "connecting" ? "#facc15" : "#888";
		});

		// Discover game sessions via announcements.
		this.#signals.run((effect) => {
			const conn = effect.get(this.connection.established);
			if (!conn) return;

			const announced = conn.announced(Moq.Path.from("boy"));
			effect.cleanup(() => announced.close());

			effect.spawn(async () => {
				for (;;) {
					const entry = await Promise.race([effect.cancel, announced.next()]);
					if (!entry) break;

					// Strip "boy/" prefix, skip nested paths like "boy/x/viewer/..."
					const suffix = Moq.Path.stripPrefix(Moq.Path.from("boy"), entry.path);
					if (!suffix || suffix.includes("/")) continue;

					const id = suffix;
					if (entry.active && !this.#sessions.has(id)) {
						const card = new GameCard({
							sessionId: id,
							connection: this.connection,
							expanded: this.#expanded,
							root: shadow,
						});
						this.#sessions.set(id, card);
						this.#gridEl.appendChild(card.el);
						this.#updateEmptyState();
					} else if (!entry.active) {
						const card = this.#sessions.get(id);
						if (card) {
							card.close();
							card.el.remove();
							this.#sessions.delete(id);
							this.#updateEmptyState();
						}
					}
				}
			});
		});
	}

	#updateEmptyState() {
		this.#emptyState.style.display = this.#sessions.size === 0 ? "block" : "none";
	}

	connectedCallback() {
		this.#enabled.set(true);
	}

	disconnectedCallback() {
		this.#enabled.set(false);
	}

	attributeChangedCallback(name: Observed, _oldValue: string | null, newValue: string | null) {
		if (name === "url") {
			this.connection.url.set(newValue ? new URL(newValue) : undefined);
		}
	}

	get url(): URL | undefined {
		return this.connection.url.peek();
	}

	set url(value: string | URL | undefined) {
		this.connection.url.set(value ? new URL(value) : undefined);
	}
}

customElements.define("moq-boy", MoqBoy);

declare global {
	interface HTMLElementTagNameMap {
		"moq-boy": MoqBoy;
	}
}

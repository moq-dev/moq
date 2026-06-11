import { Effect, Signal } from "@moq/signals";
import * as DOM from "@moq/signals/dom";
import type MoqWatch from "../element";
import { bufferingIndicator } from "./components/buffering-indicator";
import { centerPlay } from "./components/center-play";
import { controlBar } from "./components/control-bar";
import { offlineIndicator } from "./components/offline-indicator";
import { settingsPanel } from "./components/settings-panel";
import type { Tab, UiState } from "./state";
import styles from "./styles/index.css?inline";

// How long the chrome lingers after the pointer stops moving (while playing).
const HIDE_MS = 2800;

export default class MoqWatchUi extends HTMLElement {
	#signals?: Effect;
	#root: ShadowRoot;
	#watch = new Signal<MoqWatch | undefined>(undefined);
	#observer: MutationObserver;

	constructor() {
		super();
		this.#root = this.attachShadow({ mode: "open" });

		const style = document.createElement("style");
		style.textContent = styles;
		this.#root.appendChild(style);

		this.#observer = new MutationObserver(() => this.#updateWatch());
	}

	connectedCallback() {
		this.#updateWatch();
		this.#observer.observe(this, { childList: true });

		const signals = new Effect();
		this.#signals = signals;
		signals.run(this.#render.bind(this));
	}

	disconnectedCallback() {
		this.#observer.disconnect();
		this.#signals?.close();
		this.#signals = undefined;
	}

	#updateWatch() {
		const watch = this.querySelector("moq-watch") as MoqWatch | null;
		this.#watch.set(watch ?? undefined);
	}

	#render(effect: Effect) {
		const watch = effect.get(this.#watch);
		if (!watch) return;

		const state: UiState = {
			chrome: new Signal(true),
			panel: new Signal(false),
			tab: new Signal<Tab>("quality"),
		};

		const player = DOM.create("div", { className: "player" });

		// The slotted <moq-watch> (canvas/video) sits at the base of the stack.
		player.appendChild(DOM.create("slot"));

		// Center affordances: play prompt + buffering spinner + offline notice.
		const center = DOM.create("div", { className: "center" });
		center.append(centerPlay(effect, watch), bufferingIndicator(effect, watch), offlineIndicator(effect, watch));

		// Top scrim keeps the bottom bar legible and hosts ambient gradient.
		const scrimTop = DOM.create("div", { className: "scrim scrim--top" });

		// Bottom chrome: gradient scrim + the control bar.
		const chrome = DOM.create("div", { className: "chrome" });
		chrome.append(DOM.create("div", { className: "scrim scrim--bottom" }), controlBar(effect, watch, state));

		const panel = settingsPanel(effect, watch, state);

		player.append(scrimTop, center, chrome, panel);
		DOM.render(effect, this.#root, player);

		this.#wireChrome(effect, watch, state, player);
	}

	// Show the chrome on activity, auto-hide while playing once the pointer
	// settles. Stays pinned while paused or when the settings panel is open.
	#wireChrome(effect: Effect, watch: MoqWatch, state: UiState, player: HTMLElement) {
		let hideTimer: ReturnType<typeof setTimeout> | undefined;
		const clearHide = () => {
			if (hideTimer !== undefined) {
				clearTimeout(hideTimer);
				hideTimer = undefined;
			}
		};
		effect.cleanup(clearHide);

		const pinned = () => watch.backend.paused.peek() || state.panel.peek();

		const show = () => {
			state.chrome.set(true);
			clearHide();
			if (!pinned()) hideTimer = setTimeout(() => state.chrome.set(false), HIDE_MS);
		};

		effect.event(this, "pointermove", show);
		effect.event(this, "pointerdown", show);
		effect.event(this, "focusin", show);
		effect.event(this, "pointerleave", () => {
			if (pinned()) return;
			clearHide();
			state.chrome.set(false);
		});

		// Becoming pinned immediately reveals (and locks) the chrome.
		effect.run((e) => {
			if (e.get(watch.backend.paused) || e.get(state.panel)) {
				state.chrome.set(true);
				clearHide();
			}
		});

		effect.run((e) => {
			player.classList.toggle("player--chrome", e.get(state.chrome));
		});
	}
}

customElements.define("moq-watch-ui", MoqWatchUi);

declare global {
	interface HTMLElementTagNameMap {
		"moq-watch-ui": MoqWatchUi;
	}
}

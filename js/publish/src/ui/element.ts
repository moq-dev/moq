import { Effect, Signal } from "@moq/signals";
import * as DOM from "@moq/signals/dom";
import type MoqPublish from "../element";
import { center } from "./components/center";
import { controlBar } from "./components/control-bar";
import { settingsPanel } from "./components/settings-panel";
import type { Tab, UiState } from "./state";
import styles from "./styles/index.css?inline";

// How long the chrome lingers after the pointer stops moving while previewing.
const HIDE_MS = 2800;

export default class MoqPublishUi extends HTMLElement {
	#signals?: Effect;
	#root: ShadowRoot;
	#publish = new Signal<MoqPublish | undefined>(undefined);
	#observer: MutationObserver;
	#initialized = false;

	constructor() {
		super();
		this.#root = this.attachShadow({ mode: "open" });

		const style = document.createElement("style");
		style.textContent = styles;
		this.#root.appendChild(style);

		this.#observer = new MutationObserver(() => this.#updatePublish());
	}

	connectedCallback() {
		this.#updatePublish();
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

	#updatePublish() {
		const publish = this.querySelector("moq-publish") as MoqPublish | null;
		this.#publish.set(publish ?? undefined);
	}

	#render(effect: Effect) {
		const publish = effect.get(this.#publish);
		if (!publish) return;

		// Start idle (no capture) unless the host was preconfigured via HTML/JS,
		// so we don't clobber the user's state.
		if (!this.#initialized) {
			this.#initialized = true;
			const pristine =
				publish.state.source.peek() === undefined &&
				!publish.state.muted.peek() &&
				!publish.state.invisible.peek();
			if (pristine) {
				publish.muted = true;
				publish.invisible = true;
			}
		}

		const state: UiState = {
			chrome: new Signal(true),
			panel: new Signal(false),
			tab: new Signal<Tab>("source"),
		};

		const player = DOM.create("div", { className: "player" });
		player.appendChild(DOM.create("slot"));

		const scrimTop = DOM.create("div", { className: "scrim scrim--top" });

		const chrome = DOM.create("div", { className: "chrome" });
		chrome.append(
			DOM.create("div", { className: "scrim scrim--bottom" }),
			controlBar(effect, publish, state, player),
		);

		const panel = settingsPanel(effect, publish, state);

		player.append(scrimTop, center(effect, publish), chrome, panel);
		DOM.render(effect, this.#root, player);

		// Reserve a 16:9 box (and show a backdrop) when there's no live preview.
		effect.run((e) => {
			const hasPreview = !!e.get(publish.broadcast.video.source);
			player.classList.toggle("player--empty", !hasPreview);
		});

		this.#wireChrome(effect, publish, state, player);
	}

	// Show the chrome on activity, auto-hide while previewing once the pointer
	// settles. Stays pinned while idle (no source) or with the panel open.
	#wireChrome(effect: Effect, publish: MoqPublish, state: UiState, player: HTMLElement) {
		let hideTimer: ReturnType<typeof setTimeout> | undefined;
		const clearHide = () => {
			if (hideTimer !== undefined) {
				clearTimeout(hideTimer);
				hideTimer = undefined;
			}
		};
		effect.cleanup(clearHide);

		const pinned = () => state.panel.peek() || publish.state.source.peek() === undefined;

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

		effect.run((e) => {
			if (e.get(state.panel) || e.get(publish.state.source) === undefined) {
				state.chrome.set(true);
				clearHide();
			}
		});

		effect.run((e) => {
			player.classList.toggle("player--chrome", e.get(state.chrome));
		});
	}
}

customElements.define("moq-publish-ui", MoqPublishUi);

declare global {
	interface HTMLElementTagNameMap {
		"moq-publish-ui": MoqPublishUi;
	}
}

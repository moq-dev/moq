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
		chrome.append(
			DOM.create("div", { className: "scrim scrim--bottom" }),
			controlBar(effect, watch, state, player),
		);

		const panel = settingsPanel(effect, watch, state);

		player.append(scrimTop, center, chrome, panel);
		DOM.render(effect, this.#root, player);

		this.#fitMedia(effect, watch, player);
		this.#wireChrome(effect, watch, state, player, chrome, panel);
	}

	#fullscreen(player: HTMLElement): boolean {
		const doc = document as Document & { webkitFullscreenElement?: Element | null };
		// Firefox reports shadow fullscreen on the shadow root.
		const root = this.#root as ShadowRoot & { fullscreenElement?: Element | null };
		return (
			root.fullscreenElement === player ||
			document.fullscreenElement === player ||
			doc.webkitFullscreenElement === player ||
			player.classList.contains("player--pseudo-fullscreen")
		);
	}

	#fitMedia(effect: Effect, watch: MoqWatch, player: HTMLElement) {
		const apply = () => {
			const media = watch.querySelector("canvas, video") as HTMLElement | null;
			if (!media) return;
			const fullscreen = this.#fullscreen(player);

			watch.style.width = "100%";
			watch.style.height = fullscreen ? "100%" : "";

			media.style.width = "100%";
			media.style.height = fullscreen ? "100%" : "auto";
			media.style.maxWidth = "100%";
			media.style.maxHeight = "100%";
			media.style.objectFit = "contain";
		};

		const observer = new MutationObserver(apply);
		observer.observe(watch, { childList: true, subtree: true });

		const playerObserver = new MutationObserver(apply);
		playerObserver.observe(player, { attributes: true, attributeFilter: ["class"] });

		effect.cleanup(() => observer.disconnect());
		effect.cleanup(() => playerObserver.disconnect());

		effect.event(document, "fullscreenchange", apply);
		effect.event(document, "webkitfullscreenchange", apply);
		effect.event(this.#root, "fullscreenchange", apply);
		apply();
	}

	// Show the chrome on activity, auto-hide while playing once the pointer
	// settles. Stays pinned while paused or when the settings panel is open.
	#wireChrome(
		effect: Effect,
		watch: MoqWatch,
		state: UiState,
		player: HTMLElement,
		chrome: HTMLElement,
		panel: HTMLElement,
	) {
		// Mobile devices don't support hover, so the controls remain visible in non-fullscreen mode.
		// In fullscreen mode, tap the video to toggle the controls.
		const touchUi = window.matchMedia("(pointer: coarse)").matches || window.matchMedia("(hover: none)").matches;
		if (touchUi) {
			this.#wireTouchChrome(effect, state, player, chrome, panel);
			return;
		}

		// Bump on any pointer/focus activity to re-arm the auto-hide.
		const activity = new Signal(0);
		const bump = () => activity.update((n) => n + 1);
		effect.event(this, "pointermove", bump);
		effect.event(this, "pointerdown", bump);
		effect.event(this, "focusin", bump);

		const pinned = () => watch.controls.paused.peek() || state.panel.peek();

		// Reveal on activity and reschedule the hide timer. Reruns when pinned
		// state changes too, so leaving pinned (e.g. closing settings while
		// playing) re-arms the auto-hide. effect.timer auto-clears on rerun.
		effect.run((e) => {
			const isPinned = e.get(watch.controls.paused) || e.get(state.panel);
			e.get(activity);
			state.chrome.set(true);
			if (isPinned) return;
			e.timer(() => state.chrome.set(false), HIDE_MS);
		});

		effect.event(this, "pointerleave", () => {
			if (pinned()) return;
			state.chrome.set(false);
		});

		effect.run((e) => {
			player.classList.toggle("player--chrome", e.get(state.chrome));
		});
	}

	#wireTouchChrome(effect: Effect, state: UiState, player: HTMLElement, chrome: HTMLElement, panel: HTMLElement) {
		state.chrome.set(true);

		const interactive = (event: Event) =>
			event.composedPath().some((target) => target === chrome || target === panel);

		effect.event(player, "pointerdown", (event) => {
			if (interactive(event)) return;
			if (this.#fullscreen(player)) {
				state.chrome.update((shown) => !shown);
			} else {
				state.chrome.set(true);
			}
		});

		const showChrome = () => state.chrome.set(true);
		effect.event(document, "fullscreenchange", showChrome);
		effect.event(document, "webkitfullscreenchange", showChrome);
		effect.event(this.#root, "fullscreenchange", showChrome);

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

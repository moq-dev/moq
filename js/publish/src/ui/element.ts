/**
 * The `<moq-publish-ui>` custom element: capture controls wrapped around a `<moq-publish>`.
 *
 * Side-effectful: importing this registers the element.
 *
 * @module
 */
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
				publish.controls.source.peek() === undefined &&
				!publish.controls.muted.peek() &&
				!publish.controls.invisible.peek();
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

		// Keep the player box from jumping when video toggles off: reserve the
		// last live preview's aspect ratio (defaults to 16:9) instead of snapping.
		let lastAspect = 16 / 9;
		effect.run((e) => {
			const src = e.get(publish.capture.in.source);
			if (src) {
				player.classList.remove("player--empty");
				player.style.aspectRatio = "";
				const video = this.querySelector("video") as HTMLVideoElement | null;
				if (video) {
					const measure = () => {
						if (video.videoWidth > 0 && video.videoHeight > 0)
							lastAspect = video.videoWidth / video.videoHeight;
					};
					measure();
					e.event(video, "loadedmetadata", measure);
					e.event(video, "resize", measure);
				}
			} else {
				player.style.aspectRatio = String(lastAspect);
				player.classList.add("player--empty");
			}
		});

		this.#wireChrome(effect, publish, state, player);
	}

	// Show the chrome on activity, auto-hide while previewing once the pointer
	// settles. Stays pinned while idle (no source) or with the panel open.
	#wireChrome(effect: Effect, publish: MoqPublish, state: UiState, player: HTMLElement) {
		// Bump on any pointer/focus activity to re-arm the auto-hide.
		const activity = new Signal(0);
		const bump = () => activity.update((n) => n + 1);
		effect.event(this, "pointermove", bump);
		effect.event(this, "pointerdown", bump);
		effect.event(this, "focusin", bump);

		const pinned = () => state.panel.peek() || publish.controls.source.peek() === undefined;

		// Reveal on activity and reschedule the hide timer. Reruns when pinned
		// state changes too, so leaving pinned re-arms the auto-hide.
		// effect.timer auto-clears on rerun.
		effect.run((e) => {
			const isPinned = e.get(state.panel) || e.get(publish.controls.source) === undefined;
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
}

customElements.define("moq-publish-ui", MoqPublishUi);

declare global {
	interface HTMLElementTagNameMap {
		"moq-publish-ui": MoqPublishUi;
	}
}

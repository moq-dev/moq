import { Effect, Signal } from "@moq/signals";
import * as DOM from "@moq/signals/dom";
import type MoqWatch from "../element";
import { bufferControl } from "./components/buffer-control";
import { bufferingIndicator } from "./components/buffering-indicator";
import { fullscreenButton } from "./components/fullscreen-button";
import { playPauseButton } from "./components/play-pause";
import { qualitySelector } from "./components/quality-selector";
import { statsButton } from "./components/stats-button";
import { volumeSlider } from "./components/volume-slider";
import { watchStatusIndicator } from "./components/watch-status-indicator";
import { statsPanel } from "./stats";
import styles from "./styles/index.css?inline";

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

		const visible = new Signal(false);

		const videoContainer = DOM.create("div", { className: "video-container" });
		videoContainer.append(
			DOM.create("slot"),
			statsPanel(effect, watch, visible),
			bufferingIndicator(effect, watch),
		);

		const controls = DOM.create("div", { className: "controls" });

		const playback = DOM.create("div", { className: "playback-controls flex-align-center" });
		playback.append(
			playPauseButton(effect, watch),
			volumeSlider(effect, watch),
			watchStatusIndicator(effect, watch),
			statsButton(effect, visible),
			fullscreenButton(effect, watch),
		);

		const latency = DOM.create("div", { className: "latency-controls" });
		latency.append(bufferControl(effect, watch), qualitySelector(effect, watch));

		controls.append(playback, latency);

		DOM.render(effect, this.#root, videoContainer);
		DOM.render(effect, this.#root, controls);
	}
}

customElements.define("moq-watch-ui", MoqWatchUi);

declare global {
	interface HTMLElementTagNameMap {
		"moq-watch-ui": MoqWatchUi;
	}
}

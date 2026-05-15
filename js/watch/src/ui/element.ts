import { Effect, Signal } from "@moq/signals";
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

const cleanup = new FinalizationRegistry<Effect>((signals) => signals.close());

export default class MoqWatchUi extends HTMLElement {
	signals = new Effect();
	#root: ShadowRoot;
	#mounted = false;

	constructor() {
		super();
		cleanup.register(this, this.signals);
		this.#root = this.attachShadow({ mode: "open" });

		const style = document.createElement("style");
		style.textContent = styles;
		this.#root.appendChild(style);
	}

	connectedCallback() {
		if (this.#mounted) return;
		this.#mounted = true;

		void customElements.whenDefined("moq-watch").then(() => {
			const watch = this.querySelector("moq-watch") as MoqWatch | null;
			if (!watch) return;
			this.#mount(watch);
		});
	}

	#mount(watch: MoqWatch) {
		const root = this.#root;
		const visible = new Signal(false);

		const videoContainer = document.createElement("div");
		videoContainer.className = "watch-ui__video-container";

		const slot = document.createElement("slot");
		videoContainer.append(slot, statsPanel(this.signals, watch, visible), bufferingIndicator(this.signals, watch));
		root.appendChild(videoContainer);

		const controls = document.createElement("div");
		controls.className = "watch-ui__controls";

		const playback = document.createElement("div");
		playback.className = "watch-ui__playback-controls flex--align-center";
		playback.append(
			playPauseButton(this.signals, watch),
			volumeSlider(this.signals, watch),
			watchStatusIndicator(this.signals, watch),
			statsButton(this.signals, visible),
			fullscreenButton(this.signals, watch),
		);

		const latency = document.createElement("div");
		latency.className = "watch-ui__latency-controls";
		latency.append(bufferControl(this.signals, watch), qualitySelector(this.signals, watch));

		controls.append(playback, latency);
		root.appendChild(controls);
	}
}

customElements.define("moq-watch-ui", MoqWatchUi);

declare global {
	interface HTMLElementTagNameMap {
		"moq-watch-ui": MoqWatchUi;
	}
}

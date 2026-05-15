import { Effect } from "@moq/signals";
import type MoqPublish from "../element";
import { cameraSourceButton } from "./components/camera-source-button";
import { fileSourceButton } from "./components/file-source-button";
import { microphoneSourceButton } from "./components/microphone-source-button";
import { nothingSourceButton } from "./components/nothing-source-button";
import { publishStatusIndicator } from "./components/publish-status-indicator";
import { screenSourceButton } from "./components/screen-source-button";
import styles from "./styles/index.css?inline";

const cleanup = new FinalizationRegistry<Effect>((signals) => signals.close());

export default class MoqPublishUi extends HTMLElement {
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

		void customElements.whenDefined("moq-publish").then(() => {
			const publish = this.querySelector("moq-publish") as MoqPublish | null;
			if (!publish) return;
			this.#mount(publish);
		});
	}

	#mount(publish: MoqPublish) {
		// Start with "nothing" selected so the UI matches what the user sees.
		publish.muted = true;
		publish.invisible = true;
		publish.source = undefined;

		const root = this.#root;
		root.appendChild(document.createElement("slot"));

		const controls = document.createElement("div");
		controls.className = "publish-ui__controls flex--center flex--space-between";

		const selector = document.createElement("div");
		selector.className = "publish-ui__source-selector flex--center";

		const label = document.createElement("span");
		label.className = "publish-ui__source-label";
		label.textContent = "Source:";

		selector.append(
			label,
			microphoneSourceButton(this.signals, publish),
			cameraSourceButton(this.signals, publish),
			screenSourceButton(this.signals, publish),
			fileSourceButton(this.signals, publish),
			nothingSourceButton(this.signals, publish),
		);

		controls.append(selector, publishStatusIndicator(this.signals, publish));
		root.appendChild(controls);
	}
}

customElements.define("moq-publish-ui", MoqPublishUi);

declare global {
	interface HTMLElementTagNameMap {
		"moq-publish-ui": MoqPublishUi;
	}
}

import { Effect, Signal } from "@moq/signals";
import * as DOM from "@moq/signals/dom";
import type MoqPublish from "../element";
import { cameraSourceButton } from "./components/camera-source-button";
import { fileSourceButton } from "./components/file-source-button";
import { microphoneSourceButton } from "./components/microphone-source-button";
import { nothingSourceButton } from "./components/nothing-source-button";
import { publishStatusIndicator } from "./components/publish-status-indicator";
import { screenSourceButton } from "./components/screen-source-button";
import styles from "./styles/index.css?inline";

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

		this.#root.appendChild(DOM.create("slot"));

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

		// Start with "nothing" selected on first mount, but only if the host hasn't been
		// preconfigured (via HTML attributes or JS), so we don't clobber the user's state.
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

		const controls = DOM.create("div", { className: "controls flex-center flex-space-between" });

		const selector = DOM.create("div", { className: "source-selector flex-center" });
		selector.append(
			DOM.create("span", { className: "source-label" }, "Source:"),
			microphoneSourceButton(effect, publish),
			cameraSourceButton(effect, publish),
			screenSourceButton(effect, publish),
			fileSourceButton(effect, publish),
			nothingSourceButton(effect, publish),
		);

		controls.append(selector, publishStatusIndicator(effect, publish));

		DOM.render(effect, this.#root, controls);
	}
}

customElements.define("moq-publish-ui", MoqPublishUi);

declare global {
	interface HTMLElementTagNameMap {
		"moq-publish-ui": MoqPublishUi;
	}
}

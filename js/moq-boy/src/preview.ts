import * as Moq from "@moq/lite";
import * as Watch from "@moq/watch";
import { previewStyles } from "./styles.ts";

const OBSERVED = ["url", "name", "href"] as const;
type Observed = (typeof OBSERVED)[number];

const cleanup = new FinalizationRegistry<Moq.Signals.Effect>((signals) => signals.close());

// A lightweight single-game preview: live video + audio on hover.
// Use as a thumbnail on demo listing pages.
export default class MoqBoyPreview extends HTMLElement {
	static observedAttributes = OBSERVED;

	connection: Moq.Connection.Reload;
	#signals = new Moq.Signals.Effect();
	#enabled = new Moq.Signals.Signal(false);
	#name = new Moq.Signals.Signal<string>("");
	#href: string | null = null;

	constructor() {
		super();

		cleanup.register(this, this.#signals);

		const shadow = this.attachShadow({ mode: "open" });

		// Inject styles.
		const style = document.createElement("style");
		style.textContent = previewStyles;
		shadow.appendChild(style);

		// Canvas for video.
		const canvas = document.createElement("canvas");
		shadow.appendChild(canvas);

		// Label overlay.
		const label = document.createElement("div");
		label.className = "label";
		shadow.appendChild(label);

		// Update label when name changes.
		this.#signals.run((effect) => {
			const name = effect.get(this.#name);
			label.textContent = name;
		});

		// Click navigates to href or dispatches event.
		this.addEventListener("click", () => {
			if (this.#href) {
				window.location.href = this.#href;
			} else {
				this.dispatchEvent(new CustomEvent("select", { detail: { name: this.#name.peek() } }));
			}
		});

		// Connection.
		this.connection = new Moq.Connection.Reload({ enabled: this.#enabled });
		this.#signals.cleanup(() => this.connection.close());

		// Set up broadcast, video, and audio.
		const broadcastName = new Moq.Signals.Signal(Moq.Path.from(""));

		// Sync broadcast name with the name signal.
		this.#signals.run((effect) => {
			const name = effect.get(this.#name);
			if (name) {
				broadcastName.set(Moq.Path.from(`boy/${name}`));
			}
		});

		const broadcast = new Watch.Broadcast({
			connection: this.connection.established,
			name: broadcastName,
			enabled: this.#enabled,
		});
		this.#signals.cleanup(() => broadcast.close());

		const sync = new Watch.Sync({ jitter: 50 as Moq.Time.Milli });
		this.#signals.cleanup(() => sync.close());

		// Video.
		const videoSource = new Watch.Video.Source(sync, { broadcast });
		this.#signals.cleanup(() => videoSource.close());

		// Native GB resolution for thumbnail.
		videoSource.target.set({ pixels: 160 * 144 });

		const videoDecoder = new Watch.Video.Decoder(videoSource);
		this.#signals.cleanup(() => videoDecoder.close());

		const videoRenderer = new Watch.Video.Renderer(videoDecoder, { canvas });
		this.#signals.cleanup(() => videoRenderer.close());

		// Audio — muted by default, unmute on hover.
		const audioSource = new Watch.Audio.Source(sync, { broadcast });
		this.#signals.cleanup(() => audioSource.close());

		const audioDecoder = new Watch.Audio.Decoder(audioSource);
		this.#signals.cleanup(() => audioDecoder.close());

		const audioEmitter = new Watch.Audio.Emitter(audioDecoder, { volume: 0.5, muted: true });
		this.#signals.cleanup(() => audioEmitter.close());

		// Resume AudioContext on first user interaction.
		const resumeEvents = ["click", "touchstart", "mousedown", "keydown"];
		const resumeAudio = () => {
			const ctx = audioDecoder.context.peek();
			if (ctx && ctx.state === "suspended") {
				ctx.resume();
			}
		};
		for (const event of resumeEvents) {
			document.addEventListener(event, resumeAudio, { once: true });
		}
		this.#signals.cleanup(() => {
			for (const event of resumeEvents) {
				document.removeEventListener(event, resumeAudio);
			}
		});

		// Track hover state.
		const hovered = new Moq.Signals.Signal(false);
		this.addEventListener("mouseenter", () => hovered.set(true));
		this.addEventListener("mouseleave", () => hovered.set(false));

		// Enable audio decoding only when hovered.
		this.#signals.run((effect) => {
			const hover = effect.get(hovered);
			audioDecoder.enabled.set(hover);
			audioEmitter.muted.set(!hover);
		});
	}

	connectedCallback() {
		this.#enabled.set(true);
	}

	disconnectedCallback() {
		this.#enabled.set(false);
	}

	attributeChangedCallback(attr: Observed, _oldValue: string | null, newValue: string | null) {
		if (attr === "url") {
			this.connection.url.set(newValue ? new URL(newValue) : undefined);
		} else if (attr === "name") {
			this.#name.set(newValue ?? "");
		} else if (attr === "href") {
			this.#href = newValue;
		}
	}

	get url(): URL | undefined {
		return this.connection.url.peek();
	}

	set url(value: string | URL | undefined) {
		this.connection.url.set(value ? new URL(value) : undefined);
	}

	get name(): string {
		return this.#name.peek();
	}

	set name(value: string) {
		this.#name.set(value);
	}

	get href(): string | null {
		return this.#href;
	}

	set href(value: string | null) {
		this.#href = value;
	}
}

customElements.define("moq-boy-preview", MoqBoyPreview);

declare global {
	interface HTMLElementTagNameMap {
		"moq-boy-preview": MoqBoyPreview;
	}
}

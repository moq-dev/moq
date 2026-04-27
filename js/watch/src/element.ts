import type { Time } from "@moq/lite";
import * as Moq from "@moq/lite";
import { Effect, Signal } from "@moq/signals";
import { MultiBackend } from "./backend";
import { Broadcast, type CatalogFormat } from "./broadcast";
import type { Latency } from "./sync";

function parseCatalogFormat(value: string | null): CatalogFormat {
	return value === "msf" ? "msf" : "hang";
}

const OBSERVED = ["url", "name", "paused", "volume", "muted", "reload", "latency", "jitter", "catalog-format"] as const;
type Observed = (typeof OBSERVED)[number];

// Close everything when this element is garbage collected.
// This is primarily to avoid a console.warn that we didn't close() before GC.
// There's no destructor for web components so this is the best we can do.
const cleanup = new FinalizationRegistry<Effect>((signals) => signals.close());

// An optional web component that wraps a <canvas>
export default class MoqWatch extends HTMLElement {
	static observedAttributes = OBSERVED;

	// The connection to the moq-relay server.
	connection: Moq.Connection.Reload;

	// The broadcast being watched.
	broadcast: Broadcast;

	// The backend that powers this element.
	backend: MultiBackend;

	// Set when the element is connected to the DOM.
	#enabled = new Signal(false);

	// Expose the Effect class, so users can easily create effects scoped to this element.
	signals = new Effect();

	constructor() {
		super();

		cleanup.register(this, this.signals);

		this.connection = new Moq.Connection.Reload({
			enabled: this.#enabled,
		});
		this.signals.cleanup(() => this.connection.close());

		this.broadcast = new Broadcast({
			connection: this.connection.established,
			announced: this.connection.announced,
			enabled: this.#enabled,
		});
		this.signals.cleanup(() => this.broadcast.close());

		// Flatten the RTT signal from the connection for the backend.
		const rtt = new Signal<number | undefined>(undefined);
		this.signals.run((effect) => {
			const conn = effect.get(this.connection.established);
			const rttSignal = conn?.rtt;
			rtt.set(rttSignal ? effect.get(rttSignal) : undefined);
		});

		this.backend = new MultiBackend({
			broadcast: this.broadcast,
			rtt,
		});
		this.signals.cleanup(() => this.backend.close());

		// Watch to see if the canvas element is added or removed.
		const setElement = () => {
			const canvas = this.querySelector("canvas") as HTMLCanvasElement | undefined;
			const video = this.querySelector("video") as HTMLVideoElement | undefined;
			if (canvas && video) {
				throw new Error("Cannot have both canvas and video elements");
			}
			this.backend.element.set(canvas ?? video);
		};

		const observer = new MutationObserver(setElement);
		observer.observe(this, { childList: true, subtree: true });
		this.signals.cleanup(() => observer.disconnect());
		setElement();

		// Optionally update attributes to match the library state.
		// This is kind of dangerous because it can create loops.
		// NOTE: This only runs when the element is connected to the DOM, which is not obvious.
		// This is because there's no destructor for web components to clean up our effects.
		this.signals.run((effect) => {
			const url = effect.get(this.connection.url);
			if (url) {
				this.setAttribute("url", url.toString());
			} else {
				this.removeAttribute("url");
			}
		});

		this.signals.run((effect) => {
			const name = effect.get(this.broadcast.name);
			this.setAttribute("name", name.toString());
		});

		this.signals.run((effect) => {
			const muted = effect.get(this.backend.audio.muted);
			if (muted) {
				this.setAttribute("muted", "");
			} else {
				this.removeAttribute("muted");
			}
		});

		this.signals.run((effect) => {
			const paused = effect.get(this.backend.paused);
			if (paused) {
				this.setAttribute("paused", "true");
			} else {
				this.removeAttribute("paused");
			}
		});

		this.signals.run((effect) => {
			const volume = effect.get(this.backend.audio.volume);
			this.setAttribute("volume", volume.toString());
		});

		this.signals.run((effect) => {
			const latency = effect.get(this.backend.latency);
			if (latency === "real-time") {
				this.setAttribute("latency", "real-time");
			} else {
				const jitter = Math.floor(effect.get(this.backend.jitter));
				this.setAttribute("latency", jitter.toString());
			}
		});
	}

	// Annoyingly, we have to use these callbacks to figure out when the element is connected to the DOM.
	// This wouldn't be so bad if there was a destructor for web components to clean up our effects.
	connectedCallback() {
		this.#enabled.set(true);
		this.style.display = "block";
		this.style.position = "relative";
	}

	disconnectedCallback() {
		// Stop everything but don't actually cleanup just in case we get added back to the DOM.
		this.#enabled.set(false);
	}

	#setLatencyNumber(value: string | null) {
		const parsed = value ? Number.parseFloat(value) : Number.NaN;
		this.backend.latency.set((Number.isFinite(parsed) ? parsed : 100) as Time.Milli);
	}

	attributeChangedCallback(name: Observed, oldValue: string | null, newValue: string | null) {
		if (oldValue === newValue) {
			return;
		}

		if (name === "url") {
			this.connection.url.set(newValue ? new URL(newValue) : undefined);
		} else if (name === "name") {
			this.broadcast.name.set(Moq.Path.from(newValue ?? ""));
		} else if (name === "paused") {
			this.backend.paused.set(newValue !== null);
		} else if (name === "volume") {
			const volume = newValue ? Number.parseFloat(newValue) : 0.5;
			this.backend.audio.volume.set(volume);
		} else if (name === "muted") {
			this.backend.audio.muted.set(newValue !== null);
		} else if (name === "reload") {
			this.broadcast.reload.set(newValue !== null);
		} else if (name === "latency") {
			if (!newValue || newValue === "real-time") {
				this.backend.latency.set("real-time");
			} else {
				this.#setLatencyNumber(newValue);
			}
		} else if (name === "jitter") {
			// Deprecated: use latency="<number>" instead.
			this.#setLatencyNumber(newValue);
		} else if (name === "catalog-format") {
			this.broadcast.catalogFormat.set(parseCatalogFormat(newValue));
		} else {
			const exhaustive: never = name;
			throw new Error(`Invalid attribute: ${exhaustive}`);
		}
	}

	get url(): URL | undefined {
		return this.connection.url.peek();
	}

	set url(value: string | URL | undefined) {
		this.connection.url.set(value ? new URL(value) : undefined);
	}

	get name(): Moq.Path.Valid {
		return this.broadcast.name.peek();
	}

	set name(value: string | Moq.Path.Valid) {
		this.broadcast.name.set(Moq.Path.from(value));
	}

	get paused(): boolean {
		return this.backend.paused.peek();
	}

	set paused(value: boolean) {
		this.backend.paused.set(value);
	}

	get volume(): number {
		return this.backend.audio.volume.peek();
	}

	set volume(value: number) {
		this.backend.audio.volume.set(value);
	}

	get muted(): boolean {
		return this.backend.audio.muted.peek();
	}

	set muted(value: boolean) {
		this.backend.audio.muted.set(value);
	}

	get reload(): boolean {
		return this.broadcast.reload.peek();
	}

	set reload(value: boolean) {
		this.broadcast.reload.set(value);
	}

	get latency(): Latency {
		return this.backend.latency.peek();
	}

	set latency(value: Latency) {
		this.backend.latency.set(value);
	}

	/** The jitter buffer in milliseconds. */
	get jitter(): Time.Milli {
		return this.backend.jitter.peek();
	}

	/** @deprecated Use `latency = <number>` instead. */
	set jitter(value: number) {
		this.backend.latency.set(value as Time.Milli);
	}

	get catalogFormat(): CatalogFormat {
		return this.broadcast.catalogFormat.peek();
	}

	set catalogFormat(value: CatalogFormat) {
		this.broadcast.catalogFormat.set(value);
	}
}

customElements.define("moq-watch", MoqWatch);

declare global {
	interface HTMLElementTagNameMap {
		"moq-watch": MoqWatch;
	}
}

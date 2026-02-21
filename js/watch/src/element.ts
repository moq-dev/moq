import type { Time } from "@moq/lite";
import * as Moq from "@moq/lite";
import { Effect, Signal } from "@moq/signals";
import { MultiBackend } from "./backend";
import { Broadcast } from "./broadcast";
import { Sync } from "./sync";

// TODO remove name; replaced with path
// TODO remove latency; replaced with buffer
const OBSERVED = ["url", "name", "path", "paused", "volume", "muted", "reload", "jitter", "latency"] as const;
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

	// Used to sync audio and video playback at a target latency.
	sync = new Sync();

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
			enabled: this.#enabled,
		});
		this.signals.cleanup(() => this.broadcast.close());

		this.backend = new MultiBackend({
			broadcast: this.broadcast,
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
			const broadcast = effect.get(this.broadcast.path);
			if (broadcast) {
				this.setAttribute("path", broadcast.toString());
			} else {
				this.removeAttribute("path");
			}
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
			const jitter = Math.floor(effect.get(this.backend.jitter));
			this.setAttribute("jitter", jitter.toString());
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

	attributeChangedCallback(name: Observed, oldValue: string | null, newValue: string | null) {
		if (oldValue === newValue) {
			return;
		}

		if (name === "url") {
			this.connection.url.set(newValue ? new URL(newValue) : undefined);
		} else if (name === "name" || name === "path") {
			this.broadcast.path.set(newValue ? Moq.Path.from(newValue) : undefined);
		} else if (name === "paused") {
			this.backend.paused.set(newValue !== null);
		} else if (name === "volume") {
			const volume = newValue ? Number.parseFloat(newValue) : 0.5;
			this.backend.audio.volume.set(volume);
		} else if (name === "muted") {
			this.backend.audio.muted.set(newValue !== null);
		} else if (name === "reload") {
			this.broadcast.reload.set(newValue !== null);
		} else if (name === "jitter" || name === "latency") {
			// "latency" is a legacy alias for "jitter"
			this.backend.jitter.set((newValue ? Number.parseFloat(newValue) : 100) as Time.Milli);
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

	get path(): Moq.Path.Valid | undefined {
		return this.broadcast.path.peek();
	}

	set path(value: string | Moq.Path.Valid | undefined) {
		this.broadcast.path.set(value ? Moq.Path.from(value) : undefined);
	}

	get paused(): boolean {
		return this.backend.paused.peek();
	}

	set paused(value: boolean) {
		this.backend.paused.set(value);
	}

	get jitter(): Time.Milli {
		return this.backend.jitter.peek();
	}

	set jitter(value: number) {
		this.backend.jitter.set(value as Time.Milli);
	}
}

customElements.define("moq-watch", MoqWatch);

declare global {
	interface HTMLElementTagNameMap {
		"moq-watch": MoqWatch;
	}
}

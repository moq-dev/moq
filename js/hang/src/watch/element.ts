import type { Time } from "@moq/lite";
import * as Moq from "@moq/lite";
import { Effect, Signal } from "@moq/signals";
import * as Audio from "./audio";
import { Broadcast } from "./broadcast";
import * as Video from "./video";

// TODO remove name; replaced with path
const OBSERVED = ["url", "name", "path", "paused", "volume", "muted", "reload", "buffer"] as const;
type Observed = (typeof OBSERVED)[number];

// Close everything when this element is garbage collected.
// This is primarily to avoid a console.warn that we didn't close() before GC.
// There's no destructor for web components so this is the best we can do.
const cleanup = new FinalizationRegistry<Effect>((signals) => signals.close());

// An optional web component that wraps a <canvas>
export default class HangWatch extends HTMLElement {
	static observedAttributes = OBSERVED;

	// The connection to the moq-relay server.
	connection: Moq.Connection.Reload;

	// The broadcast being watched.
	broadcast: Broadcast;

	// Responsible for rendering the video.
	video: Video.Renderer;

	// Responsible for emitting the audio.
	audio: Audio.Emitter;

	// The URL of the moq-relay server
	url = new Signal<URL | undefined>(undefined);

	// The path of the broadcast relative to the URL (may be empty).
	path = new Signal<Moq.Path.Valid | undefined>(undefined);

	// Whether audio/video playback is paused.
	paused = new Signal(false);

	// The volume of the audio, between 0 and 1.
	volume = new Signal(0.5);

	// Whether the audio is muted.
	muted = new Signal(false);

	// Whether the controls are shown.
	controls = new Signal(false);

	// Don't automatically reload the broadcast.
	// TODO: Temporarily defaults to false because Cloudflare doesn't support it yet.
	reload = new Signal(false);

	// Additional buffer in milliseconds on top of the catalog's minBuffer (default: 100ms).
	// The effective latency = catalog.minBuffer + buffer
	buffer = new Signal(100 as Time.Milli);

	// Set when the element is connected to the DOM.
	#enabled = new Signal(false);

	// The canvas element to render the video to.
	canvas = new Signal<HTMLCanvasElement | undefined>(undefined);

	// Expose the Effect class, so users can easily create effects scoped to this element.
	signals = new Effect();

	constructor() {
		super();

		cleanup.register(this, this.signals);

		this.connection = new Moq.Connection.Reload({
			url: this.url,
			enabled: this.#enabled,
		});
		this.signals.cleanup(() => this.connection.close());

		this.broadcast = new Broadcast({
			connection: this.connection.established,
			path: this.path,
			enabled: this.#enabled,
			reload: this.reload,
			sync: {
				buffer: this.buffer,
			},
		});
		this.signals.cleanup(() => this.broadcast.close());

		this.video = new Video.Renderer(this.broadcast.video, { canvas: this.canvas, paused: this.paused });
		this.signals.cleanup(() => this.video.close());

		this.audio = new Audio.Emitter(this.broadcast.audio, {
			volume: this.volume,
			muted: this.muted,
			paused: this.paused,
		});
		this.signals.cleanup(() => this.audio.close());

		// Watch to see if the canvas element is added or removed.
		const setCanvas = () => {
			this.canvas.set(this.querySelector("canvas") as HTMLCanvasElement | undefined);
		};
		const observer = new MutationObserver(setCanvas);
		observer.observe(this, { childList: true, subtree: true });
		this.signals.cleanup(() => observer.disconnect());
		setCanvas();

		// Optionally update attributes to match the library state.
		// This is kind of dangerous because it can create loops.
		// NOTE: This only runs when the element is connected to the DOM, which is not obvious.
		// This is because there's no destructor for web components to clean up our effects.
		this.signals.effect((effect) => {
			const url = effect.get(this.url);
			if (url) {
				this.setAttribute("url", url.toString());
			} else {
				this.removeAttribute("url");
			}
		});

		this.signals.effect((effect) => {
			const broadcast = effect.get(this.path);
			if (broadcast) {
				this.setAttribute("path", broadcast.toString());
			} else {
				this.removeAttribute("path");
			}
		});

		this.signals.effect((effect) => {
			const muted = effect.get(this.muted);
			if (muted) {
				this.setAttribute("muted", "");
			} else {
				this.removeAttribute("muted");
			}
		});

		this.signals.effect((effect) => {
			const paused = effect.get(this.paused);
			if (paused) {
				this.setAttribute("paused", "true");
			} else {
				this.removeAttribute("paused");
			}
		});

		this.signals.effect((effect) => {
			const volume = effect.get(this.volume);
			this.setAttribute("volume", volume.toString());
		});

		this.signals.effect((effect) => {
			const controls = effect.get(this.controls);
			if (controls) {
				this.setAttribute("controls", "");
			} else {
				this.removeAttribute("controls");
			}
		});

		this.signals.effect((effect) => {
			const buffer = Math.floor(effect.get(this.buffer));
			this.setAttribute("buffer", buffer.toString());
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
			this.url.set(newValue ? new URL(newValue) : undefined);
		} else if (name === "name" || name === "path") {
			this.path.set(newValue ? Moq.Path.from(newValue) : undefined);
		} else if (name === "paused") {
			this.paused.set(newValue !== null);
		} else if (name === "volume") {
			const volume = newValue ? Number.parseFloat(newValue) : 0.5;
			this.volume.set(volume);
		} else if (name === "muted") {
			this.muted.set(newValue !== null);
		} else if (name === "reload") {
			this.reload.set(newValue !== null);
		} else if (name === "buffer") {
			this.buffer.set((newValue ? Number.parseFloat(newValue) : 100) as Time.Milli);
		} else {
			const exhaustive: never = name;
			throw new Error(`Invalid attribute: ${exhaustive}`);
		}
	}
}

customElements.define("hang-watch", HangWatch);

declare global {
	interface HTMLElementTagNameMap {
		"hang-watch": HangWatch;
	}
}

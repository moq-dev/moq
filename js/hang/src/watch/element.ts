import type { Time } from "@moq/lite";
import * as Moq from "@moq/lite";
import { Effect, Signal } from "@moq/signals";
import type * as Audio from "./audio";
import { type Backend, MultiBackend } from "./backend";
import { Broadcast } from "./broadcast";
import { Sync } from "./sync";
import type * as Video from "./video";

// TODO remove name; replaced with path
// TODO remove latency; replaced with buffer
const OBSERVED = ["url", "name", "path", "paused", "volume", "muted", "reload", "jitter", "latency"] as const;
type Observed = (typeof OBSERVED)[number];

// Close everything when this element is garbage collected.
// This is primarily to avoid a console.warn that we didn't close() before GC.
// There's no destructor for web components so this is the best we can do.
const cleanup = new FinalizationRegistry<Effect>((signals) => signals.close());

/**
 * @tag hang-watch
 * @summary Watch live or on-demand video streams over Media over QUIC (MOQ)
 * @description A custom element that provides core playback functionality for watching video streams
 * transmitted over Media over QUIC protocol. Wraps a canvas or video element and handles connection,
 * decoding, synchronization, and rendering. Supports configurable jitter buffer, volume control,
 * pause/resume, and automatic reconnection.
 *
 * @attr {string} url - The WebTransport URL of the MOQ relay server (e.g., "https://relay.example.com")
 * @attr {string} path - The broadcast path to subscribe to (e.g., "/live/stream")
 * @attr {boolean} paused - Whether playback is paused
 * @attr {number} volume - Audio volume level (0.0 to 1.0, default: 0.5)
 * @attr {boolean} muted - Whether audio is muted
 * @attr {boolean} reload - Whether to automatically reconnect on connection loss
 * @attr {number} jitter - Target jitter buffer in milliseconds (default: 100)
 *
 * @slot default - Container for the canvas or video element
 *
 * @example HTML with canvas
 * ```html
 * <hang-watch url="https://relay.example.com" path="/live/stream" jitter="100" muted reload volume="0.5">
 *   <canvas style="width: 100%; height: auto;" width="1280" height="720"></canvas>
 * </hang-watch>
 * ```
 *
 * @example HTML with video element
 * ```html
 * <hang-watch url="https://relay.example.com" path="/broadcast" reload>
 *   <video style="width: 100%; height: auto;" controls></video>
 * </hang-watch>
 * ```
 *
 * @example React
 * ```tsx
 * import '@moq/hang/watch/element';
 * import { HangWatch } from '@moq/hang/react';
 *
 * export function HangWatchComponent({ url, path }) {
 *   return (
 *     <HangWatch url={url} path={path} jitter={100} muted reload volume={0}>
 *       <canvas style={{ width: '100%', height: 'auto' }} width="1280" height="720" />
 *     </HangWatch>
 *   );
 * }
 * ```
 */
export default class HangWatch extends HTMLElement implements Backend {
	static observedAttributes = OBSERVED;

	// The connection to the moq-relay server.
	connection: Moq.Connection.Reload;

	// The broadcast being watched.
	broadcast: Broadcast;

	// Used to sync audio and video playback at a target latency.
	sync = new Sync();

	// The backend that powers this element.
	#backend: MultiBackend;

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

		this.#backend = new MultiBackend({
			broadcast: this.broadcast,
		});
		this.signals.cleanup(() => this.#backend.close());

		// Watch to see if the canvas element is added or removed.
		const setElement = () => {
			const canvas = this.querySelector("canvas") as HTMLCanvasElement | undefined;
			const video = this.querySelector("video") as HTMLVideoElement | undefined;
			if (canvas && video) {
				throw new Error("Cannot have both canvas and video elements");
			}
			this.#backend.element.set(canvas ?? video);
		};

		const observer = new MutationObserver(setElement);
		observer.observe(this, { childList: true, subtree: true });
		this.signals.cleanup(() => observer.disconnect());
		setElement();

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
			const muted = effect.get(this.audio.muted);
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
			const volume = effect.get(this.audio.volume);
			this.setAttribute("volume", volume.toString());
		});

		this.signals.effect((effect) => {
			const jitter = Math.floor(effect.get(this.jitter));
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
			this.url.set(newValue ? new URL(newValue) : undefined);
		} else if (name === "name" || name === "path") {
			this.path.set(newValue ? Moq.Path.from(newValue) : undefined);
		} else if (name === "paused") {
			this.paused.set(newValue !== null);
		} else if (name === "volume") {
			const volume = newValue ? Number.parseFloat(newValue) : 0.5;
			this.audio.volume.set(volume);
		} else if (name === "muted") {
			this.audio.muted.set(newValue !== null);
		} else if (name === "reload") {
			this.broadcast.reload.set(newValue !== null);
		} else if (name === "jitter" || name === "latency") {
			// "latency" is a legacy alias for "jitter"
			this.jitter.set((newValue ? Number.parseFloat(newValue) : 100) as Time.Milli);
		} else {
			const exhaustive: never = name;
			throw new Error(`Invalid attribute: ${exhaustive}`);
		}
	}

	get url(): Signal<URL | undefined> {
		return this.connection.url;
	}

	set url(value: string | URL | undefined) {
		value ? this.setAttribute("url", String(value)) : this.removeAttribute("url");
	}

	get path(): Signal<Moq.Path.Valid | undefined> {
		return this.broadcast.path;
	}

	set path(value: string | Moq.Path.Valid | undefined) {
		value ? this.setAttribute("path", String(value)) : this.removeAttribute("path");
	}

	get jitter(): Signal<Time.Milli> {
		return this.#backend.jitter;
	}

	set jitter(value: string | Time.Milli | undefined) {
		value != null ? this.setAttribute("jitter", String(value)) : this.removeAttribute("jitter");
	}

	get paused(): Signal<boolean> {
		return this.#backend.paused;
	}

	get audio(): Audio.Backend {
		return this.#backend.audio;
	}

	get video(): Video.Backend {
		return this.#backend.video;
	}
}

customElements.define("hang-watch", HangWatch);

export { HangWatch };

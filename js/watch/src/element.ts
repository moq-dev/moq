/**
 * The `<moq-watch>` custom element: a broadcast player driven by HTML attributes.
 *
 * Side-effectful: importing this registers the element.
 *
 * @module
 */
import type * as Catalog from "@moq/hang/catalog";
import type { Time } from "@moq/net";
import * as Moq from "@moq/net";
import { Effect, Signal } from "@moq/signals";
import * as Audio from "./audio";
import { Broadcast, type CatalogFormat, parseCatalogFormat } from "./broadcast";
import { type Bound, type Latency, latencyBounds, latencyFromBounds, Sync } from "./sync";
import * as Video from "./video";

const OBSERVED = [
	"url",
	"name",
	"paused",
	"volume",
	"muted",
	"visible",
	"reload",
	"latency",
	"latency-min",
	"latency-max",
	"jitter",
	"catalog-format",
] as const;
type Observed = (typeof OBSERVED)[number];

// Parse the `visible` attribute into a Visible value, falling back to "20%".
function parseVisible(value: string | null): Video.Visible {
	const trimmed = value?.trim();
	if (!trimmed) return "20%";
	if (trimmed === "never" || trimmed === "always") return trimmed;
	// A CSS length usable as an IntersectionObserver rootMargin (px or %).
	if (/^-?\d+(\.\d+)?(px|%)$/.test(trimmed)) return trimmed;
	// Allow a bare number as a px convenience (e.g. visible="200").
	if (/^-?\d+(\.\d+)?$/.test(trimmed)) return `${trimmed}px`;
	console.warn(`moq-watch: invalid visible="${value}", expected "never", "always", or a CSS length like "200px"`);
	return "20%";
}

/**
 * Parse a boolean attribute: absent uses `defaultValue`, bare presence is true, and an explicit
 * `"false"`/`"0"` is false. Presence alone can't express false, and attributes that default to
 * true (`reload`) need to, so every boolean attribute accepts the explicit form.
 */
function parseBoolean(value: string | null, defaultValue: boolean): boolean {
	if (value === null) return defaultValue;
	const normalized = value.trim().toLowerCase();
	return normalized !== "false" && normalized !== "0";
}

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

	/** Downloads and decodes the video track. `video.source` picks the rendition. */
	video: Video.Decoder;

	/** Downloads and decodes the audio track. `audio.source` picks the rendition. */
	audio: Audio.Decoder;

	/** Paints decoded frames to the nested <canvas>. */
	renderer: Video.Renderer;

	/** Plays decoded samples through the speakers. */
	emitter: Audio.Emitter;

	/** Keeps audio and video playing at the target latency. */
	sync: Sync;

	// The mutable user controls. As the top of the tree, this element owns the
	// writable Signals and wires read-only views into the pipeline. The UI and
	// the attribute/property accessors read and write these directly.
	readonly controls = {
		paused: new Signal(false),
		volume: new Signal(0.5),
		muted: new Signal(false),
		// When video is downloaded relative to the canvas position. See {@link Video.Visible}.
		visible: new Signal<Video.Visible>("20%"),
		latency: new Signal<Latency>("real-time"),
		// The desired video rendition (resolution/bitrate cap).
		target: new Signal<Video.Target | undefined>(undefined),
	};

	// Broadcast configuration owned here and wired into `broadcast` as inputs.
	#name = new Signal<Moq.Path.Valid>(Moq.Path.empty());
	#reload = new Signal(true);
	#catalogFormat = new Signal<CatalogFormat | undefined>(undefined);
	#catalog = new Signal<Catalog.Root | undefined>(undefined);

	// The canvas element to render into.
	#canvas = new Signal<HTMLCanvasElement | undefined>(undefined);

	// Whether to download. Driven by the renderer/emitter policy, read by the decoders.
	#videoEnabled = new Signal(false);
	#audioEnabled = new Signal(false);

	// Set when the element is connected to the DOM.
	#enabled = new Signal(false);

	// Stashed volume to restore on unmute.
	#unmuteVolume = 0.5;

	/**
	 * Effects scoped to this element's lifetime, closed on disconnect.
	 *
	 * Public because the element is the top of the tree: it's where an application hangs its own
	 * reactivity. The components underneath keep theirs private, so `close()` is the only handle.
	 */
	readonly signals = new Effect();

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
			name: this.#name,
			reload: this.#reload,
			catalogFormat: this.#catalogFormat,
			catalog: this.#catalog,
		});
		this.signals.cleanup(() => this.broadcast.close());

		// The decoders' support probes drive rendition selection: anything WebCodecs can't play is filtered out.
		const videoSource = new Video.Source({
			broadcast: this.broadcast,
			target: this.controls.target,
			supported: Video.Decoder.supported,
		});
		const audioSource = new Audio.Source({
			broadcast: this.broadcast,
			supported: Audio.Decoder.supported,
		});
		this.signals.cleanup(() => {
			videoSource.close();
			audioSource.close();
		});

		// Sources produce the per-rendition jitter that Sync reads, so they're created
		// before Sync to avoid a construction cycle.
		this.sync = new Sync({
			latency: this.controls.latency,
			connection: this.connection.established,
			video: videoSource.out.jitter,
			audio: audioSource.out.jitter,
		});
		this.signals.cleanup(() => this.sync.close());

		this.video = new Video.Decoder(videoSource, this.sync, { enabled: this.#videoEnabled });
		this.audio = new Audio.Decoder(audioSource, this.sync, { enabled: this.#audioEnabled });
		this.signals.cleanup(() => {
			this.video.close();
			this.audio.close();
		});

		this.emitter = new Audio.Emitter(this.audio, {
			volume: this.controls.volume,
			muted: this.controls.muted,
			paused: this.controls.paused,
		});
		this.renderer = new Video.Renderer(this.video, {
			canvas: this.#canvas,
			visible: this.controls.visible,
		});
		this.signals.cleanup(() => {
			this.emitter.close();
			this.renderer.close();
		});

		// Audio download follows the emitter's enable policy (paused/muted).
		this.signals.proxy(this.#audioEnabled, this.emitter.out.enabled);

		// Video downloads while playing and on-screen. When paused, keep downloading only
		// until a frame is on the canvas, then stop: a cold paused start still shows a poster
		// instead of black, without streaming while paused. Read the rendered frame only in
		// the paused branch so playback doesn't re-run this every painted frame.
		this.signals.run((effect) => {
			const visible = effect.get(this.renderer.out.visible);
			if (!effect.get(this.controls.paused)) {
				this.#videoEnabled.set(visible);
				return;
			}
			const frame = effect.get(this.renderer.out.frame);
			this.#videoEnabled.set(visible && !frame);
		});

		// Mute/volume coupling. The element owns the writable volume/muted Signals, so
		// the policy lives here: muting stashes and zeroes the volume; a zero volume
		// reports as muted.
		this.signals.run((effect) => {
			const muted = effect.get(this.controls.muted);
			if (muted) {
				this.#unmuteVolume = this.controls.volume.peek() || 0.5;
				this.controls.volume.set(0);
			} else {
				this.controls.volume.set(this.#unmuteVolume);
			}
		});
		this.signals.run((effect) => {
			const volume = effect.get(this.controls.volume);
			this.controls.muted.set(volume === 0);
		});

		// Watch to see if the canvas element is added or removed.
		const setCanvas = () => {
			const canvas = this.querySelector("canvas") ?? undefined;

			// A <video> child used to render via MSE. Nothing renders it now, and audio still plays,
			// so the failure looks like a bug in the page instead of a removed feature.
			if (!canvas && this.querySelector("video")) {
				console.warn("moq-watch: rendering requires a <canvas> child; a <video> child does nothing.");
			}

			this.#canvas.set(canvas);
		};

		const observer = new MutationObserver(setCanvas);
		observer.observe(this, { childList: true, subtree: true });
		this.signals.cleanup(() => observer.disconnect());
		setCanvas();

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
			const name = effect.get(this.#name);
			this.setAttribute("name", name.toString());
		});

		this.signals.run((effect) => {
			const muted = effect.get(this.controls.muted);
			if (muted) {
				this.setAttribute("muted", "");
			} else {
				this.removeAttribute("muted");
			}
		});

		this.signals.run((effect) => {
			const paused = effect.get(this.controls.paused);
			if (paused) {
				this.setAttribute("paused", "");
			} else {
				this.removeAttribute("paused");
			}
		});

		this.signals.run((effect) => {
			const volume = effect.get(this.controls.volume);
			this.setAttribute("volume", volume.toString());
		});

		this.signals.run((effect) => {
			const visible = effect.get(this.controls.visible);
			this.setAttribute("visible", visible);
		});

		this.signals.run((effect) => {
			const { min, max } = latencyBounds(effect.get(this.controls.latency));
			// Only reflect the collapsed `latency` sugar attribute when the range is actually
			// collapsed. An open range is expressed via latency-min/latency-max, and writing
			// `latency` here would round-trip back through attributeChangedCallback and collapse it.
			if (min !== max) return;
			if (min === "real-time") {
				this.setAttribute("latency", "real-time");
			} else {
				const jitter = Math.floor(effect.get(this.sync.out.jitter));
				this.setAttribute("latency", jitter.toString());
			}
		});

		// Track the element's rendered size and feed it into the rendition picker,
		// scaled by devicePixelRatio so high-DPI screens still get sharp renditions.
		const updateDimensions = (width: number, height: number) => {
			if (width <= 0 || height <= 0) return;
			const dpr = window.devicePixelRatio || 1;
			this.controls.target.update((prev) => ({
				...prev,
				width: Math.round(width * dpr),
				height: Math.round(height * dpr),
			}));
		};

		const resizeObserver = new ResizeObserver((entries) => {
			const entry = entries[0];
			if (!entry) return;
			updateDimensions(entry.contentRect.width, entry.contentRect.height);
		});
		resizeObserver.observe(this);
		this.signals.cleanup(() => resizeObserver.disconnect());

		// Seed with the current size in case the observer doesn't fire immediately
		// (e.g. the element is still 0x0 when we attach).
		const rect = this.getBoundingClientRect();
		updateDimensions(rect.width, rect.height);
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

	// Parse a single latency bound: absent or "real-time" is adaptive, otherwise a fixed ms value.
	#parseBound(value: string | null): Bound {
		if (!value || value === "real-time") return "real-time";
		const parsed = Number.parseFloat(value);
		return Moq.Time.Milli(Number.isFinite(parsed) ? parsed : 100);
	}

	attributeChangedCallback(name: Observed, oldValue: string | null, newValue: string | null) {
		if (oldValue === newValue) {
			return;
		}

		if (name === "url") {
			this.connection.url.set(newValue ? new URL(newValue) : undefined);
		} else if (name === "name") {
			this.#name.set(Moq.Path.from(newValue ?? ""));
		} else if (name === "paused") {
			this.controls.paused.set(parseBoolean(newValue, false));
		} else if (name === "volume") {
			const volume = newValue ? Number.parseFloat(newValue) : 0.5;
			this.controls.volume.set(volume);
		} else if (name === "muted") {
			this.controls.muted.set(parseBoolean(newValue, false));
		} else if (name === "visible") {
			this.controls.visible.set(parseVisible(newValue));
		} else if (name === "reload") {
			this.#reload.set(parseBoolean(newValue, true));
		} else if (name === "latency") {
			// Sugar: collapse the floor and ceiling to a single value.
			this.latency = this.#parseBound(newValue);
		} else if (name === "latency-min") {
			this.latencyMin = this.#parseBound(newValue);
		} else if (name === "latency-max") {
			this.latencyMax = this.#parseBound(newValue);
		} else if (name === "jitter") {
			// Deprecated: use latency="<number>" instead.
			this.latency = this.#parseBound(newValue);
		} else if (name === "catalog-format") {
			this.#catalogFormat.set(parseCatalogFormat(newValue));
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
		return this.#name.peek();
	}

	set name(value: string | Moq.Path.Valid) {
		this.#name.set(Moq.Path.from(value));
	}

	get paused(): boolean {
		return this.controls.paused.peek();
	}

	set paused(value: boolean) {
		this.controls.paused.set(value);
	}

	get volume(): number {
		return this.controls.volume.peek();
	}

	set volume(value: number) {
		this.controls.volume.set(value);
	}

	get muted(): boolean {
		return this.controls.muted.peek();
	}

	set muted(value: boolean) {
		this.controls.muted.set(value);
	}

	get visible(): Video.Visible {
		return this.controls.visible.peek();
	}

	set visible(value: Video.Visible) {
		this.controls.visible.set(value);
	}

	get reload(): boolean {
		return this.#reload.peek();
	}

	set reload(value: boolean) {
		this.#reload.set(value);
	}

	/**
	 * The latency target. Assign a scalar (or `"real-time"`) to minimize latency, or an object
	 * `{ min, max }` to open a range and buffer future-dated frames. See {@link Latency}.
	 */
	get latency(): Latency {
		return this.controls.latency.peek();
	}

	set latency(value: Latency) {
		this.controls.latency.set(value);
	}

	/** The latency floor (jitter/startup buffer). Read-modify-writes `latency`, leaving the ceiling. */
	get latencyMin(): Bound {
		return latencyBounds(this.controls.latency.peek()).min;
	}

	set latencyMin(value: Bound) {
		const { max } = latencyBounds(this.controls.latency.peek());
		this.controls.latency.set(latencyFromBounds(value, max));
	}

	/**
	 * The latency ceiling: `"real-time"` (default) minimizes, a number caps at that many ms. A
	 * ceiling above the floor enables buffered playback: build up a buffer from future-dated frames
	 * (e.g. TTS written faster than real-time) and only skip ahead past the cap. Call `reset()` at
	 * each utterance boundary. Read-modify-writes `latency`, leaving the floor untouched.
	 */
	get latencyMax(): Bound {
		return latencyBounds(this.controls.latency.peek()).max;
	}

	set latencyMax(value: Bound) {
		const { min } = latencyBounds(this.controls.latency.peek());
		this.controls.latency.set(latencyFromBounds(min, value));
	}

	/** The jitter buffer in milliseconds. */
	get jitter(): Time.Milli {
		return this.sync.out.jitter.peek();
	}

	/**
	 * Re-anchor playback at an utterance boundary in buffered mode: reset the sync reference
	 * and flush the audio buffer so the next utterance plays from its own first frame.
	 */
	reset(): void {
		this.sync.reset();
		this.audio.reset();
	}

	get catalogFormat(): CatalogFormat | undefined {
		return this.#catalogFormat.peek();
	}

	set catalogFormat(value: CatalogFormat | undefined) {
		this.#catalogFormat.set(value);
	}

	/**
	 * The active catalog. Assign directly when `catalogFormat` is `"manual"`;
	 * for `"hang"` and `"msf"` this is overwritten by the fetch loop.
	 */
	get catalog(): Catalog.Root | undefined {
		return this.broadcast.out.catalog.peek();
	}

	set catalog(value: Catalog.Root | undefined) {
		this.#catalog.set(value);
	}
}

customElements.define("moq-watch", MoqWatch);

declare global {
	interface HTMLElementTagNameMap {
		"moq-watch": MoqWatch;
	}
}

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
import type * as Audio from "./audio";
import { type Broadcast, CATALOG_FORMATS, type CatalogFormat } from "./broadcast";
import { formatDuration, parseDuration } from "./duration";
import { Player } from "./player";
import type { Delay, Sync } from "./sync";
import type * as Text from "./text";
import type * as Video from "./video";

const OBSERVED = [
	"url",
	"name",
	"paused",
	"volume",
	"muted",
	"visible",
	"announced",
	"delay",
	"buffer",
	// Released spellings are observed only so assigning them can fail loudly instead of being ignored.
	"reload",
	"latency",
	"latency-min",
	"jitter",
	"catalog-format",
	"captions",
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

// Parse the `delay` attribute: "auto" (adaptive), "instant" (no buffer, no pacing), or a duration.
function parseDelay(value: string | null): Delay {
	const trimmed = value?.trim();
	if (!trimmed || trimmed === "auto") return "auto";
	if (trimmed === "instant") return "instant";
	const parsed = parseDuration(trimmed);
	if (parsed !== undefined) return parsed;
	console.warn(`moq-watch: invalid delay="${value}", expected "auto", "instant", or a duration like "300ms"`);
	return "auto";
}

// Parse the `buffer` attribute: a duration, or none when absent.
function parseBuffer(value: string | null): Time.Milli {
	const trimmed = value?.trim();
	if (!trimmed) return Moq.Time.Milli.zero;
	const parsed = parseDuration(trimmed);
	if (parsed !== undefined) return parsed;
	console.warn(`moq-watch: invalid buffer="${value}", expected a duration like "30s"`);
	return Moq.Time.Milli.zero;
}

/** Parse the element's catalog-format attribute. */
export function parseCatalogFormat(value: string | null): CatalogFormat | undefined {
	if (value === null) return undefined;
	return CATALOG_FORMATS.find((format) => format === value);
}

/**
 * Parse a boolean attribute: absent uses `defaultValue`, bare presence is true, and an explicit
 * `"false"`/`"0"` is false. Presence alone can't express false, and attributes that default to
 * true (`announced`) need to, so every boolean attribute accepts the explicit form.
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
	/**
	 * The relay connection, shared with every other element on the page pointing at the
	 * same URL. Its `origin` is where the broadcasts live.
	 */
	connection: Moq.Connection;

	/** Headless playback pipeline behind this element. */
	readonly player: Player;

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

	/** Selects the caption track. `text.out.available` lists the renditions for a picker. */
	text: Text.Source;

	/** Renders the selected text cues into an overlay above the canvas. */
	textRenderer: Text.Renderer;

	/** Keeps audio and video playing at the configured delay. */
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
		// How far playback trails the live edge.
		delay: new Signal<Delay>("auto"),
		// Future-dated media held beyond the live edge before playback skips ahead.
		buffer: new Signal<Time.Milli>(Moq.Time.Milli.zero),
		// The desired video rendition (resolution/bitrate cap).
		target: new Signal<Video.Target | undefined>(undefined),
		// The selected caption track name, or undefined for off (the default; captions are opt-in).
		captions: new Signal<string | undefined>(undefined),
	};

	// Broadcast configuration owned here and wired into `broadcast` as inputs.
	#name = new Signal<Moq.Path.Valid>(Moq.Path.empty());
	#announced = new Signal(true);
	#catalogFormat = new Signal<CatalogFormat | undefined>(undefined);
	#catalog = new Signal<Catalog.Root | undefined>(undefined);

	// The canvas element to render into.
	#canvas = new Signal<HTMLCanvasElement | undefined>(undefined);

	// The overlay element captions are drawn into, created lazily on connect (custom elements may not
	// touch children in their constructor). Positioned to fill the element, above the canvas.
	#captionsOverlay = new Signal<HTMLElement | undefined>(undefined);
	#captionsOverlayEl?: HTMLDivElement;

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

		this.connection = new Moq.Connection({
			enabled: this.#enabled,
		});
		this.signals.cleanup(() => this.connection.close());

		this.player = new Player({
			origin: this.connection.origin,
			probe: this.connection.probe,
			enabled: this.#enabled,
			name: this.#name,
			announced: this.#announced,
			catalogFormat: this.#catalogFormat,
			catalog: this.#catalog,
			canvas: this.#canvas,
			container: this.#captionsOverlay,
			...this.controls,
		});
		this.signals.cleanup(() => this.player.close());
		this.broadcast = this.player.broadcast;
		this.video = this.player.video;
		this.audio = this.player.audio;
		this.renderer = this.player.renderer;
		this.emitter = this.player.emitter;
		this.text = this.player.text;
		this.textRenderer = this.player.textRenderer;
		this.sync = this.player.sync;

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

		// Each knob is 1:1 with its attribute, so the echo back through attributeChangedCallback
		// parses to the value already held and the effect settles.
		this.signals.run((effect) => {
			const delay = effect.get(this.controls.delay);
			this.setAttribute("delay", typeof delay === "number" ? formatDuration(delay) : delay);
		});

		this.signals.run((effect) => {
			this.setAttribute("buffer", formatDuration(effect.get(this.controls.buffer)));
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

		// Create the caption overlay once, on first connect (the constructor may not add children).
		if (!this.#captionsOverlayEl) {
			const overlay = document.createElement("div");
			overlay.style.position = "absolute";
			overlay.style.inset = "0";
			overlay.style.pointerEvents = "none";
			// Above the canvas, which paints at the default stacking level.
			overlay.style.zIndex = "1";
			this.appendChild(overlay);
			this.#captionsOverlayEl = overlay;
			this.#captionsOverlay.set(overlay);
		}
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
		} else if (name === "announced") {
			this.#announced.set(parseBoolean(newValue, true));
		} else if (name === "delay") {
			this.controls.delay.set(parseDelay(newValue));
		} else if (name === "buffer") {
			this.controls.buffer.set(parseBuffer(newValue));
		} else if (name === "reload") {
			console.warn("moq-watch: `reload` was renamed to `announced`");
		} else if (name === "latency" || name === "latency-min" || name === "jitter") {
			console.warn(`moq-watch: \`${name}\` is gone; use \`delay\` and \`buffer\``);
		} else if (name === "catalog-format") {
			this.#catalogFormat.set(parseCatalogFormat(newValue));
		} else if (name === "captions") {
			// The selected caption track name; absent or empty turns captions off.
			this.controls.captions.set(newValue || undefined);
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

	get announced(): boolean {
		return this.#announced.peek();
	}

	set announced(value: boolean) {
		this.#announced.set(value);
	}

	/** @internal */
	set reload(_value: unknown) {
		throw new Error("moq-watch: `reload` was renamed to `announced`");
	}

	/**
	 * How far playback trails the live edge, in milliseconds. See {@link Delay}.
	 *
	 * `"auto"` (the default) sizes the jitter buffer from the connection RTT. `"instant"` drops the
	 * clock instead: video paints the moment it decodes and audio is disabled.
	 */
	get delay(): Delay {
		return this.controls.delay.peek();
	}

	set delay(value: Delay) {
		this.controls.delay.set(value);
	}

	/**
	 * Future-dated media held beyond the live edge before playback skips ahead, in milliseconds.
	 *
	 * Zero (the default) minimizes latency. A larger value enables buffered playback: build up a
	 * buffer from future-dated frames (e.g. TTS written faster than real-time) and only skip ahead
	 * once they would sit further than `delay + buffer` past the playhead. Call `reset()` at each
	 * utterance boundary.
	 */
	get buffer(): Time.Milli {
		return this.controls.buffer.peek();
	}

	set buffer(value: Time.Milli) {
		this.controls.buffer.set(value);
	}

	/** @internal */
	get latency(): Delay {
		return this.controls.delay.peek();
	}

	set latency(_value: unknown) {
		throw new Error("moq-watch: `latency` is gone; use `delay` and `buffer`");
	}

	/** @internal */
	get latencyMin(): Delay {
		return this.controls.delay.peek();
	}

	set latencyMin(_value: unknown) {
		throw new Error("moq-watch: `latencyMin` is gone; use `delay` and `buffer`");
	}

	/** @internal */
	set latencyMax(_value: unknown) {
		throw new Error(
			"moq-watch: `latencyMax` is gone. Use `buffer`, the media held beyond the live edge; the old ceiling included the floor, so it is `latencyMax - delay`.",
		);
	}

	/** The jitter buffer in milliseconds. */
	get jitter(): Time.Milli {
		return this.sync.out.jitter.peek();
	}

	set jitter(_value: unknown) {
		throw new Error("moq-watch: `jitter` is a readout; set `delay` instead");
	}

	/**
	 * Re-anchor playback at an utterance boundary in buffered mode: reset the sync reference
	 * and flush the audio buffer so the next utterance plays from its own first frame.
	 */
	reset(): void {
		this.player.reset();
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

	/**
	 * The selected caption track name, or `undefined` for off (the default). Captions are opt-in:
	 * assign a track name from `text.out.available` to turn them on. See the `text` source for the
	 * list of renditions the broadcast publishes.
	 */
	get captions(): string | undefined {
		return this.controls.captions.peek();
	}

	set captions(value: string | undefined) {
		this.controls.captions.set(value || undefined);
	}
}

customElements.define("moq-watch", MoqWatch);

declare global {
	interface HTMLElementTagNameMap {
		"moq-watch": MoqWatch;
	}
}

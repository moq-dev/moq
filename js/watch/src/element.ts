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
import { formatDuration, parseDuration } from "./duration";
import { type Delay, Sync } from "./sync";
import * as Text from "./text";
import * as Video from "./video";

const OBSERVED = [
	"url",
	"name",
	"paused",
	"volume",
	"muted",
	"visible",
	"reload",
	"delay",
	"buffer",
	// Released spellings, kept parsing but off the documented surface. `latency-max` is absent
	// deliberately: its old ceiling included the floor, so translating it faithfully would mean
	// tracking the resolved delay reactively, which is the coupling `buffer` exists to remove.
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

// The released `latency` / `latencyMin` property spellings, translated onto `delay`. The range
// object is refused rather than half-applied: its ceiling included the floor, so it has no faithful
// `buffer` without tracking the resolved delay, which is the coupling `buffer` exists to remove.
function coerceLegacyDelay(value: unknown): Delay {
	if (value === "instant") return "instant";
	if (value === undefined || value === "auto" || value === "real-time") return "auto";
	if (typeof value === "number" && Number.isFinite(value)) return Moq.Time.Milli(value);
	throw new Error(
		"moq-watch: the latency range is gone. Set `delay` (how far playback trails the live edge) and `buffer` (media held beyond it).",
	);
}

// The released spellings of `delay`, in bare milliseconds. Kept unitless so pages still on them
// behave exactly as they did; `delay` is the current surface and does require a unit.
function parseLegacyDelay(value: string | null): Delay {
	const trimmed = value?.trim();
	if (!trimmed || trimmed === "real-time") return "auto";
	if (trimmed === "instant") return "instant";
	const parsed = Number.parseFloat(trimmed);
	return Moq.Time.Milli(Number.isFinite(parsed) ? parsed : 100);
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
	/**
	 * The relay connection, shared with every other element on the page pointing at the
	 * same URL; see `Moq.Connection.Shared`. Its `origin` is where the broadcasts live.
	 */
	connection: Moq.Connection.Shared;

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

	/** Renders caption cues into an overlay above the canvas. */
	captionsRenderer: Text.Renderer;

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
	#reload = new Signal(true);
	#catalogFormat = new Signal<CatalogFormat | undefined>(undefined);
	#catalog = new Signal<Catalog.Root | undefined>(undefined);

	// The canvas element to render into.
	#canvas = new Signal<HTMLCanvasElement | undefined>(undefined);

	// The overlay element captions are drawn into, created lazily on connect (custom elements may not
	// touch children in their constructor). Positioned to fill the element, above the canvas.
	#captionsOverlay = new Signal<HTMLElement | undefined>(undefined);
	#captionsOverlayEl?: HTMLDivElement;

	// Whether to download. Driven by the renderer/emitter policy, read by the decoders.
	#captionsEnabled = new Signal(false);
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

		this.connection = new Moq.Connection.Shared({
			enabled: this.#enabled,
		});
		this.signals.cleanup(() => this.connection.close());

		this.broadcast = new Broadcast({
			origin: this.connection.origin,
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
			probe: this.connection.probe,
		});
		const audioSource = new Audio.Source({
			broadcast: this.broadcast,
			supported: Audio.Decoder.supported,
		});
		this.signals.cleanup(() => {
			videoSource.close();
			audioSource.close();
		});

		this.text = new Text.Source({
			broadcast: this.broadcast,
			target: this.controls.captions,
		});
		this.signals.cleanup(() => this.text.close());

		// The video decoder owns rendition handoffs but also needs Sync. Bridge its output through a
		// parent-owned signal so Sync can be constructed first without exposing mutable wiring.
		const videoJitter = new Signal<Time.Milli | undefined>(undefined);

		this.sync = new Sync({
			delay: this.controls.delay,
			buffer: this.controls.buffer,
			probe: this.connection.probe,
			video: videoJitter,
			audio: audioSource.out.jitter,
		});
		this.signals.cleanup(() => this.sync.close());

		this.video = new Video.Decoder(videoSource, this.sync, { enabled: this.#videoEnabled });
		this.signals.proxy(videoJitter, this.video.out.jitter);
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

		this.captionsRenderer = new Text.Renderer(this.text, this.sync, {
			container: this.#captionsOverlay,
			enabled: this.#captionsEnabled,
		});
		this.signals.cleanup(() => this.captionsRenderer.close());

		// Captions follow playback, like audio and video. The caption clock runs off wall time, so
		// leaving them on while paused scrolls text over a frozen frame.
		this.signals.run((effect) => {
			this.#captionsEnabled.set(effect.get(this.#enabled) && !effect.get(this.controls.paused));
		});

		// Audio download follows the emitter's enable policy (paused/muted), except an instant
		// delay turns it off outright: the ring needs a target depth to avoid underrunning, and
		// unpaced video has nothing pulling it back toward the audio clock.
		this.signals.run((effect) => {
			const enabled = effect.get(this.emitter.out.enabled);
			this.#audioEnabled.set(enabled && effect.get(this.controls.delay) !== "instant");
		});

		// Stopping the download leaves the ring holding a floor's worth of PCM, and the emitter
		// stays connected to drain it. Flush on the way in so audio stops now instead of playing
		// against video that just jumped to the live edge.
		this.signals.run((effect) => {
			if (effect.get(this.controls.delay) !== "instant") return;
			this.audio.reset();
		});

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
		} else if (name === "reload") {
			this.#reload.set(parseBoolean(newValue, true));
		} else if (name === "delay") {
			this.controls.delay.set(parseDelay(newValue));
		} else if (name === "buffer") {
			this.controls.buffer.set(parseBuffer(newValue));
		} else if (name === "latency" || name === "latency-min" || name === "jitter") {
			this.controls.delay.set(parseLegacyDelay(newValue));
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

	get reload(): boolean {
		return this.#reload.peek();
	}

	set reload(value: boolean) {
		this.#reload.set(value);
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

	set latency(value: unknown) {
		this.controls.delay.set(coerceLegacyDelay(value));
	}

	/** @internal */
	get latencyMin(): Delay {
		return this.controls.delay.peek();
	}

	set latencyMin(value: unknown) {
		this.controls.delay.set(coerceLegacyDelay(value));
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

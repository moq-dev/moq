/**
 * The `<moq-watch>` custom element: a broadcast player driven by HTML attributes.
 *
 * Side-effectful: importing this registers the element.
 *
 * @module
 */

import { Milli } from "@moq/clock";
import type * as Catalog from "@moq/hang/catalog";
import * as Moq from "@moq/net";
import { Derived, Effect, Signal } from "@moq/signals";
import type * as Audio from "./audio";
import { type Broadcast, CATALOG_FORMATS, type CatalogFormat } from "./broadcast";
import { formatDuration, parseDuration } from "./duration";
import { Player } from "./player";
import type { Delay, Sync } from "./sync";
import type * as Text from "./text";
import type * as Video from "./video";

function invalidAttribute(name: string, value: unknown, expected: string): Error {
	return new Error(`moq-watch: invalid ${name}="${value}", expected ${expected}`);
}

function parseUrl(value: string | null): URL | undefined {
	if (value === null) return undefined;
	try {
		return new URL(value);
	} catch {
		throw invalidAttribute("url", value, "an absolute URL");
	}
}

// Parse the `versions` attribute: a comma-separated list, or the defaults when absent or empty.
// The setter refuses an unsupported or duplicate name.
function parseVersions(value: string | null): string[] | undefined {
	const versions = value
		?.split(",")
		.map((version) => version.trim())
		.filter((version) => version !== "");
	return versions?.length ? versions : undefined;
}

function parseVolume(value: string | null): number {
	if (value === null) return 0.5;
	const volume = Number(value);
	if (value.trim() && Number.isFinite(volume) && volume >= 0 && volume <= 1) return volume;
	throw invalidAttribute("volume", value, "a number from 0 to 1");
}

/** The attributes the element observes. */
const OBSERVED = [
	"url",
	"versions",
	"name",
	"paused",
	"volume",
	"muted",
	"visible",
	"backend",
	"follow",
	// Renamed to `follow`, observed only to refuse it.
	"announced",
	"delay",
	"buffer",
	"catalog-format",
	"captions",
] as const;

// Parse visibility, with the default only when the attribute is absent.
function parseVisible(value: string | null): Video.Visible {
	if (value === null) return "20%";
	const trimmed = value.trim();
	if (trimmed === "never" || trimmed === "always") return trimmed;
	if (/^-?\d+(\.\d+)?(px|%)$/.test(trimmed)) return trimmed as Video.Visible;
	if (/^-?\d+(\.\d+)?$/.test(trimmed)) return `${trimmed}px`;
	throw invalidAttribute("visible", value, '"never", "always", or a CSS length like "200px"');
}

// Parse the graphics backend, with the default only when absent.
function parseBackend(value: string | null): Video.Backend {
	if (value === null) return "auto";
	const trimmed = value.trim();
	if (trimmed === "auto" || trimmed === "webgpu" || trimmed === "2d") return trimmed;
	throw invalidAttribute("backend", value, '"auto", "webgpu", or "2d"');
}

// Parse the `delay` attribute: "auto" (adaptive), "instant" (no buffer, no pacing), or a duration.
function parseDelay(value: string | null): Delay {
	if (value === null) return "auto";
	const trimmed = value.trim();
	if (trimmed === "auto" || trimmed === "instant") return trimmed;
	const parsed = parseDuration(trimmed);
	if (parsed !== undefined) return parsed;
	throw invalidAttribute("delay", value, '"auto", "instant", or a duration like "300ms"');
}

// Parse the `buffer` attribute: a duration, or zero when absent.
function parseBuffer(value: string | null): Milli {
	if (value === null) return Milli.zero;
	const parsed = parseDuration(value);
	if (parsed !== undefined) return parsed;
	throw invalidAttribute("buffer", value, 'a duration like "30s"');
}

/** Parse the element's catalog-format attribute, refusing unknown formats. */
export function parseCatalogFormat(value: string | null): CatalogFormat | undefined {
	if (value === null) return undefined;
	const format = CATALOG_FORMATS.find((format) => format === value);
	if (format) return format;
	throw invalidAttribute("catalog-format", value, CATALOG_FORMATS.join(", "));
}

/**
 * Parse a boolean attribute: absent uses `defaultValue`, bare presence is true, and an explicit
 * `"false"`/`"0"` is false. Presence alone can't express false, and attributes that default to
 * true (`follow`) need to, so every boolean attribute accepts the explicit form.
 */
function parseBoolean(value: string | null, defaultValue: boolean): boolean {
	if (value === null) return defaultValue;
	const normalized = value.trim().toLowerCase();
	return normalized !== "false" && normalized !== "0";
}

/** The `<moq-watch>` element: plays a broadcast into a nested <canvas>. */
export default class MoqWatch extends HTMLElement {
	/** The attributes the browser reports changes to. */
	static observedAttributes: typeof OBSERVED = OBSERVED;

	/**
	 * The relay connection, shared with every other element on the page pointing at the
	 * same URL. Its `origin` is where the broadcasts live.
	 */
	connection: Moq.Connection;

	/** Headless playback pipeline behind this element. */
	readonly player: Player;
	/** The broadcast being watched. */
	readonly broadcast: Broadcast;

	/** Downloads and decodes the video track. `video.source` picks the rendition. */
	readonly video: Video.Decoder;

	/** Downloads and decodes the audio track. `audio.source` picks the rendition. */
	readonly audio: Audio.Decoder;

	/** Paints decoded frames to the nested <canvas>. */
	readonly renderer: Video.Renderer;

	/** Plays decoded samples through the speakers. */
	emitter: Audio.Emitter;

	/** Selects the caption track. `text.out.available` lists the renditions for a picker. */
	text: Text.Source;

	/** Renders the selected text cues into an overlay above the canvas. */
	textRenderer: Text.Renderer;

	/** Keeps audio and video playing at the configured delay. */
	readonly sync: Sync;

	/**
	 * The mutable user controls. As the top of the tree, this element owns the
	 * writable Signals and wires read-only views into the pipeline. The UI and
	 * the attribute/property accessors read and write these directly.
	 */
	readonly controls = {
		paused: new Signal(false),
		// How far playback trails the live edge.
		delay: new Signal<Delay>("auto"),
		// Future-dated media held beyond the live edge before playback skips ahead.
		buffer: new Signal<Milli>(Milli.zero),
		video: {
			// The desired video rendition (resolution/bitrate cap).
			target: new Signal<Video.Target | undefined>(undefined),
			// When video is downloaded relative to the canvas position. See {@link Video.Visible}.
			visible: new Signal<Video.Visible>("20%"),
			// Which graphics API paints the canvas. See {@link Video.Backend}.
			backend: new Signal<Video.Backend>("auto"),
		},
		audio: {
			// The desired audio rendition, or automatic selection when unset.
			target: new Signal<Audio.Target | undefined>(undefined),
			volume: new Signal(0.5),
			muted: new Signal(false),
		},
		text: {
			// The selected caption track name, or undefined for off (the default; captions are opt-in).
			target: new Signal<string | undefined>(undefined),
		},
	};

	// Broadcast configuration owned here and wired into `broadcast` as inputs.
	#name = new Signal<Moq.Path.Valid>(Moq.Path.empty());
	#follow = new Signal(true);
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
	#connected = false;
	#invalid = new Signal<ReadonlySet<string>>(new Set());
	// Whether clearing the last invalid attribute re-enables media: false when the page had
	// already disabled the connection, so recovery does not override that.
	#resume = false;
	#reflecting = false;

	/**
	 * Effects owned by this element; media resources are suspended while disconnected.
	 *
	 * Public because the element is the top of the tree: it's where an application hangs its own
	 * reactivity. The components underneath keep theirs private, so `close()` is the only handle.
	 */
	readonly signals = new Effect();

	constructor() {
		super();

		this.connection = new Moq.Connection({
			enabled: this.#enabled,
		});
		this.signals.cleanup(() => this.connection.close());

		this.player = new Player({
			origin: this.connection.origin,
			probe: this.connection.probe,
			enabled: this.#enabled,
			name: this.#name,
			follow: this.#follow,
			catalogFormat: this.#catalogFormat,
			catalog: this.#catalog,
			...this.controls,
			video: {
				...this.controls.video,
				canvas: this.signals.computed((effect) =>
					effect.get(this.#enabled) ? effect.get(this.#canvas) : undefined,
				),
			},
			audio: {
				...this.controls.audio,
				// A zero level stops downloads without rewriting the user's mute control.
				muted: new Derived(
					[this.controls.audio.muted, this.controls.audio.volume],
					(muted, volume) => muted || volume === 0,
				),
			},
			text: { ...this.controls.text, container: this.#captionsOverlay },
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

		// A canvas that lost its GPU never hands out another context, so swap in a fresh copy. The
		// observer above picks it up, which clears the error and selects a renderer again.
		this.signals.run((effect) => {
			if (effect.get(this.renderer.out.error)?.name !== "InvalidStateError") return;
			// Peek: rerunning on the swap itself would replace the fresh canvas before the error clears.
			const canvas = this.#canvas.peek();
			canvas?.replaceWith(canvas.cloneNode());
		});

		// Reflect state without replaying its setters through attributeChangedCallback.
		this.signals.run((effect) => this.#reflect(effect, "url", effect.get(this.connection.url)?.toString()));
		this.signals.run((effect) =>
			this.#reflect(effect, "versions", effect.get(this.connection.versions)?.join(",")),
		);
		this.signals.run((effect) => this.#reflect(effect, "name", effect.get(this.#name).toString()));
		this.signals.run((effect) =>
			this.#reflect(effect, "muted", effect.get(this.controls.audio.muted) ? "" : undefined),
		);
		this.signals.run((effect) =>
			this.#reflect(effect, "paused", effect.get(this.controls.paused) ? "" : undefined),
		);
		this.signals.run((effect) =>
			this.#reflect(effect, "volume", effect.get(this.controls.audio.volume).toString()),
		);
		this.signals.run((effect) => this.#reflect(effect, "visible", effect.get(this.controls.video.visible)));
		this.signals.run((effect) => this.#reflect(effect, "backend", effect.get(this.controls.video.backend)));
		this.signals.run((effect) => this.#reflect(effect, "follow", effect.get(this.#follow) ? "" : "false"));
		this.signals.run((effect) => this.#reflect(effect, "catalog-format", effect.get(this.#catalogFormat)));
		this.signals.run((effect) => this.#reflect(effect, "captions", effect.get(this.controls.text.target)));
		this.signals.run((effect) => {
			const delay = effect.get(this.controls.delay);
			this.#reflect(effect, "delay", typeof delay === "number" ? formatDuration(delay) : delay);
		});
		this.signals.run((effect) => this.#reflect(effect, "buffer", formatDuration(effect.get(this.controls.buffer))));

		// Track the element's rendered size and feed it into the rendition picker,
		// scaled by devicePixelRatio so high-DPI screens still get sharp renditions.
		const updateDimensions = (width: number, height: number) => {
			if (width <= 0 || height <= 0) return;
			const dpr = window.devicePixelRatio || 1;
			this.controls.video.target.update((prev) => ({
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
	/** Called by the browser when the element joins a document. */
	connectedCallback() {
		this.#connected = true;
		if (this.#invalid.peek().size) this.#resume = true;
		else this.#enabled.set(true);
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

	/** Called by the browser when the element leaves its document. */
	disconnectedCallback() {
		// Stop everything but don't actually cleanup just in case we get added back to the DOM.
		this.#connected = false;
		this.#enabled.set(false);
	}

	/** Called by the browser when an observed attribute changes. */
	attributeChangedCallback(name: (typeof OBSERVED)[number], oldValue: string | null, newValue: string | null) {
		if (this.#reflecting || oldValue === newValue) return;
		try {
			if (name === "url") this.url = parseUrl(newValue);
			else if (name === "versions") this.versions = parseVersions(newValue);
			else if (name === "name") this.name = newValue ?? "";
			else if (name === "paused") this.paused = parseBoolean(newValue, false);
			else if (name === "volume") this.volume = parseVolume(newValue);
			else if (name === "muted") this.muted = parseBoolean(newValue, false);
			else if (name === "visible") this.visible = parseVisible(newValue);
			else if (name === "backend") this.backend = parseBackend(newValue);
			else if (name === "follow") this.follow = parseBoolean(newValue, true);
			else if (name === "announced" && newValue !== null)
				throw new Error("moq-watch: `announced` was renamed to `follow`");
			else if (name === "announced") this.#accept("announced");
			else if (name === "delay") this.delay = parseDelay(newValue);
			else if (name === "buffer") this.buffer = parseBuffer(newValue);
			else if (name === "catalog-format") this.catalogFormat = parseCatalogFormat(newValue);
			else if (name === "captions") this.captions = newValue || undefined;
			else {
				const exhaustive: never = name;
				throw new Error(`Invalid attribute: ${exhaustive}`);
			}
		} catch (error) {
			if (this.#invalid.peek().size === 0) this.#resume = this.#enabled.peek();
			this.#invalid.update((previous) => new Set(previous).add(name));
			this.#enabled.set(false);
			throw error;
		}
	}

	#accept(name: string) {
		// Ordinary edits must preserve an explicitly disabled connection.
		if (!this.#invalid.peek().has(name)) return;
		this.#invalid.update((previous) => {
			const next = new Set(previous);
			next.delete(name);
			return next;
		});
		if (this.#invalid.peek().size || !this.#resume) return;
		this.#resume = false;
		this.#enabled.set(this.#connected);
	}

	#reflect(effect: Effect, name: (typeof OBSERVED)[number], value: string | undefined) {
		// Keep refused markup observable until corrected. An echo must never reconfigure a File
		// source or another control whose serialized value omits some of its state.
		if (effect.get(this.#invalid).size) return;
		this.#reflecting = true;
		try {
			if (value === undefined) this.removeAttribute(name);
			else if (this.getAttribute(name) !== value) this.setAttribute(name, value);
		} finally {
			this.#reflecting = false;
		}
	}

	/** The relay URL, mirrored by the `url` attribute. */
	get url(): URL | undefined {
		return this.connection.url.peek();
	}

	set url(value: string | URL | undefined) {
		this.connection.url.set(parseUrl(value instanceof URL ? value.toString() : (value ?? null)));
		this.#accept("url");
	}

	/** The protocol versions to offer, most preferred first, mirrored by the `versions` attribute; undefined offers the defaults. */
	get versions(): readonly string[] | undefined {
		return this.connection.versions.peek();
	}

	set versions(value: readonly string[] | undefined) {
		this.connection.versions.set(value === undefined ? undefined : Moq.Connection.checkVersions(value));
		this.#accept("versions");
	}

	/** The broadcast name, mirrored by the `name` attribute. */
	get name(): Moq.Path.Valid {
		return this.#name.peek();
	}

	set name(value: string | Moq.Path.Valid) {
		this.#name.set(Moq.Path.from(value));
		this.#accept("name");
	}

	/** Whether playback is paused, mirrored by the `paused` attribute. */
	get paused(): boolean {
		return this.controls.paused.peek();
	}

	set paused(value: boolean) {
		this.controls.paused.set(value);
		this.#accept("paused");
	}

	/** The playback level from 0 to 1, preserved while muted and mirrored by the `volume` attribute. */
	get volume(): number {
		return this.controls.audio.volume.peek();
	}

	set volume(value: number) {
		if (!Number.isFinite(value) || value < 0 || value > 1)
			throw invalidAttribute("volume", value, "a number from 0 to 1");
		this.controls.audio.volume.set(value);
		this.#accept("volume");
	}

	/** Whether audio is muted independently of volume, mirrored by the `muted` attribute. */
	get muted(): boolean {
		return this.controls.audio.muted.peek();
	}

	set muted(value: boolean) {
		this.controls.audio.muted.set(value);
		this.#accept("muted");
	}

	/** When to download and render video, mirrored by the `visible` attribute. See {@link Video.Visible}. */
	get visible(): Video.Visible {
		return this.controls.video.visible.peek();
	}

	set visible(value: Video.Visible) {
		this.controls.video.visible.set(parseVisible(value));
		this.#accept("visible");
	}

	/** Which graphics API paints the canvas. See {@link Video.Backend}. */
	get backend(): Video.Backend {
		return this.controls.video.backend.peek();
	}

	set backend(value: Video.Backend) {
		this.controls.video.backend.set(parseBackend(value));
		this.#accept("backend");
	}

	/** Whether to follow the broadcast's announcements, waiting for one before subscribing, mirrored by the `follow` attribute. */
	get follow(): boolean {
		return this.#follow.peek();
	}

	set follow(value: boolean) {
		this.#follow.set(value);
		this.#accept("follow");
	}

	/**
	 * How far playback trails the live edge, in milliseconds. See {@link Delay}.
	 *
	 * `"auto"` (the default) sizes the jitter buffer from how late frames arrive. `"instant"` drops the
	 * clock instead: video paints the moment it decodes and audio is disabled.
	 */
	get delay(): Delay {
		return this.controls.delay.peek();
	}

	set delay(value: Delay) {
		if (typeof value !== "number" && value !== "auto" && value !== "instant")
			throw invalidAttribute("delay", value, '"auto", "instant", or non-negative milliseconds');
		this.controls.delay.set(parseDelay(typeof value === "number" ? formatDuration(value) : value));
		this.#accept("delay");
	}

	/**
	 * Future-dated media held beyond the live edge before playback skips ahead, in milliseconds.
	 *
	 * Zero (the default) minimizes latency. A larger value enables buffered playback: build up a
	 * buffer from future-dated frames (e.g. TTS written faster than real-time) and only skip ahead
	 * once they would sit further than `delay + buffer` past the playhead. Call `reset()` at each
	 * utterance boundary.
	 */
	get buffer(): Milli {
		return this.controls.buffer.peek();
	}

	set buffer(value: Milli) {
		if (typeof value !== "number") throw invalidAttribute("buffer", value, "non-negative milliseconds");
		this.controls.buffer.set(parseBuffer(formatDuration(value)));
		this.#accept("buffer");
	}

	/**
	 * Re-anchor playback at an utterance boundary in buffered mode: reset the sync reference
	 * and flush the audio buffer so the next utterance plays from its own first frame.
	 */
	reset(): void {
		this.player.reset();
	}

	/** The catalog format to read, mirrored by the `catalog-format` attribute; undefined detects it. */
	get catalogFormat(): CatalogFormat | undefined {
		return this.#catalogFormat.peek();
	}

	set catalogFormat(value: CatalogFormat | undefined) {
		this.#catalogFormat.set(parseCatalogFormat(value ?? null));
		this.#accept("catalog-format");
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
		return this.controls.text.target.peek();
	}

	set captions(value: string | undefined) {
		this.controls.text.target.set(value || undefined);
		this.#accept("captions");
	}
}

customElements.define("moq-watch", MoqWatch);

declare global {
	interface HTMLElementTagNameMap {
		"moq-watch": MoqWatch;
	}
}

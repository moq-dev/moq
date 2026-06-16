import type * as Catalog from "@moq/hang/catalog";
import { Effect, Signal } from "@moq/signals";
import type { Time } from "@moq/wasm";
import * as Moq from "@moq/wasm";
import { MultiBackend } from "./backend";
import { Broadcast, type CatalogFormat, parseCatalogFormat } from "./broadcast";
import type { Latency } from "./sync";
import type * as Video from "./video";

const OBSERVED = [
	"url",
	"name",
	"paused",
	"volume",
	"muted",
	"visible",
	"reload",
	"latency",
	"jitter",
	"catalog-format",
] as const;
type Observed = (typeof OBSERVED)[number];

// Parse the `visible` attribute into a Visible value, falling back to "0px" (on screen only).
function parseVisible(value: string | null): Video.Visible {
	const trimmed = value?.trim();
	if (!trimmed) return "0px";
	if (trimmed === "never" || trimmed === "always") return trimmed;
	// A CSS length usable as an IntersectionObserver rootMargin (px or %).
	if (/^-?\d+(\.\d+)?(px|%)$/.test(trimmed)) return trimmed;
	// Allow a bare number as a px convenience (e.g. visible="200").
	if (/^-?\d+(\.\d+)?$/.test(trimmed)) return `${trimmed}px`;
	console.warn(`moq-watch: invalid visible="${value}", expected "never", "always", or a CSS length like "200px"`);
	return "0px";
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

	// The backend that powers this element.
	backend: MultiBackend;

	// The mutable user controls. As the top of the tree, this element owns the
	// writable Signals and wires read-only views into broadcast/backend. The UI and
	// the attribute/property accessors read and write these directly.
	readonly controls = {
		paused: new Signal(false),
		volume: new Signal(0.5),
		muted: new Signal(false),
		// When video is downloaded relative to the canvas position. See {@link Video.Visible}.
		visible: new Signal<Video.Visible>("0px"),
		latency: new Signal<Latency>("real-time"),
		// The desired video rendition (resolution/bitrate cap).
		target: new Signal<Video.Target | undefined>(undefined),
	};

	// Broadcast configuration owned here and wired into `broadcast` as inputs.
	#name = new Signal<Moq.Path.Valid>(Moq.Path.empty());
	#reload = new Signal(false);
	#catalogFormat = new Signal<CatalogFormat | undefined>(undefined);
	#catalog = new Signal<Catalog.Root | undefined>(undefined);

	// The canvas/video element to render into.
	#element = new Signal<HTMLCanvasElement | HTMLVideoElement | undefined>(undefined);

	// Set when the element is connected to the DOM.
	#enabled = new Signal(false);

	// Stashed volume to restore on unmute.
	#unmuteVolume = 0.5;

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
			name: this.#name,
			reload: this.#reload,
			catalogFormat: this.#catalogFormat,
			catalog: this.#catalog,
		});
		this.signals.cleanup(() => this.broadcast.close());

		this.backend = new MultiBackend({
			element: this.#element,
			broadcast: this.broadcast,
			connection: this.connection.established,
			paused: this.controls.paused,
			visible: this.controls.visible,
			latency: this.controls.latency,
			volume: this.controls.volume,
			muted: this.controls.muted,
			target: this.controls.target,
		});
		this.signals.cleanup(() => this.backend.close());

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

		// Keep the volume control in sync with native <video> controls (MSE backend).
		this.signals.run((effect) => {
			const element = effect.get(this.#element);
			if (!(element instanceof HTMLVideoElement)) return;
			effect.event(element, "volumechange", () => {
				this.controls.volume.set(element.volume);
			});
		});

		// Watch to see if the canvas element is added or removed.
		const setElement = () => {
			const canvas = this.querySelector("canvas") as HTMLCanvasElement | undefined;
			const video = this.querySelector("video") as HTMLVideoElement | undefined;
			if (canvas && video) {
				throw new Error("Cannot have both canvas and video elements");
			}
			this.#element.set(canvas ?? video);
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
				this.setAttribute("paused", "true");
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
			const latency = effect.get(this.controls.latency);
			if (latency === "real-time") {
				this.setAttribute("latency", "real-time");
			} else {
				const jitter = Math.floor(effect.get(this.backend.output.jitter));
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

	#setLatencyNumber(value: string | null) {
		const parsed = value ? Number.parseFloat(value) : Number.NaN;
		this.controls.latency.set(Moq.Time.Milli(Number.isFinite(parsed) ? parsed : 100));
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
			this.controls.paused.set(newValue !== null);
		} else if (name === "volume") {
			const volume = newValue ? Number.parseFloat(newValue) : 0.5;
			this.controls.volume.set(volume);
		} else if (name === "muted") {
			this.controls.muted.set(newValue !== null);
		} else if (name === "visible") {
			this.controls.visible.set(parseVisible(newValue));
		} else if (name === "reload") {
			this.#reload.set(newValue !== null);
		} else if (name === "latency") {
			if (!newValue || newValue === "real-time") {
				this.controls.latency.set("real-time");
			} else {
				this.#setLatencyNumber(newValue);
			}
		} else if (name === "jitter") {
			// Deprecated: use latency="<number>" instead.
			this.#setLatencyNumber(newValue);
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

	get latency(): Latency {
		return this.controls.latency.peek();
	}

	set latency(value: Latency) {
		this.controls.latency.set(value);
	}

	/** The jitter buffer in milliseconds. */
	get jitter(): Time.Milli {
		return this.backend.output.jitter.peek();
	}

	/** @deprecated Use `latency = <number>` instead. */
	set jitter(value: number) {
		this.controls.latency.set(Moq.Time.Milli(value));
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
		return this.broadcast.output.catalog.peek();
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

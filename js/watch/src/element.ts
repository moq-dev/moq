import type * as Catalog from "@moq/hang/catalog";
import type { Time } from "@moq/net";
import * as Moq from "@moq/net";
import { Effect, Signal } from "@moq/signals";
import { MultiBackend } from "./backend";
import { Broadcast, type CatalogFormat, parseCatalogFormat } from "./broadcast";
import { type Bound, type Latency, latencyBounds, latencyFromBounds } from "./sync";
import type { Visible } from "./video";

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

// Parse the `visible` attribute into a Visible value, falling back to "0px" (on screen only).
function parseVisible(value: string | null): Visible {
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

		this.backend = new MultiBackend({
			broadcast: this.broadcast,
			connection: this.connection.established,
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
			const visible = effect.get(this.backend.visible);
			this.setAttribute("visible", visible);
		});

		this.signals.run((effect) => {
			const volume = effect.get(this.backend.audio.volume);
			this.setAttribute("volume", volume.toString());
		});

		this.signals.run((effect) => {
			const { min, max } = latencyBounds(effect.get(this.backend.latency));
			// Only reflect the collapsed `latency` sugar attribute when the range is actually
			// collapsed. An open range is expressed via latency-min/latency-max, and writing
			// `latency` here would round-trip back through attributeChangedCallback and collapse it.
			if (min !== max) return;
			if (min === "real-time") {
				this.setAttribute("latency", "real-time");
			} else {
				const jitter = Math.floor(effect.get(this.backend.jitter));
				this.setAttribute("latency", jitter.toString());
			}
		});

		// Track the element's rendered size and feed it into the rendition picker,
		// scaled by devicePixelRatio so high-DPI screens still get sharp renditions.
		const updateDimensions = (width: number, height: number) => {
			if (width <= 0 || height <= 0) return;
			const dpr = window.devicePixelRatio || 1;
			this.backend.video.source.target.update((prev) => ({
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
		return (Number.isFinite(parsed) ? parsed : 100) as Time.Milli;
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
		} else if (name === "visible") {
			this.backend.visible.set(parseVisible(newValue));
		} else if (name === "reload") {
			this.broadcast.reload.set(newValue !== null);
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

	/** When video is downloaded relative to the canvas position. See {@link Visible}. */
	get visible(): Visible {
		return this.backend.visible.peek();
	}

	set visible(value: Visible) {
		this.backend.visible.set(value);
	}

	get reload(): boolean {
		return this.broadcast.reload.peek();
	}

	set reload(value: boolean) {
		this.broadcast.reload.set(value);
	}

	/**
	 * The latency target. Assign a scalar (or `"real-time"`) to minimize latency, or an object
	 * `{ min, max }` to open a range and buffer future-dated frames. See {@link Latency}.
	 */
	get latency(): Latency {
		return this.backend.latency.peek();
	}

	set latency(value: Latency) {
		this.backend.latency.set(value);
	}

	/** The latency floor (jitter/startup buffer). Read-modify-writes `latency`, leaving the ceiling. */
	get latencyMin(): Bound {
		return latencyBounds(this.backend.latency.peek()).min;
	}

	set latencyMin(value: Bound) {
		const { max } = latencyBounds(this.backend.latency.peek());
		this.backend.latency.set(latencyFromBounds(value, max));
	}

	/**
	 * The latency ceiling: `"real-time"` (default) minimizes, a number caps at that many ms. A
	 * ceiling above the floor enables buffered playback: build up a buffer from future-dated frames
	 * (e.g. TTS written faster than real-time) and only skip ahead past the cap. Call `reset()` at
	 * each utterance boundary. Read-modify-writes `latency`, leaving the floor untouched.
	 */
	get latencyMax(): Bound {
		return latencyBounds(this.backend.latency.peek()).max;
	}

	set latencyMax(value: Bound) {
		const { min } = latencyBounds(this.backend.latency.peek());
		this.backend.latency.set(latencyFromBounds(min, value));
	}

	/** The jitter buffer in milliseconds. */
	get jitter(): Time.Milli {
		return this.backend.jitter.peek();
	}

	/** @deprecated Use `latency = <number>` instead. */
	set jitter(value: number) {
		this.latency = value as Time.Milli;
	}

	/** Re-anchor playback and flush the audio buffer at an utterance boundary (buffered mode). */
	reset(): void {
		this.backend.reset();
	}

	get catalogFormat(): CatalogFormat | undefined {
		return this.broadcast.catalogFormat.peek();
	}

	set catalogFormat(value: CatalogFormat | undefined) {
		this.broadcast.catalogFormat.set(value);
	}

	/**
	 * The active catalog. Assign directly when `catalogFormat` is `"manual"`;
	 * for `"hang"` and `"msf"` this is overwritten by the fetch loop.
	 */
	get catalog(): Catalog.Root | undefined {
		return this.broadcast.catalog.peek();
	}

	set catalog(value: Catalog.Root | undefined) {
		this.broadcast.catalog.set(value);
	}
}

customElements.define("moq-watch", MoqWatch);

declare global {
	interface HTMLElementTagNameMap {
		"moq-watch": MoqWatch;
	}
}

import type * as Catalog from "@moq/hang/catalog";
import type { Time } from "@moq/lite";
import * as Moq from "@moq/lite";
import { Effect, Signal } from "@moq/signals";
import { MultiBackend } from "./backend";
import { Broadcast, CATALOG_FORMATS, type CatalogFormat } from "./broadcast";
import type { Latency } from "./sync";

const DEFAULT_CATALOG_FORMAT: CatalogFormat = "hang";

function parseCatalogFormat(value: string | null): CatalogFormat {
	return CATALOG_FORMATS.find((f) => f === value) ?? DEFAULT_CATALOG_FORMAT;
}

function parsePixelsMode(value: string | null): PixelsMode {
	if (value === null || value === "" || value === "auto") return "auto";
	const parsed = Number.parseInt(value, 10);
	if (Number.isFinite(parsed) && parsed >= 0) return parsed;
	return "auto";
}

const OBSERVED = [
	"url",
	"name",
	"paused",
	"volume",
	"muted",
	"reload",
	"latency",
	"jitter",
	"catalog-format",
	"pixels",
] as const;
type Observed = (typeof OBSERVED)[number];

/**
 * Pixel budget for video rendition selection.
 * - `"auto"` tracks the rendered size of the element (default).
 * - A non-negative number caps selection at that pixel count.
 */
export type PixelsMode = number | "auto";

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

	// Controls the video pixel budget. "auto" derives it from the element's
	// rendered size so we don't download a 1080p stream for a 360p slot.
	#pixelsMode = new Signal<PixelsMode>("auto");

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

		this.signals.run(this.#runPixelBudget.bind(this));
	}

	#setPixels(pixels: number | undefined): void {
		this.backend.video.source.target.update((prev) => ({ ...prev, pixels }));
	}

	#runPixelBudget(effect: Effect): void {
		const mode = effect.get(this.#pixelsMode);

		if (typeof mode === "number") {
			this.#setPixels(mode);
			return;
		}

		// Auto mode: track the element's rendered size, scaled by devicePixelRatio
		// so high-DPI screens still get appropriately sharp renditions.
		const update = (width: number, height: number) => {
			if (width <= 0 || height <= 0) return;
			const dpr = window.devicePixelRatio || 1;
			this.#setPixels(Math.round(width * dpr * height * dpr));
		};

		const observer = new ResizeObserver((entries) => {
			const entry = entries[0];
			if (!entry) return;
			update(entry.contentRect.width, entry.contentRect.height);
		});

		observer.observe(this);
		effect.cleanup(() => observer.disconnect());

		// Seed with the current size in case the observer doesn't fire immediately
		// (e.g. the element is still 0x0 when we attach).
		const rect = this.getBoundingClientRect();
		update(rect.width, rect.height);
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
		} else if (name === "pixels") {
			this.#pixelsMode.set(parsePixelsMode(newValue));
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

	/**
	 * Maximum pixel count (width * height) used to cap rendition selection.
	 * `"auto"` (the default) tracks the element's rendered size scaled by
	 * `devicePixelRatio`, so a 360px slot won't pull a 1080p stream.
	 */
	get pixels(): PixelsMode {
		return this.#pixelsMode.peek();
	}

	set pixels(value: PixelsMode | null | undefined) {
		this.#pixelsMode.set(value == null ? "auto" : value);
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

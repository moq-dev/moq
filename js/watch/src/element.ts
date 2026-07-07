import type * as Catalog from "@moq/hang/catalog";
import * as Util from "@moq/hang/util";
import type { Time } from "@moq/net";
import * as Moq from "@moq/net";
import { Effect, Signal } from "@moq/signals";
import { MultiBackend } from "./backend";
import { Broadcast, type CatalogFormat, parseCatalogFormat } from "./broadcast";
import { type Bound, type Latency, latencyBounds, latencyFromBounds } from "./sync";
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

function parseBoolean(value: string | null, defaultValue: boolean): boolean {
	if (value === null) return defaultValue;
	const normalized = value.trim().toLowerCase();
	return normalized !== "false" && normalized !== "0";
}

// Close everything when this element is garbage collected.
// This is primarily to avoid a console.warn that we didn't close() before GC.
// There's no destructor for web components so this is the best we can do.
const cleanup = new FinalizationRegistry<Effect>((signals) => signals.close());

// If the video stays stalled while the broadcast is still "live" for this long, the WebTransport
// session has likely wedged silently (seen on Safari, where it stops delivering incoming streams
// without settling `WebTransport.closed`), so force a reconnect. Backs off up to the max on repeats
// so a genuinely idle-but-live broadcast doesn't reconnect in a tight loop.
const STALL_RECOVERY_MS = 10_000;
const STALL_RECOVERY_MAX_MS = 60_000;

// On becoming visible while stalled, wait this long before forcing a reconnect. A briefly-suspended
// Safari WebTransport (tab occluded by a window resize, or a quick tab-switch) usually resumes on its
// own within a few hundred ms; reconnecting instantly turns that into a needless multi-second reload.
// A frame arriving within the window clears `stalled` and the reconnect never fires.
const VISIBILITY_RECOVERY_GRACE_MS = 1_000;

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

	// The canvas/video element to render into.
	#element = new Signal<HTMLCanvasElement | HTMLVideoElement | undefined>(undefined);

	// Set when the element is connected to the DOM.
	#enabled = new Signal(false);

	// Set by the pagehide hook when the tab is torn down/frozen, so the connection closes immediately
	// (freeing the relay's egress to this viewer) instead of lingering until the transport idle timeout.
	#suspended = new Signal(false);

	// The effective gate: connected to the DOM AND not page-suspended.
	#active = new Signal(false);

	// Stashed volume to restore on unmute.
	#unmuteVolume = 0.5;

	// Current stall-recovery delay; grows on repeated forced reconnects, resets when frames flow.
	#stallDelay = STALL_RECOVERY_MS;

	// Expose the Effect class, so users can easily create effects scoped to this element.
	signals = new Effect();

	constructor() {
		super();

		cleanup.register(this, this.signals);

		// #active = connected AND not page-suspended; gates both the connection and the subscription.
		this.signals.run((effect) => {
			this.#active.set(effect.get(this.#enabled) && !effect.get(this.#suspended));
		});

		this.connection = new Moq.Connection.Reload({
			enabled: this.#active,
		});
		this.signals.cleanup(() => this.connection.close());

		this.broadcast = new Broadcast({
			connection: this.connection.established,
			announced: this.connection.announced,
			announcedGenerations: this.connection.announcedGenerations,
			enabled: this.#active,
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

		// Close the connection eagerly when the tab is torn down or frozen, so the relay stops forwarding
		// media to this dead viewer immediately instead of at the transport idle timeout (and so the page
		// stays bfcache-eligible). Gated on #enabled so the window listeners drop on DOM disconnect.
		this.signals.run((effect) => {
			if (!effect.get(this.#enabled)) return;
			effect.event(window, "pagehide", () => {
				this.connection.established.peek()?.close();
				this.#suspended.set(true);
			});
			effect.event(window, "pageshow", (event) => {
				if ((event as PageTransitionEvent).persisted) this.#suspended.set(false);
			});
		});

		// Recover from a silently-wedged connection (see STALL_RECOVERY_MS). Safari-only: the wedge is
		// a Safari WebTransport bug, and reconnecting on any >10s stall would destabilize healthy
		// Chrome/Firefox sessions that recover on their own.
		if (Util.Hacks.isSafari) {
			this.signals.run(this.#runStallRecovery.bind(this));
			this.signals.run(this.#runVisibilityRecovery.bind(this));
		}

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
			const { min, max } = latencyBounds(effect.get(this.controls.latency));
			// Only reflect the collapsed `latency` sugar attribute when the range is actually
			// collapsed. An open range is expressed via latency-min/latency-max, and writing
			// `latency` here would round-trip back through attributeChangedCallback and collapse it.
			if (min !== max) return;
			if (min === "real-time") {
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

	// Force a reconnect if playback stalls while the broadcast is still live: the WebTransport session
	// can wedge silently (Safari) so `connection.closed` never rejects and the reconnect loop never
	// fires. A new frame re-runs this effect and cancels the timer, so it only fires on a real stall.
	#runStallRecovery(effect: Effect): void {
		const stalled = effect.get(this.backend.video.output.stalled);
		if (!stalled) {
			// Healthy (or nothing playing): reset the backoff.
			this.#stallDelay = STALL_RECOVERY_MS;
			return;
		}

		// Only act while live; "loading"/"offline" recovery is owned by the normal connect path.
		if (effect.get(this.broadcast.output.status) !== "live") return;

		effect.timer(() => {
			this.connection.reconnect();
			this.#stallDelay = Math.min(this.#stallDelay * 2, STALL_RECOVERY_MAX_MS);
		}, this.#stallDelay);
	}

	// Fast path for the Safari tab-switch wedge: returning to a hidden tab often leaves playback stalled
	// against a suspended WebTransport session. On becoming visible while stalled and still live, force a
	// reconnect, but only after a short grace period: a briefly-suspended session (a quick tab-switch, or
	// the tab occluded by a window resize) usually resumes on its own, so we re-check at the deadline and
	// reconnect only if it's still wedged. A frame arriving first clears `stalled` and the timer no-ops.
	#runVisibilityRecovery(effect: Effect): void {
		// Subscribe to #enabled so this effect only listens while connected AND subscribes to a signal,
		// which avoids the "will never rerun" warning for an otherwise listener-only effect.
		if (!effect.get(this.#enabled)) return;

		effect.event(document, "visibilitychange", () => {
			if (document.hidden) return;
			if (!this.backend.video.output.stalled.peek()) return;
			if (this.broadcast.output.status.peek() !== "live") return;

			effect.timer(() => {
				if (document.hidden) return;
				if (!this.backend.video.output.stalled.peek()) return;
				if (this.broadcast.output.status.peek() !== "live") return;

				this.connection.reconnect();
				this.#stallDelay = STALL_RECOVERY_MS;
			}, VISIBILITY_RECOVERY_GRACE_MS);
		});
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
			this.controls.paused.set(newValue !== null);
		} else if (name === "volume") {
			const volume = newValue ? Number.parseFloat(newValue) : 0.5;
			this.controls.volume.set(volume);
		} else if (name === "muted") {
			this.controls.muted.set(newValue !== null);
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
		return this.backend.output.jitter.peek();
	}

	/** Re-anchor playback and flush the audio buffer at an utterance boundary (buffered mode). */
	reset(): void {
		this.backend.reset();
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

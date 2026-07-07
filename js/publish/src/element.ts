import * as Util from "@moq/hang/util";
import * as Moq from "@moq/net";
import { Effect, Signal } from "@moq/signals";
import { Broadcast } from "./broadcast";
import * as Preview from "./preview";
import * as Source from "./source";

const OBSERVED = ["url", "name", "muted", "invisible", "source", "simulcast", "preview", "announce"] as const;
type Observed = (typeof OBSERVED)[number];

type SourceType = "camera" | "screen" | "file";

// "always" announces immediately, "never" never announces, "source" waits until a source is selected.
// Defaults to "source" so we don't announce an empty broadcast with no audio/video.
type AnnounceMode = "always" | "source" | "never";

// Close everything when this element is garbage collected.
// This is primarily to avoid a console.warn that we didn't close() before GC.
// There's no destructor for web components so this is the best we can do.
const cleanup = new FinalizationRegistry<Effect>((signals) => signals.close());

// If the broadcast is announced but the session never receives a single subscribe request, the
// WebTransport session has likely wedged silently (seen on Safari, which stops delivering
// server-initiated streams without settling `WebTransport.closed`), so force a reconnect. Relays
// subscribe to the catalog within ~100ms of an announce, so prolonged zero-request silence is a
// strong wedge signal. Backs off so a legitimately unwatched broadcast re-announces at most once
// per minute, which is harmless.
const STARVATION_RECOVERY_MS = 10_000;
const STARVATION_RECOVERY_MAX_MS = 60_000;

export default class MoqPublish extends HTMLElement {
	static observedAttributes = OBSERVED;

	// Reactive state for element properties that are also HTML attributes.
	// Access these Signals directly for reactive subscriptions (e.g. effect.get(el.state.source)).
	state = {
		source: new Signal<SourceType | File | undefined>(undefined),
		muted: new Signal(false),
		invisible: new Signal(false),
		simulcast: new Signal(false),
		// What a <canvas> preview renders: the raw capture, or a decoded copy of the encoded video.
		preview: new Signal<Preview.Mode>("source"),
		// When to announce/publish the broadcast: always, never, or only once a source is selected.
		announce: new Signal<AnnounceMode>("source"),
	};

	connection: Moq.Connection.Reload;
	broadcast: Broadcast;

	// The preview element, either a <video> (raw source via srcObject) or a <canvas> (rendered frames).
	#preview = new Signal<HTMLVideoElement | HTMLCanvasElement | undefined>(undefined);

	video = new Signal<Source.Camera | Source.Screen | undefined>(undefined);
	audio = new Signal<Source.Microphone | Source.Screen | undefined>(undefined);
	file = new Signal<Source.File | undefined>(undefined);

	// The inverse of the `muted` and `invisible` signals.
	#videoEnabled: Signal<boolean>;
	#audioEnabled: Signal<boolean>;
	#eitherEnabled: Signal<boolean>;

	// Set when `simulcast` is enabled and video is not `invisible`.
	#sdEnabled: Signal<boolean>;

	// Set when the element is connected to the DOM.
	#enabled = new Signal(false);

	// Set by the pagehide hook when the tab is being torn down (or bfcache-frozen), so the connection
	// closes immediately and the relay unannounces without waiting for the transport idle timeout.
	#suspended = new Signal(false);

	// The effective gate: connected to the DOM AND not page-suspended. Drives the connection + publishing.
	#active = new Signal(false);

	// Whether to actually publish the broadcast: connected to the DOM and allowed by the `announce` mode.
	#publishEnabled = new Signal(false);

	// Current starvation-recovery delay; grows on repeated forced reconnects, resets on activity.
	// True while the tab is backgrounded. Safari suspends a hidden tab's WebTransport session, so the
	// starvation recovery pauses instead of reconnecting (a reconnect would re-announce a zombie that
	// collides with any other publisher of this name). Set by a visibilitychange listener (Safari only).
	#hidden = new Signal(false);

	#starvationDelay = STARVATION_RECOVERY_MS;

	signals = new Effect();

	constructor() {
		super();

		cleanup.register(this, this.signals);

		// #active = connected AND not page-suspended; gates both the connection and publishing.
		this.signals.run((effect) => {
			this.#active.set(effect.get(this.#enabled) && !effect.get(this.#suspended));
		});

		this.connection = new Moq.Connection.Reload({
			enabled: this.#active,
		});
		this.signals.cleanup(() => this.connection.close());

		// The inverse of the `muted` and `invisible` signals.
		// TODO make this.signals.computed to simplify the code.
		this.#videoEnabled = new Signal(false);
		this.#audioEnabled = new Signal(false);
		this.#eitherEnabled = new Signal(false);
		this.#sdEnabled = new Signal(false);

		this.signals.run((effect) => {
			const muted = effect.get(this.state.muted);
			const invisible = effect.get(this.state.invisible);
			const simulcast = effect.get(this.state.simulcast);
			this.#videoEnabled.set(!invisible);
			this.#audioEnabled.set(!muted);
			this.#eitherEnabled.set(!muted || !invisible);
			this.#sdEnabled.set(simulcast && !invisible);
		});

		this.signals.run((effect) => {
			const enabled = effect.get(this.#active);
			const announce = effect.get(this.state.announce);
			const hasSource = effect.get(this.state.source) !== undefined;
			const announcing = announce === "always" || (announce === "source" && hasSource);
			this.#publishEnabled.set(enabled && announcing);
		});

		this.broadcast = new Broadcast({
			connection: this.connection.established,
			enabled: this.#publishEnabled,

			audio: {
				enabled: this.#audioEnabled,
			},
			video: {
				hd: {
					enabled: this.#videoEnabled,
				},
				sd: {
					enabled: this.#sdEnabled,
				},
			},
		});
		this.signals.cleanup(() => this.broadcast.close());

		// Close the connection eagerly when the tab is torn down or frozen, so the relay unannounces
		// immediately instead of holding the broadcast route until the transport idle timeout (which would
		// block a same-name republish). All browsers. Gated on #enabled so the window listeners are removed
		// on DOM disconnect (element stays GC-eligible), and this effect never re-runs when #suspended flips.
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

		// Recover from a silently-wedged connection (see STARVATION_RECOVERY_MS). Safari-only: the
		// "no request yet" trigger false-fires on a normal lazy-pull relay, so arming it on
		// Chrome/Firefox would drop healthy sessions in a reconnect loop.
		if (Util.Hacks.isSafari) {
			this.signals.run((effect) => {
				// effect.get(#enabled) scopes the listener to while-connected AND subscribes to a signal,
				// avoiding the "will never rerun" warning for this otherwise listener-only effect.
				if (!effect.get(this.#enabled)) return;
				const update = () => this.#hidden.set(document.hidden);
				update();
				effect.event(document, "visibilitychange", update);
			});
			this.signals.run(this.#runStarvationRecovery.bind(this));
		}

		// Watch to see if the preview element is added or removed.
		const setPreview = () => {
			this.#preview.set(this.querySelector("video, canvas") as HTMLVideoElement | HTMLCanvasElement | undefined);
		};
		const observer = new MutationObserver(setPreview);
		observer.observe(this, { childList: true, subtree: true });
		this.signals.cleanup(() => observer.disconnect());
		setPreview();

		this.signals.run((effect) => {
			const preview = effect.get(this.#preview);
			if (!preview) return;

			// A <canvas> renders the decoded frames; a <video> shows the raw source via srcObject.
			if (preview instanceof HTMLCanvasElement) {
				const renderer = new Preview.Renderer({
					canvas: preview,
					video: this.broadcast.video,
					mode: this.state.preview,
					enabled: this.#videoEnabled,
				});
				effect.cleanup(() => renderer.close());
				return;
			}

			// preview="none" disables the preview entirely.
			if (effect.get(this.state.preview) === "none") {
				preview.style.display = "none";
				return;
			}

			const source = effect.get(this.broadcast.video.source);
			if (!source) {
				preview.style.display = "none";
				return;
			}

			preview.srcObject = new MediaStream([source]);
			preview.style.display = "block";

			effect.cleanup(() => {
				preview.srcObject = null;
			});
		});

		// `encoded` decodes the wire output to a <canvas>; a <video> can only show the raw source.
		// Warn once per state change rather than on every source/frame update.
		this.signals.run((effect) => {
			if (!(effect.get(this.#preview) instanceof HTMLVideoElement)) return;
			if (effect.get(this.state.preview) !== "encoded") return;
			console.warn('moq-publish: preview="encoded" requires a <canvas> element; showing the raw source.');
		});

		this.signals.run(this.#runSource.bind(this));
	}

	connectedCallback() {
		this.#enabled.set(true);
	}

	disconnectedCallback() {
		this.#enabled.set(false);
	}

	// Force a reconnect if the announced broadcast never receives a subscribe request: the
	// WebTransport session can wedge silently (Safari) so requests never arrive and watchers
	// buffer forever against a zombie announcement. Any request disarms the timer.
	#runStarvationRecovery(effect: Effect): void {
		if (!effect.get(this.#publishEnabled)) return;
		if (!effect.get(this.connection.established)) return;

		// Paused while hidden: Safari suspends the session, so reconnecting would only re-announce a
		// zombie that collides with any other publisher of this name. Re-arms on unhide (this effect
		// re-runs when #hidden flips).
		if (effect.get(this.#hidden)) return;

		if (effect.get(this.broadcast.requestCount) > 0) {
			// This session received a server-initiated stream, so it is not wedged. Note the backoff
			// resets ONLY here: every reconnect flaps `established` through undefined and re-runs this
			// effect disarmed, so resetting on disarm would turn an unwatched broadcast into a tight
			// reconnect loop.
			this.#starvationDelay = STARVATION_RECOVERY_MS;
			return;
		}

		effect.timer(() => {
			console.warn(`no subscribe request within ${this.#starvationDelay / 1000}s; reconnecting`);
			this.connection.reconnect();
			this.#starvationDelay = Math.min(this.#starvationDelay * 2, STARVATION_RECOVERY_MAX_MS);
		}, this.#starvationDelay);
	}

	attributeChangedCallback(name: Observed, oldValue: string | null, newValue: string | null) {
		if (oldValue === newValue) return;

		if (name === "url") {
			this.connection.url.set(newValue ? new URL(newValue) : undefined);
		} else if (name === "name") {
			this.broadcast.name.set(Moq.Path.from(newValue ?? ""));
		} else if (name === "source") {
			if (newValue === "camera" || newValue === "screen" || newValue === "file" || newValue === null) {
				this.state.source.set(newValue as SourceType | undefined);
			} else {
				throw new Error(`Invalid source: ${newValue}`);
			}
		} else if (name === "announce") {
			if (newValue === "source" || newValue === null) {
				this.state.announce.set("source");
			} else if (newValue === "always") {
				this.state.announce.set("always");
			} else if (newValue === "never") {
				this.state.announce.set("never");
			} else {
				throw new Error(`Invalid announce: ${newValue}`);
			}
		} else if (name === "muted") {
			this.state.muted.set(newValue !== null);
		} else if (name === "invisible") {
			this.state.invisible.set(newValue !== null);
		} else if (name === "simulcast") {
			this.state.simulcast.set(newValue !== null);
		} else if (name === "preview") {
			if (newValue === "encoded" || newValue === "source" || newValue === "none") {
				this.state.preview.set(newValue);
			} else if (newValue === null) {
				this.state.preview.set("source");
			} else {
				throw new Error(`Invalid preview: ${newValue}`);
			}
		} else {
			const exhaustive: never = name;
			throw new Error(`Invalid attribute: ${exhaustive}`);
		}
	}

	#runSource(effect: Effect) {
		const source = effect.get(this.state.source);
		if (!source) return;

		if (source === "camera") {
			const video = new Source.Camera({ enabled: this.#videoEnabled });
			this.signals.run((effect) => {
				const source = effect.get(video.source);
				this.broadcast.video.source.set(source);
			});

			const audio = new Source.Microphone({ enabled: this.#audioEnabled });
			this.signals.run((effect) => {
				const source = effect.get(audio.source);
				this.broadcast.audio.source.set(source);
			});

			effect.set(this.video, video);
			effect.set(this.audio, audio);

			effect.cleanup(() => {
				video.close();
				audio.close();
			});

			return;
		}

		if (source === "screen") {
			const screen = new Source.Screen({
				enabled: this.#eitherEnabled,
			});

			this.signals.run((effect) => {
				const source = effect.get(screen.source);
				if (!source) return;

				effect.set(this.broadcast.video.source, source.video);
				effect.set(this.broadcast.audio.source, source.audio);
			});

			effect.set(this.video, screen);
			effect.set(this.audio, screen);

			effect.cleanup(() => {
				screen.close();
			});

			return;
		}

		if (source === "file" || source instanceof File) {
			const fileSource = new Source.File({
				// If a File is provided, use it directly.
				file: source instanceof File ? source : undefined,
				enabled: this.#eitherEnabled,
			});

			// Otherwise prompt the user to pick one. The selection click is still the
			// active user gesture (effects run a microtask later, which preserves it).
			if (!(source instanceof File)) {
				fileSource.prompt();
			}

			effect.set(this.file, fileSource);

			this.signals.run((effect) => {
				const source = effect.get(fileSource.source);
				this.broadcast.video.source.set(source.video);
				this.broadcast.audio.source.set(source.audio);
			});

			effect.cleanup(() => {
				fileSource.close();
			});

			return;
		}

		const exhaustive: never = source;
		throw new Error(`Invalid source: ${exhaustive}`);
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

	get source(): SourceType | File | undefined {
		return this.state.source.peek();
	}

	set source(value: SourceType | File | undefined) {
		this.state.source.set(value);
	}

	get muted(): boolean {
		return this.state.muted.peek();
	}

	set muted(value: boolean) {
		this.state.muted.set(value);
	}

	get invisible(): boolean {
		return this.state.invisible.peek();
	}

	set invisible(value: boolean) {
		this.state.invisible.set(value);
	}

	/**
	 * When enabled, publish an additional lower-resolution `video/sd` rendition alongside `video/hd`.
	 * Mirrors the `simulcast` attribute and has no effect while `invisible` is set.
	 */
	get simulcast(): boolean {
		return this.state.simulcast.peek();
	}

	set simulcast(value: boolean) {
		this.state.simulcast.set(value);
	}

	get preview(): Preview.Mode {
		return this.state.preview.peek();
	}

	set preview(value: Preview.Mode) {
		this.state.preview.set(value);
	}

	get announce(): AnnounceMode {
		return this.state.announce.peek();
	}

	set announce(value: AnnounceMode) {
		this.state.announce.set(value);
	}
}

customElements.define("moq-publish", MoqPublish);

declare global {
	interface HTMLElementTagNameMap {
		"moq-publish": MoqPublish;
	}
}

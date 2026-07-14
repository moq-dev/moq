import * as Moq from "@moq/net";
import { Effect, Signal } from "@moq/signals";
import * as Audio from "./audio";
import { Broadcast } from "./broadcast";
import * as Preview from "./preview";
import * as Source from "./source";
import * as Video from "./video";

const OBSERVED = ["url", "name", "muted", "invisible", "source", "simulcast", "preview", "announce"] as const;
type Observed = (typeof OBSERVED)[number];

type SourceType = "camera" | "screen" | "file";

// "always" announces immediately, "never" never announces, "source" waits until media is
// actually being captured (a live audio/video track, i.e. permission granted).
// Defaults to "source" so we don't announce an empty broadcast with no audio/video.
type AnnounceMode = "always" | "source" | "never";

// Close everything when this element is garbage collected.
// This is primarily to avoid a console.warn that we didn't close() before GC.
// There's no destructor for web components so this is the best we can do.
const cleanup = new FinalizationRegistry<Effect>((signals) => signals.close());

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
	capture: Video.Capture;
	broadcast: Broadcast;

	// The per-rendition encoders. "hd"/"sd" naming survives only here, in the batteries-included element.
	hd: Video.Encoder;
	sd: Video.Encoder;
	audio: Audio.Encoder;

	// The video encoder tuning knobs, applied to the `hd` rendition.
	videoConfig = new Signal<Video.Config | undefined>(undefined);

	// The captured media tracks, written by #runSource and read by the capture/encoders/preview/UI.
	videoSource = new Signal<Video.Source | undefined>(undefined);
	audioSource = new Signal<Audio.Source | undefined>(undefined);

	// The broadcast name, wired into the broadcast's `name` input.
	#name = new Signal<Moq.Path.Valid>(Moq.Path.empty());

	// Whether to flip the video horizontally on playback. No attribute yet.
	#flip = new Signal(false);

	// The estimated send bandwidth (bits/sec), shared by both video encoders.
	#bandwidth = new Signal<number | undefined>(undefined);

	// The preview element, either a <video> (raw source via srcObject) or a <canvas> (rendered frames).
	#preview = new Signal<HTMLVideoElement | HTMLCanvasElement | undefined>(undefined);

	video = new Signal<Source.Camera | Source.Screen | undefined>(undefined);
	audioInput = new Signal<Source.Microphone | Source.Screen | undefined>(undefined);
	file = new Signal<Source.File | undefined>(undefined);

	// The inverse of the `muted` and `invisible` signals.
	#videoEnabled: Signal<boolean>;
	#audioEnabled: Signal<boolean>;
	#eitherEnabled: Signal<boolean>;

	// Set when `simulcast` is enabled and video is not `invisible`.
	#sdEnabled: Signal<boolean>;

	// Set when the element is connected to the DOM.
	#enabled = new Signal(false);

	// Whether to actually publish the broadcast: connected to the DOM and allowed by the `announce` mode.
	#publishEnabled = new Signal(false);

	signals = new Effect();

	constructor() {
		super();

		cleanup.register(this, this.signals);

		this.connection = new Moq.Connection.Reload({
			enabled: this.#enabled,
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
			const enabled = effect.get(this.#enabled);
			const announce = effect.get(this.state.announce);
			// "source" waits until media is actually being captured -- a live audio or
			// video track exists -- not merely a source *type* selected. Otherwise we'd
			// announce an empty broadcast while the getUserMedia/getDisplayMedia
			// permission prompt is still pending (or after the user denies it).
			const hasMedia = effect.get(this.videoSource) !== undefined || effect.get(this.audioSource) !== undefined;
			const announcing = announce === "always" || (announce === "source" && hasMedia);
			this.#publishEnabled.set(enabled && announcing);
		});

		// Track the connection's send bandwidth estimate, shared by both encoders as their bitrate cap.
		this.signals.run((effect) => {
			const conn = effect.get(this.connection.established);
			const bandwidth = conn?.sendBandwidth;
			this.#bandwidth.set(bandwidth ? effect.get(bandwidth) : undefined);
		});

		this.capture = new Video.Capture({ source: this.videoSource });
		this.signals.cleanup(() => this.capture.close());

		this.broadcast = new Broadcast({
			connection: this.connection.established,
			enabled: this.#publishEnabled,
			name: this.#name,
			display: this.capture.out.display,
			flip: this.#flip,
		});
		this.signals.cleanup(() => this.broadcast.close());

		this.hd = new Video.Encoder("video/hd", {
			broadcast: this.broadcast,
			capture: this.capture,
			enabled: this.#videoEnabled,
			config: this.videoConfig,
			bandwidth: this.#bandwidth,
		});
		this.signals.cleanup(() => this.hd.close());

		// Default the sd rendition to ~3/16 of the source pixel count (e.g. ~480p from a 1080p source),
		// scaling relative to the source so simulcast stays generic instead of baking in a fixed baseline.
		this.sd = new Video.Encoder("video/sd", {
			broadcast: this.broadcast,
			capture: this.capture,
			enabled: this.#sdEnabled,
			config: { maxScale: 0.1875 },
			bandwidth: this.#bandwidth,
		});
		this.signals.cleanup(() => this.sd.close());

		this.audio = new Audio.Encoder("audio/data", {
			broadcast: this.broadcast,
			enabled: this.#audioEnabled,
			source: this.audioSource,
		});
		this.signals.cleanup(() => this.audio.close());

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
					frame: this.capture.out.frame,
					display: this.capture.out.display,
					flip: this.#flip,
					encoder: this.hd,
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

			const source = effect.get(this.videoSource);
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

	attributeChangedCallback(name: Observed, oldValue: string | null, newValue: string | null) {
		if (oldValue === newValue) return;

		if (name === "url") {
			this.connection.url.set(newValue ? new URL(newValue) : undefined);
		} else if (name === "name") {
			this.#name.set(Moq.Path.from(newValue ?? ""));
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
				this.videoSource.set(source);
			});

			const audio = new Source.Microphone({ enabled: this.#audioEnabled });
			this.signals.run((effect) => {
				const source = effect.get(audio.source);
				this.audioSource.set(source);
			});

			effect.set(this.video, video);
			effect.set(this.audioInput, audio);

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

				effect.set(this.videoSource, source.video);
				effect.set(this.audioSource, source.audio);
			});

			effect.set(this.video, screen);
			effect.set(this.audioInput, screen);

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
				this.videoSource.set(source.video);
				this.audioSource.set(source.audio);
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
		return this.#name.peek();
	}

	set name(value: string | Moq.Path.Valid) {
		this.#name.set(Moq.Path.from(value));
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

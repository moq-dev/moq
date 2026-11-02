/**
 * The `<moq-publish>` custom element: a broadcast publisher driven by HTML attributes.
 *
 * Side-effectful: importing this registers the element.
 *
 * @module
 */
import * as Moq from "@moq/net";
import { Effect, readonlys, Signal } from "@moq/signals";
import * as Announce from "./announce";
import * as Audio from "./audio";
import { Broadcast } from "./broadcast";
import * as Preview from "./preview";
import * as Source from "./source";
import { clearSourceState } from "./source-state";
import * as Video from "./video";

function invalidAttribute(name: string, value: unknown, expected: string): Error {
	return new Error(`moq-publish: invalid ${name}="${value}", expected ${expected}`);
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

/** The attributes the element observes. */
const OBSERVED = ["url", "versions", "name", "muted", "invisible", "source", "preview", "backend", "announce"] as const;

/** The built-in capture sources selectable via the `source` attribute. */
export type SourceType = "camera" | "screen" | "file";

/**
 * When to announce the broadcast. Nobody can see or subscribe to it until it is announced.
 *
 * `always` announces immediately, and `source` waits until media is actually being captured. A camera source waits for every enabled track, so a refused microphone
 * cannot advertise a partial broadcast. `source` also waits until every captured track's config
 * resolves or fails, so a subscriber's first catalog lists every rendition. Defaults to `source`.
 * To keep a broadcast out of discovery, give it a hidden name with a `.`-prefixed path segment
 * instead; it is still subscribable by name.
 */
export type AnnounceMode = "always" | "source";

/**
 * Parse a boolean attribute: absent uses `defaultValue`, bare presence is true, and an explicit
 * `"false"`/`"0"` is false. Matches `<moq-watch>`.
 */
function parseBoolean(value: string | null, defaultValue: boolean): boolean {
	if (value === null) return defaultValue;
	const normalized = value.trim().toLowerCase();
	return normalized !== "false" && normalized !== "0";
}

function parseSource(value: string | null): SourceType | undefined {
	if (value === null) return undefined;
	if (value === "camera" || value === "screen" || value === "file") return value;
	throw invalidAttribute("source", value, '"camera", "screen", or "file"');
}

function parseAnnounce(value: string | null): AnnounceMode {
	if (value === null) return "source";
	if (value === "source" || value === "always") return value;
	throw invalidAttribute("announce", value, '"source" or "always"');
}

function parsePreview(value: string | null): Preview.Mode {
	if (value === null) return "source";
	if (value === "encoded" || value === "source" || value === "none") return value;
	throw invalidAttribute("preview", value, '"encoded", "source", or "none"');
}

function parseBackend(value: string | null): Preview.Backend {
	if (value === null) return "auto";
	const trimmed = value.trim();
	if (trimmed === "auto" || trimmed === "webgpu" || trimmed === "2d") return trimmed;
	throw invalidAttribute("backend", value, '"auto", "webgpu", or "2d"');
}

/** The `<moq-publish>` element: captures a source and publishes it as a broadcast. */
export default class MoqPublish extends HTMLElement {
	/** The attributes the browser reports changes to. */
	static observedAttributes: typeof OBSERVED = OBSERVED;

	/**
	 * The mutable user controls. As the top of the tree, this element owns the writable Signals
	 * and wires read-only views into the pipeline. The UI and the attribute/property accessors
	 * read and write these directly (e.g. effect.get(el.controls.source)).
	 */
	readonly controls = {
		source: new Signal<SourceType | File | undefined>(undefined),
		muted: new Signal(false),
		invisible: new Signal(false),
		// What a <canvas> preview renders: the raw capture, or a decoded copy of the encoded video.
		preview: new Signal<Preview.Mode>("source"),
		// Which graphics API draws a <canvas> preview.
		backend: new Signal<Preview.Backend>("auto"),
		// When to advertise the broadcast: always, or only once a source is live. Undefined while the
		// `announce` attribute is invalid, which never announces.
		announce: new Signal<AnnounceMode | undefined>("source"),
	};

	/**
	 * The relay connection, shared with every other element on the page pointing at the
	 * same URL. The broadcast publishes into its `origin`, so a `<moq-watch>` on the same
	 * page and URL resolves it locally with no round trip.
	 */
	connection: Moq.Connection;
	#capture: Video.Capture;
	/** The broadcast being published. */
	broadcast: Broadcast;

	/**
	 * The single video encoder, tuned via `video.config`. For multiple renditions (e.g. simulcast), drop
	 * the element and register your own encoders on a Broadcast via the JS API.
	 */
	video: Video.Encoder;
	/** The single audio encoder, tuned via its knobs (`codec`, `volume`, ...). */
	audio: Audio.Encoder;

	// The selected input sources: the Camera/Screen, Microphone/Screen, and File holders driving capture.
	// Read by the UI (device pickers) and written by #runSource.
	readonly #sources = {
		video: new Signal<Source.Camera | Source.Screen | undefined>(undefined),
		audio: new Signal<Source.Microphone | Source.Screen | undefined>(undefined),
		file: new Signal<Source.File | undefined>(undefined),
	};
	/** The selected input sources driving capture, read-only. */
	readonly sources = readonlys(this.#sources);

	// The captured media tracks, written by #runSource. Fed to the video and audio captures, so
	// consumers read them back via `video.capture` / `audio.capture` rather than here.
	#videoSource = new Signal<Video.Source | undefined>(undefined);
	#audioSource = new Signal<Audio.Source | undefined>(undefined);

	// The broadcast name, wired into the broadcast's `name` input.
	#name = new Signal<Moq.Path.Valid>(Moq.Path.empty());

	// Whether to flip the video horizontally on playback. No attribute yet.
	#flip = new Signal(false);
	// The camera's screen orientation, which the capture turns into a rotation for landscape frames.
	#orientation = new Signal(0);

	// The preview element, either a <video> (raw source via srcObject) or a <canvas> (rendered frames).
	#preview = new Signal<HTMLVideoElement | HTMLCanvasElement | undefined>(undefined);

	// Whether to encode each track: connected and allowed by `muted` and `invisible`. Capture follows
	// `#enabled` instead, so muting or hiding keeps the devices open and resuming is instant.
	#videoEnabled = new Signal(false);
	#audioEnabled = new Signal(false);

	// Set when the element is connected to the DOM. Gates capture: devices stay open while connected
	// with a source selected, whatever `muted` and `invisible` say.
	#enabled = new Signal(false);
	#connected = false;
	#invalid = new Signal<ReadonlySet<string>>(new Set());
	// Whether clearing the last invalid attribute re-enables media: false when the page had
	// already disabled the connection, so recovery does not override that.
	#resume = false;
	#reflecting = false;

	// Whether to advertise the broadcast, driven by the `announce` mode.
	#announcing = new Signal(false);

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

		this.signals.run((effect) => {
			const enabled = effect.get(this.#enabled);
			this.#videoEnabled.set(enabled && !effect.get(this.controls.invisible));
			this.#audioEnabled.set(enabled && !effect.get(this.controls.muted));
		});

		this.#capture = new Video.Capture({ source: this.#videoSource, orientation: this.#orientation });
		this.signals.cleanup(() => this.#capture.close());

		// Reached as `audio.capture` rather than a field of its own, so audio and video read alike.
		const audioCapture = new Audio.Capture({
			source: this.#audioSource,
			enabled: this.#enabled,
		});
		this.signals.cleanup(() => audioCapture.close());

		this.broadcast = new Broadcast({
			origin: this.connection.origin,
			enabled: this.#enabled,
			announce: this.#announcing,
			name: this.#name,
			display: this.#capture.out.display,
			flip: this.#flip,
			rotation: this.#capture.out.rotation,
		});
		this.signals.cleanup(() => this.broadcast.close());

		this.video = new Video.Encoder("video", {
			broadcast: this.broadcast,
			capture: this.#capture,
			enabled: this.#videoEnabled,
			bandwidth: this.connection.allocator,
		});
		this.signals.cleanup(() => this.video.close());

		this.audio = new Audio.Encoder("audio", {
			broadcast: this.broadcast,
			enabled: this.#audioEnabled,
			capture: audioCapture,
			bandwidth: this.connection.allocator,
		});
		this.signals.cleanup(() => this.audio.close());

		Announce.run(this.signals, this.#announcing, {
			mode: this.controls.announce,
			source: this.signals.computed((effect) =>
				effect.get(this.#enabled) ? effect.get(this.controls.source) : undefined,
			),
			video: { enabled: this.#videoEnabled, source: this.#videoSource, settled: this.video.settled },
			audio: { enabled: this.#audioEnabled, source: this.#audioSource, settled: this.audio.settled },
		});

		// Watch to see if the preview element is added or removed.
		const setPreview = () => {
			this.#preview.set(this.querySelector("video, canvas") as HTMLVideoElement | HTMLCanvasElement | undefined);
		};
		const observer = new MutationObserver(setPreview);
		observer.observe(this, { childList: true, subtree: true });
		this.signals.cleanup(() => observer.disconnect());
		setPreview();

		this.signals.run((effect) => {
			if (!effect.get(this.#enabled)) return;
			const preview = effect.get(this.#preview);
			if (!preview) return;

			// A <canvas> renders the decoded frames; a <video> shows the raw source via srcObject.
			if (preview instanceof HTMLCanvasElement) {
				const renderer = new Preview.Renderer({
					canvas: preview,
					frames: this.#capture.out.frames,
					display: this.#capture.out.display,
					flip: this.#flip,
					rotation: this.#capture.out.rotation,
					encoder: this.video,
					mode: this.controls.preview,
					enabled: this.#videoEnabled,
					backend: this.controls.backend,
				});
				effect.cleanup(() => renderer.close());

				// A canvas that lost its GPU never hands out another context, so swap in a fresh copy.
				// The observer above picks it up and this effect builds a renderer for it.
				effect.run((inner) => {
					if (inner.get(renderer.out.error)?.name === "InvalidStateError")
						preview.replaceWith(preview.cloneNode());
				});
				return;
			}

			// preview="none" disables the preview entirely.
			if (effect.get(this.controls.preview) === "none") {
				preview.style.display = "none";
				return;
			}

			// A hidden camera stays open, so blank the preview to match what viewers get.
			const source = effect.get(this.#videoEnabled) ? effect.get(this.#videoSource) : undefined;
			if (!source) {
				preview.style.display = "none";
				return;
			}

			// srcObject only takes a MediaStream, so a source that hands us frames directly (a
			// decoded file, an image) has nothing to show here. A <canvas> renders those.
			if ("frames" in source) {
				preview.style.display = "none";
				console.warn("moq-publish: this source needs a <canvas> preview; a <video> can't show it.");
				return;
			}

			preview.srcObject = new MediaStream([Video.normalizeSource(source).track]);
			preview.style.display = "block";

			effect.cleanup(() => {
				preview.srcObject = null;
			});
		});

		// `encoded` decodes the wire output to a <canvas>; a <video> can only show the raw source.
		// Warn once per state change rather than on every source/frame update.
		this.signals.run((effect) => {
			if (!(effect.get(this.#preview) instanceof HTMLVideoElement)) return;
			if (effect.get(this.controls.preview) !== "encoded") return;
			console.warn('moq-publish: preview="encoded" requires a <canvas> element; showing the raw source.');
		});

		// Reflect state without replaying its setters through attributeChangedCallback.
		this.signals.run((effect) => this.#reflect(effect, "url", effect.get(this.connection.url)?.toString()));
		this.signals.run((effect) =>
			this.#reflect(effect, "versions", effect.get(this.connection.versions)?.join(",")),
		);
		this.signals.run((effect) => this.#reflect(effect, "name", effect.get(this.#name).toString()));
		this.signals.run((effect) => this.#reflect(effect, "muted", effect.get(this.controls.muted) ? "" : undefined));
		this.signals.run((effect) =>
			this.#reflect(effect, "invisible", effect.get(this.controls.invisible) ? "" : undefined),
		);
		this.signals.run((effect) => {
			const source = effect.get(this.controls.source);
			this.#reflect(effect, "source", source instanceof File ? "file" : source);
		});
		this.signals.run((effect) => this.#reflect(effect, "preview", effect.get(this.controls.preview)));
		this.signals.run((effect) => this.#reflect(effect, "backend", effect.get(this.controls.backend)));
		this.signals.run((effect) => this.#reflect(effect, "announce", effect.get(this.controls.announce)));
		this.signals.run(this.#runSource.bind(this));
	}

	/** Called by the browser when the element joins a document. */
	connectedCallback() {
		this.#connected = true;
		if (this.#invalid.peek().size) this.#resume = true;
		else this.#enabled.set(true);
	}

	/** Called by the browser when the element leaves its document. */
	disconnectedCallback() {
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
			else if (name === "source") this.source = parseSource(newValue);
			else if (name === "announce") this.announce = parseAnnounce(newValue);
			else if (name === "muted") this.muted = parseBoolean(newValue, false);
			else if (name === "invisible") this.invisible = parseBoolean(newValue, false);
			else if (name === "preview") this.preview = parsePreview(newValue);
			else if (name === "backend") this.backend = parseBackend(newValue);
			else {
				const exhaustive: never = name;
				throw new Error(`Invalid attribute: ${exhaustive}`);
			}
		} catch (error) {
			if (this.#invalid.peek().size === 0) this.#resume = this.#enabled.peek();
			if (name === "announce") this.controls.announce.set(undefined);
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

	#runSource(effect: Effect) {
		const source = effect.get(this.controls.source);

		// Every selection owns the complete source state. Explicitly clear every inactive slot so a
		// switch cannot leave a holder or captured track from the previous selection observable.
		clearSourceState(effect, {
			holders: this.#sources,
			video: this.#videoSource,
			audio: this.#audioSource,
		});
		effect.set(this.#orientation, 0, 0);

		// Sources stay alive while disconnected so device and file selections survive a reconnect;
		// the connection gate releases their capture and device discovery. `muted` and `invisible`
		// only stop the encoders, so a source holds its devices until it is deselected or disconnected.
		if (!source) return;

		if (source === "camera") {
			const video = new Source.Camera({ enabled: this.#enabled, device: { enabled: this.#enabled } });
			const audio = new Source.Microphone({ enabled: this.#enabled, device: { enabled: this.#enabled } });

			effect.set(this.#sources.video, video);
			effect.set(this.#sources.audio, audio);
			effect.run((nested) => {
				nested.set(this.#videoSource, nested.get(video.out.source)?.video);
				nested.set(this.#audioSource, nested.get(audio.out.source)?.audio);
				nested.set(this.#orientation, nested.get(video.out.orientation), 0);
			});

			effect.cleanup(() => {
				video.close();
				audio.close();
			});

			return;
		}

		if (source === "screen") {
			const screen = new Source.Screen({
				enabled: this.#enabled,
			});

			effect.run((nested) => {
				const media = nested.get(screen.out.source);
				nested.set(this.#videoSource, media?.video);
				nested.set(this.#audioSource, media?.audio);
			});

			effect.set(this.#sources.video, screen);
			effect.set(this.#sources.audio, screen);

			effect.cleanup(() => {
				screen.close();
			});

			return;
		}

		if (source === "file" || source instanceof File) {
			const fileSource = new Source.File({
				// If a File is provided, use it directly.
				file: source instanceof File ? source : undefined,
				enabled: this.#enabled,
			});

			// Otherwise prompt the user to pick one. The selection click is still the
			// active user gesture (effects run a microtask later, which preserves it).
			if (!(source instanceof File)) {
				fileSource.prompt();
			}

			effect.set(this.#sources.file, fileSource);

			effect.run((nested) => {
				const media = nested.get(fileSource.out.source);
				nested.set(this.#videoSource, media?.video);
				nested.set(this.#audioSource, media?.audio);
			});

			effect.cleanup(() => {
				fileSource.close();
			});

			return;
		}

		const exhaustive: never = source;
		throw new Error(`Invalid source: ${exhaustive}`);
	}

	/** The relay URL, mirrored by the `url` attribute. */
	get url(): URL | undefined {
		return this.connection.url.peek();
	}

	set url(value: string | URL | undefined) {
		this.connection.url.set(parseUrl(value instanceof URL ? value.toString() : (value ?? null)));
		this.#accept("url");
	}

	/** The protocol versions to offer, most preferred first; undefined offers the defaults. */
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

	/** What to capture: a built-in source, a file, or nothing. Mirrored by the `source` attribute. */
	get source(): SourceType | File | undefined {
		return this.controls.source.peek();
	}

	set source(value: SourceType | File | undefined) {
		this.controls.source.set(value instanceof File ? value : parseSource(value ?? null));
		this.#accept("source");
	}

	/**
	 * Whether audio is muted, mirrored by the `muted` attribute. Muting stops encoding but keeps the
	 * microphone open, so unmuting is instant and the browser's microphone indicator stays lit.
	 * Clear `source` (or remove the element) to release the device.
	 */
	get muted(): boolean {
		return this.controls.muted.peek();
	}

	set muted(value: boolean) {
		this.controls.muted.set(value);
		this.#accept("muted");
	}

	/**
	 * Whether video is hidden, mirrored by the `invisible` attribute. Hiding stops encoding and the
	 * preview but keeps the camera open, so showing it again is instant and the browser's camera
	 * indicator stays lit. Clear `source` (or remove the element) to release the device.
	 */
	get invisible(): boolean {
		return this.controls.invisible.peek();
	}

	set invisible(value: boolean) {
		this.controls.invisible.set(value);
		this.#accept("invisible");
	}

	/** What a <canvas> preview renders, mirrored by the `preview` attribute. */
	get preview(): Preview.Mode {
		return this.controls.preview.peek();
	}

	set preview(value: Preview.Mode) {
		this.controls.preview.set(parsePreview(value));
		this.#accept("preview");
	}

	/** Which graphics API draws a <canvas> preview, mirrored by the `backend` attribute. */
	get backend(): Preview.Backend {
		return this.controls.backend.peek();
	}

	set backend(value: Preview.Backend) {
		this.controls.backend.set(parseBackend(value));
		this.#accept("backend");
	}

	/** When to announce the broadcast, mirrored by the `announce` attribute; undefined while that attribute is invalid. */
	get announce(): AnnounceMode | undefined {
		return this.controls.announce.peek();
	}

	set announce(value: AnnounceMode) {
		this.controls.announce.set(parseAnnounce(value));
		this.#accept("announce");
	}
}

customElements.define("moq-publish", MoqPublish);

declare global {
	interface HTMLElementTagNameMap {
		"moq-publish": MoqPublish;
	}
}

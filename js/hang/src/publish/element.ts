import * as Moq from "@moq/lite";
import { Effect, Signal } from "@moq/signals";
import { Broadcast } from "./broadcast";
import * as Source from "./source";

// TODO remove name; replaced with path
const OBSERVED = ["url", "name", "path", "muted", "invisible", "source"] as const;
type Observed = (typeof OBSERVED)[number];

type SourceType = "camera" | "screen" | "file";

// Close everything when this element is garbage collected.
// This is primarily to avoid a console.warn that we didn't close() before GC.
// There's no destructor for web components so this is the best we can do.
const cleanup = new FinalizationRegistry<Effect>((signals) => signals.close());

export default class HangPublish extends HTMLElement {
	static observedAttributes = OBSERVED;

	url = new Signal<URL | undefined>(undefined);
	path = new Signal<Moq.Path.Valid | undefined>(undefined);
	source = new Signal<SourceType | File | undefined>(undefined);

	// Controls whether audio/video is enabled.
	muted = new Signal(false);
	invisible = new Signal(false);

	connection: Moq.Connection.Reload;
	broadcast: Broadcast;

	#preview = new Signal<HTMLVideoElement | undefined>(undefined);

	video = new Signal<Source.Camera | Source.Screen | undefined>(undefined);
	audio = new Signal<Source.Microphone | Source.Screen | undefined>(undefined);
	file = new Signal<Source.File | undefined>(undefined);

	// The inverse of the `muted` and `invisible` signals.
	#videoEnabled: Signal<boolean>;
	#audioEnabled: Signal<boolean>;
	#eitherEnabled: Signal<boolean>;

	// Set when the element is connected to the DOM.
	#enabled = new Signal(false);

	signals = new Effect();

	constructor() {
		super();

		cleanup.register(this, this.signals);

		this.connection = new Moq.Connection.Reload({
			url: this.url,
			enabled: this.#enabled,
		});
		this.signals.cleanup(() => this.connection.close());

		// The inverse of the `muted` and `invisible` signals.
		// TODO make this.signals.computed to simplify the code.
		this.#videoEnabled = new Signal(false);
		this.#audioEnabled = new Signal(false);
		this.#eitherEnabled = new Signal(false);

		this.signals.effect((effect) => {
			const muted = effect.get(this.muted);
			const invisible = effect.get(this.invisible);
			this.#videoEnabled.set(!invisible);
			this.#audioEnabled.set(!muted);
			this.#eitherEnabled.set(!muted || !invisible);
		});

		this.broadcast = new Broadcast({
			connection: this.connection.established,
			enabled: this.#enabled,
			path: this.path,

			audio: {
				enabled: this.#audioEnabled,
			},
			video: {
				hd: {
					enabled: this.#videoEnabled,
				},
			},
		});
		this.signals.cleanup(() => this.broadcast.close());

		// Watch to see if the preview element is added or removed.
		const setPreview = () => {
			this.#preview.set(this.querySelector("video") as HTMLVideoElement | undefined);
		};
		const observer = new MutationObserver(setPreview);
		observer.observe(this, { childList: true, subtree: true });
		this.signals.cleanup(() => observer.disconnect());
		setPreview();

		this.signals.effect((effect) => {
			const preview = effect.get(this.#preview);
			if (!preview) return;

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

		this.signals.effect(this.#runSource.bind(this));
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
			this.url.set(newValue ? new URL(newValue) : undefined);
		} else if (name === "name" || name === "path") {
			this.path.set(newValue ? Moq.Path.from(newValue) : undefined);
		} else if (name === "source") {
			if (newValue === "camera" || newValue === "screen" || newValue === "file" || newValue === null) {
				this.source.set(newValue as SourceType | undefined);
			} else {
				throw new Error(`Invalid source: ${newValue}`);
			}
		} else if (name === "muted") {
			this.muted.set(newValue !== null);
		} else if (name === "invisible") {
			this.invisible.set(newValue !== null);
		} else {
			const exhaustive: never = name;
			throw new Error(`Invalid attribute: ${exhaustive}`);
		}
	}

	#runSource(effect: Effect) {
		const source = effect.get(this.source);
		if (!source) return;

		if (source === "camera") {
			const video = new Source.Camera({ enabled: this.#videoEnabled });
			this.signals.effect((effect) => {
				const source = effect.get(video.source);
				this.broadcast.video.source.set(source);
			});

			const audio = new Source.Microphone({ enabled: this.#audioEnabled });
			this.signals.effect((effect) => {
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

			this.signals.effect((effect) => {
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
				// TODO: Show a file picker otherwise.
				file: source instanceof File ? source : undefined,
				enabled: this.#eitherEnabled,
			});

			this.signals.effect((effect) => {
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
}

customElements.define("hang-publish", HangPublish);

declare global {
	interface HTMLElementTagNameMap {
		"hang-publish": HangPublish;
	}
}

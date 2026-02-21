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

export default class MoqPublish extends HTMLElement {
	static observedAttributes = OBSERVED;

	// Reactive state for element properties that are also HTML attributes.
	// Access these Signals directly for reactive subscriptions (e.g. effect.get(el.state.source)).
	state = {
		source: new Signal<SourceType | File | undefined>(undefined),
		muted: new Signal(false),
		invisible: new Signal(false),
	};

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
			enabled: this.#enabled,
		});
		this.signals.cleanup(() => this.connection.close());

		// The inverse of the `muted` and `invisible` signals.
		// TODO make this.signals.computed to simplify the code.
		this.#videoEnabled = new Signal(false);
		this.#audioEnabled = new Signal(false);
		this.#eitherEnabled = new Signal(false);

		this.signals.run((effect) => {
			const muted = effect.get(this.state.muted);
			const invisible = effect.get(this.state.invisible);
			this.#videoEnabled.set(!invisible);
			this.#audioEnabled.set(!muted);
			this.#eitherEnabled.set(!muted || !invisible);
		});

		this.broadcast = new Broadcast({
			connection: this.connection.established,
			enabled: this.#enabled,

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

		this.signals.run((effect) => {
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
		} else if (name === "name" || name === "path") {
			this.broadcast.path.set(newValue ? Moq.Path.from(newValue) : undefined);
		} else if (name === "source") {
			if (newValue === "camera" || newValue === "screen" || newValue === "file" || newValue === null) {
				this.state.source.set(newValue as SourceType | undefined);
			} else {
				throw new Error(`Invalid source: ${newValue}`);
			}
		} else if (name === "muted") {
			this.state.muted.set(newValue !== null);
		} else if (name === "invisible") {
			this.state.invisible.set(newValue !== null);
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
				// TODO: Show a file picker otherwise.
				file: source instanceof File ? source : undefined,
				enabled: this.#eitherEnabled,
			});

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

	get path(): Moq.Path.Valid | undefined {
		return this.broadcast.path.peek();
	}

	set path(value: string | Moq.Path.Valid | undefined) {
		this.broadcast.path.set(value ? Moq.Path.from(value) : undefined);
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
}

customElements.define("moq-publish", MoqPublish);

declare global {
	interface HTMLElementTagNameMap {
		"moq-publish": MoqPublish;
	}
}

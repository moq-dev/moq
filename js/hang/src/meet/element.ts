import * as Moq from "@moq/lite";
import { Effect, Signal } from "@moq/signals";
import * as DOM from "@moq/signals/dom";
import { type Publish, Watch } from "..";
import HangPublish from "../publish/element";
import { Room } from "./room";

const OBSERVED = ["url", "name", "path"] as const;
type Observed = (typeof OBSERVED)[number];

// This is primarily to avoid a console.warn that we didn't close() before GC.
// There's no destructor for web components so this is the best we can do.
const cleanup = new FinalizationRegistry<Effect>((signals) => signals.close());

// NOTE: This element is more of an example of how to use the library.
// You likely want your own layout, rendering, controls, etc.
// This element instead creates a crude NxN grid of broadcasts.
export default class HangMeet extends HTMLElement {
	static observedAttributes = OBSERVED;

	url = new Signal<URL | undefined>(undefined);
	path = new Signal<Moq.Path.Valid | undefined>(undefined);

	connection: Moq.Connection.Reload;
	room: Room;

	#enabled = new Signal(false);

	// Save a reference to the <video> tag used to render the local broadcast.
	#locals = new Map<Moq.Path.Valid, { video: HTMLVideoElement; cleanup: () => void }>();

	// We have to save a reference to the Video/Audio renderers so we can close them.
	#remotes = new Map<
		string,
		{ canvas: HTMLCanvasElement; renderer: Watch.Video.Renderer; emitter: Watch.Audio.Emitter }
	>();

	#container: HTMLDivElement;

	signals = new Effect();

	constructor() {
		super();

		cleanup.register(this, this.signals);

		this.connection = new Moq.Connection.Reload({ url: this.url, enabled: this.#enabled });
		this.signals.cleanup(() => this.connection.close());

		this.room = new Room({ connection: this.connection.established, path: this.path });
		this.signals.cleanup(() => this.room.close());

		this.#container = DOM.create("div", {
			style: {
				display: "grid",
				gridTemplateColumns: "repeat(auto-fit, minmax(200px, 1fr))",
				gap: "10px",
				alignItems: "center",
			},
		});

		DOM.render(this.signals, this, this.#container);

		// A callback that is fired when one of our local broadcasts is added/removed.
		this.room.onLocal(this.#onLocal.bind(this));

		// A callback that is fired when a remote broadcast is added/removed.
		this.room.onRemote(this.#onRemote.bind(this));

		this.signals.effect((effect) => {
			// This is kind of a hack to reload the effect when the DOM changes.
			const observer = new MutationObserver(() => effect.reload());
			observer.observe(this, { childList: true, subtree: true });
			effect.cleanup(() => observer.disconnect());

			this.#run(effect);
		});
	}

	connectedCallback() {
		this.#enabled.set(true);
	}

	disconnectedCallback() {
		this.#enabled.set(false);
	}

	attributeChangedCallback(name: Observed, _oldValue: string | null, newValue: string | null) {
		if (name === "url") {
			this.url.set(newValue ? new URL(newValue) : undefined);
		} else if (name === "name" || name === "path") {
			this.path.set(newValue ? Moq.Path.from(newValue) : undefined);
		} else {
			const exhaustive: never = name;
			throw new Error(`Invalid attribute: ${exhaustive}`);
		}
	}

	#run(effect: Effect) {
		// Find any nested `hang-publish` elements and mark them as local.
		for (const element of this.querySelectorAll("hang-publish")) {
			if (!(element instanceof HangPublish)) {
				console.warn("hang-publish element not found; tree-shaking?");
				continue;
			}

			const publish = element as HangPublish;

			// Monitor the name of the publish element and update the room.
			effect.effect((effect) => {
				const path = effect.get(publish.broadcast.path);
				if (!path) return;

				this.room.preview(path, publish.broadcast);
				effect.cleanup(() => this.room.unpreview(path));
			});

			// Copy the connection URL to the publish element so they're the same.
			// TODO Reuse the connection instead of dialing a new one.
			effect.effect((effect) => {
				publish.url.set(effect.get(this.connection.url));
			});
		}
	}

	#onLocal(name: Moq.Path.Valid, broadcast?: Publish.Broadcast) {
		if (!broadcast) {
			const existing = this.#locals.get(name);
			if (!existing) return;

			this.#locals.delete(name);
			existing.cleanup();
			existing.video.remove();

			return;
		}

		const video = DOM.create("video", {
			style: {
				width: "100%",
				height: "100%",
				objectFit: "contain",
			},
			muted: true,
			playsInline: true,
			autoplay: true,
		});

		const cleanup = broadcast.video.source.subscribe((media) => {
			video.srcObject = media ? new MediaStream([media]) : null;
		});

		this.#locals.set(name, { video, cleanup });
		this.#container.appendChild(video);
	}

	#onRemote(name: Moq.Path.Valid, broadcast?: Watch.Broadcast) {
		if (!broadcast) {
			const existing = this.#remotes.get(name);
			if (!existing) return;

			this.#remotes.delete(name);

			existing.renderer.close();
			existing.emitter.close();
			existing.canvas.remove();

			return;
		}

		// We're reponsible for signalling that we want to download this catalog/broadcast.
		broadcast.enabled.set(true);

		// Create a canvas to render the video to.
		const canvas = DOM.create("canvas", {
			style: {
				width: "100%",
				height: "100%",
				objectFit: "contain",
			},
		});

		const videoSource = new Watch.Video.Source({ broadcast });
		const audioSource = new Watch.Audio.Source({ broadcast });

		const renderer = new Watch.Video.Renderer(videoSource, { canvas });
		const emitter = new Watch.Audio.Emitter(audioSource);

		this.#remotes.set(name, { canvas, renderer, emitter });

		// Add the canvas to the DOM.
		this.#container.appendChild(canvas);
	}
}

customElements.define("hang-meet", HangMeet);

declare global {
	interface HTMLElementTagNameMap {
		"hang-meet": HangMeet;
	}
}

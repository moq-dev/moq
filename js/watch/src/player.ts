import { Milli } from "@moq/clock";
import type * as Catalog from "@moq/hang/catalog";
import type * as Moq from "@moq/net";
import { Path } from "@moq/net";
import { Effect, type Getter, getter, type Inputs, type Readonlys, Signal } from "@moq/signals";
import * as Audio from "./audio";
import { Broadcast, type CatalogFormat } from "./broadcast";
import { type Delay, Sync } from "./sync";
import * as Text from "./text";
import * as Video from "./video";

/** Inputs for a complete broadcast playback pipeline. */
export type PlayerInput = {
	/** Origin that supplies the broadcast. */
	origin: Getter<Moq.Origin.Table | undefined>;
	/** Broadcast path relative to the origin. */
	name: Getter<Moq.Path.Valid>;
	/** Whether the pipeline may subscribe. */
	enabled: Getter<boolean>;
	/** Follow the broadcast's announcements, waiting for one before subscribing. */
	follow: Getter<boolean>;
	/** Catalog format override, or automatic detection when absent. */
	catalogFormat: Getter<CatalogFormat | undefined>;
	/** Catalog supplied when the format is `manual`. */
	catalog: Getter<Catalog.Root | undefined>;
	/** Connection probe used for rendition selection. */
	probe: Getter<Moq.Connection.Probe | undefined>;
	/** Pause media and captions. */
	paused: Getter<boolean>;
	/** Playback distance from the live edge. */
	delay: Getter<Delay>;
	/** Future-dated media held beyond the live edge. */
	buffer: Getter<Milli>;
	/** Video rendition and canvas output. */
	video: {
		/** Desired video rendition or quality limits. */
		target: Getter<Video.Target | undefined>;
		/** Canvas to paint video into. */
		canvas: Getter<HTMLCanvasElement | undefined>;
		/** Canvas visibility policy for video downloads. */
		visible: Getter<Video.Visible>;
		/** Which graphics API paints the canvas. */
		backend: Getter<Video.Backend>;
	};
	/** Audio rendition and speaker output. */
	audio: {
		/** Desired audio rendition, or automatic selection when absent. */
		target: Getter<Audio.Target | undefined>;
		/** Speaker volume from zero to one. */
		volume: Getter<number>;
		/** Silence audio and stop its download. */
		muted: Getter<boolean>;
	};
	/** Caption track and overlay. */
	text: {
		/** Selected caption track, or undefined for off. */
		target: Getter<string | undefined>;
		/** Container to render captions into. */
		container: Getter<HTMLElement | undefined>;
	};
};

/**
 * Constructor options for {@link Player}; omitted controls use the element defaults. The inputs of
 * each media kind nest under its own key.
 */
export type PlayerProps = Inputs<Omit<PlayerInput, "video" | "audio" | "text">> & {
	[K in "video" | "audio" | "text"]?: Inputs<PlayerInput[K]>;
};

/** Owns the broadcast, synchronized media pipelines, and their download policy. */
export class Player {
	/** Inputs wired into the pipeline. Pass signals to change them after construction. */
	readonly in: Readonlys<Omit<PlayerInput, "video" | "audio" | "text">> & {
		readonly [K in "video" | "audio" | "text"]: Readonlys<PlayerInput[K]>;
	};
	/** Broadcast subscription and effective catalog. */
	readonly broadcast: Broadcast;
	/** Shared audio, video, and caption clock. */
	readonly sync: Sync;
	/** Selected caption rendition. */
	readonly text: Text.Source;
	/** Video decoder; `video.source` selects its rendition. */
	readonly video: Video.Decoder;
	/** Audio decoder; `audio.source` selects its rendition. */
	readonly audio: Audio.Decoder;
	/** Canvas video output. */
	readonly renderer: Video.Renderer;
	/** Speaker audio output. */
	readonly emitter: Audio.Emitter;
	/** Caption overlay output. */
	readonly textRenderer: Text.Renderer;

	#videoEnabled = new Signal(false);
	#audioEnabled = new Signal(false);
	#captionsEnabled = new Signal(false);
	#signals = new Effect();

	constructor(props: PlayerProps = {}) {
		for (const renamed of ["reload", "announced"]) {
			if (renamed in props) throw new Error(`Watch.Player: \`${renamed}\` was renamed to \`follow\``);
		}
		this.in = {
			origin: getter(props.origin),
			name: getter(props.name ?? Path.empty()),
			enabled: getter(props.enabled ?? true),
			follow: getter(props.follow ?? true),
			catalogFormat: getter<CatalogFormat | undefined>(props.catalogFormat),
			catalog: getter(props.catalog),
			probe: getter(props.probe),
			paused: getter(props.paused ?? false),
			delay: getter(props.delay ?? "auto"),
			buffer: getter(props.buffer ?? Milli.zero),
			video: {
				target: getter<Video.Target | undefined>(props.video?.target),
				canvas: getter(props.video?.canvas),
				visible: getter(props.video?.visible ?? "20%"),
				backend: getter<Video.Backend>(props.video?.backend ?? "auto"),
			},
			audio: {
				target: getter<Audio.Target | undefined>(props.audio?.target),
				volume: getter(props.audio?.volume ?? 0.5),
				muted: getter(props.audio?.muted ?? false),
			},
			text: {
				target: getter<string | undefined>(props.text?.target),
				container: getter(props.text?.container),
			},
		};

		this.broadcast = new Broadcast(this.in);
		this.#signals.cleanup(() => this.broadcast.close());

		const videoSource = new Video.Source({
			broadcast: this.broadcast,
			target: this.in.video.target,
			supported: Video.Decoder.supported,
			probe: this.in.probe,
		});
		const audioSource = new Audio.Source({
			broadcast: this.broadcast,
			target: this.in.audio.target,
			supported: Audio.Decoder.supported,
		});
		this.#signals.cleanup(() => {
			videoSource.close();
			audioSource.close();
		});

		this.text = new Text.Source({ broadcast: this.broadcast, target: this.in.text.target });
		this.#signals.cleanup(() => this.text.close());

		this.sync = new Sync({ delay: this.in.delay, buffer: this.in.buffer });
		this.#signals.cleanup(() => this.sync.close());

		this.video = new Video.Decoder({ source: videoSource, sync: this.sync, enabled: this.#videoEnabled });
		this.audio = new Audio.Decoder({ source: audioSource, sync: this.sync, enabled: this.#audioEnabled });
		this.#signals.cleanup(() => {
			this.video.close();
			this.audio.close();
		});

		this.emitter = new Audio.Emitter({
			source: this.audio,
			volume: this.in.audio.volume,
			muted: this.in.audio.muted,
			paused: this.in.paused,
		});
		this.renderer = new Video.Renderer({
			decoder: this.video,
			canvas: this.in.video.canvas,
			visible: this.in.video.visible,
			backend: this.in.video.backend,
		});
		this.#signals.cleanup(() => {
			this.emitter.close();
			this.renderer.close();
		});

		this.textRenderer = new Text.Renderer({
			source: this.text,
			sync: this.sync,
			container: this.in.text.container,
			enabled: this.#captionsEnabled,
		});
		this.#signals.cleanup(() => this.textRenderer.close());

		this.#signals.run((effect) => {
			this.#captionsEnabled.set(effect.get(this.in.enabled) && !effect.get(this.in.paused));
		});
		this.#signals.run((effect) => {
			this.#audioEnabled.set(effect.get(this.in.enabled) && effect.get(this.emitter.out.enabled));
		});
		this.#signals.run((effect) => {
			const visible = effect.get(this.renderer.out.visible);
			if (!effect.get(this.in.enabled)) {
				this.#videoEnabled.set(false);
			} else if (!effect.get(this.in.paused)) {
				this.#videoEnabled.set(visible);
			} else {
				// Keep downloading a paused poster until a frame is painted, then stop.
				this.#videoEnabled.set(visible && !effect.get(this.renderer.out.frame));
			}
		});
	}

	/** Re-anchor buffered playback at a content boundary. */
	reset(): void {
		this.sync.reset();
		this.audio.reset();
	}

	/** Release all subscriptions and media resources. */
	close(): void {
		this.#signals.close();
	}
}

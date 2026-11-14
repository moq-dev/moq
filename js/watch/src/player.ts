import { Duration } from "@moq/clock";
import type * as Catalog from "@moq/hang/catalog";
import type * as Moq from "@moq/net";
import { Path } from "@moq/net";
import { Effect, type Getter, getter, type Inputs, type Readonlys, Signal } from "@moq/signals";
import * as Audio from "./audio";
import { Broadcast, type CatalogFormat } from "./broadcast";
import { type Delay, STRETCH, Sync } from "./sync";
import * as Text from "./text";
import * as Video from "./video";
import { holdViewer } from "./viewer";

/** Inputs for a complete broadcast playback pipeline. */
export type PlayerInput = {
	/** Read-side origin that supplies the broadcast, borrowed without its lifecycle. */
	origin: Getter<Moq.Origin.Reader | undefined>;
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
	/** Pause media and captions. */
	paused: Getter<boolean>;
	/** Playback distance from the live edge. */
	delay: Getter<Delay>;
	/** Future-dated media held beyond the live edge. */
	lookahead: Getter<Duration>;
	/** How far audio may trail the clock before it skips ahead; zero turns the time stretch off. */
	stretch: Getter<Duration>;
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
	/**
	 * The claim this player holds on its connection's receive estimate while it downloads. Video
	 * selects from the claim's grant less the audio it covers, so without a claim it ranks alone.
	 */
	viewer: {
		/** Where to claim, normally the connection's `viewers`; absent claims nothing. */
		viewers: Getter<Pick<Moq.Bandwidth.Viewers, "watch"> | undefined>;
		/**
		 * The ceiling in bits per second, or absent to follow the catalog: the receive rate that
		 * selects the best enabled video rendition, plus the selected audio rendition's bitrate.
		 * Either applies only while something downloads, and `0` claims nothing.
		 */
		max: Getter<number | undefined>;
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
export type PlayerProps = Inputs<Omit<PlayerInput, "video" | "audio" | "text" | "viewer">> & {
	[K in "video" | "audio" | "text" | "viewer"]?: Inputs<PlayerInput[K]>;
};

/** Owns the broadcast, synchronized media pipelines, and their download policy. */
export class Player {
	/** Inputs wired into the pipeline. Pass signals to change them after construction. */
	readonly in: Readonlys<Omit<PlayerInput, "video" | "audio" | "text" | "viewer">> & {
		readonly [K in "video" | "audio" | "text" | "viewer"]: Readonlys<PlayerInput[K]>;
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
	#videoRate = new Signal<number | undefined>(undefined);
	#audioEnabled = new Signal(false);
	#captionsEnabled = new Signal(false);
	#tail = new Signal(false);
	#signals = new Effect();

	constructor(props: PlayerProps = {}) {
		for (const renamed of ["reload", "announced"]) {
			if (renamed in props) throw new Error(`Watch.Player: \`${renamed}\` was renamed to \`follow\``);
		}
		if ("buffer" in props) throw new Error("Watch.Player: `buffer` was renamed to `lookahead`");
		this.in = {
			origin: getter(props.origin),
			name: getter(props.name ?? Path.empty()),
			enabled: getter(props.enabled ?? true),
			follow: getter(props.follow ?? true),
			catalogFormat: getter<CatalogFormat | undefined>(props.catalogFormat),
			catalog: getter(props.catalog),
			paused: getter(props.paused ?? false),
			delay: getter(props.delay ?? "auto"),
			lookahead: getter(props.lookahead ?? Duration.ZERO),
			stretch: getter(props.stretch ?? STRETCH),
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
			viewer: {
				viewers: getter(props.viewer?.viewers),
				max: getter<number | undefined>(props.viewer?.max),
			},
			text: {
				target: getter<string | undefined>(props.text?.target),
				container: getter(props.text?.container),
			},
		};

		this.broadcast = new Broadcast({ ...this.in, tail: this.#tail });
		this.#signals.cleanup(() => this.broadcast.close());

		const videoSource = new Video.Source({
			broadcast: this.broadcast,
			target: this.in.video.target,
			supported: Video.Decoder.supported,
			rate: this.#videoRate,
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

		this.sync = new Sync({ delay: this.in.delay, lookahead: this.in.lookahead, stretch: this.in.stretch });
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

		// Claim only for the media being downloaded, so a hidden or muted player asks for less.
		const audioBitrate = this.#signals.computed((effect) =>
			effect.get(this.#audioEnabled) ? (effect.get(audioSource.out.config)?.bitrate ?? 0) : 0,
		);
		const max = this.#signals.computed((effect) => {
			const video = effect.get(this.#videoEnabled);
			if (!video && !effect.get(this.#audioEnabled)) return 0;
			const override = effect.get(this.in.viewer.max);
			if (override !== undefined) return override;
			const audio = effect.get(audioBitrate) ?? 0;
			return video ? (effect.get(videoSource.out.ceiling) ?? 0) + audio : audio;
		});
		const grant = holdViewer(this.#signals, this.in.viewer.viewers, max);

		// Video spends what the grant leaves after the audio it covers. A grant that excludes video,
		// while video isn't downloading, says nothing about it, so selection holds its last rate.
		this.#signals.run((effect) => {
			const rate = effect.get(this.#videoEnabled) ? effect.get(grant) : undefined;
			effect.set(
				this.#videoRate,
				rate === undefined ? undefined : Math.max(0, rate - (effect.get(audioBitrate) ?? 0)),
			);
		});

		// A finished broadcast stays live while the media being played has some of it left, so it reads
		// offline only once playback reached the end. Media not downloading has nothing to play.
		this.#signals.run((effect) => {
			const video =
				effect.get(this.#videoEnabled) &&
				effect.get(videoSource.out.config) !== undefined &&
				!effect.get(this.video.out.ended);
			const audio =
				effect.get(this.#audioEnabled) &&
				!effect.get(this.sync.out.instant) &&
				effect.get(audioSource.out.config) !== undefined &&
				!effect.get(this.audio.out.ended);
			this.#tail.set(video || audio);
		});

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
				this.#videoEnabled.set(visible && !effect.get(this.renderer.painted));
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

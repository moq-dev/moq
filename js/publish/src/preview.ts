import { Time } from "@moq/net";
import { Effect, type Getter, getter, type Inputs, type Readonlys, readonlys, Signal } from "@moq/signals";
import * as Video from "@moq/video";
import type { Fanout } from "./fanout";
import type * as Encoder from "./video";

export type { Backend, RendererError } from "@moq/video";

// What the canvas preview renders.
// - `none`: nothing, an easy way to toggle the preview off without removing the element.
// - `source`: the raw captured frames, drawn directly (cheap, no extra codec work).
// - `encoded`: a decoded copy of the encoded video, so the preview shows the same codec
//   artifacts a viewer would receive. This costs a full extra encode + decode pass.
export type Mode = "none" | "source" | "encoded";

// Signals the preview reads.
export type RendererInput = {
	// The canvas to draw into. Undefined renders nothing.
	canvas: Getter<HTMLCanvasElement | undefined>;
	// The captured frames to draw. We subscribe for our own copies and close them.
	frames: Getter<Fanout<VideoFrame> | undefined>;
	// The display size, for sizing the canvas before the first frame.
	display: Getter<{ width: number; height: number } | undefined>;
	// Whether to mirror the video horizontally.
	flip: Getter<boolean>;
	// The encoder to re-encode through in `encoded` mode. Falls back to the raw frame when unset.
	encoder: Getter<Encoder.Encoder | undefined>;
	// What to render. Defaults to `source`.
	mode: Getter<Mode>;
	// Whether to render at all. Defaults to true.
	enabled: Getter<boolean>;
	// Which graphics API draws the preview. See {@link Video.Backend}. Defaults to "auto".
	backend: Getter<Video.Backend>;
};

/** Constructor options for the canvas preview: the frame source plus the encoder to mirror in `encoded` mode. */
export type RendererProps = Inputs<RendererInput>;

/** Renders a `<canvas>` preview of the locally published video. */
export class Renderer {
	readonly in: Readonlys<RendererInput>;

	readonly out: Readonlys<{
		// Why drawing stopped, or undefined while healthy. See {@link Video.RendererError}.
		error: Getter<Video.RendererError | undefined>;
	}>;

	// Whether we've already warned about `encoded` mode without an encoder, so it fires at most once.
	#warnedNoEncoder = false;

	// Where to read the frame from: our own latest capture, or the transcoder in `encoded` mode.
	#source = new Signal<Getter<VideoFrame | undefined> | undefined>(undefined);

	// The newest captured frame, owned here: a preview draws one frame per paint, so anything older
	// is thrown away rather than queued.
	#latest = new Signal<VideoFrame | undefined>(undefined);

	#signals = new Effect();

	constructor(props?: RendererProps) {
		this.in = {
			canvas: getter(props?.canvas),
			frames: getter(props?.frames),
			display: getter(props?.display),
			flip: getter(props?.flip ?? false),
			encoder: getter(props?.encoder),
			mode: getter(props?.mode ?? "source"),
			enabled: getter(props?.enabled ?? true),
			backend: getter(props?.backend ?? "auto"),
		};

		this.#signals.run(this.#runSelect.bind(this));

		// The renderer draws on the next animation frame. A replaced frame is closed a few microtasks
		// before this computed moves on, which cancels that draw before it can touch the closed frame.
		const frame = this.#signals.computed((effect) => {
			const source = effect.get(this.#source);
			return source ? effect.get(source) : undefined;
		});

		const video = new Video.Renderer({
			canvas: this.in.canvas,
			frame,
			// Size the canvas to the frame we're drawing so `encoded` mode shows the true transmitted
			// resolution (which can be smaller than the capture). Fall back to the capture dimensions
			// until the first frame arrives.
			display: this.#signals.computed((effect) => {
				const current = effect.get(frame);
				if (current) return { width: current.displayWidth, height: current.displayHeight };
				return effect.get(this.in.display);
			}),
			presentation: this.#signals.computed((effect) => ({ flip: effect.get(this.in.flip) })),
			backend: this.in.backend,
		});
		this.#signals.cleanup(() => video.close());

		this.out = readonlys({ error: video.out.error });
	}

	// Pick the frame source based on the mode, spinning up a transcoder for `encoded`.
	#runSelect(effect: Effect): void {
		const mode = effect.get(this.in.mode);
		if (mode === "none" || !effect.get(this.in.enabled)) {
			effect.set(this.#source, undefined);
			return;
		}

		if (mode === "encoded") {
			const encoder = effect.get(this.in.encoder);
			if (encoder) {
				const transcode = new Transcode({
					source: this.in.frames,
					config: encoder.out.resolved,
					settings: encoder.config,
				});
				effect.cleanup(() => transcode.close());
				effect.set(this.#source, transcode.out.frame, undefined);
				return;
			}

			// No encoder to mirror: fall back to the raw frame rather than rendering nothing.
			if (!this.#warnedNoEncoder) {
				this.#warnedNoEncoder = true;
				console.warn('moq-publish: preview="encoded" requires an encoder; showing the raw source.');
			}
		}

		this.#pump(effect);
		effect.set(this.#source, this.#latest, undefined);
	}

	// Keep only the newest captured frame. A queue of 1 means a busy main thread drops frames at the
	// fanout instead of drawing a backlog of stale ones.
	#pump(effect: Effect): void {
		const fanout = effect.get(this.in.frames);
		if (!fanout) return;

		const reader = fanout.subscribe(effect, 1).getReader();
		effect.cleanup(() => {
			reader.cancel().catch(() => {});
		});

		effect.cleanup(() => {
			this.#latest.update((prev) => {
				prev?.close();
				return undefined;
			});
		});

		effect.spawn(async () => {
			for (;;) {
				const next = await effect.race(reader.read());
				if (!next?.value) break;

				this.#latest.update((prev) => {
					prev?.close();
					return next.value;
				});
			}
		});
	}

	close(): void {
		this.#signals.close();
	}
}

// Signals the transcoder reads.
export type TranscodeInput = {
	// The captured frames to re-encode. We subscribe for our own copies and close them.
	source: Getter<Fanout<VideoFrame> | undefined>;
	// The resolved WebCodecs config to mirror.
	config: Getter<VideoEncoderConfig | undefined>;
	// The rendition's encoder settings, read for keyframe cadence so the preview's GOP matches the wire.
	settings: Getter<Encoder.Config | undefined>;
};

/** Constructor options for {@link Transcode}. */
export type TranscodeProps = Inputs<TranscodeInput>;

type TranscodeOutput = {
	// The decoded output frame. Owned here, closed on each update and on close().
	frame: Signal<VideoFrame | undefined>;
};

/**
 * Encodes the captured frames with the live rendition settings and decodes the result, so the
 * output frame is what a viewer would actually see after transmission.
 */
export class Transcode {
	readonly in: Readonlys<TranscodeInput>;

	readonly #out: TranscodeOutput = {
		frame: new Signal<VideoFrame | undefined>(undefined),
	};
	readonly out = readonlys(this.#out);

	#signals = new Effect();

	constructor(props?: TranscodeProps) {
		this.in = {
			source: getter(props?.source),
			config: getter(props?.config),
			settings: getter(props?.settings),
		};
		this.#signals.run(this.#run.bind(this));
	}

	#run(effect: Effect): void {
		const config = effect.get(this.in.config);
		if (!config) return;

		const decoder = new VideoDecoder({
			output: (frame: VideoFrame) => {
				this.#out.frame.update((prev) => {
					prev?.close();
					return frame;
				});
			},
			error: (err: Error) => {
				console.warn("preview: decode error", err);
				effect.close();
			},
		});
		effect.cleanup(() => {
			if (decoder.state !== "closed") decoder.close();
		});

		const encoder = new VideoEncoder({
			output: (chunk: EncodedVideoChunk) => {
				if (decoder.state === "configured") decoder.decode(chunk);
			},
			error: (err: Error) => {
				console.warn("preview: encode error", err);
				effect.close();
			},
		});
		effect.cleanup(() => {
			if (encoder.state !== "closed") encoder.close();
		});

		encoder.configure(config);

		// The encoder emits Annex B (inline SPS/PPS on keyframes), so the decoder needs no description.
		decoder.configure({ codec: config.codec, optimizeForLatency: true });

		// Re-key on the same cadence as the real encoder so the decoder can start and recover.
		let lastKeyframe: Time.Micro | undefined;

		effect.run((inner) => {
			const fanout = inner.get(this.in.source);
			if (!fanout) return;

			// Only the newest matters: this feeds a preview, not the wire.
			const reader = fanout.subscribe(inner, 1).getReader();
			inner.cleanup(() => {
				reader.cancel().catch(() => {});
			});

			inner.spawn(async () => {
				for (;;) {
					const next = await inner.race(reader.read());
					if (!next?.value) break;

					// Ours now, so close it once the encoder has taken what it needs.
					const frame = next.value;
					try {
						if (encoder.state !== "configured") continue;

						// Mirror Encoder.serve: default to a 2s GOP unless the rendition overrides it.
						const settings = this.in.settings.peek();
						const interval = settings?.keyframeInterval ?? Time.Milli.fromSecond(2 as Time.Second);

						const timestamp = frame.timestamp as Time.Micro;
						const keyFrame =
							lastKeyframe === undefined || lastKeyframe + Time.Micro.fromMilli(interval) <= timestamp;
						if (keyFrame) lastKeyframe = timestamp;

						encoder.encode(frame, { keyFrame });
					} finally {
						frame.close();
					}
				}
			});
		});

		effect.cleanup(() => {
			this.#out.frame.update((prev) => {
				prev?.close();
				return undefined;
			});
		});
	}

	/** Stop transcoding and close the last output frame. */
	close(): void {
		this.#signals.close();
		this.#out.frame.update((prev) => {
			prev?.close();
			return undefined;
		});
	}
}

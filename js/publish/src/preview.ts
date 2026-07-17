import { Time } from "@moq/net";
import { Effect, type Getter, getter, type Inputs, type Readonlys, readonlys, Signal } from "@moq/signals";
import type * as Video from "./video";

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
	// The captured frame to draw (owned by the capture pipeline; never closed here).
	frame: Getter<VideoFrame | undefined>;
	// The display size, for sizing the canvas before the first frame.
	display: Getter<{ width: number; height: number } | undefined>;
	// Whether to mirror the video horizontally.
	flip: Getter<boolean>;
	// The encoder to re-encode through in `encoded` mode. Falls back to the raw frame when unset.
	encoder: Getter<Video.Encoder | undefined>;
	// What to render. Defaults to `source`.
	mode: Getter<Mode>;
	// Whether to render at all. Defaults to true.
	enabled: Getter<boolean>;
};

/** Constructor options for the canvas preview: the frame source plus the encoder to mirror in `encoded` mode. */
export type RendererProps = Inputs<RendererInput>;

/** Renders a `<canvas>` preview of the locally published video. */
export class Renderer {
	readonly in: Readonlys<RendererInput>;

	// Whether we've already warned about `encoded` mode without an encoder, so it fires at most once.
	#warnedNoEncoder = false;

	// The frame to draw. Just a pointer to a frame owned elsewhere (the capture pipeline or the
	// transcoder), so we never close it ourselves.
	#frame = new Signal<VideoFrame | undefined>(undefined);

	#ctx = new Signal<CanvasRenderingContext2D | undefined>(undefined);
	#signals = new Effect();

	constructor(props?: RendererProps) {
		this.in = {
			canvas: getter(props?.canvas),
			frame: getter(props?.frame),
			display: getter(props?.display),
			flip: getter(props?.flip ?? false),
			encoder: getter(props?.encoder),
			mode: getter(props?.mode ?? "source"),
			enabled: getter(props?.enabled ?? true),
		};

		this.#signals.run((effect) => {
			const canvas = effect.get(this.in.canvas);
			this.#ctx.set(canvas?.getContext("2d") ?? undefined);
		});

		this.#signals.run(this.#runSelect.bind(this));
		this.#signals.run(this.#runRender.bind(this));
	}

	// Pick the frame source based on the mode, spinning up a transcoder for `encoded`.
	#runSelect(effect: Effect): void {
		const mode = effect.get(this.in.mode);
		if (mode === "none" || !effect.get(this.in.enabled)) {
			effect.set(this.#frame, undefined);
			return;
		}

		if (mode === "encoded") {
			const encoder = effect.get(this.in.encoder);
			if (encoder) {
				const transcode = new Transcode({
					source: this.in.frame,
					config: encoder.out.resolved,
					settings: encoder.config,
				});
				effect.cleanup(() => transcode.close());
				effect.proxy(this.#frame, transcode.out.frame);
				return;
			}

			// No encoder to mirror: fall back to the raw frame rather than rendering nothing.
			if (!this.#warnedNoEncoder) {
				this.#warnedNoEncoder = true;
				console.warn('moq-publish: preview="encoded" requires an encoder; showing the raw source.');
			}
		}

		effect.proxy(this.#frame, this.in.frame);
	}

	#runRender(effect: Effect): void {
		const ctx = effect.get(this.#ctx);
		if (!ctx) return;

		const frame = effect.get(this.#frame);
		const display = effect.get(this.in.display);
		const flip = effect.get(this.in.flip);

		// Size the canvas to the frame we're drawing so `encoded` mode shows the true transmitted
		// resolution (which can be smaller than the capture). Fall back to the capture dimensions
		// until the first frame arrives.
		const width = frame?.displayWidth ?? display?.width;
		const height = frame?.displayHeight ?? display?.height;

		// Setting width/height clears the canvas, so only resize when the dimensions actually change.
		if (width && height && (ctx.canvas.width !== width || ctx.canvas.height !== height)) {
			ctx.canvas.width = width;
			ctx.canvas.height = height;
		}

		ctx.fillStyle = "#000";
		ctx.fillRect(0, 0, ctx.canvas.width, ctx.canvas.height);

		if (!frame) return;

		ctx.save();
		if (flip) {
			ctx.scale(-1, 1);
			ctx.translate(-ctx.canvas.width, 0);
		}
		ctx.drawImage(frame, 0, 0, ctx.canvas.width, ctx.canvas.height);
		ctx.restore();
	}

	close(): void {
		this.#signals.close();
	}
}

// Signals the transcoder reads.
export type TranscodeInput = {
	// The captured frame to re-encode (owned by the capture pipeline; never closed here).
	source: Getter<VideoFrame | undefined>;
	// The resolved WebCodecs config to mirror.
	config: Getter<VideoEncoderConfig | undefined>;
	// The rendition's encoder settings, read for keyframe cadence so the preview's GOP matches the wire.
	settings: Getter<Video.Config | undefined>;
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
			const frame = inner.get(this.in.source);
			if (!frame) return;
			if (encoder.state !== "configured") return;

			// Mirror Encoder.serve: default to a 2s GOP unless the rendition overrides it.
			const settings = inner.get(this.in.settings);
			const interval = settings?.keyframeInterval ?? Time.Milli.fromSecond(2 as Time.Second);

			const timestamp = frame.timestamp as Time.Micro;
			const keyFrame = lastKeyframe === undefined || lastKeyframe + Time.Micro.fromMilli(interval) <= timestamp;
			if (keyFrame) lastKeyframe = timestamp;

			// The capture pipeline owns and closes the frame, so we just read it here.
			encoder.encode(frame, { keyFrame });
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

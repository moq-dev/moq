import { Time } from "@moq/net";
import { Effect, type Getter, Signal } from "@moq/signals";
import type * as Video from "./video";

// What the canvas preview renders.
// - `none`: nothing, an easy way to toggle the preview off without removing the element.
// - `source`: the raw captured frames, drawn directly (cheap, no extra codec work).
// - `encoded`: a decoded copy of the encoded video, so the preview shows the same codec
//   artifacts a viewer would receive. This costs a full extra encode + decode pass.
export type Mode = "none" | "source" | "encoded";

export type RendererProps = {
	canvas: HTMLCanvasElement | Signal<HTMLCanvasElement | undefined>;
	video: Video.Root;
	mode?: Mode | Signal<Mode>;
	enabled?: boolean | Signal<boolean>;
};

// Renders a <canvas> preview of the locally published video.
export class Renderer {
	canvas: Signal<HTMLCanvasElement | undefined>;
	mode: Signal<Mode>;
	enabled: Signal<boolean>;

	#video: Video.Root;

	// The frame to draw. Just a pointer to a frame owned elsewhere (the capture pipeline or the
	// transcoder), so we never close it ourselves.
	#frame = new Signal<VideoFrame | undefined>(undefined);

	#ctx = new Signal<CanvasRenderingContext2D | undefined>(undefined);
	#signals = new Effect();

	constructor(props: RendererProps) {
		this.canvas = Signal.from(props.canvas);
		this.mode = Signal.from(props.mode ?? "source");
		this.enabled = Signal.from(props.enabled ?? true);
		this.#video = props.video;

		this.#signals.run((effect) => {
			const canvas = effect.get(this.canvas);
			this.#ctx.set(canvas?.getContext("2d") ?? undefined);
		});

		this.#signals.run(this.#runSelect.bind(this));
		this.#signals.run(this.#runRender.bind(this));
	}

	// Pick the frame source based on the mode, spinning up a transcoder for `encoded`.
	#runSelect(effect: Effect): void {
		const mode = effect.get(this.mode);
		if (mode === "none" || !effect.get(this.enabled)) {
			effect.set(this.#frame, undefined);
			return;
		}

		if (mode === "encoded") {
			const transcode = new Transcode({
				source: this.#video.frame,
				config: this.#video.hd.resolved,
				settings: this.#video.hd.config,
			});
			effect.cleanup(() => transcode.close());
			effect.proxy(this.#frame, transcode.frame);
			return;
		}

		effect.proxy(this.#frame, this.#video.frame);
	}

	#runRender(effect: Effect): void {
		const ctx = effect.get(this.#ctx);
		if (!ctx) return;

		const frame = effect.get(this.#frame);
		const display = effect.get(this.#video.display);
		const flip = effect.get(this.#video.flip);

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

type TranscodeProps = {
	source: Getter<VideoFrame | undefined>;
	config: Getter<VideoEncoderConfig | undefined>;
	// The rendition's encoder settings, read for keyframe cadence so the preview's GOP matches the wire.
	settings?: Getter<Video.EncoderConfig | undefined>;
};

// Encodes the captured frames with the live rendition settings and decodes the result, so the
// output frame is what a viewer would actually see after transmission.
export class Transcode {
	// The decoded output frame. Owned here, closed on each update and on close().
	frame = new Signal<VideoFrame | undefined>(undefined);

	#source: Getter<VideoFrame | undefined>;
	#config: Getter<VideoEncoderConfig | undefined>;
	#settings?: Getter<Video.EncoderConfig | undefined>;
	#signals = new Effect();

	constructor(props: TranscodeProps) {
		this.#source = props.source;
		this.#config = props.config;
		this.#settings = props.settings;
		this.#signals.run(this.#run.bind(this));
	}

	#run(effect: Effect): void {
		const config = effect.get(this.#config);
		if (!config) return;

		const decoder = new VideoDecoder({
			output: (frame: VideoFrame) => {
				this.frame.update((prev) => {
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
			const frame = inner.get(this.#source);
			if (!frame) return;
			if (encoder.state !== "configured") return;

			// Mirror Encoder.serve: default to a 2s GOP unless the rendition overrides it.
			const settings = this.#settings ? inner.get(this.#settings) : undefined;
			const interval = settings?.keyframeInterval ?? Time.Milli.fromSecond(2 as Time.Second);

			const timestamp = frame.timestamp as Time.Micro;
			const keyFrame = lastKeyframe === undefined || lastKeyframe + Time.Micro.fromMilli(interval) <= timestamp;
			if (keyFrame) lastKeyframe = timestamp;

			// The capture pipeline owns and closes the frame, so we just read it here.
			encoder.encode(frame, { keyFrame });
		});

		effect.cleanup(() => {
			this.frame.update((prev) => {
				prev?.close();
				return undefined;
			});
		});
	}

	close(): void {
		this.#signals.close();
		this.frame.update((prev) => {
			prev?.close();
			return undefined;
		});
	}
}

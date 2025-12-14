import type * as Moq from "@moq/lite";
import { Effect, type Getter, Signal } from "@moq/signals";
import type * as Catalog from "../../catalog";
import { u53 } from "../../catalog";
import * as Frame from "../../frame";
import * as Time from "../../time";
import { isFirefox } from "../../util/hacks";
import type { Source } from "./types";

export interface EncoderProps {
	enabled?: boolean | Signal<boolean>;
	config?: EncoderConfig | Signal<EncoderConfig | undefined>;
}

// TODO support signals?
export interface EncoderConfig {
	// If not provided, the encoder will select the best codec.
	codec?: string;

	// Constrain the encoded width/height in pixels.
	// TODO figure out how this interacts with the width/height props.
	maxPixels?: number;

	// The interval at which to insert keyframes. (default: 2000 milliseconds)
	keyframeInterval?: Time.Milli;

	// If not provided, the encoder will use the best bitrate for the given width, height, and framerate.
	maxBitrate?: number;

	// Multiply the number of pixels by this value to get the bitrate. (default: 0.07)
	// NOTE: This is multiplied by the codecScale (1.0 for h264) to get the final scale.
	bitrateScale?: number;

	// TODO actually enforce this
	frameRate?: number;
}

export class Encoder {
	enabled: Signal<boolean>;
	source: Signal<Source | undefined>;
	frame: Getter<VideoFrame | undefined>;

	#catalog = new Signal<Catalog.VideoConfig | undefined>(undefined);
	readonly catalog: Getter<Catalog.VideoConfig | undefined> = this.#catalog;

	#signals = new Effect();

	// The user provided config.
	config: Signal<EncoderConfig | undefined>;

	// The output dimensions of the video in pixels.
	#dimensions = new Signal<{ width: number; height: number } | undefined>(undefined);

	// The video encoder config.
	#config = new Signal<VideoEncoderConfig | undefined>(undefined);

	// True when the encoder is actively serving a track.
	active = new Signal<boolean>(false);

	constructor(frame: Getter<VideoFrame | undefined>, source: Signal<Source | undefined>, props?: EncoderProps) {
		this.frame = frame;
		this.source = source;
		this.enabled = Signal.from(props?.enabled ?? false);
		this.config = Signal.from(props?.config);

		this.#signals.effect(this.#runCatalog.bind(this));
		this.#signals.effect(this.#runConfig.bind(this));
		this.#signals.effect(this.#runDimensions.bind(this));
	}

	serve(track: Moq.Track, effect: Effect): void {
		const enabled = effect.get(this.enabled);
		if (!enabled) return;

		effect.set(this.active, true, false);

		effect.spawn(async () => {
			let group: Moq.Group | undefined;
			effect.cleanup(() => group?.close());

			let groupTimestamp: Time.Micro | undefined;

			const encoder = new VideoEncoder({
				output: (frame: EncodedVideoChunk) => {
					if (frame.type === "key") {
						groupTimestamp = frame.timestamp as Time.Micro;
						group?.close();
						group = track.appendGroup();
					} else if (!group) {
						throw new Error("no keyframe");
					}

					const buffer = Frame.encode(frame, frame.timestamp as Time.Micro);
					group?.writeFrame(buffer);
				},
				error: (err: Error) => {
					track.close(err);
					group?.close(err);
				},
			});

			effect.cleanup(() => encoder.close());

			effect.effect(() => {
				const config = effect.get(this.#config);
				if (!config) return;

				encoder.configure(config);
			});

			effect.effect((effect) => {
				const frame = effect.get(this.frame);
				if (!frame) return;

				if (encoder.state !== "configured") return;

				// This doesn't need to be reactive.
				const interval = this.config.peek()?.keyframeInterval ?? Time.Milli.fromSecond(2 as Time.Second);

				// Force a keyframe if this is the first frame (no group yet), or GOP elapsed.
				const keyFrame = !groupTimestamp || groupTimestamp + Time.Micro.fromMilli(interval) <= frame.timestamp;
				if (keyFrame) {
					groupTimestamp = frame.timestamp as Time.Micro;
				}

				encoder.encode(frame, { keyFrame });
			});
		});
	}

	// Returns the catalog for the configured settings.
	#runCatalog(effect: Effect): void {
		const enabled = effect.get(this.enabled);
		if (!enabled) return;

		const config = effect.get(this.#config);
		if (!config) return;

		const catalog: Catalog.VideoConfig = {
			codec: config.codec,
			bitrate: config.bitrate ? u53(config.bitrate) : undefined,
			framerate: config.framerate,
			codedWidth: u53(config.width),
			codedHeight: u53(config.height),
			optimizeForLatency: true,
		};

		effect.set(this.#catalog, catalog);
	}

	#runConfig(effect: Effect): void {
		const enabled = effect.get(this.enabled);
		if (!enabled) return;

		const source = effect.get(this.source);
		if (!source) return;

		// NOTE: These already factor in user provided maxPixels.
		// It's a separate effect in order to deduplicate.
		const dimensions = effect.get(this.#dimensions);
		if (!dimensions) return;

		const settings = source.getSettings();
		const framerate = settings.frameRate ?? 30;

		// Get the user provided config.
		const user = effect.get(this.config) ?? {};

		const maxPixels = user.maxPixels ?? dimensions.width * dimensions.height;
		const bitrateScale = user.bitrateScale ?? 0.07;

		effect.spawn(async () => {
			const codec = await this.#bestCodec(effect);
			if (!codec) return;

			// TARGET BITRATE CALCULATION (h264)
			// 480p@30 = 1.0mbps
			// 480p@60 = 1.5mbps
			// 720p@30 = 2.5mbps
			// 720p@60 = 3.5mpbs
			// 1080p@30 = 4.5mbps
			// 1080p@60 = 6.0mbps

			// 30fps is the baseline, applying a multiplier for higher framerates.
			// Framerate does not cause a multiplicative increase in bitrate because of delta encoding.
			// TODO Make this better.
			const framerateFactor = 30.0 + (framerate - 30) / 2;
			let bitrate = Math.round(maxPixels * bitrateScale * framerateFactor);

			// ACTUAL BITRATE CALCULATION
			// 480p@30 = 409920 * 30 * 0.07 = 0.9 Mb/s
			// 480p@60 = 409920 * 45 * 0.07 = 1.3 Mb/s
			// 720p@30 = 921600 * 30 * 0.07 = 1.9 Mb/s
			// 720p@60 = 921600 * 45 * 0.07 = 2.9 Mb/s
			// 1080p@30 = 2073600 * 30 * 0.07 = 4.4 Mb/s
			// 1080p@60 = 2073600 * 45 * 0.07 = 6.5 Mb/s

			// We scale the bitrate for more efficient codecs.
			// TODO This shouldn't be linear, as the efficiency is very similar at low bitrates.
			if (codec.startsWith("avc1")) {
				bitrate *= 1.0; // noop
			} else if (codec.startsWith("hev1")) {
				bitrate *= 0.7;
			} else if (codec.startsWith("vp09")) {
				bitrate *= 0.8;
			} else if (codec.startsWith("av01")) {
				bitrate *= 0.6;
			} else if (codec === "vp8") {
				// Worse than H.264 but it's a backup plan.
				bitrate *= 1.1;
			} else {
				throw new Error(`unknown codec: ${codec}`);
			}

			bitrate = Math.min(bitrate, user.maxBitrate || bitrate);

			const config: VideoEncoderConfig = {
				codec,
				width: dimensions.width,
				height: dimensions.height,
				framerate,
				bitrate,
				avc: codec.startsWith("avc1") ? { format: "annexb" } : undefined,
				// @ts-expect-error Typescript needs to be updated.
				hevc: codec.startsWith("hev1") ? { format: "annexb" } : undefined,
				latencyMode: "realtime",
				hardwareAcceleration: "prefer-hardware",
			};

			effect.set(this.#config, config);
		});
	}

	#runDimensions(effect: Effect): void {
		const user = effect.get(this.config);

		const frame = effect.get(this.frame);
		if (!frame) return;

		const maxPixels = user?.maxPixels ?? frame.codedWidth * frame.codedHeight;
		const ratio = Math.min(Math.sqrt(maxPixels / (frame.codedWidth * frame.codedHeight)), 1);

		// Make sure width/height is a power of 16
		// TODO should this be on a per-codec basis?
		const width = 16 * Math.floor((frame.codedWidth * ratio) / 16);
		const height = 16 * Math.floor((frame.codedHeight * ratio) / 16);

		effect.set(this.#dimensions, { width, height });
	}

	// Try to determine the best config for the given settings.
	async #bestCodec(effect: Effect): Promise<string | undefined> {
		const config = effect.get(this.config);
		const required = config?.codec ?? "";

		const dimensions = effect.get(this.#dimensions);
		if (!dimensions) return;

		// A list of codecs to try, in order of preference.
		const HARDWARE_CODECS = [
			// VP9
			// More likely to have hardware decoding, but hardware encoding is less likely.
			"vp09.00.10.08",
			"vp09", // Browser's choice

			// H.264
			// Almost always has hardware encoding and decoding.
			"avc1.640028",
			"avc1.4D401F",
			"avc1.42E01E",
			"avc1",

			// AV1
			// One day will get moved higher up the list, but hardware decoding is rare.
			"av01.0.08M.08",
			"av01",

			// HEVC (aka h.265)
			// More likely to have hardware encoding, but less likely to be supported (licensing issues).
			// Unfortunately, Firefox doesn't support decoding so it's down here at the bottom.
			"hev1.1.6.L93.B0",
			"hev1", // Browser's choice

			// VP8
			// A terrible codec but it's easy.
			"vp8",
		];

		const SOFTWARE_CODECS = [
			// Now try software encoding for simple enough codecs.
			// H.264
			"avc1.640028", // High
			"avc1.4D401F", // Main
			"avc1.42E01E", // Baseline
			"avc1",

			// VP8
			"vp8",

			// VP9
			// It's a bit more expensive to encode so we shy away from it.
			"vp09.00.10.08",
			"vp09",

			// HEVC (aka h.265)
			// This likely won't work because of licensing issues.
			"hev1.1.6.L93.B0",
			"hev1", // Browser's choice

			// AV1
			// Super expensive to encode so it's our last choice.
			"av01.0.08M.08",
			"av01",
		];

		// Try hardware encoding first.
		// We can't reliably detect hardware encoding on Firefox: https://github.com/w3c/webcodecs/issues/896
		if (!isFirefox) {
			for (const codec of HARDWARE_CODECS) {
				if (!codec.startsWith(required)) continue;

				const hardware: VideoEncoderConfig = {
					codec,
					width: dimensions.width,
					height: dimensions.height,
					latencyMode: "realtime",
					hardwareAcceleration: "prefer-hardware",
					avc: codec.startsWith("avc1") ? { format: "annexb" } : undefined,
					// @ts-expect-error Typescript needs to be updated.
					hevc: codec.startsWith("hev1") ? { format: "annexb" } : undefined,
				};

				const { supported } = await VideoEncoder.isConfigSupported(hardware);
				if (supported) return codec;
			}
		}

		// Try software encoding.
		for (const codec of SOFTWARE_CODECS) {
			if (!codec.startsWith(required)) continue;

			const software: VideoEncoderConfig = {
				codec,
				width: dimensions.width,
				height: dimensions.height,
				latencyMode: "realtime",
				hardwareAcceleration: "prefer-software",
				avc: codec.startsWith("avc1") ? { format: "annexb" } : undefined,
				// @ts-expect-error Typescript needs to be updated.
				hevc: codec.startsWith("hev1") ? { format: "annexb" } : undefined,
			};

			const { supported } = await VideoEncoder.isConfigSupported(software);
			if (supported) return codec;
		}

		throw new Error("no supported codec");
	}

	close() {
		this.#signals.close();
	}
}

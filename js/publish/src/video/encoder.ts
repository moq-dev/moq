import * as Catalog from "@moq/hang/catalog";
import * as Container from "@moq/hang/container";
import * as Util from "@moq/hang/util";
import type * as Moq from "@moq/net";
import { Time } from "@moq/net";
import { Effect, type Getter, getter, type Inputs, type Readonlys, readonlys, Signal } from "@moq/signals";
import type { Broadcast } from "../broadcast";
import type { Capture } from "./capture";
import type { Source } from "./types";

/** Cumulative encoder output totals, measured from the chunks the encoder produces. */
export interface Stats {
	/** Total frames encoded while serving. Monotonic; diff over an interval for a frame rate. */
	frames: number;

	/** Total bytes encoded while serving. Monotonic; diff over an interval for an upload bitrate. */
	bytes: number;

	/** Total keyframes encoded while serving. Divide frames by this for the average GOP length. */
	keyframes: number;
}

// TODO support signals?
/** Encoder tuning knobs. All optional; the encoder auto-sizes anything left unset. */
export interface Config {
	// If not provided, the encoder will select the best codec.
	codec?: string;

	// Constrain the encoded width/height in pixels. If unset, source width.max and height.max
	// constraints provide the cap when both are present.
	maxPixels?: number;

	// Cap the encoded resolution to this fraction of the source pixel count.
	// For example 0.25 yields a quarter of the pixels (half the width and height),
	// scaling with the source instead of assuming a fixed resolution.
	// When combined with maxPixels, the smaller resulting cap wins.
	maxScale?: number;

	// The interval at which to insert keyframes. (default: 2000 milliseconds)
	keyframeInterval?: Time.Milli;

	// If not provided, the encoder will use the best bitrate for the given width, height, and framerate.
	maxBitrate?: number;

	// Multiply the number of pixels by this value to get the bitrate. (default: 0.07)
	// NOTE: This is multiplied by the codecScale (1.0 for h264) to get the final scale.
	bitrateScale?: number;

	// Cap the encoded frame rate. If set below the captured rate, frames are dropped to hit this target.
	// Also feeds the bitrate calculation and the encoder config. If unset, the captured track's rate is used.
	frameRate?: number;
}

// Signals the encoder reads.
export type EncoderInput = {
	// Whether to publish (and encode) this rendition. When false the rendition drops out of the
	// catalog and stops encoding, but stays registered so a subscriber still gets an idle track.
	enabled: Getter<boolean>;

	// The broadcast to register the rendition on. Undefined resolves the config for a local preview
	// but has nowhere to publish.
	broadcast: Getter<Broadcast | undefined>;

	// The capture pipeline supplying frames and the source track.
	capture: Getter<Capture | undefined>;

	// Estimated send bandwidth cap in bits/sec. Caps the bitrate (with a safety margin) only when no
	// explicit maxBitrate is set.
	bandwidth: Getter<number | undefined>;
};

/** Constructor options: the wired inputs plus the live-editable {@link Config} tuning knobs. */
export type EncoderProps = Inputs<EncoderInput> & {
	// User tuning knobs. Seed a value or wire a Signal; also live-editable via `encoder.config`.
	config?: Config | Signal<Config | undefined>;
};

type EncoderOutput = {
	// The catalog config published for this rendition, or undefined while disabled.
	catalog: Signal<Catalog.VideoConfig | undefined>;
	// The resolved WebCodecs config (codec, bitrate, dimensions), available even with no subscriber.
	// Exposed so a local preview can re-encode with identical settings to mirror the wire output.
	resolved: Signal<VideoEncoderConfig | undefined>;
	// True when a subscriber is attached and we're encoding.
	active: Signal<boolean>;
	// Cumulative output totals (frames, bytes, keyframes) measured while serving.
	stats: Signal<Stats>;
};

/**
 * A single video rendition encoder.
 *
 * Registers itself on the {@link Broadcast} under {@link name} (via `broadcast.video(name)`), resolves
 * the best codec/bitrate/dimensions for the capture, and encodes frames only while a subscriber is
 * attached (the demand gate). Rename by constructing a new encoder; the name is not a signal.
 */
export class Encoder {
	/** The full track name of this rendition, e.g. `"video/hd"`. */
	readonly name: string;

	readonly in: Readonlys<EncoderInput>;

	/** The live-editable encoder tuning knobs (codec, dimensions, bitrate, frame rate). */
	config: Signal<Config | undefined>;

	readonly #out: EncoderOutput = {
		catalog: new Signal<Catalog.VideoConfig | undefined>(undefined),
		resolved: new Signal<VideoEncoderConfig | undefined>(undefined),
		active: new Signal<boolean>(false),
		stats: new Signal<Stats>({ frames: 0, bytes: 0, keyframes: 0 }),
	};
	readonly out = readonlys(this.#out);

	// The output dimensions of the video in pixels.
	#dimensions = new Signal<{ width: number; height: number } | undefined>(undefined);

	#signals = new Effect();

	constructor(name: string, props?: EncoderProps) {
		this.name = name;
		this.in = {
			enabled: getter(props?.enabled ?? false),
			broadcast: getter(props?.broadcast),
			capture: getter(props?.capture),
			bandwidth: getter(props?.bandwidth),
		};
		this.config = Signal.from(props?.config);

		this.#signals.run(this.#runCatalog.bind(this));
		this.#signals.run(this.#runResolved.bind(this));
		this.#signals.run(this.#runDimensions.bind(this));
		this.#signals.run(this.#runRegister.bind(this));
	}

	// Register the rendition on the broadcast and drive its catalog + encode loop. Re-registers cleanly
	// when the broadcast swaps.
	#runRegister(effect: Effect): void {
		const broadcast = effect.get(this.in.broadcast);
		if (!broadcast) return;

		const rendition = broadcast.video(this.name);
		effect.cleanup(() => rendition.close());

		// Publish the resolved catalog config; undefined (while disabled) drops it from the catalog.
		effect.proxy(rendition.config, this.out.catalog);

		// Encode only while enabled and a subscriber is attached (the demand gate).
		effect.run((effect) => {
			const enabled = effect.get(this.in.enabled);
			const track = effect.get(rendition.track);
			effect.set(this.#out.active, enabled && !!track, false);
			if (!enabled || !track) return;

			this.#encode(track, effect);
		});
	}

	// Encode captured frames into the track producer, reconfiguring when the resolved config changes.
	#encode(track: Moq.Track.Producer, effect: Effect): void {
		const capture = effect.get(this.in.capture);
		if (!capture) return;

		const producer = new Container.Legacy.Producer(track);
		effect.cleanup(() => producer.close());

		let lastKeyframe: Time.Micro | undefined;
		let lastEncoded: Time.Micro | undefined;

		effect.spawn(async () => {
			const encoder = new VideoEncoder({
				output: (frame: EncodedVideoChunk) => {
					const key = frame.type === "key";
					if (key) {
						lastKeyframe = frame.timestamp as Time.Micro;
					}

					this.#out.stats.update((stats) => ({
						frames: stats.frames + 1,
						bytes: stats.bytes + frame.byteLength,
						keyframes: key ? stats.keyframes + 1 : stats.keyframes,
					}));

					producer.encode(frame, frame.timestamp as Time.Micro, key);
				},
				error: (err: Error) => {
					producer.close(err);
				},
			});

			effect.cleanup(() => encoder.close());

			effect.run((effect) => {
				const config = effect.get(this.out.resolved);
				if (!config) return;

				encoder.configure(config);
			});

			effect.run((effect) => {
				const frame = effect.get(capture.out.frame);
				if (!frame) return;

				if (encoder.state !== "configured") return;

				// This doesn't need to be reactive.
				const config = this.config.peek();

				// Pace to the target frame rate by dropping frames that arrive too soon.
				// Allow half an interval of slack so jittery capture timestamps don't drop a frame we meant to keep.
				// The shared frame Signal owner closes frames, so we just skip encoding here.
				const targetFrameRate = config?.frameRate;
				if (targetFrameRate && lastEncoded !== undefined) {
					const minGap = Time.Micro.fromSecond((1 / targetFrameRate) as Time.Second);
					if (frame.timestamp - lastEncoded < minGap - minGap / 2) return;
				}
				lastEncoded = frame.timestamp as Time.Micro;

				const interval = config?.keyframeInterval ?? Time.Milli.fromSecond(2 as Time.Second);

				// Force a keyframe if this is the first frame (no group yet), or GOP elapsed.
				const keyFrame = !lastKeyframe || lastKeyframe + Time.Micro.fromMilli(interval) <= frame.timestamp;
				if (keyFrame) {
					lastKeyframe = frame.timestamp as Time.Micro;
				}

				encoder.encode(frame, { keyFrame });
			});
		});
	}

	// Returns the catalog for the configured settings, or undefined while disabled / unresolved.
	#runCatalog(effect: Effect): void {
		const enabled = effect.get(this.in.enabled);
		const config = effect.get(this.out.resolved);
		if (!enabled || !config) {
			effect.set(this.#out.catalog, undefined);
			return;
		}

		const catalog: Catalog.VideoConfig = {
			codec: config.codec,
			bitrate: config.bitrate ? Catalog.u53(config.bitrate) : undefined,
			framerate: config.framerate,
			codedWidth: Catalog.u53(config.width),
			codedHeight: Catalog.u53(config.height),
			optimizeForLatency: true,
			container: { kind: "legacy" } as const,
			// Each frame is flushed immediately, so the jitter is one frame duration.
			jitter: config.framerate ? Catalog.u53(Math.ceil(1000 / config.framerate)) : undefined,
		};

		effect.set(this.#out.catalog, catalog);
	}

	#runResolved(effect: Effect): void {
		// NOTE: dimensions already factors in user provided maxPixels.
		// It's a separate effect in order to deduplicate.
		if (!effect.get(this.in.enabled)) return;

		const capture = effect.get(this.in.capture);
		if (!capture) return;

		const source = effect.get(capture.in.source);
		if (!source) return;

		const dimensions = effect.get(this.#dimensions);
		if (!dimensions) return;

		const settings = source.getSettings();

		// Get the user provided config.
		const user = effect.get(this.config) ?? {};

		// Prefer the explicitly requested rate; the encode loop drops frames to enforce it.
		const framerate = user.frameRate ?? settings.frameRate ?? 30;

		const maxPixels = user.maxPixels ?? dimensions.width * dimensions.height;
		const bitrateScale = user.bitrateScale ?? 0.07;

		effect.spawn(async () => {
			const detectedCodec = await this.#bestCodec(effect);
			if (!detectedCodec) return;

			const { codec, hardwareAcceleration } = detectedCodec;

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

			bitrate = Math.round(Math.min(bitrate, user.maxBitrate || bitrate));

			// If no explicit maxBitrate, cap to the estimated send bandwidth (with 90% safety margin).
			if (!user.maxBitrate) {
				const estimate = effect.get(this.in.bandwidth);
				if (estimate != null) {
					// Reserve ~10% for audio and protocol overhead.
					const cap = Math.round(estimate * 0.9);
					bitrate = Math.min(bitrate, cap);
				}
			}

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
				hardwareAcceleration,
			};

			effect.set(this.#out.resolved, config);
		});
	}

	#runDimensions(effect: Effect): void {
		const capture = effect.get(this.in.capture);
		if (!capture) return;

		const frame = effect.get(capture.out.frame);
		if (!frame) return;

		const source = effect.get(capture.in.source);
		if (!source) return;

		const user = effect.get(this.config);

		const sourcePixels = frame.codedWidth * frame.codedHeight;

		// maxPixels caps absolutely; maxScale caps relative to the source. The smaller cap wins.
		let maxPixels = user?.maxPixels ?? sourceConstraintPixels(source) ?? sourcePixels;
		if (user?.maxScale !== undefined) {
			if (!Number.isFinite(user.maxScale) || user.maxScale <= 0) {
				throw new Error(`maxScale must be a finite number greater than 0: ${user.maxScale}`);
			}
			maxPixels = Math.min(maxPixels, sourcePixels * user.maxScale);
		}

		const ratio = Math.min(Math.sqrt(maxPixels / sourcePixels), 1);

		// Make sure width/height is a power of 16
		// TODO should this be on a per-codec basis?
		const width = 16 * Math.floor((frame.codedWidth * ratio) / 16);
		const height = 16 * Math.floor((frame.codedHeight * ratio) / 16);

		effect.set(this.#dimensions, { width, height });
	}

	// Try to determine the best config for the given settings.
	async #bestCodec(effect: Effect): Promise<
		| {
				codec: string;
				hardwareAcceleration: HardwareAcceleration;
		  }
		| undefined
	> {
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
		// Safari accepts every codec under `prefer-hardware` and echoes the hint straight back, but
		// VideoToolbox only hardware-encodes H.264 and HEVC. Skip the hardware pass and let it fall
		// through to the software pass, which is H.264 first, since Safari routes that through
		// VideoToolbox anyway regardless of the hint.
		if (!Util.Hacks.isFirefox && !Util.Hacks.isSafari) {
			for (const codec of HARDWARE_CODECS) {
				if (!codec.startsWith(required)) continue;

				const hardwareAcceleration: HardwareAcceleration = "prefer-hardware";

				const hardware: VideoEncoderConfig = {
					codec,
					width: dimensions.width,
					height: dimensions.height,
					latencyMode: "realtime",
					hardwareAcceleration,
					avc: codec.startsWith("avc1") ? { format: "annexb" } : undefined,
					// @ts-expect-error Typescript needs to be updated.
					hevc: codec.startsWith("hev1") ? { format: "annexb" } : undefined,
				};

				const { supported } = await VideoEncoder.isConfigSupported(hardware);
				if (supported) return { codec, hardwareAcceleration };
			}
		}

		// Try software encoding.
		for (const codec of SOFTWARE_CODECS) {
			if (!codec.startsWith(required)) continue;

			const hardwareAcceleration: HardwareAcceleration = "prefer-software";

			const software: VideoEncoderConfig = {
				codec,
				width: dimensions.width,
				height: dimensions.height,
				latencyMode: "realtime",
				hardwareAcceleration,
				avc: codec.startsWith("avc1") ? { format: "annexb" } : undefined,
				// @ts-expect-error Typescript needs to be updated.
				hevc: codec.startsWith("hev1") ? { format: "annexb" } : undefined,
			};

			const { supported } = await VideoEncoder.isConfigSupported(software);
			if (supported) return { codec, hardwareAcceleration };
		}

		throw new Error("no supported codec");
	}

	close() {
		this.#signals.close();
	}
}

function sourceConstraintPixels(source: Source): number | undefined {
	const constraints = source.getConstraints();
	const width = constraintMax(constraints.width);
	const height = constraintMax(constraints.height);

	return width !== undefined && height !== undefined ? width * height : undefined;
}

function constraintMax(value: MediaTrackConstraints["width"]): number | undefined {
	if (typeof value !== "object" || value === null) return undefined;

	const max = value.max;
	return typeof max === "number" && Number.isFinite(max) && max > 0 ? max : undefined;
}

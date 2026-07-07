import type * as Catalog from "@moq/hang/catalog";
import * as Container from "@moq/hang/container";
import * as Util from "@moq/hang/util";
import type * as Moq from "@moq/net";
import { Time } from "@moq/net";
import { Effect, type Getter, Signal } from "@moq/signals";
import { videoCatalog } from "./catalog";
import { hardwareCodecOrder, softwareCodecOrder } from "./codecs";
import type { Source } from "./types";

export interface EncoderProps {
	enabled?: boolean | Signal<boolean>;
	config?: EncoderConfig | Signal<EncoderConfig | undefined>;
	container?: Catalog.Container;
}

// TODO support signals?
export interface EncoderConfig {
	// If not provided, the encoder will select the best codec.
	codec?: string;

	// Constrain the encoded width/height in pixels.
	// TODO figure out how this interacts with the width/height props.
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

export class Encoder {
	enabled: Signal<boolean>;
	source: Signal<Source | undefined>;
	frame: Getter<VideoFrame | undefined>;

	#catalog = new Signal<Catalog.VideoConfig | undefined>(undefined);
	readonly catalog: Getter<Catalog.VideoConfig | undefined> = this.#catalog;

	// The decoder init the encoder actually produced (authoritative codec + hvcC/av1C description),
	// captured from keyframe metadata in serve(), tagged with the requested codec + dimensions it was
	// measured against. #runCatalog (via videoCatalog) folds it into the catalog only while that tag still
	// matches, so it survives the bitrate churn that rebuilds #config ~10x/s (bandwidth adaptation) without
	// flapping the description, yet a real codec/resolution change invalidates it until the next keyframe.
	#decoderConfig = new Signal<
		{ reqCodec: string; width: number; height: number; codec: string; description?: string } | undefined
	>(undefined);

	// Cumulative encoded bytes, for a measured (transport-agnostic) upload bitrate in the stats UI.
	// Monotonic; the reader diffs it per tick. Grows only while a subscriber is being served.
	#bytesEncoded = new Signal(0);
	readonly bytesEncoded: Getter<number> = this.#bytesEncoded;

	#signals = new Effect();

	// The user provided config.
	config: Signal<EncoderConfig | undefined>;

	// The output dimensions of the video in pixels.
	#dimensions = new Signal<{ width: number; height: number } | undefined>(undefined);

	// The video encoder config.
	#config = new Signal<VideoEncoderConfig | undefined>(undefined);

	// The resolved encoder config (codec, bitrate, dimensions), available even with no subscriber.
	// Exposed so a local preview can re-encode with identical settings to mirror the wire output.
	readonly resolved: Getter<VideoEncoderConfig | undefined> = this.#config;

	// True when the encoder is actively serving a track.
	active = new Signal<boolean>(false);

	// Connection signal for reading send bandwidth.
	connection: Getter<Moq.Connection.Established | undefined>;

	constructor(
		frame: Getter<VideoFrame | undefined>,
		source: Signal<Source | undefined>,
		connection: Getter<Moq.Connection.Established | undefined>,
		props?: EncoderProps,
	) {
		this.frame = frame;
		this.source = source;
		this.connection = connection;
		this.enabled = Signal.from(props?.enabled ?? false);
		this.config = Signal.from(props?.config);

		this.#signals.run(this.#runCatalog.bind(this));
		this.#signals.run(this.#runConfig.bind(this));
		this.#signals.run(this.#runDimensions.bind(this));
	}

	serve(track: Moq.Track.Producer, effect: Effect): void {
		if (!effect.get(this.enabled)) return;

		const producer = new Container.Legacy.Producer(track);
		effect.cleanup(() => producer.close());

		let lastKeyframe: Time.Micro | undefined;
		let lastEncoded: Time.Micro | undefined;

		effect.set(this.active, true, false);

		effect.spawn(async () => {
			const encoder = new VideoEncoder({
				output: (frame: EncodedVideoChunk, metadata?: EncodedVideoChunkMetadata) => {
					if (frame.type === "key") {
						lastKeyframe = frame.timestamp as Time.Micro;
					}

					this.#bytesEncoded.update((n) => n + frame.byteLength);

					// Capture the decoder init the encoder actually produced (present on keyframes), tagged
					// with the config it was measured against. For hvc1/av1 this carries the out-of-band
					// parameter sets (hvcC/av1C) the watcher's decoder needs; without it Chrome can't init HEVC.
					// Mirrors the AAC path in audio/encoder.ts. Stored as hex so an identical value each
					// keyframe compares equal and doesn't re-publish the catalog.
					const decoderConfig = metadata?.decoderConfig;
					const config = this.#config.peek();
					if (decoderConfig && config) {
						const desc = decoderConfig.description;
						const description = desc
							? Util.Hex.fromBytes(
									ArrayBuffer.isView(desc)
										? new Uint8Array(desc.buffer, desc.byteOffset, desc.byteLength)
										: new Uint8Array(desc),
								)
							: undefined;
						this.#decoderConfig.set({
							reqCodec: config.codec,
							width: config.width,
							height: config.height,
							codec: decoderConfig.codec,
							description,
						});
					}

					producer.encode(frame, frame.timestamp as Time.Micro, frame.type === "key");
				},
				error: (err: Error) => {
					producer.close(err);
				},
			});

			// Guard against double-close: a fatal encoder error auto-transitions the encoder to "closed", and
			// close() on a closed encoder throws InvalidStateError, which would abort the signals dispose loop
			// and leak the sibling effects. Matches the guarded close in preview.ts and the decoders.
			effect.cleanup(() => {
				if (encoder.state !== "closed") encoder.close();
			});

			// Reconfigure on the INNER effect. Subscribing via the outer `effect` would attach the #config
			// dependency to serve()'s effect and tear down + rebuild the whole encoder + producer on every
			// bitrate change, instead of reconfiguring the live encoder in place (the audio encoder does this).
			effect.run((inner) => {
				const config = inner.get(this.#config);
				if (!config) return;

				encoder.configure(config);
			});

			effect.run((effect) => {
				const frame = effect.get(this.frame);
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

	// Returns the catalog for the configured settings.
	#runCatalog(effect: Effect): void {
		const values = effect.getAll([this.enabled, this.#config]);
		if (!values) return;
		const [_, config] = values;

		// Fold in the decoder config the encoder actually produced (authoritative codec + hvcC/av1C
		// description), available after the first keyframe. See videoCatalog.
		effect.set(this.#catalog, videoCatalog(config, effect.get(this.#decoderConfig)));
	}

	#runConfig(effect: Effect): void {
		// NOTE: dimensions already factors in user provided maxPixels.
		// It's a separate effect in order to deduplicate.
		const values = effect.getAll([this.enabled, this.source, this.#dimensions]);
		if (!values) return;
		const [_, source, dimensions] = values;

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
				// Unknown codec (e.g. a caller-forced string outside our efficiency table): don't throw out
				// of the config effect; skip the efficiency scaling (1.0) so the encoder still configures.
				console.warn(`unknown codec for bitrate scaling: ${codec} (using 1.0)`);
			}

			bitrate = Math.round(Math.min(bitrate, user.maxBitrate || bitrate));

			// If no explicit maxBitrate, cap to the estimated send bandwidth (with 90% safety margin).
			if (!user.maxBitrate) {
				const conn = effect.get(this.connection);
				const sendBw = conn?.sendBandwidth;
				if (sendBw) {
					const estimate = effect.get(sendBw);
					if (estimate != null) {
						// Reserve ~10% for audio and protocol overhead.
						const cap = Math.round(estimate * 0.9);
						bitrate = Math.min(bitrate, cap);
					}
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

			effect.set(this.#config, config);
		});
	}

	#runDimensions(effect: Effect): void {
		const user = effect.get(this.config);

		const frame = effect.get(this.frame);
		if (!frame) return;

		const sourcePixels = frame.codedWidth * frame.codedHeight;

		// maxPixels caps absolutely; maxScale caps relative to the source. The smaller cap wins.
		let maxPixels = user?.maxPixels ?? sourcePixels;
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

		// Codec preference lists, including why Safari needs its own hardware order, live in ./codecs.
		const HARDWARE_CODECS = hardwareCodecOrder(Util.Hacks.isSafari);
		const SOFTWARE_CODECS = softwareCodecOrder();

		// Candidates for a preference order: entries matching the requested prefix, or the requested string
		// itself when the caller forced an exact codec our lists don't enumerate (e.g. "av01.0.04M.08"). An
		// empty `required` (auto) prefix-matches everything, so this returns the whole ordered list.
		const candidates = (order: readonly string[]): string[] => {
			const matched = order.filter((codec) => codec.startsWith(required));
			return matched.length > 0 ? matched : [required];
		};

		// Probe one codec. isConfigSupported rejects (TypeError) on some malformed/unknown codec strings
		// instead of resolving { supported: false }, so treat a throw as "not supported, try the next".
		const probe = async (codec: string, hardwareAcceleration: HardwareAcceleration): Promise<boolean> => {
			try {
				const { supported } = await VideoEncoder.isConfigSupported({
					codec,
					width: dimensions.width,
					height: dimensions.height,
					latencyMode: "realtime",
					hardwareAcceleration,
					avc: codec.startsWith("avc1") ? { format: "annexb" } : undefined,
					// @ts-expect-error Typescript needs to be updated.
					hevc: codec.startsWith("hev1") ? { format: "annexb" } : undefined,
				});
				return supported === true;
			} catch {
				return false;
			}
		};

		// Try hardware encoding first.
		// We can't reliably detect hardware encoding on Firefox: https://github.com/w3c/webcodecs/issues/896
		if (!Util.Hacks.isFirefox) {
			for (const codec of candidates(HARDWARE_CODECS)) {
				if (await probe(codec, "prefer-hardware")) return { codec, hardwareAcceleration: "prefer-hardware" };
			}
		}

		// Then software encoding.
		for (const codec of candidates(SOFTWARE_CODECS)) {
			if (await probe(codec, "prefer-software")) return { codec, hardwareAcceleration: "prefer-software" };
		}

		// Nothing encoded: skip this rendition instead of throwing out of the config effect (the caller
		// early-returns on undefined), so a forced-but-unsupported codec just drops the rendition.
		console.warn(
			`no supported video encoder: codec=${required || "auto"} ${dimensions.width}x${dimensions.height}`,
		);
		return undefined;
	}

	close() {
		this.#signals.close();
	}
}

import * as Catalog from "@moq/hang/catalog";
import * as Container from "@moq/hang/container";
import type * as Moq from "@moq/net";
import { Time } from "@moq/net";
import {
	type Computed,
	Effect,
	type Getter,
	getter,
	type Inputs,
	type Readonlys,
	readonlys,
	Signal,
} from "@moq/signals";
import type { Broadcast } from "../broadcast";
import { type Baseline, Estimator } from "../jitter";
import { hardwareReliable } from "../support/video";
import type { Capture } from "./capture";
import { normalizeSource, type Source } from "./types";

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
	// constraints provide the cap when both are present; otherwise screens default to logical pixels.
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
	// Whether to publish (and encode) this rendition. Defaults to true. When false the rendition drops out of the
	// catalog and stops encoding, but stays registered so a subscriber still gets an idle track.
	enabled: Getter<boolean>;

	// The broadcast to register the rendition on. Undefined resolves the config for a local preview
	// but has nowhere to publish.
	broadcast: Getter<Broadcast | undefined>;

	// The capture pipeline supplying frames and the source track.
	capture: Getter<Capture | undefined>;

	// The connection's bandwidth allocator. The encoder reserves its ceiling and
	// follows the grant instead of the whole-session estimate.
	bandwidth: Getter<Moq.Bandwidth.Handle | undefined>;
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

	/**
	 * The capture supplying this rendition, or undefined while none is wired.
	 *
	 * A snapshot. Read {@link in}.capture through an effect instead when you need to react to it
	 * being swapped.
	 */
	get capture(): Capture | undefined {
		return this.in.capture.peek();
	}

	readonly #out: EncoderOutput = {
		catalog: new Signal<Catalog.VideoConfig | undefined>(undefined),
		resolved: new Signal<VideoEncoderConfig | undefined>(undefined),
		active: new Signal<boolean>(false),
		stats: new Signal<Stats>({ frames: 0, bytes: 0, keyframes: 0 }),
	};
	readonly out = readonlys(this.#out);

	// The output dimensions of the video in pixels.
	#dimensions = new Signal<{ width: number; height: number } | undefined>(undefined);

	// The config the browser accepted and encoded a probe frame with, and the codec string it reported.
	// Kept through a re-probe, so the rendition stays in the catalog until the new probe replaces it.
	#codec = new Signal<Detected | undefined>(undefined);

	// The probed config capped by the bandwidth grant, and the codec string to advertise for it. One
	// signal, so the catalog never pairs a new probe's string with the previous config.
	#live = new Signal<{ config: VideoEncoderConfig; reported: string } | undefined>(undefined);

	// Uncapped target bitrate (pixels, maxBitrate), the reservation's ceiling.
	#ceiling = new Signal<number | undefined>(undefined);

	// This rendition's claim on the connection, held while a track is live.
	#reservation = new Signal<Moq.Bandwidth.Reservation | undefined>(undefined);

	// Only the knobs the probe encodes with, narrowed out of `config` and the source so tuning any
	// other knob, or a bandwidth grant, doesn't re-probe the hardware.
	#target: Computed<Target | undefined>;

	// How many resolution runs threw for their current inputs (no supported codec, an invalid knob),
	// so no config is coming until one reruns.
	#failures = new Signal(0);

	/**
	 * @internal Whether the catalog config resolved, or failed and won't until an input changes.
	 * `<moq-publish>` holds its first announce until this is set.
	 */
	readonly settled: Computed<boolean>;

	#signals = new Effect();
	#stalled = new Catalog.Stalled.Detector();
	#firstCaptured?: Time.Micro;
	#lastCaptured?: Time.Micro;
	#lastAccepted?: Time.Micro;
	#lastCaptureWall?: number;
	#estimator = new Estimator();

	constructor(name: string, props?: EncoderProps) {
		this.name = name;
		this.in = {
			enabled: getter(props?.enabled ?? true),
			broadcast: getter(props?.broadcast),
			capture: getter(props?.capture),
			bandwidth: getter(props?.bandwidth),
		};
		this.config = Signal.from(props?.config);
		this.#target = this.#signals.computed((effect): Target | undefined => {
			const capture = effect.get(this.in.capture);
			if (!capture) return;

			const source = effect.get(capture.in.source);
			if (!source) return;

			const user = effect.get(this.config) ?? {};
			return {
				required: user.codec ?? "",
				// Prefer the explicitly requested rate; the encode loop drops frames to enforce it.
				framerate: user.frameRate ?? sourceFrameRate(source) ?? 30,
				maxPixels: user.maxPixels,
				bitrateScale: user.bitrateScale ?? 0.07,
				maxBitrate: user.maxBitrate,
			};
		});
		this.settled = this.#signals.computed(
			(effect) => effect.get(this.#out.catalog) !== undefined || effect.get(this.#failures) > 0,
		);

		// Every step that resolves the config counts a throw as a failure, so a bad input settles the
		// gate instead of holding the announce forever.
		for (const run of [this.#runCatalog, this.#runCodec, this.#runResolved, this.#runDimensions]) {
			this.#signals.run((effect) => {
				try {
					run.call(this, effect);
				} catch (err) {
					this.#fail(effect);
					throw err;
				}
			});
		}
		this.#signals.run(this.#runRegister.bind(this));
	}

	// Count a failure until `effect` reruns with new inputs.
	#fail(effect: Effect): void {
		this.#failures.update((n) => n + 1);
		effect.cleanup(() => this.#failures.update((n) => n - 1));
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
			if (!enabled || !track) {
				this.#observe({ demand: false, idle: true });
				return;
			}

			this.#encode(track, broadcast.baseline, effect);
		});

		// Reserve against the connection for as long as this track is live. Wait
		// for a ceiling so we never claim 0 and starve siblings for a tick. The
		// allocator ignores an idle track, and closing the reservation hands the
		// room to siblings.
		effect.run((effect) => {
			const enabled = effect.get(this.in.enabled);
			const track = effect.get(rendition.track);
			const allocator = effect.get(this.in.bandwidth);
			if (!enabled || !track || !allocator) return;

			let reservation: Moq.Bandwidth.Reservation | undefined;
			effect.subscribe(this.#ceiling, (ceiling) => {
				if (ceiling === undefined) return;
				if (!reservation) {
					reservation = allocator.reserve(track, ceiling);
					this.#reservation.set(reservation);
				} else {
					reservation.update(ceiling);
				}
			});
			effect.cleanup(() => {
				reservation?.close();
				if (this.#reservation.peek() === reservation) this.#reservation.set(undefined);
			});
		});
	}

	// Encode captured frames into the track producer, reconfiguring when the resolved config changes.
	#encode(track: Moq.Track.Producer, baseline: Baseline, effect: Effect): void {
		const capture = effect.get(this.in.capture);
		if (!capture) {
			this.#observe({ demand: true, idle: true });
			return;
		}

		this.#observe({ demand: false, idle: true });
		this.#lastCaptureWall = performance.now();

		const producer = new Container.Legacy.Producer(track, new Container.Legacy.Format("video"));
		// The broadcast owns this static track across demand gaps. When demand disappears, cut the
		// current group, marking the break so a later subscriber resumes on the same track without
		// the pre-gap group reading as live. A fatal encoder error still aborts the track through
		// producer.close(err) below.
		effect.cleanup(() => {
			if (track.closed.peek() === undefined) producer.cut();
		});

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
					if (this.#estimator.flush(frame.timestamp, baseline)) {
						const catalog = this.#out.catalog.peek();
						if (catalog) this.#out.catalog.set({ ...catalog, ...this.#estimator.estimate });
					}
					this.#lastAccepted = frame.timestamp as Time.Micro;
					this.#observe({ demand: true, idle: false, frame: true });
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
				const fanout = effect.get(capture.out.frames);
				if (!fanout) return;

				// Our own stream off the shared capture, so the preview or another rendition reading
				// slowly can't take frames from this one.
				const reader = fanout.subscribe(effect).getReader();
				effect.cleanup(() => {
					reader.cancel().catch(() => {});
				});

				effect.spawn(async () => {
					for (;;) {
						const next = await effect.race(reader.read());
						if (!next?.value) break;

						// Ours now: every path below has to close it.
						const frame = next.value;
						try {
							if (encoder.state !== "configured") continue;

							// This doesn't need to be reactive.
							const config = this.config.peek();

							// Pace to the target frame rate by dropping frames that arrive too soon.
							// Allow half an interval of slack so jittery capture timestamps don't drop
							// a frame we meant to keep.
							const targetFrameRate = config?.frameRate;
							if (targetFrameRate && lastEncoded !== undefined) {
								const minGap = Time.Micro.fromSecond((1 / targetFrameRate) as Time.Second);
								if (frame.timestamp - lastEncoded < minGap - minGap / 2) continue;
							}
							lastEncoded = frame.timestamp as Time.Micro;
							const captured = frame.timestamp as Time.Micro;
							this.#firstCaptured ??= captured;
							this.#lastCaptured = captured;
							this.#lastCaptureWall = performance.now();
							this.#observe({ demand: true, idle: false });

							const interval = config?.keyframeInterval ?? Time.Milli.fromSecond(2 as Time.Second);

							// Force a keyframe if this is the first frame (no group yet), or GOP elapsed.
							const keyFrame =
								!lastKeyframe || lastKeyframe + Time.Micro.fromMilli(interval) <= frame.timestamp;
							if (keyFrame) {
								lastKeyframe = frame.timestamp as Time.Micro;
							}

							encoder.encode(frame, { keyFrame });
						} finally {
							frame.close();
						}
					}
				});
			});
		});

		effect.interval(() => this.#observe({ demand: true, idle: false }), 50);
	}

	#observe(state: { demand: boolean; idle: boolean; frame?: boolean }): void {
		if (state.idle) {
			this.#firstCaptured = undefined;
			this.#lastCaptured = undefined;
			this.#lastAccepted = undefined;
			this.#lastCaptureWall = undefined;
		}
		const catalog = this.#out.catalog.peek();
		const mediaLag = ((): Time.Micro => {
			if (this.#lastCaptured === undefined || this.#firstCaptured === undefined) return 0 as Time.Micro;
			const accepted = this.#lastAccepted ?? this.#firstCaptured;
			return Math.max(0, this.#lastCaptured - accepted) as Time.Micro;
		})();
		const quiet =
			this.#lastCaptureWall === undefined
				? (0 as Time.Micro)
				: Time.Micro.fromMilli((performance.now() - this.#lastCaptureWall) as Time.Milli);
		if (
			!this.#stalled.observe({
				frame: state.frame ?? false,
				mediaLag,
				quiet,
				interval: Catalog.Stalled.intervalFromFps(catalog?.framerate ?? this.out.resolved.peek()?.framerate),
				demand: state.demand,
				idle: state.idle,
			})
		) {
			return;
		}
		if (!catalog) return;
		this.#out.catalog.set({ ...catalog, stalled: this.#stalled.flag() });
	}

	// Returns the catalog for the configured settings, or undefined while disabled / unresolved.
	#runCatalog(effect: Effect): void {
		const enabled = effect.get(this.in.enabled);
		const live = effect.get(this.#live);
		if (!enabled || !live) {
			effect.set(this.#out.catalog, undefined);
			return;
		}

		// Advertise the codec string the probe's encoder reported rather than the one we configured,
		// so it names the profile and level the bitstream actually carries.
		const { config, reported } = live;
		const catalog: Catalog.VideoConfig = {
			codec: reported,
			bitrate: config.bitrate ? Catalog.u53(config.bitrate) : undefined,
			framerate: config.framerate,
			codedWidth: Catalog.u53(config.width),
			codedHeight: Catalog.u53(config.height),
			optimizeForLatency: true,
			container: { kind: "legacy" } as const,
			...this.#estimator.estimate,
			stalled: this.#stalled.flag(),
		};

		effect.set(this.#out.catalog, catalog);
	}

	// Probe the hardware for the best codec. Deliberately depends on as little as possible, since a
	// probe on a busy GPU process takes a long time. The previous result stays in place meanwhile, so
	// a re-probe swaps the catalog entry in one update, or not at all when nothing changed.
	#runCodec(effect: Effect): void {
		const enabled = effect.get(this.in.enabled);
		const dimensions = effect.get(this.#dimensions);
		const target = effect.get(this.#target);
		if (!enabled || !dimensions || !target) {
			this.#codec.set(undefined);
			return;
		}

		// Captured now: the run's signal is replaced once a rerun starts.
		const superseded = effect.abort;
		effect.spawn(async () => {
			try {
				const detected = await this.#bestCodec(dimensions, target);
				if (!superseded.aborted) this.#codec.set(detected);
			} catch (err) {
				// A newer probe reports its own outcome.
				if (superseded.aborted) return;

				this.#codec.set(undefined);
				this.#fail(effect);
				throw err;
			}
		});
	}

	// Cap the probed config's bitrate by the bandwidth grant. Synchronous, so a new bandwidth estimate
	// updates the config within the same microtask instead of leaving it blank while something
	// re-derives it. Everything else comes from the probe as-is, so the resolved config is always one
	// the hardware accepted.
	#runResolved(effect: Effect): void {
		if (!effect.get(this.in.enabled)) return;

		const detected = effect.get(this.#codec);
		if (!detected) return;

		// The reservation's ceiling is what we can ever send, not the grant: a
		// grant that followed our own output would hand the room away on a still
		// picture and not have it back when the picture moved.
		let bitrate = detected.config.bitrate;
		effect.set(this.#ceiling, bitrate);

		const reservation = effect.get(this.#reservation);
		if (reservation) {
			const grant = reservation.peek();
			effect.get(reservation.grant);
			if (grant != null) bitrate = Math.min(bitrate, grant);
		}

		const config = { ...detected.config, bitrate };
		effect.set(this.#out.resolved, config);
		effect.set(this.#live, { config, reported: detected.reported });
	}

	#runDimensions(effect: Effect): void {
		const capture = effect.get(this.in.capture);
		if (!capture) return;

		// The captured size, rather than a frame: this only needs the dimensions, and holding a frame
		// here would mean owning and closing it.
		const display = effect.get(capture.out.display);
		if (!display) return;

		const source = effect.get(capture.in.source);
		if (!source) return;

		const user = effect.get(this.config);

		const sourcePixels = display.width * display.height;

		// maxPixels caps absolutely; maxScale caps relative to the source. The smaller cap wins.
		let maxPixels =
			user?.maxPixels ??
			sourceConstraintPixels(source) ??
			(user?.maxScale === undefined ? scaledPixels(source, display.scale) : undefined) ??
			sourcePixels;
		if (user?.maxScale !== undefined) {
			if (!Number.isFinite(user.maxScale) || user.maxScale <= 0) {
				throw new Error(`maxScale must be a finite number greater than 0: ${user.maxScale}`);
			}
			maxPixels = Math.min(maxPixels, sourcePixels * user.maxScale);
		}

		const ratio = Math.min(Math.sqrt(maxPixels / sourcePixels), 1);

		// Make sure width/height is a power of 16
		// TODO should this be on a per-codec basis?
		const width = 16 * Math.floor((display.width * ratio) / 16);
		const height = 16 * Math.floor((display.height * ratio) / 16);

		effect.set(this.#dimensions, { width, height });
	}

	// Try to determine the best config for the given settings.
	async #bestCodec(dimensions: { width: number; height: number }, target: Target): Promise<Detected> {
		// A list of codecs to try, in order of preference. Only full RFC 6381 strings: Chrome, Firefox,
		// and Safari all refuse a bare `avc1` or `vp09`, and native players can't decode without the profile.
		const HARDWARE_CODECS = [
			// VP9
			// More likely to have hardware decoding, but hardware encoding is less likely.
			"vp09.00.10.08",

			// H.264
			// Almost always has hardware encoding and decoding.
			"avc1.640028",
			"avc1.4D401F",
			"avc1.42E01E",

			// AV1
			// One day will get moved higher up the list, but hardware decoding is rare.
			"av01.0.08M.08",

			// HEVC (aka h.265)
			// More likely to have hardware encoding, but less likely to be supported (licensing issues).
			// Unfortunately, Firefox doesn't support decoding so it's down here at the bottom.
			"hev1.1.6.L93.B0",

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

			// VP8
			"vp8",

			// VP9
			// It's a bit more expensive to encode so we shy away from it.
			"vp09.00.10.08",

			// HEVC (aka h.265)
			// This likely won't work because of licensing issues.
			"hev1.1.6.L93.B0",
		];

		// Try hardware encoding first.
		// Safari accepts every codec under `prefer-hardware` and echoes the hint straight back, but
		// VideoToolbox only hardware-encodes H.264 and HEVC. Skip the hardware pass and let it fall
		// through to the software pass, which is H.264 first, since Safari routes that through
		// VideoToolbox anyway regardless of the hint.
		const candidates: [string, HardwareAcceleration][] = [
			...(hardwareReliable()
				? HARDWARE_CODECS.map((codec) => [codec, "prefer-hardware"] as [string, HardwareAcceleration])
				: []),
			...SOFTWARE_CODECS.map((codec) => [codec, "prefer-software"] as [string, HardwareAcceleration]),
		];

		for (const [codec, hardwareAcceleration] of candidates) {
			if (!codec.startsWith(target.required)) continue;

			// The full config, like moq-video's probe: an encoder may pick its level from the frame rate
			// or bitrate. The bitrate is the ceiling rather than the grant, so a bandwidth estimate never
			// re-probes.
			const config: Detected["config"] = {
				codec,
				width: dimensions.width,
				height: dimensions.height,
				framerate: target.framerate,
				bitrate: ceiling(codec, dimensions, target),
				latencyMode: "realtime",
				hardwareAcceleration,
				avc: codec.startsWith("avc1") ? { format: "annexb" } : undefined,
				// @ts-expect-error Typescript needs to be updated.
				hevc: codec.startsWith("hev1") ? { format: "annexb" } : undefined,
			};

			const { supported } = await VideoEncoder.isConfigSupported(config);
			if (supported) return { config, reported: await reportedCodec(config) };
		}

		throw new Error("no supported codec");
	}

	close() {
		this.#signals.close();
	}
}

// The source's nominal frame rate: what the capture device settled on, or what a frame stream
// declared. Undefined when nothing reports one.
function sourceFrameRate(source: Source): number | undefined {
	return "frames" in source ? source.frameRate : normalizeSource(source).track.getSettings().frameRate;
}

// The knobs a probe encodes with, besides the dimensions.
type Target = {
	// The codec prefix the user required.
	required: string;
	framerate: number;
	maxPixels?: number;
	bitrateScale: number;
	maxBitrate?: number;
};

// A hardware probe result.
type Detected = {
	// The config the browser accepted and encoded a probe frame with. Its bitrate is the ceiling,
	// before any bandwidth grant.
	config: VideoEncoderConfig & { framerate: number; bitrate: number };
	// The codec string the encoder reported for it, which the catalog advertises.
	reported: string;
};

// The most this rendition ever sends with `codec`, before a bandwidth grant caps it.
function ceiling(codec: string, dimensions: { width: number; height: number }, target: Target): number {
	// NOTE: dimensions already factors in user provided maxPixels.
	const maxPixels = target.maxPixels ?? dimensions.width * dimensions.height;

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
	const framerateFactor = 30.0 + (target.framerate - 30) / 2;

	// ACTUAL BITRATE CALCULATION
	// 480p@30 = 409920 * 30 * 0.07 = 0.9 Mb/s
	// 480p@60 = 409920 * 45 * 0.07 = 1.3 Mb/s
	// 720p@30 = 921600 * 30 * 0.07 = 1.9 Mb/s
	// 720p@60 = 921600 * 45 * 0.07 = 2.9 Mb/s
	// 1080p@30 = 2073600 * 30 * 0.07 = 4.4 Mb/s
	// 1080p@60 = 2073600 * 45 * 0.07 = 6.5 Mb/s
	const bitrate = Math.round(maxPixels * target.bitrateScale * framerateFactor * codecBitrateScale(codec));
	const capped = Math.round(Math.min(bitrate, target.maxBitrate || bitrate));

	// Refuse a bad knob here, before the browser coerces it into an unsigned bitrate.
	if (!(capped > 0)) throw new Error(`bitrate must be positive: ${capped}`);
	return capped;
}

// Encode one frame with a throwaway encoder and return the codec string it reports, like
// moq-video's `Config::probe`. The encoder picks the profile and level it actually writes, so reading
// them back lets the rendition be advertised before a subscriber starts the real encoder, without a
// claim the first keyframe would contradict. It closes before the config resolves, so it never
// overlaps the first real encoder; only a re-probe while serving briefly holds two sessions.
async function reportedCodec(config: VideoEncoderConfig): Promise<string> {
	let reported: string | undefined;
	const encoder = new VideoEncoder({
		output: (_chunk, metadata) => {
			reported ??= metadata?.decoderConfig?.codec;
		},
		// flush() rejects with the same error.
		error: () => {},
	});

	try {
		encoder.configure(config);

		// Mid-gray, since the picture only has to make the encoder emit its config.
		const { width, height } = config;
		const frame = new VideoFrame(new Uint8Array((width * height * 3) / 2).fill(0x80), {
			format: "I420",
			codedWidth: width,
			codedHeight: height,
			timestamp: 0,
		});
		try {
			encoder.encode(frame, { keyFrame: true });
		} finally {
			frame.close();
		}

		await encoder.flush();
	} finally {
		if (encoder.state !== "closed") encoder.close();
	}

	if (!reported) throw new Error(`${config.codec} encoder reported no codec string`);
	return reported;
}

// Scale the bitrate for more efficient codecs, relative to H.264.
// TODO This shouldn't be linear, as the efficiency is very similar at low bitrates.
function codecBitrateScale(codec: string): number {
	if (codec.startsWith("avc1")) return 1.0;
	if (codec.startsWith("hev1")) return 0.7;
	if (codec.startsWith("vp09")) return 0.8;
	if (codec.startsWith("av01")) return 0.6;
	// Worse than H.264 but it's a backup plan.
	if (codec === "vp8") return 1.1;

	throw new Error(`unknown codec: ${codec}`);
}

function sourceConstraintPixels(source: Source): number | undefined {
	// Only a capture track has constraints; a frame stream is whatever size it produces.
	if ("frames" in source) return undefined;

	const constraints = normalizeSource(source).track.getConstraints();
	const width = constraintMax(constraints.width);
	const height = constraintMax(constraints.height);

	return width !== undefined && height !== undefined ? width * height : undefined;
}

function scaledPixels(source: Source, scale: number | undefined): number | undefined {
	if ("frames" in source) return;
	const { track } = normalizeSource(source);
	if (scale === undefined) return;
	if (!Number.isFinite(scale) || scale <= 0)
		throw new Error(`scale must be a finite number greater than 0: ${scale}`);

	// Cap against the native surface, not the current frame, which may already be downscaled.
	const capabilities = track.getCapabilities();
	const width = capabilities.width?.max;
	const height = capabilities.height?.max;
	if (!width || !height) return;
	return (width * height) / scale ** 2;
}

function constraintMax(value: MediaTrackConstraints["width"]): number | undefined {
	if (typeof value !== "object" || value === null) return undefined;

	const max = value.max;
	return typeof max === "number" && Number.isFinite(max) && max > 0 ? max : undefined;
}

import { Micro, Milli, type Second } from "@moq/clock";
import * as Catalog from "@moq/hang/catalog";
import * as Container from "@moq/hang/container";
import type * as Moq from "@moq/net";
import {
	type Computed,
	Effect,
	type Getter,
	getter,
	type Inputs,
	Once,
	type Readonlys,
	readonlys,
	Signal,
} from "@moq/signals";
import type { Broadcast } from "../broadcast";
import { errors, watched } from "../internal";
import { Estimator, type Order } from "../order";
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
	/** If not provided, the encoder will select the best codec. */
	codec?: string;

	/**
	 * Constrain the encoded width/height in pixels. If unset, source width.max and height.max
	 * constraints provide the cap when both are present; otherwise screens default to logical pixels.
	 */
	maxPixels?: number;

	/**
	 * Cap the encoded resolution to this fraction of the source pixel count.
	 * For example 0.25 yields a quarter of the pixels (half the width and height),
	 * scaling with the source instead of assuming a fixed resolution.
	 * When combined with maxPixels, the smaller resulting cap wins.
	 */
	maxScale?: number;

	/**
	 * The longest interval between keyframes. A joining subscriber forces one sooner once the
	 * group's deltas outweigh the track's refresh ratio. (default: 2000 milliseconds)
	 */
	keyframeInterval?: Milli;

	/** If not provided, the encoder will use the best bitrate for the given width, height, and framerate. */
	maxBitrate?: number;

	/**
	 * Multiply the number of pixels by this value to get the bitrate. (default: 0.07)
	 * NOTE: This is multiplied by the codecScale (1.0 for h264) to get the final scale.
	 */
	bitrateScale?: number;

	/**
	 * Cap the encoded frame rate. If set below the captured rate, frames are dropped to hit this target.
	 * Also feeds the bitrate calculation and the encoder config. If unset, the captured track's rate is used.
	 */
	frameRate?: number;
}

/** Signals the encoder reads. */
export type EncoderInput = {
	// Whether to encode this rendition. Defaults to true. When false it stops encoding but stays in the
	// catalog with `enabled: false` and its last config, and stays registered so a subscriber still gets
	// an idle track.
	enabled: Getter<boolean>;

	// The broadcast to register the rendition on. Undefined resolves the config for a local preview
	// but has nowhere to publish.
	broadcast: Getter<Broadcast | undefined>;

	// The capture pipeline supplying frames and the source track.
	capture: Getter<Capture | undefined>;

	// The connection's bandwidth allocator. While a subscriber is attached, the encoder
	// reserves its ceiling and follows the grant instead of the whole-session estimate.
	bandwidth: Getter<Moq.Bandwidth.Handle | undefined>;
};

/** Constructor options: the wired inputs plus the live-editable {@link Config} tuning knobs. */
export type EncoderProps = Inputs<EncoderInput> & {
	// User tuning knobs. Seed a value or wire a Signal; also live-editable via `encoder.config`.
	config?: Config | Signal<Config | undefined>;
};

/** Signals the encoder derives. */
export type EncoderOutput = {
	/** The encoder failure, cleared only by a new capture source or format, or new encoder settings. */
	error: Signal<Error | undefined>;
	// The catalog config published for this rendition, `enabled: false` while disabled, or undefined
	// until one resolves.
	catalog: Signal<Catalog.VideoConfig | undefined>;
	// The resolved WebCodecs config (codec, bitrate, dimensions), available even with no subscriber.
	// Exposed so a local preview can re-encode with identical settings to mirror the wire output.
	resolved: Signal<VideoEncoderConfig | undefined>;
	// True while encoding: enabled and the rendition has a track, which with the broadcast's
	// `onDemand` also takes a subscriber.
	active: Signal<boolean>;
	// Cumulative output totals (frames, bytes, keyframes) measured while serving.
	stats: Signal<Stats>;
};

/**
 * A single video rendition encoder.
 *
 * Registers itself on the {@link Broadcast} under {@link name} (via `broadcast.video(name)`), resolves
 * the best codec/bitrate/dimensions for the capture, and encodes frames while enabled, so a late
 * subscriber or fetch finds the recent groups in the track's cache. The broadcast's `onDemand` encodes
 * only while a subscriber is attached instead. A joining subscriber that wants a newer group gets a
 * fresh keyframe either way. Rename by constructing a new encoder; the name is not a signal.
 *
 * The catalog advertises the largest size encoded so far as the coded-size ceiling, so a capture that
 * shrinks resizes in band at a keyframe. Outgrowing the ceiling re-probes and advertises a larger one,
 * in place unless the codec string changes, which the broadcast serves under a new track name.
 */
export class Encoder {
	/** The full track name of this rendition, e.g. `"video/hd"`. */
	readonly name: string;

	/** The wired inputs, read-only. */
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
		error: new Signal<Error | undefined>(undefined),
		catalog: new Signal<Catalog.VideoConfig | undefined>(undefined),
		resolved: new Signal<VideoEncoderConfig | undefined>(undefined),
		active: new Signal<boolean>(false),
		stats: new Signal<Stats>({ frames: 0, bytes: 0, keyframes: 0 }),
	};
	/** The derived outputs, read-only. */
	readonly out = readonlys(this.#out);

	// The output dimensions of the video in pixels.
	#dimensions = new Signal<{ width: number; height: number } | undefined>(undefined);

	// The coded-size ceiling to probe for the current broadcast: each side's largest of its accepted
	// ceiling and the current size. A smaller capture resizes in band below it without changing what
	// the catalog advertises.
	#coded = new Signal<{ broadcast?: Broadcast; width: number; height: number } | undefined>(undefined);

	// The config the browser accepted and encoded a probe frame with, and the codec string it reported.
	// Kept through a re-probe, so the rendition stays in the catalog until the new probe replaces it.
	#codec = new Signal<Detected | undefined>(undefined);

	// The live config (current size, capped by the bandwidth grant) and the probe it runs under. One
	// signal, so the catalog never pairs a new probe with the previous config.
	#live = new Signal<{ config: VideoEncoderConfig; detected: Detected } | undefined>(undefined);

	// Uncapped target bitrate (pixels, maxBitrate), the reservation's ceiling.
	#ceiling = new Signal<number | undefined>(undefined);

	// This rendition's claim on the connection, held while a track is live.
	#reservation = new Signal<Moq.Bandwidth.Reservation | undefined>(undefined);

	// Only the knobs the probe encodes with, narrowed out of `config` and the source so tuning any
	// other knob, or a bandwidth grant, doesn't re-probe the hardware.
	#target: Computed<Target | undefined>;

	// A keyframe asked for by {@link cut} and not yet encoded.
	#cut = false;

	// How many resolution runs threw for their current inputs (no supported codec, an invalid knob),
	// so no config is coming until one reruns.
	#failures = new Signal(0);

	/**
	 * @internal Whether the catalog config resolved, or failed and won't until an input changes.
	 * `<moq-publish>` holds its first announce until this is set.
	 */
	readonly settled: Computed<boolean>;

	// Each registration keeps its terminal failure even after the public error clears.
	#failure = new Signal(new Once<Error>());
	// The capture inputs #runCaptureRecovery last saw, carried across a gap in the source or display.
	#seen?: {
		capture?: Capture;
		source?: unknown;
		display?: { width: number; height: number; scale?: number };
	};
	#signals = new Effect();
	#estimator = new Estimator();
	// The estimator's delay, republished whenever it rises.
	#estimate = new Signal<Estimator["estimate"]>({});
	// What this rendition advertised on each broadcast, keyed by the broadcast or, for a local preview,
	// the encoder. A broadcast keeps a rendition's track when it re-registers, so returning to one
	// continues its ceiling, maxima, and last config, while another broadcast starts its own.
	#advertised = new WeakMap<object, Advertised>();
	// Whether the rendition was disabled since a config last resolved, so it keeps advertising its last
	// config as disabled until the re-enabled capture resolves a new one. A capture that never reopens
	// leaves it disabled, which is accurate: no frames are coming.
	#paused = false;

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
			(effect) =>
				effect.get(this.#out.catalog) !== undefined ||
				effect.get(this.#out.error) !== undefined ||
				effect.get(this.#failures) > 0,
		);

		// Every step that resolves the config counts a throw as a failure, so a bad input settles the
		// gate instead of holding the announce forever.
		for (const run of [
			this.#runCatalog,
			this.#runCodec,
			this.#runResolved,
			this.#runDimensions,
			this.#runCeiling,
		]) {
			this.#signals.run((effect) => {
				try {
					run.call(this, effect);
				} catch (err) {
					this.#fail(effect);
					throw err;
				}
			});
		}
		this.#signals.run(this.#runSettingsRecovery.bind(this));
		this.#signals.run(this.#runCaptureRecovery.bind(this));
		this.#signals.run(this.#runRegister.bind(this));
	}

	// What this rendition advertised on `broadcast`'s track.
	#track(broadcast: Broadcast | undefined): Advertised {
		const key = broadcast ?? this;
		let track = this.#advertised.get(key);
		if (!track) {
			track = { bitrate: 0, framerate: 0 };
			this.#advertised.set(key, track);
		}
		return track;
	}

	// Count a failure until `effect` reruns with new inputs.
	#fail(effect: Effect): void {
		this.#failures.update((n) => n + 1);
		effect.cleanup(() => this.#failures.update((n) => n - 1));
	}

	// Any notification of the encoder settings re-arms a failure, including a setting mutated in place.
	#runSettingsRecovery(effect: Effect): void {
		effect.get(this.config);
		this.#rearm();
	}

	// Read only capture inputs here, never bandwidth grants or the error itself; demand is not a retry.
	// A gap in the source or display (a capture released and handed back) keeps the last ones, so only
	// a different capture re-arms a failure, such as the new track a re-enabled camera opens.
	#runCaptureRecovery(effect: Effect): void {
		const capture = effect.get(this.in.capture);
		const seen = this.#seen;
		const same = seen !== undefined && seen.capture === capture;
		const source = (capture && effect.get(capture.in.source)) ?? (same ? seen.source : undefined);
		const display = (capture && effect.get(capture.out.display)) ?? (same ? seen.display : undefined);
		this.#seen = { capture, source, display };
		if (
			same &&
			seen.source === source &&
			seen.display?.width === display?.width &&
			seen.display?.height === display?.height &&
			seen.display?.scale === display?.scale
		) {
			return;
		}
		this.#rearm();
	}

	// Clear a latched failure so a fresh registration retries.
	#rearm(): void {
		if (!this.#out.error.peek()) return;
		this.#out.error.set(undefined);
		this.#failure.set(new Once<Error>());
	}

	// Register the rendition on the broadcast and drive its catalog + encode loop. Re-registers cleanly
	// when the broadcast swaps.
	#runRegister(effect: Effect): void {
		const failure = effect.get(this.#failure);
		const broadcast = effect.get(this.in.broadcast);
		if (!broadcast) return;

		const rendition = broadcast.video(this.name);
		errors.set(rendition, failure);
		effect.cleanup(() => rendition.close());

		// Publish the resolved catalog config; undefined (until one resolves) leaves it out of the catalog.
		effect.proxy(rendition.config, this.out.catalog);

		// Encode while enabled and the broadcast hands over the track (with `onDemand`, a subscriber is attached).
		effect.run((effect) => {
			const enabled = effect.get(this.in.enabled);
			const track = effect.get(rendition.track);
			const error = effect.get(this.#out.error);
			effect.set(this.#out.active, enabled && !!track && !error, false);
			if (!enabled || !track || error) return;

			this.#encode(track, broadcast.order, effect);
		});

		// Reserve against the connection while a subscriber is attached. Wait for a ceiling so we
		// never claim 0 and starve siblings for a tick. Closing the reservation hands the room to
		// siblings, and an unwatched rendition encodes at its configured bitrate: its backlog is
		// served from the cache rather than paced live.
		effect.run((effect) => {
			const enabled = effect.get(this.in.enabled);
			const track = effect.get(rendition.track);
			const allocator = effect.get(this.in.bandwidth);
			if (!enabled || !track || !allocator || effect.get(this.#out.error)) return;

			const demanded = watched(track, effect);
			effect.run((effect) => {
				if (!effect.get(demanded)) return;

				let reservation: Moq.Bandwidth.Reservation | undefined;
				effect.subscribe(this.#ceiling, (ceiling) => {
					if (ceiling === undefined) return;
					if (!reservation) {
						reservation = allocator.reserve(track.demand(), ceiling);
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
		});
	}

	// Encode captured frames into the track producer, reconfiguring when the resolved config changes.
	#encode(track: Moq.Track.Producer, order: Order, effect: Effect): void {
		const capture = effect.get(this.in.capture);
		if (!capture) return;

		const producer = new Container.Legacy.Producer(track, new Container.Legacy.Format("video"));
		const demand = track.demand();

		let lastKeyframe: Micro | undefined;
		let lastEncoded: Micro | undefined;
		// The catalog declares no reordering, so each chunk must present after the one before it.
		let lastOutput: number | undefined;
		// The largest group demand asked for that a keyframe already answers, including one still in
		// the encoder. A join over the refresh ratio asks for one past the largest, which the next frame
		// opens; any other join is served by a group already written, encoded with or without demand.
		let answered: number | undefined;
		const answer = () => {
			const wanted = demand.wanted.peek();
			if (wanted !== undefined) answered = Math.max(answered ?? 0, wanted);
		};
		const covered = () => Math.max(answered ?? -1, track.largest()?.group ?? -1);
		let tearingDown = false;

		effect.spawn(async () => {
			const encoder = new VideoEncoder({
				output: (frame: EncodedVideoChunk) => {
					if (lastOutput !== undefined && frame.timestamp <= lastOutput) {
						this.#abort(
							new Error(`encoder reordered a frame at ${frame.timestamp}us behind ${lastOutput}us`),
							track,
						);
						return;
					}
					lastOutput = frame.timestamp;
					const key = frame.type === "key";
					if (key) {
						lastKeyframe = frame.timestamp as Micro;
					}

					this.#out.stats.update((stats) => ({
						frames: stats.frames + 1,
						bytes: stats.bytes + frame.byteLength,
						keyframes: key ? stats.keyframes + 1 : stats.keyframes,
					}));

					producer.encode(frame, frame.timestamp as Micro, key);
					// Including a keyframe the encoder placed on its own, which opened a group too.
					if (key) answer();
					if (this.#estimator.write(frame.timestamp, order)) {
						this.#estimate.set({ ...this.#estimator.estimate });
					}
				},
				error: (err: Error) => {
					// The black keyframe is cosmetic; failing it must not end the track the broadcast owns.
					if (tearingDown || effect.abort.aborted) console.warn("video encoder failed while stopping:", err);
					else {
						this.#abort(err, track);
					}
				},
			});

			// What the encoder was last configured with. The resolved config clears on disable, before
			// the black keyframe below is encoded.
			let configured: VideoEncoderConfig | undefined;
			// Set when the size changed, so the next frame opens a group whose keyframe carries it.
			let resized = false;
			effect.run((effect) => {
				const config = effect.get(this.out.resolved);
				if (!config) return;

				if (configured && (configured.width !== config.width || configured.height !== config.height)) {
					resized = true;
				}
				try {
					encoder.configure(config);
					configured = config;
				} catch (error) {
					this.#abort(error, track);
				}
			});

			effect.run((effect) => {
				const fanout = effect.get(capture.out.frames);
				if (!fanout) return;

				// Our own stream off the shared capture, so the preview or another rendition reading
				// slowly can't take frames from this one. Teardown ends a pending read by cancelling it;
				// racing the read against teardown would drop a frame that had already resolved, unclosed.
				const reader = fanout.subscribe(effect).getReader();
				effect.cleanup(() => {
					reader.cancel().catch(() => {});
				});

				effect.spawn(async () => {
					for (;;) {
						const { value: frame } = await reader.read();
						if (!frame) break;

						// Ours now: every path below has to close it.
						try {
							if (effect.abort.aborted) break;
							if (encoder.state !== "configured") continue;
							// A pending input means the codec is saturated. Keep capture live instead of
							// growing a stale encode queue whose lateness becomes permanent catalog delay.
							// Drop before answering a keyframe request or advancing the frame-rate cursor.
							if (encoder.encodeQueueSize > 0) continue;

							// This doesn't need to be reactive.
							const config = this.config.peek();

							// Pace to the target frame rate by dropping frames that arrive too soon.
							// Allow half an interval of slack so jittery capture timestamps don't drop
							// a frame we meant to keep.
							const targetFrameRate = config?.frameRate;
							// A repeated timestamp would present with the frame before it, which no reordering
							// declared allows; a backward one still fails the track at output.
							if (frame.timestamp === lastEncoded) continue;
							if (targetFrameRate && lastEncoded !== undefined) {
								const minGap = Micro.fromSecond((1 / targetFrameRate) as Second);
								if (frame.timestamp - lastEncoded < minGap - minGap / 2) continue;
							}
							lastEncoded = frame.timestamp as Micro;

							const interval = config?.keyframeInterval ?? Milli.fromSecond(2 as Second);

							// Force a keyframe if this is the first frame (no group yet), the GOP elapsed, the
							// caller asked for one, or a join wants a group past the largest. No spacing: the
							// refresh ratio already rate-limits joins by the bytes a group holds.
							const since = lastKeyframe === undefined ? undefined : frame.timestamp - lastKeyframe;
							const wanted = demand.wanted.peek();
							const keyFrame =
								resized ||
								since === undefined ||
								since >= Micro.fromMilli(interval) ||
								this.#cut ||
								(wanted !== undefined && wanted > covered());
							if (keyFrame) {
								lastKeyframe = frame.timestamp as Micro;
								resized = false;
								// Any keyframe serves an outstanding request and every join before it.
								this.#cut = false;
								answer();
							}

							encoder.encode(frame, { keyFrame });
						} catch (error) {
							this.#abort(error, track);
							return;
						} finally {
							frame.close();
						}
					}
				});
			});

			// Teardown is ours rather than a cleanup, since disabling first encodes one black keyframe and
			// has to wait for it. A viewer that predates `enabled` keeps selecting a disabled rendition,
			// and shows black instead of a frozen picture.
			await new Promise((resolve) => effect.abort.addEventListener("abort", resolve, { once: true }));
			tearingDown = true;
			try {
				const config = configured;
				if (
					!this.in.enabled.peek() &&
					config &&
					lastEncoded !== undefined &&
					encoder.state === "configured" &&
					track.closed.peek() === undefined
				) {
					// One frame interval after the last picture, in whole microseconds like any VideoFrame.
					const interval = Micro.fromSecond((1 / (config.framerate ?? 30)) as Second);
					const frame = black(config, Math.round(lastEncoded + interval));
					try {
						encoder.encode(frame, { keyFrame: true });
					} finally {
						frame.close();
					}
					// A rejected flush already reported through the error callback.
					await encoder.flush().catch(() => {});
				}
			} finally {
				if (encoder.state !== "closed") encoder.close();
				// A stopped rendition stops holding the others' delay back. Not a cleanup: the black
				// keyframe above is written after one would run, and would seat this rendition again.
				order.leave(this.#estimator);
				// The broadcast owns this static track across gaps (a disable, or with `onDemand` a demand
				// gap). Close the current group, marking the break so a later subscriber resumes on the same
				// track without the pre-gap group reading as live. A fatal encoder error aborts the track instead.
				if (track.closed.peek() === undefined) producer.discontinuity();
			}
		});
	}

	#abort(error: unknown, track: Moq.Track.Producer): void {
		if (this.#out.error.peek()) return;
		const cause = error instanceof Error ? error : new Error(String(error));
		this.#failure.peek().set(cause);
		this.#out.error.set(cause);
		track.abort(cause);
	}

	// Publishes the catalog for the probe: its codec string, the coded-size ceiling, and the bitrate and
	// frame rate maxima. Neither the live size nor the bandwidth grant reaches it. A disabled rendition
	// keeps its last config, since muting also releases the capture it would resolve a new one from. It
	// stays disabled after re-enabling until the reopened capture resolves, so an unmute is one field
	// changing too.
	#runCatalog(effect: Effect): void {
		const estimate = effect.get(this.#estimate);
		const enabled = effect.get(this.in.enabled) && !effect.get(this.#out.error);
		const track = this.#track(effect.get(this.in.broadcast));
		if (!enabled) this.#paused = true;
		const live = enabled ? effect.get(this.#live) : undefined;
		if (!live) {
			const last = this.#paused ? track.last : undefined;
			effect.set(this.#out.catalog, last && { ...last, ...estimate, enabled: false });
			return;
		}
		this.#paused = false;

		// Advertise the codec string the probe's encoder reported rather than the one we configured,
		// so it names the profile and level the bitstream actually carries at the ceiling.
		const { config, reported } = live.detected;
		track.bitrate = Math.max(track.bitrate, config.bitrate);
		track.framerate = Math.max(track.framerate, config.framerate);
		const catalog: Catalog.VideoConfig = {
			codec: reported,
			bitrate: Catalog.u53(track.bitrate),
			framerate: track.framerate,
			codedWidth: Catalog.u53(config.width),
			codedHeight: Catalog.u53(config.height),
			optimizeForLatency: true,
			// Realtime WebCodecs encoders emit no B-frames; the output callback refuses one that does.
			reorder: Catalog.u8(0),
			container: { kind: "legacy" } as const,
			...estimate,
		};

		track.last = catalog;
		effect.set(this.#out.catalog, catalog);
	}

	// Probe the hardware for the best codec at the coded-size ceiling. Deliberately depends on as little
	// as possible, since a probe on a busy GPU process takes a long time. The previous result stays in
	// place meanwhile, so a re-probe swaps the catalog entry in one update, or not at all when nothing
	// changed.
	#runCodec(effect: Effect): void {
		if (effect.get(this.#out.error)) return;
		const enabled = effect.get(this.in.enabled);
		const dimensions = effect.get(this.#coded);
		const target = effect.get(this.#target);
		if (!enabled || !dimensions || !target) {
			this.#codec.set(undefined);
			return;
		}

		// Captured now: the run's signal is replaced once a rerun starts.
		const superseded = effect.abort;
		effect.spawn(async () => {
			const track = this.#track(dimensions.broadcast);
			try {
				let detected: Detected;
				try {
					detected = await this.#bestCodec(dimensions, target);
				} catch (err) {
					// A capture that outgrew the accepted ceiling to a size the hardware refuses encodes scaled
					// down to fit it, until a smaller capture reruns the probe. Re-probing the ceiling with the
					// current target, rather than reusing the old probe, still refuses a target the hardware
					// can't meet and survives a disable that dropped the old probe.
					const accepted = track.ceiling;
					if (
						superseded.aborted ||
						!accepted ||
						(dimensions.width <= accepted.width && dimensions.height <= accepted.height)
					) {
						throw err;
					}
					console.warn(
						`video ${dimensions.width}x${dimensions.height} unsupported, keeping ${accepted.width}x${accepted.height}:`,
						err,
					);
					detected = await this.#bestCodec(accepted, target);
				}
				if (superseded.aborted) return;

				// Only an accepted probe commits a ceiling, so a size the hardware refuses never sticks.
				const { width, height } = detected.config;
				track.ceiling = { width, height };
				this.#codec.set({ ...detected, broadcast: dimensions.broadcast });
			} catch (err) {
				// A newer probe reports its own outcome.
				if (superseded.aborted) return;

				this.#codec.set(undefined);
				this.#fail(effect);
				throw err;
			}
		});
	}

	// Size the probed config to the current dimensions and cap its bitrate by the bandwidth grant.
	// Synchronous, so a new bandwidth estimate updates the config within the same microtask instead of
	// leaving it blank while something re-derives it. The codec comes from the probe as-is, and the size
	// never exceeds the probe's, so the resolved config is always one the hardware accepted or smaller.
	#runResolved(effect: Effect): void {
		if (!effect.get(this.in.enabled)) return;

		// A probe for another broadcast describes another track, so wait for this one's.
		const broadcast = effect.get(this.in.broadcast);
		const detected = effect.get(this.#codec);
		const dimensions = effect.get(this.#dimensions);
		const target = effect.get(this.#target);
		if (!detected || detected.broadcast !== broadcast || !dimensions || !target) return;

		// A capture that outgrew the probe encodes scaled down to fit until the re-probe at the new
		// ceiling lands, so the live config never exceeds what the catalog advertises.
		const { width, height } = fit(dimensions, detected.config);

		// The reservation's ceiling is what we can ever send at this size, not the grant: a grant that
		// followed our own output would hand the room away on a still picture and not have it back when
		// the picture moved. It never exceeds the probe's, which a raised knob outruns until it re-probes.
		let bitrate = Math.min(ceiling(detected.config.codec, { width, height }, target), detected.config.bitrate);
		effect.set(this.#ceiling, bitrate);

		const reservation = effect.get(this.#reservation);
		if (reservation) {
			const grant = reservation.peek();
			effect.get(reservation.rate);
			if (grant != null) bitrate = Math.min(bitrate, grant);
		}

		const config = { ...detected.config, width, height, bitrate };
		effect.set(this.#out.resolved, config);
		effect.set(this.#live, { config, detected });
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

	// The ceiling to probe: each side's largest of this broadcast's accepted ceiling and the current
	// size. Reruns once a probe commits a ceiling, which leaves it unchanged.
	#runCeiling(effect: Effect): void {
		const dimensions = effect.get(this.#dimensions);
		if (!dimensions) return;

		const broadcast = effect.get(this.in.broadcast);
		effect.get(this.#codec);
		const ceiling = this.#track(broadcast).ceiling;
		effect.set(this.#coded, {
			broadcast,
			width: Math.max(ceiling?.width ?? 0, dimensions.width),
			height: Math.max(ceiling?.height ?? 0, dimensions.height),
		});
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
				// @ts-expect-error TypeScript's DOM lib lacks the HEVC codec registration's `hevc` config.
				hevc: codec.startsWith("hev1") ? { format: "annexb" } : undefined,
			};

			const { supported } = await VideoEncoder.isConfigSupported(config);
			if (supported) return { config, reported: await reportedCodec(config) };
		}

		throw new Error("no supported codec");
	}

	/**
	 * Request a keyframe, opening a new group at a frame no earlier than this call.
	 *
	 * For a resume, a recording cut, or a known tune-in moment; {@link Config.keyframeInterval} is the
	 * longest a group runs, and a joining subscriber already opens one through the track's refresh
	 * ratio. Requests between two frames coalesce into one keyframe on the next, with no spacing: a
	 * caller in a loop pins the encoder at all-keyframe. A request while not encoding is served by the
	 * keyframe every encode starts with.
	 */
	cut(): void {
		this.#cut = true;
	}

	/** Stop and release everything this holds. */
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
	// The broadcast the probe ran for; another one starts its own ceiling.
	broadcast?: Broadcast;
};

// What a rendition advertised on one broadcast's track.
type Advertised = {
	// The coded-size ceiling of the largest accepted probe.
	ceiling?: { width: number; height: number };
	// The largest bitrate and frame rate advertised, since the catalog may only raise them.
	bitrate: number;
	framerate: number;
	// The last config published while enabled, which a disabled rendition keeps advertising.
	last?: Catalog.VideoConfig;
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

// Scale `size` down to fit within `ceiling`, keeping its aspect ratio and the multiple of 16.
function fit(size: { width: number; height: number }, ceiling: { width: number; height: number }) {
	const ratio = Math.min(ceiling.width / size.width, ceiling.height / size.height);
	if (ratio >= 1) return size;
	return { width: 16 * Math.floor((size.width * ratio) / 16), height: 16 * Math.floor((size.height * ratio) / 16) };
}

// One black I420 picture the size of `config`, in limited range like most encoders expect.
function black(config: VideoEncoderConfig, timestamp: number): VideoFrame {
	const { width, height } = config;
	const luma = width * height;
	const data = new Uint8Array((luma * 3) / 2);
	data.fill(0x10, 0, luma);
	data.fill(0x80, luma);
	return new VideoFrame(data, { format: "I420", codedWidth: width, codedHeight: height, timestamp });
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

import { type Micro, Milli, Nano } from "@moq/clock";
import * as Catalog from "@moq/hang/catalog";
import * as Container from "@moq/hang/container";
import * as Util from "@moq/hang/util";
import type * as Moq from "@moq/net";
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
import { rotateVideoDimensions } from "@moq/video";
import { Producer as ArrivalProducer, type Arrivals, gap } from "../arrivals";
import { base64ToBytes } from "../base64";
import { type Failure, failureMoved, nextMedia, subscribeMedia } from "../media";
import type { Sync } from "../sync";
import {
	type DecoderConfig,
	decoderConfigKey,
	type PlaybackIdentity,
	playbackIdentity,
	supportCacheKey,
} from "./config";
import { caughtUp, renditionJitter, switchJitter } from "./playhead";
import { Recovery } from "./recovery";
import type { Source } from "./source";

// The amount of time to wait before considering the video to be buffering.
const BUFFERING = Milli(500);

export type DecoderInput = {
	/** Whether to download the video track. Defaults to true; the parent may wire it from the renderer's output. */
	enabled: Getter<boolean>;
};

/** Constructor properties for {@link Decoder}. */
export type DecoderProps = Inputs<DecoderInput> & {
	/** Rendition selector supplying encoded video. */
	source: Source;
	/** Shared playback clock. */
	sync: Sync;
};

/** Cumulative video statistics since the decoder started. */
export interface Stats {
	/** Number of decoded frames. */
	frameCount: number;

	/** Number of encoded bytes received. */
	bytesReceived: number;
}

type DecoderOutput = {
	/** Fatal decode failure, cleared when another source is selected. */
	error: Signal<Error | undefined>;
	// The current frame to render.
	frame: Signal<VideoFrame | undefined>;

	// The timestamp of the current frame.
	timestamp: Signal<Milli | undefined>;

	// The display size of the video in pixels, ideally sourced from the catalog.
	display: Signal<{ width: number; height: number } | undefined>;

	stalled: Signal<boolean>;
	stats: Signal<Stats | undefined>;

	// The rendition delay Sync must cover while switching tracks.
	jitter: Signal<Milli | undefined>;

	// Combined buffered ranges (network jitter + decode buffer)
	buffered: Signal<Container.BufferedRanges>;
};

/** Downloads video from a track and decodes it into {@link VideoFrame}s with WebCodecs. */
export class Decoder {
	readonly in: Readonlys<DecoderInput>;
	readonly source: Source;
	readonly sync: Sync;

	readonly #out: DecoderOutput = {
		error: new Signal<Error | undefined>(undefined),
		frame: new Signal<VideoFrame | undefined>(undefined),
		timestamp: new Signal<Milli | undefined>(undefined),
		display: new Signal<{ width: number; height: number } | undefined>(undefined),
		stalled: new Signal<boolean>(false),
		stats: new Signal<Stats | undefined>(undefined),
		jitter: new Signal<Milli | undefined>(undefined),
		buffered: new Signal<Container.BufferedRanges>([]),
	};
	readonly out = readonlys(this.#out);

	// The current track running, held so we can cancel it when the new track is ready.
	#active = new Signal<DecoderTrack | undefined>(undefined);
	// The rendition preparing to replace the active one, with the catalog of the broadcast it
	// subscribed to. One value so a later source that reuses the track name still notifies:
	// the name alone would compare equal, and the jitter effect would keep the previous catalog.
	#pending = new Signal<{ track: string; catalog: Getter<Catalog.Root | undefined> } | undefined>(undefined);
	// The active track's arrival estimate from its container consumer.
	#spread = new Signal<Milli | undefined>(undefined);
	// The path of the media the last rendition read, and its instance. Every handle to one instance
	// shares `closed`, so it identifies the instance where the handle itself does not.
	#instance?: { path: Moq.Path.Valid; closed: unknown };
	readonly #identity: Computed<PlaybackIdentity | undefined>;

	// Fans received frames out to open `arrivals()` handles; retains nothing without one.
	#arrivals = new ArrivalProducer();

	#signals = new Effect();
	#selected?: DecoderTrack;
	#failed?: Failure;

	#clearCurrentFrame(): void {
		this.#out.frame.update((prev) => {
			prev?.close();
			return undefined;
		});
		this.#out.timestamp.set(undefined);
	}

	constructor(props: DecoderProps) {
		this.in = {
			enabled: getter(props?.enabled ?? true),
		};

		this.source = props.source;
		this.sync = props.sync;
		// "auto" holds the arrival estimate plus the active rendition's catalog `delay`, floored at the
		// catalog requirement. The estimate cancels any offset between tracks, so the delay is added
		// back. The audio frame term has no video counterpart: the rendition jitter already covers a
		// frame interval.
		const target = this.#signals.computed((effect) => {
			const jitter = effect.get(this.out.jitter);
			const spread = effect.get(this.#spread);
			if (spread === undefined) return jitter;
			const active = effect.get(this.#active);
			const delay = active && effect.get(active.catalog)?.video?.renditions?.[active.track]?.delay;
			return Milli(Math.max(spread + (delay ?? 0), jitter ?? Milli.zero));
		});
		this.#signals.cleanup(this.sync.register(target));
		this.#identity = this.#signals.computed((effect) => {
			const config = effect.get(this.source.out.config);
			return config ? playbackIdentity(config) : undefined;
		});

		this.#signals.run(this.#runJitter.bind(this));
		this.#signals.run(this.#runPending.bind(this));
		this.#signals.run(this.#runActive.bind(this));
		this.#signals.run(this.#runDisplay.bind(this));
		this.#signals.run(this.#runBuffering.bind(this));
	}

	// Read from the full catalog entry, which the decoder config omits, so a rising entry resizes Sync.
	// Each in-flight rendition stays tied to the catalog it subscribed to: during a source switch the
	// shared catalog already describes the next broadcast, and the same track name there is a
	// different rendition.
	#runJitter(effect: Effect): void {
		const floor = (track: string | undefined, catalog: Getter<Catalog.Root | undefined> | undefined) => {
			if (track === undefined || catalog === undefined) return undefined;
			const config = effect.get(catalog)?.video?.renditions?.[track];
			return config && renditionJitter(config);
		};
		const activeTrack = effect.get(this.#active);
		const active = floor(activeTrack?.track, activeTrack?.catalog);
		const pendingTrack = effect.get(this.#pending);
		const pending = floor(pendingTrack?.track, pendingTrack?.catalog);
		effect.set(this.#out.jitter, switchJitter({ active, pending }));
	}

	#runPending(effect: Effect): void {
		this.#selected = undefined;
		const enabled = effect.get(this.in.enabled);
		const broadcast = effect.get(this.source.in.broadcast);
		const track = effect.get(this.source.out.track);
		const identity = effect.get(this.#identity);

		// Honor a per-rendition `broadcast` override: subscribe on the resolved source
		// broadcast instead of the catalog's own broadcast. A disabled decoder resolves it only to
		// tell whether a failure's source is still selected.
		const active: Moq.Broadcast.Consumer | undefined =
			broadcast && track && identity && (enabled || this.#failed)
				? broadcast.relativeBroadcast(effect, identity.broadcast)
				: undefined;
		const key = identity && JSON.stringify(identity);

		// A failure belongs to its source: selecting another clears it, even while disabled.
		const failed = this.#failed;
		const selected = broadcast && failed ? effect.get(broadcast.out.active)?.closed : undefined;
		if (failed && failureMoved(failed, { broadcast: selected, media: active?.closed, track, identity: key })) {
			this.#failed = undefined;
			this.#out.error.set(undefined);
		}

		if (!enabled || !broadcast || !track || !identity) {
			// Close the active track when disabled (e.g. paused or not visible).
			// The pending cleanup won't do this because it was already promoted to #active.
			this.#active.set(undefined);
			// A paused or hidden player keeps its picture, but with nothing selectable (every rendition
			// disabled or removed) it shows black rather than a frozen frame.
			if (enabled && broadcast && !track) {
				this.#clearCurrentFrame();
				this.#out.buffered.set([]);
			}
			return;
		}

		if (!active || !key) {
			// Going offline should clear the last rendered frame.
			this.#active.set(undefined);
			this.#clearCurrentFrame();
			this.#out.buffered.set([]);
			return;
		}

		// This source already failed; wait for another.
		if (this.#failed) return;

		// A republish of the media this rendition reads (the catalog's own broadcast, or the one a
		// `broadcast` override names) is another instance, whose timeline starts over. Waiting for
		// it to catch up to the old picture would freeze video until it passed the old timestamps,
		// so switch at once and re-anchor the clock. A rendition switch within one instance, or a
		// switch to another path, still waits.
		const republished = this.#instance?.path === active.path && this.#instance.closed !== active.closed;
		this.#instance = { path: active.path, closed: active.closed };
		if (republished) this.sync.reset();

		// Start a new pending effect.
		let pending: DecoderTrack | undefined = new DecoderTrack({
			sync: this.sync,
			broadcast: active,
			track,
			name: Catalog.track(track, identity),
			config: identity.decoder,
			identity: key,
			stats: this.#out.stats,
			catalog: broadcast.out.catalog,
		});
		this.#selected = pending;
		effect.run((inner) => {
			if (!pending) {
				inner.close(); // Promoted tracks are observed by their active lifetime.
				return;
			}
			const error = inner.get(pending.error);
			if (!error) return;
			this.#recordError(pending, error);
			pending.close();
			pending = undefined;
			this.#pending.set(undefined);
		});
		effect.set(this.#pending, { track, catalog: broadcast.out.catalog });

		effect.cleanup(() => pending?.close());

		effect.run((effect) => {
			if (!pending) {
				effect.close();
				return;
			}

			const current = effect.get(this.#active);
			if (current && !republished) {
				// A zero timestamp is a rendered frame, not a missing one.
				const pendingTimestamp = effect.get(pending.timestamp);
				if (pendingTimestamp === undefined) return;

				// Hold off until the new rendition has caught up to the picture it's replacing.
				if (!caughtUp({ playhead: pendingTimestamp, active: effect.get(current.timestamp) })) return;
			}

			// Upgrade the pending track to active.
			// #runActive will be in charge of it now.
			if (current) current.arrivals = undefined;
			pending.arrivals = this.#arrivals;
			this.#active.set(pending);
			this.#pending.set(undefined);
			pending = undefined;

			// This effect is done; close it to avoid a useless re-run.
			effect.close();
		});
	}

	#runActive(effect: Effect): void {
		const active = effect.get(this.#active);
		if (!active) {
			// Clear stale data when disabled (e.g. paused or not visible).
			this.#out.buffered.set([]);
			this.#spread.set(undefined);
			return;
		}

		effect.cleanup(() => active.close());

		effect.run((inner) => {
			const error = inner.get(active.error);
			if (!error) return;
			this.#recordError(active, error);
			this.#active.set(undefined);
			this.#clearCurrentFrame();
		});

		// Clone the frame so we own it independently of the DecoderTrack.
		// proxy() would share the same reference, allowing the source to close our frame.
		effect.run((inner) => {
			const frame = inner.get(active.frame);
			if (!frame) return; // A promoted track holds the previous picture until its first frame.
			if (active.error.peek()) return;
			if (this.#selected === active) {
				this.#failed = undefined;
				this.#out.error.set(undefined);
			}
			this.#out.timestamp.set(Milli.fromMicro(frame.timestamp as Micro));
			this.#out.frame.update((prev) => {
				prev?.close();
				return frame.clone();
			});
		});
		effect.proxy(this.#out.buffered, active.buffered);
		effect.proxy(this.#spread, active.spread);
	}

	#recordError(track: DecoderTrack, error: Error): void {
		// An outgoing failure must not replace the selected rendition's failure latch.
		if (track !== this.#selected && this.#selected?.error.peek()) return;
		this.#failed = {
			broadcast: this.source.in.broadcast.peek()?.out.active.peek()?.closed,
			media: track.broadcast.closed,
			track: track.track,
			identity: track.identity,
		};
		this.#out.error.set(error);
	}

	#runDisplay(effect: Effect): void {
		const catalog = effect.get(this.source.out.catalog);
		if (!catalog) return;

		const display = catalog.display;
		if (display) {
			effect.set(this.#out.display, { width: display.width, height: display.height });
			return;
		}

		const frame = effect.get(this.#out.frame);
		if (!frame) return;

		effect.set(
			this.#out.display,
			rotateVideoDimensions({ width: frame.displayWidth, height: frame.displayHeight }, catalog.rotation),
		);
	}

	#runBuffering(effect: Effect): void {
		const enabled = effect.get(this.in.enabled);
		if (!enabled) return;

		const frame = effect.get(this.#out.frame);
		if (!frame) {
			this.#out.stalled.set(true);
			return;
		}

		this.#out.stalled.set(false);

		effect.timer(() => {
			this.#out.stalled.set(true);
		}, Nano.fromMilli(BUFFERING));
	}

	/** Observe every frame and gap the displayed rendition receives until the handle or this decoder closes. */
	arrivals(): Arrivals {
		return this.#arrivals.consume();
	}

	close() {
		this.#clearCurrentFrame();
		this.#arrivals.close();

		this.#signals.close();
	}

	// Whether the WebCodecs video decoder can play this config.
	static supported = supported;
}

interface DecoderTrackProps {
	sync: Sync;
	broadcast: Moq.Broadcast.Consumer;
	// The rendition ID; `name` is its track's name in `broadcast`.
	track: string;
	name: string;
	config: DecoderConfig;
	identity: string;
	// The broadcast catalog this subscription started from. Updates on that broadcast still apply;
	// a later source does not.
	catalog: Getter<Catalog.Root | undefined>;

	stats: Signal<Stats | undefined>;
}

class DecoderTrack {
	error = new Signal<Error | undefined>(undefined);
	sync: Sync;
	broadcast: Moq.Broadcast.Consumer;
	track: string;
	name: string;
	config: DecoderConfig;
	identity: string;
	catalog: Getter<Catalog.Root | undefined>;
	stats: Signal<Stats | undefined>;
	// Set once promoted, so only the rendition feeding the picture reports arrivals.
	arrivals?: ArrivalProducer;

	timestamp = new Signal<Milli | undefined>(undefined);
	frame = new Signal<VideoFrame | undefined>(undefined);

	// Network jitter + decode buffer.
	buffered = new Signal<Container.BufferedRanges>([]);

	// How late frames arrive relative to the earliest one, from the container consumer.
	spread = new Signal<Milli | undefined>(undefined);

	// Decoded frames waiting to be rendered.
	#buffered = new Signal<Container.BufferedRanges>([]);

	// Codec description from the last configure, reused after a playhead reset.
	#description?: Uint8Array;

	// The playhead moved and the next decoded frame decides whether the decoder resets.
	#resetPending = false;

	#signals = new Effect();

	constructor(props: DecoderTrackProps) {
		this.sync = props.sync;
		this.broadcast = props.broadcast;
		this.track = props.track;
		this.name = props.name;
		this.config = props.config;
		this.identity = props.identity;
		this.catalog = props.catalog;
		this.stats = props.stats;

		this.#signals.run(this.#run.bind(this));
	}

	#run(effect: Effect): void {
		const sub = subscribeMedia(effect, {
			broadcast: this.broadcast,
			track: this.name,
			priority: Catalog.PRIORITY.video,
			maxDelay: this.sync.out.maxDelay,
		});
		if (!sub) return;

		const decoder = new VideoDecoder({
			output: async (frame: VideoFrame) => {
				try {
					// The clock epoch this frame was decoded in. A reset while we wait below (this
					// track's playhead moving, or a shared reset such as audio's at an utterance
					// boundary) means the frame belongs to the previous timeline and must be dropped.
					const epoch = this.sync.epoch;

					const timestamp = Milli.fromMicro(frame.timestamp as Micro);
					if (timestamp < (this.timestamp.peek() ?? 0)) {
						// Late frame, don't render it.
						return;
					}

					// `received()` runs at submit, so a frame is anchored by the time it decodes.
					// Reaching here unanchored means a reset cleared the clock after this frame's
					// received(), so it predates the current timeline. Drop it: painting it would
					// set `timestamp` from the old timeline and late-reject what follows.
					if (this.sync.out.reference.peek() === undefined) return;
					// WebCodecs can hold a picture until after another track skipped the playhead past it.
					if (this.sync.skipped(timestamp)) return;

					if (this.frame.peek() === undefined) {
						// Render something while we wait for the sync to catch up.
						this.frame.set(frame.clone());
					}

					// Returns immediately when the latency is "instant". Closing the track releases it.
					// Capture the signal: a rerun replaces `effect.abort` before this continuation runs.
					const signal = effect.abort;
					await this.sync.wait(timestamp, signal);
					if (signal.aborted) return;
					if (epoch !== this.sync.epoch || this.sync.skipped(timestamp)) return;

					if (timestamp < (this.timestamp.peek() ?? 0)) {
						// Late frame, don't render it.
						// NOTE: This can happen when the ref is updated, such as on playback start.
						return;
					}

					this.timestamp.set(timestamp);

					// Trim the decode buffer as frames are rendered
					this.#trimBuffered(timestamp);

					this.frame.update((prev) => {
						prev?.close();
						return frame.clone(); // avoid closing the frame here
					});
				} finally {
					frame.close();
				}
			},
			error: (error) => {
				this.error.set(error);
				effect.close();
			},
		});
		effect.cleanup(() => {
			if (decoder.state !== "closed") decoder.close();
		});

		// Input processing - depends on container type
		if (this.config.container.kind === "cmaf") {
			this.#runCmaf(effect, sub, decoder);
		} else {
			this.#runLegacy(effect, sub, decoder);
		}
	}

	#runLegacy(effect: Effect, sub: Moq.Track.Subscriber, decoder: VideoDecoder): void {
		const format =
			this.config.container.kind === "loc"
				? new Container.Loc.Format("video")
				: new Container.Legacy.Format(this.config);
		// Create consumer that reorders groups/frames up to the provided latency.
		const consumer = new Container.Consumer(sub, {
			format,
			maxDelay: this.sync.out.maxDelay,
		});
		effect.cleanup(() => consumer.close());

		// Combine network jitter buffer with decode buffer
		effect.run((inner) => {
			const network = inner.get(consumer.buffered);
			const decode = inner.get(this.#buffered);
			this.buffered.update(() => Container.mergeBufferedRanges(network, decode));
		});

		// Publish the arrival estimate for the "auto" target.
		effect.run((inner) => this.spread.set(inner.get(consumer.spread)));

		this.#description = this.config.description ? Util.Hex.toBytes(this.config.description) : undefined;
		this.#configure(decoder);

		let previous: Micro | undefined;
		let latest: number | undefined;
		const recovery = new Recovery();

		effect.spawn(async () => {
			for (;;) {
				const next = await nextMedia(consumer);
				if (!next) break;

				// A gap or a declared end breaks the timeline: re-anchor before decoding further, and
				// never bridge the buffer across it.
				if (next.kind !== "frame") {
					if (next.kind === "gap") {
						recovery.gap();
						this.arrivals?.write(gap(next));
					}
					if (latest !== undefined) this.#onBreak();
					previous = undefined;
					continue;
				}
				const { frame } = next;

				// An older keyframe would replace the codec references needed by live deltas. Group
				// sequences only grow within a subscription, so a break (which a stale marker can
				// raise) never resets this guard. Rejected payloads still count as received bytes.
				const stale = latest !== undefined && next.group < latest;
				const accepted = !stale && recovery.accept(frame);
				this.stats.update((current) => ({
					frameCount: (current?.frameCount ?? 0) + (accepted ? 1 : 0),
					bytesReceived: (current?.bytesReceived ?? 0) + frame.payload.byteLength,
				}));
				if (!accepted) continue;

				// Classify ingress before decoder backpressure changes the delivery clock.
				const timestamp = Milli.fromMicro(frame.timestamp as Micro);
				const late = this.sync.received(timestamp, "video", frame.arrival);
				this.arrivals?.write({ kind: "frame", timestamp, arrival: frame.arrival, late });

				const chunk = new EncodedVideoChunk({
					type: frame.keyframe ? "key" : "delta",
					data: frame.payload,
					timestamp: frame.timestamp,
				});

				// Track decode buffer: frames sent to decoder but not yet rendered. Only bridge from
				// the previous frame when no gap or end came between them. Group ids can't answer that:
				// they aren't required to be sequential (some encoders derive them from DTS), so
				// adjacency neither proves continuity nor rules out a gap the consumer skipped, and
				// reporting a skipped span as decoded overstates the buffer.
				if (previous !== undefined) {
					this.#addBuffered(Milli.fromMicro(previous), Milli.fromMicro(frame.timestamp));
				}

				previous = frame.timestamp;

				latest = next.group;
				this.#resetAtKeyframe(decoder, frame.keyframe);
				decoder.decode(chunk);
			}
		});
	}

	#runCmaf(effect: Effect, sub: Moq.Track.Subscriber, decoder: VideoDecoder): void {
		const container = this.config.container;
		if (container.kind !== "cmaf") return;

		const initSegment = base64ToBytes(container.init);
		const init = Container.Cmaf.decodeInitSegment(initSegment);
		const description = this.config.description ? Util.Hex.toBytes(this.config.description) : init.description;

		const consumer = new Container.Consumer(sub, {
			format: new Container.Cmaf.Format(init),
			maxDelay: this.sync.out.maxDelay,
		});
		effect.cleanup(() => consumer.close());

		// Combine network jitter buffer with decode buffer
		effect.run((inner) => {
			const network = inner.get(consumer.buffered);
			const decode = inner.get(this.#buffered);
			this.buffered.update(() => Container.mergeBufferedRanges(network, decode));
		});

		// Publish the arrival estimate for the "auto" target.
		effect.run((inner) => this.spread.set(inner.get(consumer.spread)));

		// Configure decoder with description from catalog
		this.#description = description;
		this.#configure(decoder);

		let previous: Micro | undefined;
		let latest: number | undefined;
		const recovery = new Recovery();

		effect.spawn(async () => {
			for (;;) {
				const next = await nextMedia(consumer);
				if (!next) break;

				// A gap or a declared end breaks the timeline (see #runLegacy).
				if (next.kind !== "frame") {
					if (next.kind === "gap") {
						recovery.gap();
						this.arrivals?.write(gap(next));
					}
					if (latest !== undefined) this.#onBreak();
					previous = undefined;
					continue;
				}
				const { frame } = next;

				// An older keyframe would replace the codec references needed by live deltas. Group
				// sequences only grow within a subscription, so a break (which a stale marker can
				// raise) never resets this guard. Rejected payloads still count as received bytes.
				const stale = latest !== undefined && next.group < latest;
				const accepted = !stale && recovery.accept(frame);
				this.stats.update((current) => ({
					frameCount: (current?.frameCount ?? 0) + (accepted ? 1 : 0),
					bytesReceived: (current?.bytesReceived ?? 0) + frame.payload.byteLength,
				}));
				if (!accepted) continue;

				// Classify ingress before decoder backpressure changes the delivery clock.
				const timestamp = Milli.fromMicro(frame.timestamp);
				const late = this.sync.received(timestamp, "video", frame.arrival);
				this.arrivals?.write({ kind: "frame", timestamp, arrival: frame.arrival, late });

				// Track decode buffer (see #runLegacy: bridge only when no gap or end came between,
				// never on group adjacency, which proves nothing about the timeline).
				if (previous !== undefined) {
					this.#addBuffered(Milli.fromMicro(previous), Milli.fromMicro(frame.timestamp));
				}

				previous = frame.timestamp;

				if (decoder.state === "closed") break;
				latest = next.group;
				this.#resetAtKeyframe(decoder, frame.keyframe);
				decoder.decode(
					new EncodedVideoChunk({
						type: frame.keyframe ? "key" : "delta",
						data: frame.payload,
						timestamp: frame.timestamp,
					}),
				);
			}
		});
	}

	// The container read a gap or a declared end after media. Re-anchor and let the next decoded
	// frame decide the decoder reset (see #resetAtKeyframe). Clearing `timestamp` keeps a stale
	// high value from late-rejecting the following frames, which may overlap the previous group's
	// tail. The held picture stays until the new keyframe renders.
	#onBreak(): void {
		this.timestamp.set(undefined);
		this.#buffered.set([]);
		this.sync.reset();
		this.#resetPending = true;
	}

	// Call before each decode. After a break, a keyframe starts a new timeline, so drop the
	// pictures still queued in the decoder: one from later in a group that restarts would
	// otherwise come out above the new keyframe and late-reject it. A delta continues a group the
	// break interrupted (a stale marker or a max-delay hole), so it keeps the decoder's references;
	// a reset would make WebCodecs require a key chunk and throw. `reset()` cannot cancel an
	// output callback that already holds a frame; the clock epoch it captured rejects that one.
	#resetAtKeyframe(decoder: VideoDecoder, keyframe: boolean): void {
		if (!this.#resetPending) return;
		this.#resetPending = false;
		// A closed decoder throws on reset.
		if (!keyframe || decoder.state === "closed") return;
		decoder.reset();
		this.#configure(decoder);
	}

	#configure(decoder: VideoDecoder): void {
		decoder.configure({
			codec: this.config.codec,
			description: this.#description,
			displayAspectWidth: this.config.displayAspectWidth,
			displayAspectHeight: this.config.displayAspectHeight,
			optimizeForLatency: this.config.optimizeForLatency,
			// @ts-expect-error Only supported by Chrome, so the renderer has to flip manually.
			flip: false,
		});
	}

	// Add a range to the decode buffer (decoded, waiting to render)
	#addBuffered(start: Milli, end: Milli): void {
		if (start > end) return;

		this.#buffered.mutate((current) => {
			for (const range of current) {
				// Check if there's any overlap, then merge
				if (range.start <= end && range.end >= start) {
					range.start = Milli(Math.min(range.start, start));
					range.end = Milli(Math.max(range.end, end));
					return;
				}
			}

			current.push({ start, end });
			current.sort((a, b) => a.start - b.start);
		});
	}

	// Trim the decode buffer up to the rendered timestamp
	#trimBuffered(timestamp: Milli): void {
		this.#buffered.mutate((current) => {
			while (current.length > 0) {
				if (current[0].end >= timestamp) {
					current[0].start = Milli(Math.max(current[0].start, timestamp));
					break;
				}
				current.shift();
			}
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

async function supported(config: Catalog.VideoConfig): Promise<boolean> {
	if (!Catalog.containerSupported(config.container)) {
		// `kind` is the literal "unknown" tag; the container the publisher actually named is in `raw`.
		const kind = config.container.kind === "unknown" ? config.container.raw.kind : config.container.kind;
		console.warn(`video: ignoring rendition with unknown container: ${kind}`);
		return false;
	}

	let description: Uint8Array | undefined;
	if (config.description) {
		description = Util.Hex.toBytes(config.description);
	} else if (config.container.kind === "cmaf") {
		try {
			description = Container.Cmaf.decodeInitSegment(base64ToBytes(config.container.init)).description;
		} catch (err) {
			// A malformed init segment means we can't extract the codec
			// description, so we can't probe support reliably. Reject the
			// track rather than letting isConfigSupported pass on a
			// description-less config and then having runCmaf fail later.
			console.warn(`video: malformed CMAF init segment for codec ${config.codec}`, err);
			return false;
		}
	}
	const { supported } = await VideoDecoder.isConfigSupported({
		codec: config.codec,
		description,
		displayAspectWidth: config.displayAspectWidth,
		displayAspectHeight: config.displayAspectHeight,
		optimizeForLatency: config.optimizeForLatency ?? true,
	});

	if (supported) return true;

	// Safari rejects `avc3.*` codec strings even though its H.264 decoder handles
	// inline SPS/PPS. Rewrite to `avc1.*` and retry; mutate config.codec so the
	// later `decoder.configure()` call uses the accepted string too.
	if (config.codec.startsWith("avc3.")) {
		const avc1 = `avc1.${config.codec.slice("avc3.".length)}`;
		const retry = await VideoDecoder.isConfigSupported({
			codec: avc1,
			description,
			displayAspectWidth: config.displayAspectWidth,
			displayAspectHeight: config.displayAspectHeight,
			optimizeForLatency: config.optimizeForLatency ?? true,
		});
		if (retry.supported) {
			config.codec = avc1;
			return true;
		}
	}

	return false;
}

Object.assign(supported, { [supportCacheKey]: decoderConfigKey });

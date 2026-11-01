import { type Micro, Milli, Nano } from "@moq/clock";
import * as Catalog from "@moq/hang/catalog";
import * as Container from "@moq/hang/container";
import * as Util from "@moq/hang/util";
import type * as Moq from "@moq/net";
import {
	type Computed,
	Derived,
	Effect,
	type Getter,
	getter,
	type Inputs,
	Once,
	type Readonlys,
	readonlys,
	Signal,
} from "@moq/signals";
import { rotateVideoDimensions } from "@moq/video";
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

// The startup floor counts received media across holes, without counting the missing spans.
function startupSpan(ranges: Container.BufferedRanges, first: Milli): { start: Milli; end: Milli } {
	let duration = 0;
	for (const range of ranges) duration += Math.max(0, range.end - Math.max(range.start, first));
	return { start: first, end: Milli(first + duration) };
}

/** Signals the decoder reads. */
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

/** Signals the decoder derives. */
export type DecoderOutput = {
	/** Fatal decode failure, cleared when another source is selected. */
	error: Signal<Error | undefined>;
	// The current frame to render.
	frame: Signal<VideoFrame | undefined>;

	// Whether the current frame weaves two fields, per the rendition that decoded it.
	interlaced: Signal<boolean>;

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
	/** The wired inputs, read-only. */
	readonly in: Readonlys<DecoderInput>;
	/** The source picking the rendition to decode. */
	readonly source: Source;
	/** The clock that schedules playback. */
	readonly sync: Sync;

	readonly #out: DecoderOutput = {
		error: new Signal<Error | undefined>(undefined),
		frame: new Signal<VideoFrame | undefined>(undefined),
		interlaced: new Signal<boolean>(false),
		timestamp: new Signal<Milli | undefined>(undefined),
		display: new Signal<{ width: number; height: number } | undefined>(undefined),
		stalled: new Signal<boolean>(false),
		stats: new Signal<Stats | undefined>(undefined),
		jitter: new Signal<Milli | undefined>(undefined),
		buffered: new Signal<Container.BufferedRanges>([]),
	};
	/** The derived outputs, read-only. */
	readonly out = readonlys(this.#out);

	// The current track running, held so we can cancel it when the new track is ready.
	#active = new Signal<Rendition | undefined>(undefined);
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

	#signals = new Effect();
	#selected?: Rendition;
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
		const startup = this.#signals.computed((effect) => {
			if (!effect.get(this.in.enabled)) return undefined;
			if (effect.get(this.source.pending)) return null;
			// A held disabled rendition has no frames coming, so startup does not wait on it.
			const config = effect.get(this.source.out.config);
			if (!config || config.enabled === false) return undefined;
			const active = effect.get(this.#active);
			if (!active) return null;
			const timestamp = effect.get(active.first);
			if (timestamp === undefined) return null;
			const ranges = effect.get(active.buffered);
			return startupSpan(ranges, timestamp);
		});
		this.#signals.cleanup(this.sync.register(target, startup));
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
				? broadcast.relativeBroadcast(effect, identity.broadcast, identity.epoch)
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
			// Pause and visibility keep the picture while the broadcast is active. Disconnecting
			// the broadcast or removing every rendition releases it.
			if (!broadcast || !effect.get(broadcast.out.active) || (enabled && !track)) {
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

		// Only a rendition read from the same path continues the outgoing arrival estimate: another
		// path's arrivals have nothing to do with what this one learned.
		const outgoing = this.#active.peek();
		const predecessor = outgoing?.broadcast.path === active.path ? outgoing : undefined;

		// Start a new pending effect.
		let pending: Rendition | undefined = new Rendition({
			sync: this.sync,
			broadcast: active,
			track,
			name: Catalog.track(track, identity),
			config: identity.decoder,
			identity: key,
			stats: this.#out.stats,
			catalog: broadcast.out.catalog,
			predecessor,
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
			this.#out.interlaced.set(active.config.interlaced);
			this.#out.frame.update((prev) => {
				prev?.close();
				return frame.clone();
			});
		});
		effect.proxy(this.#out.buffered, active.buffered);
		effect.proxy(this.#spread, active.spread);
	}

	#recordError(track: Rendition, error: Error): void {
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

	/** Stop and release everything this holds. */
	close() {
		this.#clearCurrentFrame();

		this.#signals.close();
	}

	/** Whether the WebCodecs video decoder can play this config. */
	static supported = supported;
}

interface DecoderTrackProps {
	sync: Sync;
	// Set for the max-delay decoder of a rendition with a catalog `warmup`: its start and its
	// lateness budget reach this much further, and it leaves the shared clock and stats alone.
	warmup?: Getter<Milli>;
	// Whether the rendition has a catalog `warmup`, so its groups may open on a recovery point that
	// is not an IDR. Set for both decoders.
	refresh?: Getter<boolean>;
	broadcast: Moq.Broadcast.Consumer;
	// The rendition ID; `name` is its track's name in `broadcast`.
	track: string;
	name: string;
	config: DecoderConfig;
	identity: string;
	// The broadcast catalog this subscription started from. Updates on that broadcast still apply;
	// a later source does not.
	catalog: Getter<Catalog.Root | undefined>;
	// The consumer of the rendition on screen when this one was selected from the same path, whose
	// arrival estimate it continues.
	predecessor?: Container.Consumer;

	// Absent for the max-delay decoder: the bytes arrive once and the min-delay decoder counts them.
	stats?: Signal<Stats | undefined>;
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
	stats?: Signal<Stats | undefined>;
	timestamp = new Signal<Milli | undefined>(undefined);
	frame = new Signal<VideoFrame | undefined>(undefined);

	// The group the current `frame` was read from. Group sequences are shared by every subscription to
	// the track, so they place a picture on a timeline for either decoder.
	group?: number;
	// The first group of the timeline this decoder reads, undefined until a publisher restart.
	readonly start = new Signal<number | undefined>(undefined);

	// Counts the timeline restarts this decoder read. A picture decoded before one is from the old
	// timeline and never presents after it, even when its clock wait already resolved.
	#generation = 0;
	// The group each frame in the codec was read from, by timestamp. Each output found here retires its
	// entry and every one before it. A publisher that never outputs a picture grows this by one entry
	// per chunk; the maintainer accepted that as an exception to the bounded-state rule
	// (moq-dev/moq#7555, 2026-11-01), since WebCodecs bounds no codec's pending output, so any cap is
	// a guess that can evict a picture the codec still owes.
	#groups = new Map<number, number>();

	// How far past the shared clock's budgets this decoder reaches.
	#warmup: Getter<Milli>;
	// Whether this decoder drives the shared clock: only the min-delay one reports arrivals and
	// restarts the timeline.
	#drives: boolean;
	#refresh: Getter<boolean>;

	// Network jitter + decode buffer.
	buffered = new Signal<Container.BufferedRanges>([]);
	// First decoded picture on this timeline, independent of the previously held poster.
	readonly first = new Signal<Milli | undefined>(undefined);

	// How late frames arrive relative to the earliest one, from the container consumer.
	spread = new Signal<Milli | undefined>(undefined);

	// Decoded frames waiting to be rendered.
	#buffered = new Signal<Container.BufferedRanges>([]);

	// Codec description from the last configure, reused after a playhead reset.
	#description?: Uint8Array;
	// Whether the next chunk opens the codec, after a configure or a reset.
	#opening = false;

	// A hole or timeline restart makes the next decoded frame decide whether the codec resets.
	#resetPending = false;
	// A declared endpoint stays in force through holes until a later group starts a new timeline.
	#endGroup?: number;

	// The newest consumer reading this track, whose estimate a replacement continues. The predecessor
	// is only held until the first one opens.
	consumer?: Container.Consumer;
	#predecessor?: Container.Consumer;

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
		this.#warmup = props.warmup ?? getter(Milli.zero);
		this.#drives = props.warmup === undefined;
		this.#refresh = props.refresh ?? getter(false);
		this.#predecessor = props.predecessor;

		this.#signals.run(this.#run.bind(this));
	}

	// Reorders groups and frames up to the shared max delay. A rendition switch is not a cold start:
	// the path's arrivals are what they were, so the estimate continues from whatever read before.
	#consume(effect: Effect, sub: Moq.Track.Subscriber, format: Container.Format): Container.Consumer {
		// The playout delay is where a cold join starts, and the shared max delay how late it may fall.
		// The max-delay decoder adds the warmup to both, so it starts at the group before and keeps
		// it, as Rust's does, rather than passing the consumer a `warmup` that would reach further.
		const delay = new Derived([this.sync.out.delay, this.sync.out.maxDelay, this.#warmup], (min, max, warmup) => ({
			min: Milli(min + warmup),
			max: Milli(max + warmup),
		}));
		const consumer = new Container.Consumer(sub, {
			format,
			delay,
			predecessor: this.consumer ?? this.#predecessor,
		});
		effect.cleanup(() => consumer.close());
		this.consumer = consumer;
		this.#predecessor = undefined;
		return consumer;
	}

	#run(effect: Effect): void {
		const sub = subscribeMedia(effect, {
			broadcast: this.broadcast,
			track: this.name,
			priority: Catalog.PRIORITY.video,
			// js/net merges both decoders' subscriptions into one request at the larger max delay.
			maxDelay: new Derived([this.sync.out.maxDelay, this.#warmup], (max, warmup) => Milli(max + warmup)),
		});
		if (!sub) return;

		// Keep output tied to this codec lifetime even if the effect opens another run.
		const signal = effect.abort;
		const decoder = new VideoDecoder({
			output: async (frame: VideoFrame) => {
				try {
					// A cancelled codec must not recreate the poster its track already released. Firefox can
					// still deliver output after close().
					if (signal.aborted) return;
					const generation = this.#generation;
					const group = this.#groups.get(frame.timestamp);
					// Pictures come out in presentation order, so a chunk at or before this one that
					// produced none, such as a hidden VP9 frame, never will. Only a picture with an entry
					// proves that: one without was decoded before the last reset cleared the map, so its
					// timestamp says nothing about the chunks fed since.
					if (group !== undefined) {
						for (const timestamp of this.#groups.keys()) {
							if (timestamp <= frame.timestamp) this.#groups.delete(timestamp);
						}
					}
					const timestamp = Milli.fromMicro(frame.timestamp as Micro);
					if (timestamp < (this.timestamp.peek() ?? 0)) {
						// Late frame, don't render it.
						return;
					}

					if (this.first.peek() === undefined) this.first.set(timestamp);

					// Render something while we wait for the sync to catch up, unless the frame is
					// already off the timeline: WebCodecs can hold a picture past a reset or a skip.
					// Only the min-delay decoder posts one: the max-delay picture shows on the clock.
					if (this.#drives && this.frame.peek() === undefined && this.sync.presentable(timestamp)) {
						this.group = group;
						this.frame.set(frame.clone());
					}

					// Resolves at once when the delay is "instant". Closing the track releases it.
					// Check the captured signal again in case cancellation followed the resolved wait.
					if (!(await this.sync.wait(timestamp, signal)) || signal.aborted) return;
					// The max-delay decoder restarts its timeline after the clock did, so the clock's own
					// epoch cannot tell its leftover pictures apart.
					if (this.#generation !== generation) return;

					if (timestamp < (this.timestamp.peek() ?? 0)) {
						// Late frame, don't render it.
						// NOTE: This can happen when the ref is updated, such as on playback start.
						return;
					}

					this.timestamp.set(timestamp);

					// Trim the decode buffer as frames are rendered
					this.#trimBuffered(timestamp);

					this.group = group;
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
		this.#spawn(effect, async () => {
			if (this.config.container.kind === "cmaf") await this.#runCmaf(effect, sub, decoder);
			else this.#runLegacy(effect, sub, decoder);
		});
	}

	// Run `task` for this track. A setup, read, or decode failure that is not its cancellation is
	// fatal: reported through `error`, which the decoder latches, with the track's effect closed.
	#spawn(effect: Effect, task: () => Promise<void>): void {
		effect.spawn(async () => {
			try {
				await task();
			} catch (err) {
				if (effect.abort.aborted) return;
				this.error.set(err instanceof Error ? err : new Error(String(err)));
				effect.close();
			}
		});
	}

	#runLegacy(effect: Effect, sub: Moq.Track.Subscriber, decoder: VideoDecoder): void {
		const format =
			this.config.container.kind === "loc"
				? new Container.Loc.Format("video")
				: new Container.Legacy.Format(this.config);
		const consumer = this.#consume(effect, sub, format);

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

		this.#spawn(effect, async () => {
			for (;;) {
				const next = await nextMedia(consumer);
				if (!next) break;
				this.#onRead(next, latest);

				// A gap or declared end cuts the decoded span; neither bridges buffered ranges.
				if (next.kind !== "frame") {
					if (next.kind === "gap") recovery.gap();
					previous = undefined;
					continue;
				}
				const { frame } = next;

				// An older keyframe would replace the codec references needed by live deltas. Group
				// sequences only grow within a subscription, so neither a hole nor an endpoint
				// resets this guard. Rejected payloads still count as received bytes.
				const stale = latest !== undefined && next.group < latest;
				const accepted = !stale && recovery.accept(frame);
				this.stats?.update((current) => ({
					frameCount: (current?.frameCount ?? 0) + (accepted ? 1 : 0),
					bytesReceived: (current?.bytesReceived ?? 0) + frame.payload.byteLength,
				}));
				if (!accepted) continue;

				// Classify ingress before decoder backpressure changes the delivery clock. Only the
				// min-delay decoder moves the clock: the other reads frames already behind it.
				if (this.#drives) this.sync.received(Milli.fromMicro(frame.timestamp as Micro), frame.arrival);

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
				this.#groups.set(frame.timestamp, next.group);
				decoder.decode(this.#chunk(frame));
				if (!this.sync.playing.peek())
					await this.#waitStartup(effect, Milli.fromMicro(frame.timestamp as Micro));
			}
		});
	}

	async #runCmaf(effect: Effect, sub: Moq.Track.Subscriber, decoder: VideoDecoder): Promise<void> {
		const container = this.config.container;
		if (container.kind !== "cmaf") return;

		const abort = effect.abort;
		const Cmaf = await import("@moq/hang/cmaf");
		if (abort.aborted) return;

		const initSegment = base64ToBytes(container.init);
		const init = Cmaf.decodeInitSegment(initSegment);
		const description = this.config.description ? Util.Hex.toBytes(this.config.description) : init.description;

		const consumer = this.#consume(effect, sub, new Cmaf.Format(init));

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

		this.#spawn(effect, async () => {
			for (;;) {
				const next = await nextMedia(consumer);
				if (!next) break;
				this.#onRead(next, latest);

				// Do not bridge buffered ranges across a gap or end (see #runLegacy).
				if (next.kind !== "frame") {
					if (next.kind === "gap") recovery.gap();
					previous = undefined;
					continue;
				}
				const { frame } = next;

				// An older keyframe would replace the codec references needed by live deltas. Group
				// sequences only grow within a subscription, so neither a hole nor an endpoint
				// resets this guard. Rejected payloads still count as received bytes.
				const stale = latest !== undefined && next.group < latest;
				const accepted = !stale && recovery.accept(frame);
				this.stats?.update((current) => ({
					frameCount: (current?.frameCount ?? 0) + (accepted ? 1 : 0),
					bytesReceived: (current?.bytesReceived ?? 0) + frame.payload.byteLength,
				}));
				if (!accepted) continue;

				// Classify ingress before decoder backpressure changes the delivery clock (see #runLegacy).
				if (this.#drives) this.sync.received(Milli.fromMicro(frame.timestamp), frame.arrival);

				// Track decode buffer (see #runLegacy: bridge only when no gap or end came between,
				// never on group adjacency, which proves nothing about the timeline).
				if (previous !== undefined) {
					this.#addBuffered(Milli.fromMicro(previous), Milli.fromMicro(frame.timestamp));
				}

				previous = frame.timestamp;

				if (decoder.state === "closed") break;
				latest = next.group;
				this.#resetAtKeyframe(decoder, frame.keyframe);
				this.#groups.set(frame.timestamp, next.group);
				decoder.decode(this.#chunk(frame));
				if (!this.sync.playing.peek()) await this.#waitStartup(effect, Milli.fromMicro(frame.timestamp));
			}
		});
	}

	// Decoded pictures park until startup releases, so leave further media encoded upstream once this
	// track covers its floor, or once `timestamp` sits the max delay past the opening picture: the
	// most any held frame may lead the playhead, which startup anchors there. Holes keep coverage
	// short without bound, so only the second limit bounds the parked pictures.
	async #waitStartup(effect: Effect, timestamp: Milli): Promise<void> {
		// Some codecs need more input before their first output. Do not park that input
		// behind a gate that needs the very picture the decoder has not produced yet.
		const first = this.first.peek();
		if (first === undefined) return;
		const filled = (get: <T>(signal: Getter<T>) => T) => {
			const span = startupSpan(get(this.#buffered), first);
			return (
				span.end - span.start >= get(this.sync.out.delay) || timestamp - first >= get(this.sync.out.maxDelay)
			);
		};
		if (!filled((signal) => signal.peek())) return;
		const waiting = new Effect();
		const refill = new Once<true>();
		waiting.run((inner) => {
			// An adaptive floor can rise while another track fills. Resume decoding to reach it,
			// rather than waiting on a clock whose startup now needs more video.
			if (!filled((signal) => inner.get(signal))) refill.set(true);
		});
		try {
			// Wait on the first picture, which is due the moment startup releases, so decoding
			// resumes with the whole floor still ahead instead of once it has played out.
			await effect.race(refill, this.sync.wait(first, waiting.abort));
		} finally {
			waiting.close();
		}
	}

	// Container gaps restart prediction on the existing clock. Only a declared end followed by
	// a later group establishes another timeline; a marker from older history cannot end live media.
	#onRead(read: Container.Read, latest: number | undefined): void {
		if (read.kind === "gap") {
			if (latest !== undefined) this.#resetPending = true;
		} else if (read.kind === "end") {
			if (latest === undefined || read.group >= latest) this.#endGroup = read.group;
		} else if (this.#endGroup !== undefined && read.group > this.#endGroup) {
			this.#endGroup = undefined;
			this.#onBreak(read.group);
		}
	}

	// A fresh timeline invalidates parked pictures and its previous decoded span. The held picture
	// stays until the new keyframe renders; the codec resets before that keyframe is decoded.
	#onBreak(group: number): void {
		this.first.set(undefined);
		this.timestamp.set(undefined);
		this.#buffered.set([]);
		this.#generation++;
		this.start.set(group);
		// The min-delay decoder restarts the shared clock. The max-delay one reads the same end later.
		if (this.#drives) this.sync.reset();
		this.#resetPending = true;
	}

	// Re-prime at a keyframe after a hole or restart, dropping pictures still queued in the codec.
	// A delta may continue a group that a max-delay hole interrupted and still need its references;
	// resetting before it would make WebCodecs require a key chunk. An output callback that already
	// holds a frame stays parked on Sync; only a new timeline invalidates its clock epoch.
	#resetAtKeyframe(decoder: VideoDecoder, keyframe: boolean): void {
		if (!this.#resetPending) return;
		this.#resetPending = false;
		// A closed decoder throws on reset.
		if (!keyframe || decoder.state === "closed") return;
		decoder.reset();
		this.#groups.clear();
		this.#configure(decoder);
	}

	// The chunk for `frame`, run through the opening rewrite when it opens the codec.
	#chunk(frame: Container.ReceivedFrame): EncodedVideoChunk {
		let data = frame.payload;
		if (this.#opening) {
			this.#opening = false;
			if (frame.keyframe && this.#refresh.peek() && isH264(this.config.codec)) {
				data = insertRecoveryPoint(data, this.#description);
			}
		}
		return new EncodedVideoChunk({ type: frame.keyframe ? "key" : "delta", data, timestamp: frame.timestamp });
	}

	#configure(decoder: VideoDecoder): void {
		this.#opening = true;
		decoder.configure({
			codec: this.config.codec,
			description: this.#description,
			displayAspectWidth: this.config.displayAspectWidth,
			displayAspectHeight: this.config.displayAspectHeight,
			optimizeForLatency: this.config.optimizeForLatency,
			hardwareAcceleration: hardwareAcceleration(this.config),
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

/** Constructor properties for {@link Rendition}. */
interface RenditionProps extends Omit<DecoderTrackProps, "warmup" | "refresh" | "predecessor" | "stats"> {
	stats: Signal<Stats | undefined>;
	// The rendition on screen when this one was selected from the same path, whose decoders' arrival
	// estimates these continue.
	predecessor?: Rendition;
}

/**
 * One rendition's playback: a min-delay decoder, plus a max-delay decoder while the catalog gives the
 * rendition a nonzero `warmup` (intra refresh, so no keyframes).
 *
 * The min-delay decoder holds the app's latency and drives the shared clock, so it may show refresh
 * stripes after a cold join or a skip. The max-delay decoder starts one group earlier and tolerates
 * the warmup more lateness, so its pictures are clean, and it decodes what is already due as fast as
 * it can without moving the clock. Its picture is shown whenever its timestamp is at or past the
 * min-delay one's, and nothing shown ever steps back in time. Rust players do the same in
 * `moq_video::decode::Consumer`.
 */
class Rendition {
	readonly error = new Signal<Error | undefined>(undefined);
	readonly broadcast: Moq.Broadcast.Consumer;
	readonly track: string;
	readonly config: DecoderConfig;
	readonly identity: string;
	readonly catalog: Getter<Catalog.Root | undefined>;

	// The picture chosen between the two decoders, and its timestamp once it presented on the clock:
	// undefined while it is only a poster.
	readonly frame = new Signal<VideoFrame | undefined>(undefined);
	readonly timestamp = new Signal<Milli | undefined>(undefined);

	readonly min: DecoderTrack;
	readonly max = new Signal<DecoderTrack | undefined>(undefined);

	// The lane picture last shown, its timestamp, and the first group of the min-delay timeline it was
	// shown on. Nothing below that timestamp shows again until the timeline restarts.
	#shown?: VideoFrame;
	#last?: Milli;
	#start?: number;

	#signals = new Effect();

	constructor(props: RenditionProps) {
		this.broadcast = props.broadcast;
		this.track = props.track;
		this.config = props.config;
		this.identity = props.identity;
		this.catalog = props.catalog;

		// A missing or zero `warmup` keeps one decoder. The budgets follow a changed value without
		// reopening the max-delay decoder.
		const warmup = new Signal(Milli.zero);
		const dual = new Signal(false);

		const predecessor = props.predecessor;
		this.min = new DecoderTrack({ ...props, refresh: dual, predecessor: predecessor?.consumer });

		this.#signals.run((effect) => {
			const config = effect.get(props.catalog)?.video?.renditions?.[props.track];
			warmup.set(Milli(config?.warmup ?? 0));
			dual.set(warmup.peek() > 0);
		});
		this.#signals.run((effect) => {
			if (!effect.get(dual)) return;
			const max = new DecoderTrack({
				...props,
				stats: undefined,
				warmup,
				refresh: dual,
				predecessor: predecessor?.max.peek()?.consumer ?? predecessor?.consumer,
			});
			effect.cleanup(() => max.close());
			effect.set(this.max, max);
		});

		// Either decoder failing fails the rendition, as an HEVC refresh frame sent as `key` does.
		this.#signals.run((effect) => {
			const max = effect.get(this.max);
			const failed = effect.get(this.min.error) ?? (max && effect.get(max.error));
			if (failed) this.error.set(failed);
		});

		this.#signals.run((effect) => {
			const max = effect.get(this.max);
			this.#select(
				{ frame: effect.get(this.min.frame), lane: this.min },
				max && { frame: effect.get(max.frame), lane: max },
				effect.get(this.min.start),
			);
		});
	}

	// The min-delay decoder's arrival estimate, which a replacement rendition continues.
	get consumer(): Container.Consumer | undefined {
		return this.min.consumer;
	}

	// Startup reads the clock-driving decoder's first picture and its buffered media.
	get first(): Getter<Milli | undefined> {
		return this.min.first;
	}

	get buffered(): Getter<Container.BufferedRanges> {
		return this.min.buffered;
	}

	get spread(): Getter<Milli | undefined> {
		return this.min.spread;
	}

	// Show the max-delay picture when it is at or past the min-delay one, so it wins ties, else the
	// min-delay picture. Only pictures read from `start` on, the min-delay decoder's timeline, are
	// candidates: one held from before a restart neither shows nor sets the floor. Not `caughtUp`: its
	// slack, applied per picture, would alternate backwards and forwards.
	#select(min: Lane, max: Lane | undefined, start: number | undefined): void {
		// A restarted timeline begins below the previous one's pictures. Like a decoder's own timestamp
		// on a break, the selection's clears until the new timeline presents, so a pending rendition is
		// not held to the old timeline's playhead.
		if (start !== this.#start) {
			this.#start = start;
			this.#last = undefined;
			this.timestamp.set(undefined);
		}
		const current = (lane: Lane | undefined): lane is Lane & { frame: VideoFrame } =>
			lane?.frame !== undefined && (start === undefined || (lane.lane.group ?? -1) >= start);
		// Even a clean max-delay picture is a sweep behind until the min-delay decoder has one.
		if (!current(min)) return;

		let pick = min;
		if (current(max) && max.frame.timestamp >= min.frame.timestamp) pick = max;
		if (pick.frame === this.#shown) return;

		const timestamp = Milli.fromMicro(pick.frame.timestamp as Micro);
		if (this.#last !== undefined && timestamp < this.#last) return;
		this.#shown = pick.frame;
		this.#last = timestamp;

		this.timestamp.set(pick.lane.timestamp.peek());
		this.frame.update((prev) => {
			prev?.close();
			return pick.frame.clone();
		});
	}

	close(): void {
		this.#signals.close();
		this.min.close();
		this.frame.update((prev) => {
			prev?.close();
			return undefined;
		});
	}
}

// One decoder's current picture, for the selector.
interface Lane {
	frame: VideoFrame | undefined;
	lane: DecoderTrack;
}

function isH264(codec: string): boolean {
	return codec.startsWith("avc1.") || codec.startsWith("avc3.");
}

// An SEI NAL unit (type 6) holding one recovery point message (payload type 6, one byte):
// recovery_frame_cnt = 0, exact_match_flag = 0, broken_link_flag = 0, changing_slice_group_idc = 0,
// then payload alignment and the RBSP trailing bits. It needs no emulation prevention.
const RECOVERY_SEI = new Uint8Array([0x06, 0x06, 0x01, 0x84, 0x80]);

/**
 * Insert an SEI recovery point with `recovery_frame_cnt = 0` before the first slice of an H.264
 * access unit that has no IDR, so a codec can open on it. `description` is the avcC record when the
 * payload is length-prefixed, and absent for Annex B. An IDR, a payload without a slice, or one that
 * does not parse comes back unchanged.
 *
 * Chromium's WebCodecs checks the chunk that opens a codec, after `configure()` or `reset()`: an
 * H.264 chunk sent as `key` must hold an IDR, or an SEI recovery point with `recovery_frame_cnt ==
 * 0` (`AnalyzeAnnexB` in `media/formats/mp4/avc.cc`). Otherwise `decode()` throws "A key frame is
 * required". An intra-refresh group opens on a recovery point that counts a whole sweep, so every
 * join and skip of a `warmup` rendition would fail. The extra message declares that one picture
 * recovered, which is what a `warmup` rendition already accepts: the min-delay decoder shows the
 * refresh stripes until the sweep completes, and the max-delay decoder covers them. The slices
 * are unchanged; the message goes right before the first one, after any SEI the stream carries.
 * @internal
 */
export function insertRecoveryPoint(payload: Uint8Array, description?: Uint8Array): Uint8Array {
	if (description) {
		// lengthSizeMinusOne, in the low two bits of the avcC record's fifth byte.
		if (description.byteLength < 5) return payload;
		const size = (description[4] & 0x03) + 1;
		for (let offset = 0; offset + size <= payload.byteLength; ) {
			let length = 0;
			for (let i = 0; i < size; i++) length = length * 256 + payload[offset + i];
			const start = offset + size;
			if (length === 0 || start + length > payload.byteLength) return payload;
			const type = payload[start] & 0x1f;
			if (type === 5) return payload;
			if (type >= 1 && type <= 4) {
				const prefix = new Uint8Array(size);
				for (let i = 0, n = RECOVERY_SEI.byteLength; i < size; i++, n = Math.floor(n / 256)) {
					prefix[size - 1 - i] = n % 256;
				}
				return splice(payload, offset, prefix, RECOVERY_SEI);
			}
			offset = start + length;
		}
		return payload;
	}

	// Annex B: each NAL unit follows a 00 00 01 start code, with an optional leading zero byte.
	for (let i = 0; i + 3 < payload.byteLength; i++) {
		if (payload[i] !== 0 || payload[i + 1] !== 0 || payload[i + 2] !== 1) continue;
		const type = payload[i + 3] & 0x1f;
		if (type === 5) return payload;
		if (type >= 1 && type <= 4) return splice(payload, i, new Uint8Array([0, 0, 1]), RECOVERY_SEI);
		i += 2;
	}
	return payload;
}

// `payload` with `parts` inserted at `at`.
function splice(payload: Uint8Array, at: number, ...parts: Uint8Array[]): Uint8Array {
	const length = parts.reduce((sum, part) => sum + part.byteLength, payload.byteLength);
	const out = new Uint8Array(length);
	out.set(payload.subarray(0, at), 0);
	let offset = at;
	for (const part of parts) {
		out.set(part, offset);
		offset += part.byteLength;
	}
	out.set(payload.subarray(at), offset);
	return out;
}

// Chrome configures a hardware H.264 decoder that then fails the first interlaced picture: field
// coding only shows in the SPS, and Chrome falls back to software only when configure() fails.
function hardwareAcceleration(config: { interlaced?: boolean }): HardwareAcceleration | undefined {
	return config.interlaced ? "prefer-software" : undefined;
}

/** Whether the WebCodecs video decoder can play this config. */
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
			const Cmaf = await import("@moq/hang/cmaf");
			description = Cmaf.decodeInitSegment(base64ToBytes(config.container.init)).description;
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
		hardwareAcceleration: hardwareAcceleration(config),
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
			hardwareAcceleration: hardwareAcceleration(config),
		});
		if (retry.supported) {
			config.codec = avc1;
			return true;
		}
	}

	return false;
}

Object.assign(supported, { [supportCacheKey]: decoderConfigKey });

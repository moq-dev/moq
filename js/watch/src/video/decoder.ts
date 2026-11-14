import { Duration, type Wall } from "@moq/clock";
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
import { arrivedAt, type Counter, type Key, type Received, Tally } from "../feedback";
import { type Failure, failureMoved, nextMedia, subscribeMedia } from "../media";
import { DELAY_MAX, type Sync, type Track } from "../sync";
import { type DecoderConfig, hardwareAcceleration, type PlaybackIdentity, playbackIdentity } from "./config";
import { caughtUp, renditionJitter, switchJitter } from "./playhead";
import { Recovery } from "./recovery";
import type { Source } from "./source";
import { accepted, supported } from "./supported";

// The amount of time to wait before considering the video to be buffering.
const BUFFERING = Duration.millis(500);

// The startup floor counts received media across holes, without counting the missing spans.
function startupSpan(ranges: Container.BufferedRanges, first: Duration): { start: Duration; end: Duration } {
	let duration = Duration.ZERO;
	for (const range of ranges) duration = duration.add(range.end.saturatingSub(range.start.max(first)));
	return { start: first, end: first.add(duration) };
}

// The rendition #runPending chose, or the failure that stopped its source until another is selected.
// A selected rendition is pending until it replaces the active one, and stays selected on screen.
type Selection = { kind: "none" } | { kind: "selected"; rendition: Rendition } | { kind: "failed"; failure: Failure };

// One run of the active rendition's playback. A poster may show while it starts, and only a picture
// that played and then froze is a stall.
// A picture held through the publisher's declared end is no stall either.
type Playback = "starting" | "playing" | "ended" | "stalled";

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

	/** Frames that arrived after their playout time. */
	late: number;

	/** Renditions that failed to decode. */
	errors: number;

	/** Times playback stalled: no new picture for half a second once one had played. */
	stalls: number;

	/** Total time spent in finished stalls. */
	stalled: Duration;

	/** The newest received media timestamp and the wall time it arrived. */
	arrival?: { timestamp: Duration; wall: Wall };
}

// The running totals before anything is counted.
const EMPTY: Stats = { frameCount: 0, bytesReceived: 0, late: 0, errors: 0, stalls: 0, stalled: Duration.ZERO };

/** Signals the decoder derives. */
export type DecoderOutput = {
	/** Fatal decode failure, cleared when another source is selected. */
	error: Signal<Error | undefined>;
	/**
	 * The current frame to render.
	 *
	 * @internal Feeds the renderer; a `VideoFrame` cannot leave the decoding thread.
	 */
	frame: Signal<VideoFrame | undefined>;

	/**
	 * Whether the current frame weaves two fields, per the rendition that decoded it.
	 *
	 * @internal Feeds the renderer.
	 */
	interlaced: Signal<boolean>;

	// The timestamp of the current frame.
	timestamp: Signal<Duration | undefined>;

	// The display size of the video in pixels, ideally sourced from the catalog.
	display: Signal<{ width: number; height: number } | undefined>;

	stalled: Signal<boolean>;

	// Whether the publisher finished the selected rendition and the clock reached its last picture.
	ended: Signal<boolean>;

	stats: Signal<Stats | undefined>;

	/**
	 * The rendition delay Sync must cover while switching tracks.
	 *
	 * @internal Feeds the decoder's playout target.
	 */
	jitter: Signal<Duration | undefined>;

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
	// Video's hold on the clock: its target, startup span, arrivals, and paced frames.
	readonly #clock: Track;

	readonly #out: DecoderOutput = {
		error: new Signal<Error | undefined>(undefined),
		frame: new Signal<VideoFrame | undefined>(undefined),
		interlaced: new Signal<boolean>(false),
		timestamp: new Signal<Duration | undefined>(undefined),
		display: new Signal<{ width: number; height: number } | undefined>(undefined),
		stalled: new Signal<boolean>(false),
		ended: new Signal<boolean>(false),
		stats: new Signal<Stats | undefined>(undefined),
		jitter: new Signal<Duration | undefined>(undefined),
		buffered: new Signal<Container.BufferedRanges>([]),
	};
	/** The derived outputs, read-only. */
	readonly out = readonlys(this.#out);

	// The rendition on screen, held so we can cancel it when the selected one is ready.
	#active = new Signal<Rendition | undefined>(undefined);
	#selection = new Signal<Selection>({ kind: "none" });
	// The active track's arrival estimate from its container consumer.
	#spread = new Signal<Duration | undefined>(undefined);
	// The path of the media the last rendition read, and its instance. Every handle to one instance
	// shares `closed`, so it identifies the instance where the handle itself does not.
	#instance?: { path: Moq.Path.Valid; closed: unknown };
	readonly #identity: Computed<PlaybackIdentity | undefined>;

	#signals = new Effect();

	// What each rendition delivered, for the feedback a viewer reports.
	#tally = new Tally();

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
			return spread.add(Duration.millis(delay ?? 0)).max(jitter ?? Duration.ZERO);
		});
		const startup = this.#signals.computed((effect) => {
			if (!effect.get(this.in.enabled)) return undefined;
			if (effect.get(this.source.pending)) return null;
			if (!effect.get(this.source.out.config)) return undefined;
			const active = effect.get(this.#active);
			if (!active) return null;
			const timestamp = effect.get(active.first);
			if (timestamp === undefined) return null;
			const ranges = effect.get(active.buffered);
			return startupSpan(ranges, timestamp);
		});
		this.#clock = this.sync.track({ target, startup });
		this.#signals.cleanup(() => this.#clock.close());
		this.#identity = this.#signals.computed((effect) => {
			const config = effect.get(this.source.out.config);
			return config ? playbackIdentity(config) : undefined;
		});

		this.#signals.run(this.#runJitter.bind(this));
		this.#signals.run(this.#runPending.bind(this));
		this.#signals.run(this.#runActive.bind(this));
		this.#signals.run(this.#runDisplay.bind(this));
		this.#signals.run(this.#runBuffering.bind(this));
		this.#signals.run(this.#runEnded.bind(this));
		this.#signals.run(this.#runStalls.bind(this));
	}

	/**
	 * What this decoder received and played of each rendition in the catalog of the broadcast at
	 * `path`, keyed by rendition ID, for the feedback a viewer reports.
	 * @internal
	 */
	feedback(path: Moq.Path.Valid): Record<string, Received> {
		return this.#tally.report(path);
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
		const pendingTrack = this.#pending(effect.get(this.#selection), activeTrack);
		const pending = floor(pendingTrack?.track, pendingTrack?.catalog);
		effect.set(this.#out.jitter, switchJitter({ active, pending }));
	}

	// The selected rendition still preparing to replace `active`.
	#pending(selection: Selection, active: Rendition | undefined): Rendition | undefined {
		return selection.kind === "selected" && selection.rendition !== active ? selection.rendition : undefined;
	}

	#runPending(effect: Effect): void {
		const enabled = effect.get(this.in.enabled);
		const broadcast = effect.get(this.source.in.broadcast);
		const track = effect.get(this.source.out.track);
		const identity = effect.get(this.#identity);
		const selection = this.#selection.peek();

		// Honor a per-rendition `broadcast` override: subscribe on the resolved source
		// broadcast instead of the catalog's own broadcast. A disabled decoder resolves it only to
		// tell whether a failure's source is still selected.
		const active: Moq.Broadcast.Consumer | undefined =
			broadcast && track && identity && (enabled || selection.kind === "failed")
				? broadcast.relativeBroadcast(effect, identity.broadcast, identity.instance)
				: undefined;
		const key = identity && JSON.stringify(identity);

		// A failure belongs to its source: selecting another clears it, even while disabled. Without a
		// latched failure, any error shown is an outgoing rendition's, which a selection change clears.
		if (selection.kind !== "failed") {
			this.#out.error.set(undefined);
		} else {
			const selected = broadcast ? effect.get(broadcast.out.active)?.closed : undefined;
			if (failureMoved(selection.failure, { broadcast: selected, media: active?.closed, track, identity: key })) {
				this.#selection.set({ kind: "none" });
				this.#out.error.set(undefined);
			}
		}
		// Nothing is selected without a source, but a failure stays latched.
		const deselect = () => {
			if (this.#selection.peek().kind === "selected") this.#selection.set({ kind: "none" });
		};

		if (!enabled || !broadcast || !track || !identity) {
			deselect();
			// Close the active track when disabled (e.g. paused or not visible).
			// The pending cleanup won't do this because it was already promoted to #active.
			this.#active.set(undefined);
			// Nothing arrives until it resumes, so its arrivals stop holding the clock.
			this.#clock.reset();
			// Pause and visibility keep the picture while the broadcast is active. Disconnecting
			// the broadcast or removing every rendition releases it.
			if (!broadcast || !effect.get(broadcast.out.active) || (enabled && !track)) {
				this.#clearCurrentFrame();
				this.#out.buffered.set([]);
			}
			return;
		}

		if (!active || !key) {
			deselect();
			// Going offline should clear the last rendered frame.
			this.#active.set(undefined);
			this.#clock.reset();
			this.#clearCurrentFrame();
			this.#out.buffered.set([]);
			return;
		}

		// This source already failed; wait for another.
		if (this.#selection.peek().kind === "failed") return;

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

		const rendition = new Rendition({
			sync: this.sync,
			clock: this.#clock,
			broadcast: active,
			track,
			name: Catalog.track(track, identity),
			config: identity.decoder,
			identity: key,
			stats: this.#out.stats,
			tally: this.#tally,
			key: { path: broadcast.in.name.peek(), id: track },
			catalog: broadcast.out.catalog,
			predecessor,
		});
		this.#selection.set({ kind: "selected", rendition });
		// Whether the rendition is still waiting to replace the active one: neither promoted nor failed.
		const pending = () => this.#pending(this.#selection.peek(), this.#active.peek()) === rendition;

		effect.run((inner) => {
			if (!pending()) {
				inner.close(); // Promoted tracks are observed by their active lifetime.
				return;
			}
			const error = inner.get(rendition.error);
			if (!error) return;
			this.#recordError(rendition, error);
			rendition.close();
			// With no active rendition left, nothing holds video's timeline.
			if (!this.#active.peek()) this.#clock.reset();
		});

		// Once promoted, #runActive closes it.
		effect.cleanup(() => {
			if (this.#active.peek() !== rendition) rendition.close();
		});

		effect.run((effect) => {
			if (!pending()) {
				effect.close();
				return;
			}

			const current = effect.get(this.#active);
			if (current && !republished) {
				// A zero timestamp is a rendered frame, not a missing one.
				const pendingTimestamp = effect.get(rendition.timestamp);
				if (pendingTimestamp === undefined) return;

				// Hold off until the new rendition has caught up to the picture it's replacing.
				if (!caughtUp({ playhead: pendingTimestamp, active: effect.get(current.timestamp) })) return;
			}

			// Promote the rendition; #runActive will be in charge of it now.
			this.#active.set(rendition);

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
			// A pending replacement carries video's timeline on; its parked frames must stay.
			if (!this.#pending(this.#selection.peek(), undefined)) this.#clock.reset();
			this.#clearCurrentFrame();
		});

		// Clone the frame so we own it independently of the DecoderTrack.
		// proxy() would share the same reference, allowing the source to close our frame.
		effect.run((inner) => {
			const frame = inner.get(active.frame);
			if (!frame) return; // A promoted track holds the previous picture until its first frame.
			if (active.error.peek()) return;
			// The selected rendition playing clears an outgoing one's failure.
			const selection = this.#selection.peek();
			if (selection.kind === "selected" && selection.rendition === active) this.#out.error.set(undefined);
			this.#out.timestamp.set(Duration.micros(frame.timestamp));
			this.#out.interlaced.set(active.config.interlaced);
			this.#out.frame.update((prev) => {
				prev?.close();
				return frame.clone();
			});
		});
		effect.proxy(this.#out.buffered, active.buffered);
		effect.proxy(this.#spread, active.spread);
	}

	// Only the selected rendition's failure stops its source. An outgoing one's shows until the
	// selected rendition plays, and never replaces the selected one's.
	#recordError(track: Rendition, error: Error): void {
		this.#tally.count(track.key, { errors: 1 });
		this.#out.stats.update((stats) => ({ ...(stats ?? EMPTY), errors: (stats?.errors ?? 0) + 1 }));
		const selection = this.#selection.peek();
		if (selection.kind === "failed") return;
		if (selection.kind === "selected" && selection.rendition === track) {
			const failure: Failure = {
				broadcast: this.source.in.broadcast.peek()?.out.active.peek()?.closed,
				media: track.broadcast.closed,
				track: track.track,
				identity: track.identity,
			};
			this.#selection.set({ kind: "failed", failure });
		}
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
		// A disabled decoder is not waiting for anything.
		if (!effect.get(this.in.enabled)) {
			this.#out.stalled.set(false);
			return;
		}

		// After the publisher's declared end no picture is coming, whether one shows or not.
		const active = effect.get(this.#active);
		if (active && effect.get(active.ended) !== undefined) {
			this.#out.stalled.set(false);
			return;
		}

		const frame = effect.get(this.#out.frame);
		if (!frame) {
			this.#out.stalled.set(true);
			return;
		}

		this.#out.stalled.set(false);
		effect.timer(() => {
			this.#out.stalled.set(true);
		}, BUFFERING);
	}

	#runEnded(effect: Effect): void {
		const active = effect.get(this.#active);
		effect.set(this.#out.ended, active !== undefined && effect.get(active.finished), false);
	}

	// Count each stall, and how long it lasted, against the rendition on screen. Each active rendition
	// starts a run, and disabling the decoder clears it: the wait for its first picture on the clock
	// is a startup even while a poster shows, and so is a restarted timeline's.
	#runStalls(effect: Effect): void {
		const active = effect.get(this.#active);
		if (!active) return;

		const playback = new Signal<Playback>("starting");
		effect.run((inner) => {
			// Undefined while the picture is only a poster, as it is again once the timeline restarts.
			if (inner.get(active.timestamp) === undefined) {
				playback.set("starting");
				return;
			}
			if (inner.get(active.ended) !== undefined) {
				playback.set("ended");
				return;
			}
			playback.set("playing");
			inner.timer(() => playback.set("stalled"), BUFFERING);
		});
		effect.run((inner) => {
			if (inner.get(playback) !== "stalled") return;
			const end = this.#tally.stall(active.key);
			this.#out.stats.update((stats) => ({ ...(stats ?? EMPTY), stalls: (stats?.stalls ?? 0) + 1 }));
			inner.cleanup(() => {
				const duration = end();
				this.#out.stats.update((stats) => ({
					...(stats ?? EMPTY),
					stalled: (stats?.stalled ?? Duration.ZERO).add(Duration.millis(duration)),
				}));
			});
		});
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
	// Video's hold on the clock, shared by every rendition: their frames are one timeline.
	clock: Track;
	// Set for the max-delay decoder of a rendition with a catalog `warmup`: its start and its
	// lateness budget reach this much further, and it leaves the shared clock and stats alone.
	warmup?: Getter<Duration>;
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
	// The rendition's feedback counters, under `key`: its ID in the catalog it was selected from.
	// Absent for the max-delay decoder, like `stats`.
	tally?: Tally;
	key: Key;
}

class DecoderTrack {
	error = new Signal<Error | undefined>(undefined);
	sync: Sync;
	clock: Track;
	broadcast: Moq.Broadcast.Consumer;
	track: string;
	name: string;
	config: DecoderConfig;
	identity: string;
	catalog: Getter<Catalog.Root | undefined>;
	stats?: Signal<Stats | undefined>;
	tally?: Tally;
	key: Key;
	timestamp = new Signal<Duration | undefined>(undefined);
	frame = new Signal<VideoFrame | undefined>(undefined);

	// The group the current `frame` was read from. Group sequences are shared by every subscription to
	// the track, so they place a picture on a timeline for either decoder.
	group?: bigint;
	// The first group of the timeline this decoder reads, undefined until a publisher restart.
	readonly start = new Signal<bigint | undefined>(undefined);

	// Counts the timeline restarts this decoder read. A picture decoded before one is from the old
	// timeline and never presents after it, even when its clock wait already resolved.
	#generation = 0;
	// The group each frame in the codec was read from, by timestamp. Each output found here retires its
	// entry and every one before it. A publisher that never outputs a picture grows this by one entry
	// per chunk; the maintainer accepted that as an exception to the bounded-state rule
	// (moq-dev/moq#7555, 2026-11-01), since WebCodecs bounds no codec's pending output, so any cap is
	// a guess that can evict a picture the codec still owes.
	#groups = new Map<number, bigint>();

	// How far past the shared clock's budgets this decoder reaches.
	#warmup: Getter<Duration>;
	// The playout delay is where a cold join starts, and the shared max delay how late it may fall.
	// The max-delay decoder adds the warmup to both, so it starts at the group before and keeps
	// it, as Rust's does, rather than passing the consumer a `warmup` that would reach further.
	// Its subscription and its consumer take the same delay.
	#delay: Getter<Moq.Track.Delay>;
	// Whether this decoder drives the shared clock: only the min-delay one reports arrivals and
	// restarts the timeline.
	#drives: boolean;
	#refresh: Getter<boolean>;

	// Network jitter + decode buffer.
	buffered = new Signal<Container.BufferedRanges>([]);
	// First decoded picture on this timeline, independent of the previously held poster.
	readonly first = new Signal<Duration | undefined>(undefined);

	// How late frames arrive relative to the earliest one, from the container consumer.
	spread = new Signal<Duration | undefined>(undefined);

	// Decoded frames waiting to be rendered.
	#buffered = new Signal<Container.BufferedRanges>([]);

	// The codec string WebCodecs accepted for this track, resolved before its first configure.
	#codec?: string;
	// Codec description from the last configure, reused after a playhead reset.
	#description?: Uint8Array;
	// Whether the next chunk opens the codec, after a configure or a reset.
	#opening = false;

	// A timeline restart makes the next decoded frame decide whether the codec resets.
	#resetPending = false;
	// A hole was read, so the next decoded frame resets the codec only if it rewinds.
	#holed = false;
	// The newest timestamp fed to the codec since it last opened.
	#submitted?: Duration;
	// The group the publisher declared the media ends with. It stays in force through holes until a
	// later group starts a new timeline.
	readonly ended = new Signal<bigint | undefined>(undefined);
	// Whether the publisher finished the track and the clock reached the last picture fed to the codec.
	readonly finished = new Signal<boolean>(false);

	// The newest consumer reading this track, whose estimate a replacement continues. The predecessor
	// is only held until the first one opens.
	consumer?: Container.Consumer;
	#predecessor?: Container.Consumer;

	#signals = new Effect();

	constructor(props: DecoderTrackProps) {
		this.sync = props.sync;
		this.clock = props.clock;
		this.broadcast = props.broadcast;
		this.track = props.track;
		this.name = props.name;
		this.config = props.config;
		this.identity = props.identity;
		this.catalog = props.catalog;
		this.stats = props.stats;
		this.tally = props.tally;
		this.key = props.key;
		this.#warmup = props.warmup ?? getter(Duration.ZERO);
		this.#delay = new Derived([this.sync.out.delay, this.sync.out.maxDelay, this.#warmup], (min, max, warmup) => ({
			min: min.add(warmup),
			max: max.add(warmup),
		}));
		this.#drives = props.warmup === undefined;
		this.#refresh = props.refresh ?? getter(false);
		this.#predecessor = props.predecessor;

		this.#signals.run(this.#run.bind(this));
	}

	// Count into this rendition's feedback and the decoder's running totals.
	#count(counts: Partial<Record<Counter, number>>, totals?: (stats: Stats) => Partial<Stats>): void {
		this.tally?.count(this.key, counts);
		if (totals) this.stats?.update((current) => ({ ...(current ?? EMPTY), ...totals(current ?? EMPTY) }));
	}

	// Count a frame read from the track: its bytes even when rejected, and whether an accepted one
	// arrived too late for playout. Classifies ingress before decoder backpressure changes the
	// delivery clock. Only the min-delay decoder moves the clock and counts: the other reads the same
	// frames behind it.
	#receive(frame: Container.ReceivedFrame, accepted: boolean): void {
		if (!this.#drives) return;
		const timestamp = frame.timestamp as Duration;
		const late = accepted && this.clock.received(timestamp, frame.arrival);
		const bytes = frame.payload.byteLength;
		const arrived = arrivedAt(frame.arrival);
		this.tally?.arrive(this.key, timestamp, arrived);
		this.#count({ frames: 1, bytes, late: late ? 1 : 0 }, (stats) => ({
			frameCount: stats.frameCount + (accepted ? 1 : 0),
			bytesReceived: stats.bytesReceived + bytes,
			late: stats.late + (late ? 1 : 0),
			arrival: { timestamp, wall: arrived },
		}));
	}

	// Reorders groups and frames up to the shared max delay. A rendition switch is not a cold start:
	// the path's arrivals are what they were, so the estimate continues from whatever read before.
	#consume(effect: Effect, sub: Moq.Track.Subscriber, format: Container.Format): Container.Consumer {
		const consumer = new Container.Consumer(sub, {
			format,
			delay: this.#delay,
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
			// js/net merges both decoders' subscriptions into one request at the smaller stated min delay
			// and the larger max delay.
			delay: this.#delay,
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
					this.#count({ decoded: 1 });
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
					const timestamp = Duration.micros(frame.timestamp);
					if (this.#late(timestamp)) {
						// Late frame, don't render it.
						return;
					}

					if (this.first.peek() === undefined) this.first.set(timestamp);

					// Render something while we wait for the sync to catch up, unless the frame is
					// already off the timeline: WebCodecs can hold a picture past a reset or a skip.
					// Only the min-delay decoder posts one: the max-delay picture shows on the clock.
					if (this.#drives && this.frame.peek() === undefined && this.clock.presentable(timestamp)) {
						this.group = group;
						this.frame.set(frame.clone());
					}

					// Resolves at once when the delay is "instant". Closing the track releases it.
					// Check the captured signal again in case cancellation followed the resolved wait.
					if (!(await this.clock.wait(timestamp, signal)) || signal.aborted) return;
					// The max-delay decoder restarts its timeline after the clock did, so the clock's own
					// epoch cannot tell its leftover pictures apart.
					if (this.#generation !== generation) return;

					if (this.#late(timestamp)) {
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
			else await this.#runLegacy(effect, sub, decoder);
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

	async #runLegacy(effect: Effect, sub: Moq.Track.Subscriber, decoder: VideoDecoder): Promise<void> {
		if (!(await this.#accept(effect))) return;
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

		let previous: Duration | undefined;
		let latest: bigint | undefined;
		const recovery = new Recovery();

		this.#spawn(effect, async () => {
			for (;;) {
				const next = await nextMedia(consumer, effect.abort);
				if (!next) break;
				if (next.kind === "finish") {
					await this.#finish(effect, decoder, latest);
					break;
				}
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
				this.#receive(frame, accepted);
				if (!accepted) continue;

				// Track decode buffer: frames sent to decoder but not yet rendered. Only bridge from
				// the previous frame when no gap or end came between them. Group ids can't answer that:
				// they aren't required to be sequential (some encoders derive them from DTS), so
				// adjacency neither proves continuity nor rules out a gap the consumer skipped, and
				// reporting a skipped span as decoded overstates the buffer.
				if (previous !== undefined) {
					this.#addBuffered(previous, frame.timestamp);
				}

				previous = frame.timestamp;

				latest = next.group;
				this.#resetAtKeyframe(decoder, frame);
				this.#groups.set(frame.timestamp.asMicros(), next.group);
				this.#submit(decoder, frame);
				if (!this.sync.playing.peek()) await this.#waitStartup(effect, frame.timestamp);
			}
		});
	}

	async #runCmaf(effect: Effect, sub: Moq.Track.Subscriber, decoder: VideoDecoder): Promise<void> {
		const container = this.config.container;
		if (container.kind !== "cmaf") return;

		const abort = effect.abort;
		const Cmaf = await import("@moq/hang/cmaf");
		if (abort.aborted) return;
		if (!(await this.#accept(effect))) return;

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

		let previous: Duration | undefined;
		let latest: bigint | undefined;
		const recovery = new Recovery();

		this.#spawn(effect, async () => {
			for (;;) {
				const next = await nextMedia(consumer, effect.abort);
				if (!next) break;
				if (next.kind === "finish") {
					await this.#finish(effect, decoder, latest);
					break;
				}
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
				this.#receive(frame, accepted);
				if (!accepted) continue;

				// Track decode buffer (see #runLegacy: bridge only when no gap or end came between,
				// never on group adjacency, which proves nothing about the timeline).
				if (previous !== undefined) {
					this.#addBuffered(previous, frame.timestamp);
				}

				previous = frame.timestamp;

				if (decoder.state === "closed") break;
				latest = next.group;
				this.#resetAtKeyframe(decoder, frame);
				this.#groups.set(frame.timestamp.asMicros(), next.group);
				this.#submit(decoder, frame);
				if (!this.sync.playing.peek()) await this.#waitStartup(effect, frame.timestamp);
			}
		});
	}

	// Decoded pictures park until startup releases, so leave further media encoded upstream once this
	// track covers its floor, or once `timestamp` sits the max delay past the opening picture: the
	// most any held frame may lead the playhead, which startup anchors there. Holes keep coverage
	// short without bound, so only the second limit bounds the parked pictures.
	async #waitStartup(effect: Effect, timestamp: Duration): Promise<void> {
		// Some codecs need more input before their first output. Do not park that input
		// behind a gate that needs the very picture the decoder has not produced yet.
		const first = this.first.peek();
		if (first === undefined) return;
		const filled = (get: <T>(signal: Getter<T>) => T) => {
			const span = startupSpan(get(this.#buffered), first);
			return (
				span.end.saturatingSub(span.start).ge(get(this.sync.out.delay)) ||
				timestamp.saturatingSub(first).ge(get(this.sync.out.maxDelay))
			);
		};
		if (!filled((signal) => signal.peek())) return;
		const waiting = new Effect();
		const refill = new Once<true>();
		waiting.run((inner) => {
			// An adaptive floor can rise while another track fills. Resume decoding to reach it,
			// rather than waiting on a clock whose startup now needs more video.
			if (refill.peek() === undefined && !filled((signal) => inner.get(signal))) refill.set(true);
		});
		try {
			// Wait on the first picture, which is due the moment startup releases, so decoding
			// resumes with the whole floor still ahead instead of once it has played out.
			await effect.race(refill, this.clock.wait(first, waiting.abort));
		} finally {
			waiting.close();
		}
	}

	// A container gap keeps the clock, and the codec unless the media resumes at or below a picture
	// already fed to it (see #resetAtKeyframe): resetting at every gap would visibly stall each group
	// whose tail a zero max delay gave up on. Only a declared end followed by a later group establishes
	// another timeline; a marker from older history cannot end live media.
	#onRead(read: Container.Read, latest: bigint | undefined): void {
		if (read.kind === "gap") {
			if (latest !== undefined) this.#holed = true;
		} else if (read.kind === "end") {
			if (latest === undefined || read.group >= latest) this.ended.set(read.group);
		} else {
			const ended = this.ended.peek();
			if (ended === undefined || read.group <= ended) return;
			this.ended.set(undefined);
			this.#onBreak(read.group);
		}
	}

	// The publisher finished the track, which ends it like a declared end. Drain the pictures the codec
	// still holds, and mark it finished once the clock reaches the last one.
	async #finish(effect: Effect, decoder: VideoDecoder, latest: bigint | undefined): Promise<void> {
		if (this.ended.peek() === undefined) this.ended.set(latest ?? 0n);
		if (decoder.state === "configured") await decoder.flush();
		const last = this.#submitted;
		if (last !== undefined) await this.clock.wait(last, effect.abort);
		if (!effect.abort.aborted) this.finished.set(true);
	}

	// A fresh timeline invalidates parked pictures and its previous decoded span. The held picture
	// stays until the new keyframe renders; the codec resets before that keyframe is decoded.
	#onBreak(group: bigint): void {
		this.first.set(undefined);
		this.timestamp.set(undefined);
		this.#buffered.set([]);
		this.#generation++;
		this.start.set(group);
		// The min-delay decoder restarts video's timeline, leaving audio's. The max-delay one reads the
		// same end later.
		if (this.#drives) this.clock.reset();
		this.#resetPending = true;
	}

	// Re-prime at a keyframe after a restart, or a hole that resumes at or below a picture already fed
	// to the codec, dropping pictures still queued there: one could sit above the new keyframe and
	// late-reject it. A delta may continue a group that a max-delay hole interrupted and still need its
	// references; resetting before it would make WebCodecs require a key chunk. An output callback that
	// already holds a frame stays parked on Sync; only a new timeline invalidates its clock epoch.
	#resetAtKeyframe(decoder: VideoDecoder, frame: Container.ReceivedFrame): void {
		const rewound = this.#holed && this.#submitted !== undefined && frame.timestamp.le(this.#submitted);
		this.#holed = false;
		if (!this.#resetPending && !rewound) return;
		this.#resetPending = false;
		// A closed decoder throws on reset.
		if (!frame.keyframe || decoder.state === "closed") return;
		decoder.reset();
		this.#groups.clear();
		this.#configure(decoder);
	}

	// Feed `frame` to the codec, noting the newest timestamp it holds.
	#submit(decoder: VideoDecoder, frame: Container.ReceivedFrame): void {
		this.#submitted = this.#submitted?.max(frame.timestamp) ?? frame.timestamp;
		decoder.decode(this.#chunk(frame));
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
		return new EncodedVideoChunk({
			type: frame.keyframe ? "key" : "delta",
			data,
			timestamp: frame.timestamp.asMicros(),
		});
	}

	// Whether a picture at `timestamp` is behind the one already rendered.
	#late(timestamp: Duration): boolean {
		const rendered = this.timestamp.peek();
		return rendered !== undefined && timestamp.lt(rendered);
	}

	// Resolve the codec string WebCodecs accepts once per track, from the same probe as `supported`, so a
	// custom `supported` that skipped the probe still opens an `avc3.*` rendition on Safari. False when
	// torn down meanwhile.
	async #accept(effect: Effect): Promise<boolean> {
		if (this.#codec !== undefined) return true;
		const codec = await accepted(this.config);
		if (effect.abort.aborted) return false;
		if (codec === undefined) throw new Error(`video: WebCodecs does not support codec ${this.config.codec}`);
		this.#codec = codec;
		return true;
	}

	#configure(decoder: VideoDecoder): void {
		if (this.#codec === undefined) throw new Error("video: configured before the codec was accepted");
		this.#opening = true;
		this.#submitted = undefined;
		decoder.configure({
			codec: this.#codec,
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
	#addBuffered(start: Duration, end: Duration): void {
		if (start.gt(end)) return;

		this.#buffered.mutate((current) => {
			for (const range of current) {
				// Check if there's any overlap, then merge
				if (range.start.le(end) && range.end.ge(start)) {
					range.start = range.start.min(start);
					range.end = range.end.max(end);
					return;
				}
			}

			current.push({ start, end });
			current.sort((a, b) => a.start.cmp(b.start));
		});
	}

	// Trim the decode buffer up to the rendered timestamp
	#trimBuffered(timestamp: Duration): void {
		this.#buffered.mutate((current) => {
			while (current.length > 0) {
				if (current[0].end.ge(timestamp)) {
					current[0].start = current[0].start.max(timestamp);
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
interface RenditionProps extends Omit<DecoderTrackProps, "warmup" | "refresh" | "predecessor" | "stats" | "tally"> {
	stats: Signal<Stats | undefined>;
	tally: Tally;
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
	// The rendition's feedback counters' key: its ID in the catalog it was selected from.
	readonly key: Key;

	// The picture chosen between the two decoders, and its timestamp once it presented on the clock:
	// undefined while it is only a poster.
	readonly frame = new Signal<VideoFrame | undefined>(undefined);
	readonly timestamp = new Signal<Duration | undefined>(undefined);

	readonly min: DecoderTrack;
	readonly max = new Signal<DecoderTrack | undefined>(undefined);

	// The lane picture last shown, its timestamp, and the first group of the min-delay timeline it was
	// shown on. Nothing below that timestamp shows again until the timeline restarts.
	#shown?: VideoFrame;
	#last?: Duration;
	#start?: bigint;

	#signals = new Effect();

	constructor(props: RenditionProps) {
		this.broadcast = props.broadcast;
		this.track = props.track;
		this.config = props.config;
		this.identity = props.identity;
		this.catalog = props.catalog;
		this.key = props.key;

		// A missing or zero `warmup` keeps one decoder. The budgets follow a changed value without
		// reopening the max-delay decoder.
		const warmup = new Signal(Duration.ZERO);
		const dual = new Signal(false);

		const predecessor = props.predecessor;
		this.min = new DecoderTrack({ ...props, refresh: dual, predecessor: predecessor?.consumer });

		this.#signals.run((effect) => {
			const config = effect.get(props.catalog)?.video?.renditions?.[props.track];
			// Capped like the delay, so a catalog `warmup` cannot hold the viewer without bound.
			warmup.set(Duration.millis(config?.warmup ?? 0).min(DELAY_MAX));
			dual.set(!warmup.peek().isZero());
		});
		this.#signals.run((effect) => {
			if (!effect.get(dual)) return;
			const max = new DecoderTrack({
				...props,
				stats: undefined,
				tally: undefined,
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
	get first(): Getter<Duration | undefined> {
		return this.min.first;
	}

	get buffered(): Getter<Container.BufferedRanges> {
		return this.min.buffered;
	}

	get spread(): Getter<Duration | undefined> {
		return this.min.spread;
	}

	// The group the publisher declared the clock-driving decoder's media ends with.
	get ended(): Getter<bigint | undefined> {
		return this.min.ended;
	}

	// Whether the publisher finished the clock-driving decoder's track and its last picture is due.
	get finished(): Getter<boolean> {
		return this.min.finished;
	}

	// Show the max-delay picture when it is at or past the min-delay one, so it wins ties, else the
	// min-delay picture. Only pictures read from `start` on, the min-delay decoder's timeline, are
	// candidates: one held from before a restart neither shows nor sets the floor. Not `caughtUp`: its
	// slack, applied per picture, would alternate backwards and forwards.
	#select(min: Lane, max: Lane | undefined, start: bigint | undefined): void {
		// A restarted timeline begins below the previous one's pictures. Like a decoder's own timestamp
		// on a break, the selection's clears until the new timeline presents, so a pending rendition is
		// not held to the old timeline's playhead.
		if (start !== this.#start) {
			this.#start = start;
			this.#last = undefined;
			this.timestamp.set(undefined);
		}
		const current = (lane: Lane | undefined): lane is Lane & { frame: VideoFrame } =>
			lane?.frame !== undefined && (start === undefined || (lane.lane.group ?? -1n) >= start);
		// Even a clean max-delay picture is a sweep behind until the min-delay decoder has one.
		if (!current(min)) return;

		let pick = min;
		if (current(max) && max.frame.timestamp >= min.frame.timestamp) pick = max;
		if (pick.frame === this.#shown) return;

		const timestamp = Duration.micros(pick.frame.timestamp);
		if (this.#last?.gt(timestamp)) return;
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

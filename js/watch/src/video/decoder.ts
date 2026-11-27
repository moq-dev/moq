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
import { rotateDimensions } from "@moq/video";
import { base64ToBytes } from "../base64";
import { arrivedAt, type Counter, type Key, type Received, Tally } from "../feedback";
import { type Failure, failureMoved, nextMedia, subscribeMedia } from "../media";
import type { Sync, Track } from "../sync";
import { type DecoderConfig, hardwareAcceleration, type PlaybackIdentity, playbackIdentity } from "./config";
import { caughtUp, renditionJitter, switchJitter } from "./playhead";
import { Recovery } from "./recovery";
import type { Source } from "./source";
import { accepted } from "./supported";

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

// A finished track: every frame read, then its last picture due on the clock.
type Finish = "read" | "played";

/** Signals the decoder reads. */
export type DecoderInput = {
	/** Whether to download the video track. Defaults to true; the parent may wire it from the renderer's output. */
	enabled: Getter<boolean>;
	/**
	 * Delivery priority of the video track against the connection's other subscriptions, in
	 * `-128..=127` with higher sent first. Defaults to `Catalog.PRIORITY.video`.
	 */
	priority: Getter<number>;
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
	// The path of the media the last rendition read, and its identity. Every handle to one identity
	// shares `closed`, so it identifies the identity where the handle itself does not.
	#stream?: { path: Moq.Path.Valid; closed: unknown };
	// The consumer the last released rendition read, and its path, so a rendition resumed after a pause
	// or a hidden tab continues its arrival estimate: the viewer's stop says nothing about the path.
	#retired?: { path: Moq.Path.Valid; consumer: Container.Consumer };
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
			priority: getter(props?.priority ?? Catalog.PRIORITY.video),
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
			// A track the publisher finished grows no further, so its span is ready however short, and one
			// that finished without a picture has nothing to start.
			const finish = effect.get(active.finish);
			const timestamp = effect.get(active.first);
			if (timestamp === undefined) return finish ? undefined : null;
			const ranges = effect.get(active.buffered);
			const complete = finish !== undefined || effect.get(active.complete);
			return { ...startupSpan(ranges, timestamp), complete };
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
		const playback = effect.get(this.#identity);
		const selection = this.#selection.peek();

		// Honor a per-rendition `broadcast` override: subscribe on the resolved source
		// broadcast instead of the catalog's own broadcast. A disabled decoder resolves it only to
		// tell whether a failure's source is still selected.
		const active: Moq.Broadcast.Consumer | undefined =
			broadcast && track && playback && (enabled || selection.kind === "failed")
				? broadcast.relativeBroadcast(effect, playback.broadcast, playback.identity)
				: undefined;
		const key = playback && JSON.stringify(playback);

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

		if (!enabled || !broadcast || !track || !playback) {
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
		// `broadcast` override names) is another identity, whose timeline starts over. Waiting for
		// it to catch up to the old picture would freeze video until it passed the old timestamps,
		// so switch at once and re-anchor the clock. A rendition switch within one identity, or a
		// switch to another path, still waits.
		const republished = this.#stream?.path === active.path && this.#stream.closed !== active.closed;
		this.#stream = { path: active.path, closed: active.closed };
		if (republished) this.sync.reset();

		// Only a rendition read from the same path continues the outgoing arrival estimate, or the one
		// released last when none is active: another path's arrivals have nothing to do with what this
		// one learned.
		const outgoing = this.#active.peek();
		const previous = outgoing ? { path: outgoing.broadcast.path, consumer: outgoing.consumer } : this.#retired;
		const predecessor = previous?.path === active.path ? previous.consumer : undefined;

		const rendition = new Rendition({
			sync: this.sync,
			clock: this.#clock,
			broadcast: active,
			track,
			name: Catalog.track(track, playback),
			config: playback.decoder,
			identity: key,
			stats: this.#out.stats,
			tally: this.#tally,
			key: { path: broadcast.in.name.peek(), id: track },
			catalog: broadcast.out.catalog,
			priority: this.in.priority,
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

		effect.cleanup(() => {
			if (active.consumer) this.#retired = { path: active.broadcast.path, consumer: active.consumer };
			active.close();
		});

		effect.run((inner) => {
			const error = inner.get(active.error);
			if (!error) return;
			this.#recordError(active, error);
			this.#active.set(undefined);
			// A pending replacement carries video's timeline on; its parked frames must stay.
			if (!this.#pending(this.#selection.peek(), undefined)) this.#clock.reset();
			this.#clearCurrentFrame();
		});

		// Clone the frame so we own it independently of the Rendition.
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
			rotateDimensions({ width: frame.displayWidth, height: frame.displayHeight }, catalog.rotation),
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
		effect.set(this.#out.ended, active !== undefined && effect.get(active.finish) === "played", false);
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
}

interface RenditionProps {
	sync: Sync;
	// Video's hold on the clock, shared by every rendition: their frames are one timeline.
	clock: Track;
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

	stats: Signal<Stats | undefined>;
	// The rendition's feedback counters, under `key`: its ID in the catalog it was selected from.
	tally: Tally;
	key: Key;
	// The subscription's priority.
	priority: Getter<number>;
}

/** One rendition's playback through one WebCodecs decoder. */
class Rendition {
	error = new Signal<Error | undefined>(undefined);
	sync: Sync;
	clock: Track;
	broadcast: Moq.Broadcast.Consumer;
	track: string;
	name: string;
	config: DecoderConfig;
	identity: string;
	catalog: Getter<Catalog.Root | undefined>;
	stats: Signal<Stats | undefined>;
	tally: Tally;
	key: Key;
	timestamp = new Signal<Duration | undefined>(undefined);
	frame = new Signal<VideoFrame | undefined>(undefined);

	// The playout delay is where a cold join starts, and the shared max delay how late it may fall.
	// Its subscription and its consumer take the same delay.
	#delay: Getter<Container.Delay>;
	#priority: Getter<number>;

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

	// A timeline restart makes the next decoded frame decide whether the codec resets.
	#resetPending = false;
	// Counts timeline restarts. A picture whose clock wait resolved before one, but whose callback
	// resumes after it, belongs to the old timeline and must neither show nor stamp `timestamp`.
	#generation = 0;
	// A hole was read, so the next decoded frame resets the codec only if it rewinds.
	#holed = false;
	// The newest timestamp fed to the codec since it last opened.
	#submitted?: Duration;
	// The group the publisher declared the media ends with. It stays in force through holes until a
	// later group starts a new timeline.
	readonly ended = new Signal<bigint | undefined>(undefined);
	// The publisher finishing the track: "read" once every frame is fed to the codec, then "played"
	// once the clock reached the last one.
	readonly finish = new Signal<Finish | undefined>(undefined);
	// Whether the running subscription's media all arrived, its track finished. The read loop reaches
	// the finish only past #waitStartup, which parks once the max delay is buffered, short of the floor
	// when holes cut its coverage.
	readonly complete = new Signal(false);

	// The newest consumer reading this track, whose estimate a replacement continues. The predecessor
	// is only held until the first one opens.
	consumer?: Container.Consumer;
	#predecessor?: Container.Consumer;

	#signals = new Effect();

	constructor(props: RenditionProps) {
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
		this.#delay = new Derived([this.sync.out.delay, this.sync.out.maxDelay], (min, max) => ({ min, max }));
		this.#priority = props.priority;
		this.#predecessor = props.predecessor;

		this.#signals.run(this.#run.bind(this));
	}

	// Count into this rendition's feedback and the decoder's running totals.
	#count(counts: Partial<Record<Counter, number>>, totals?: (stats: Stats) => Partial<Stats>): void {
		this.tally.count(this.key, counts);
		if (totals) this.stats.update((current) => ({ ...(current ?? EMPTY), ...totals(current ?? EMPTY) }));
	}

	// Count a frame read from the track: its bytes even when rejected, and whether an accepted one
	// arrived too late for playout. Classifies ingress before decoder backpressure changes the
	// delivery clock.
	#receive(frame: Container.ReceivedFrame, accepted: boolean): void {
		const timestamp = frame.timestamp as Duration;
		const late = accepted && this.clock.received(timestamp, frame.arrival);
		const bytes = frame.payload.byteLength;
		const arrived = arrivedAt(frame.arrival);
		this.tally.arrive(this.key, timestamp, arrived);
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
		effect.run((inner) => {
			if (inner.get(consumer.finished)) this.complete.set(true);
		});
		effect.cleanup(() => this.complete.set(false));
		return consumer;
	}

	#run(effect: Effect): void {
		const sub = subscribeMedia(effect, {
			broadcast: this.broadcast,
			track: this.name,
			priority: this.#priority,
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
					const timestamp = Duration.micros(frame.timestamp);
					const generation = this.#generation;
					if (this.#late(timestamp)) {
						// Late frame, don't render it.
						return;
					}

					if (this.first.peek() === undefined) this.first.set(timestamp);

					// Render something while we wait for the sync to catch up, unless the frame is
					// already off the timeline: WebCodecs can hold a picture past a reset or a skip.
					if (this.frame.peek() === undefined && this.clock.presentable(timestamp)) {
						this.frame.set(frame.clone());
					}

					// Resolves at once when the delay is "instant". Closing the track releases it.
					// Check the captured signal again in case cancellation followed the resolved wait.
					if (!(await this.clock.wait(timestamp, signal)) || signal.aborted) return;
					// The wait checks the clock's epoch, but a break can still land between its resolution
					// and this continuation, clearing the state #late reads below.
					if (this.#generation !== generation) return;

					if (this.#late(timestamp)) {
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
				: this.config.container.kind === "raw"
					? new Container.Raw.Format(this.config)
					: this.config.container.kind === "cts"
						? new Container.Cts.Format(this.config)
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
					await this.#onFinish(effect, decoder, latest);
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
				// starts only grow within a subscription, so neither a hole nor an endpoint
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
					await this.#onFinish(effect, decoder, latest);
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
				// starts only grow within a subscription, so neither a hole nor an endpoint
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
			this.#onBreak();
		}
	}

	// The publisher finished the track, which ends it like a declared end. Drain the pictures the codec
	// still holds, and mark it finished once the clock reaches the last one.
	async #onFinish(effect: Effect, decoder: VideoDecoder, latest: bigint | undefined): Promise<void> {
		if (effect.abort.aborted) return;
		if (this.ended.peek() === undefined) this.ended.set(latest ?? 0n);
		this.finish.set("read");
		if (decoder.state === "configured") await decoder.flush();
		const last = this.#submitted;
		if (last !== undefined) await this.clock.wait(last, effect.abort);
		if (!effect.abort.aborted) this.finish.set("played");
	}

	// A fresh timeline invalidates parked pictures and its previous decoded span. The held picture
	// stays until the new keyframe renders; the codec resets before that keyframe is decoded.
	#onBreak(): void {
		this.#generation++;
		this.first.set(undefined);
		this.timestamp.set(undefined);
		this.#buffered.set([]);
		// Restarts video's timeline, leaving audio's.
		this.clock.reset();
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
		this.#configure(decoder);
	}

	// Feed `frame` to the codec, noting the newest timestamp it holds.
	#submit(decoder: VideoDecoder, frame: Container.ReceivedFrame): void {
		this.#submitted = this.#submitted?.max(frame.timestamp) ?? frame.timestamp;
		decoder.decode(
			new EncodedVideoChunk({
				type: frame.keyframe ? "key" : "delta",
				data: frame.payload,
				timestamp: frame.timestamp.asMicros(),
			}),
		);
	}

	// Whether a picture at `timestamp` is behind the one already rendered.
	#late(timestamp: Duration): boolean {
		const rendered = this.timestamp.peek();
		return rendered !== undefined && timestamp.lt(rendered);
	}

	// Resolve the codec string WebCodecs accepts once per track, from the probe the source ran, so Safari
	// opens an `avc3.*` rendition as `avc1.*`. False when torn down meanwhile.
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

		// A new array of new ranges, since the merged copy hands them to `buffered` by reference.
		this.#buffered.update((current) => {
			// Merge into the first range it overlaps.
			const i = current.findIndex((range) => range.start.le(end) && range.end.ge(start));
			if (i >= 0) {
				const range = current[i];
				return current.with(i, { start: range.start.min(start), end: range.end.max(end) });
			}
			return [...current, { start, end }].sort((a, b) => a.start.cmp(b.start));
		});
	}

	// Trim the decode buffer up to the rendered timestamp
	#trimBuffered(timestamp: Duration): void {
		this.#buffered.update((current) => {
			// Drop the ranges that ended before `timestamp` and clip the first one left.
			const i = current.findIndex((range) => range.end.ge(timestamp));
			if (i < 0) return [];
			const first = current[i];
			return [{ start: first.start.max(timestamp), end: first.end }, ...current.slice(i + 1)];
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

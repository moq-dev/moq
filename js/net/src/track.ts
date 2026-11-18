/**
 * Track role handles: a live stream of groups within a broadcast.
 *
 * @module
 */

import { Duration, Instant, now, sleepUntil } from "@moq/clock";
import { Derived, type Dispose, type GetPromise, type Getter, Once, Signal } from "@moq/signals";
import * as cache from "./cache.ts";
import { Delivered } from "./delivered.ts";
import { DropSet, Runs } from "./drops.ts";
import {
	GroupTooLarge,
	ProtocolViolation,
	Skipped,
	StreamCode,
	StreamError,
	TimestampMismatch,
	TooFarBehind,
	toStreamCode,
} from "./error.ts";
import { type Asker, FillState, type Lookup, type Requests, requestsOf } from "./fill.ts";
import { type Frame, type Consumer as GroupConsumer, Producer as GroupProducer } from "./group.ts";
import type { IetfVersion } from "./ietf/version.ts";
import {
	type Ask,
	cutRun,
	type DropRun,
	dropAt,
	type FillStarts,
	finalKey,
	groupBounds,
	type Hole,
	hooks,
	insertDrop,
	insertDrops,
	insertRun,
	type Land,
	maxDelayOf,
	NO_MAX_DELAY,
	type Recv,
	type ResumeSource,
	type Run,
	removeRun,
	runGaps,
	runsFrom,
	type StagedTrack,
	sameHole,
	skipHole,
	staleHole,
	subtractRuns,
	type TrackRequestOptions,
} from "./internal.ts";
import { grace, Ledger } from "./ledger.ts";
import { rescale } from "./rescale.ts";
import { floorAt, Timescale, type Timestamp } from "./time.ts";
import { checkEnd, checkKey, max, min, UNBOUNDED } from "./util/key.ts";
import {
	bounded,
	type Holdings,
	isWire,
	joinsLive,
	rangesOf,
	sameRanges,
	UNHELD,
	union,
	Window,
	wireSpan,
} from "./window.ts";
import { type Broadcast as BroadcastWire, registerTrackConsumer } from "./wire.ts";

export { type Fill, FillGroups, FillLookup, Requests } from "./fill.ts";
export { rescale, rescaleIn } from "./rescale.ts";

// The group a non-blocking read takes, skipping drops and throwing the track's error;
// `undefined` when nothing is deliverable now.
function taken(read: () => Recv, ended: () => undefined): GroupConsumer | undefined {
	for (;;) {
		const recv = read();
		if (recv.kind === "group") return recv.group;
		if (recv.kind === "error") throw recv.error;
		if (recv.kind === "done") return ended();
		if (recv.kind !== "drop") return undefined;
	}
}

/** What {@link Ordered.nextItem} returns next. Mirrors the Rust `track::Item`. */
export type Item =
	/** A group, with a higher key than any group or gap returned before. */
	| { kind: "group"; group: GroupConsumer }
	/**
	 * Groups `[start, end)` this reader gave up on, which it will not return, with what a read of
	 * them would have ended with. `group` is absent, so `item?.group` reads a group or nothing.
	 */
	| { kind: "gap"; start: bigint; end: bigint; error: Error; group?: never };

// What an ordered read found: an item, or the same idle, end, and error as a group read.
type ItemRecv = Item | { kind: "idle" } | { kind: "done" } | { kind: "error"; error: Error };

// The unset priority, the midpoint of the signed range: moq-lite carries it as 128 and
// moq-transport as 127, one urgency on both.
const DEFAULT_PRIORITY = 0;

// A group ended with one of these is a deliberate hole, never a cut readers see: cache policy,
// a reader that fell behind, or a cancel such as a relay's own unsubscribe. Mirrors Rust's
// `group::Handle::commit_abort`.
const HOLES: ReadonlySet<number> = new Set([
	StreamCode.Cancel,
	StreamCode.DeliveryTimeout,
	StreamCode.TooFarBehind,
	StreamCode.Old,
	StreamCode.Evicted,
]);

// The errors this cache closes a group with when it evicts it unfinished: an aged-out group,
// not a failed one. Mirrors Rust's cache-policy aborts in `group::Handle::commit_abort`.
const EVICTED = new WeakSet<Error>();

// Whether `successor`'s copy failed: closed with an error other than its eviction from this
// cache. Read from the copy, since a reader's own verdict (an eviction's gap, a stale group)
// says nothing about whether the group arrived.
function failed(successor: Successor): boolean {
	const abort = successor.fate();
	return abort !== undefined && !EVICTED.has(abort);
}

/**
 * A position within a track: a group's key and a frame's index inside it.
 *
 * As a track's end it is exclusive: `{ group: 90, frame: 0 }` ends before group 90, and
 * `{ group: 89, frame: 5 }` ends group 89 after its frames 0 to 4. Mirrors the Rust
 * `track::Position`.
 */
export interface Position {
	/** The group's sequence number within the track. */
	group: bigint;
	/** The frame's index within that group. */
	frame: number;
}

/**
 * How a track's frames are compressed, mirroring the Rust `track::Compression`: `"deflate"` makes
 * each group one DEFLATE stream (draft-lcurley-moq-flate), which suits small, repetitive frames
 * like JSON.
 */
export type Compression = "none" | "deflate";

/**
 * A track's immutable publisher properties, fixed for the lifetime of the track.
 *
 * A producer declares these once from an {@link InfoInit} (via {@link Producer.accept});
 * a consumer awaits them via {@link Subscriber.info}
 * (resolved from the wire TRACK_INFO on lite-05+). They map 1:1 onto TRACK_INFO.
 */
export interface Info {
	/** How frame payloads are compressed per group on lite-07; other sessions receive plain frames. */
	compression: Compression;
	/**
	 * Units per second for this track's frame timestamps, or `null` for a scale-less (untimed)
	 * track, whose frames carry no time: each one's timestamp is {@link Timestamp.unscaled} at
	 * its group's key.
	 *
	 * A timed track declares its scale, such as {@link Timescale | Timescale.MILLI}, or
	 * {@link Timescale | Timescale.MICRO} for media that needs sub-millisecond timing. Reported in
	 * TRACK_INFO on Lite05+ and as the TIMESCALE property on moq-transport. A track received
	 * without one (moq-lite before Lite05, or moq-transport without TIMESCALE) is untimed,
	 * and stays untimed when served onward.
	 */
	timescale: Timescale | null;
	/**
	 * Publisher Max Age: how long, in milliseconds, a superseded group stays fresh, or `null` for
	 * no publisher limit. It is stale once either clock reaches the budget: the wall-clock time
	 * since its successor arrived at this hop, or how far its successor's first timestamp trails
	 * the live edge. The newest group is never stale, and an untimed track is judged by the wall
	 * clock alone. Reported in TRACK_INFO (Lite05+) so relays re-serve with the same bound. A
	 * track received without one has no limit; zero serves only the live edge.
	 * This clamps each subscriber budget. A freshness bound, not retention: a hop's cache bounds
	 * the bytes it keeps with its own window.
	 */
	maxAge: Duration | null;
	/** Tie-break priority between subscriptions of equal subscriber priority (`-128..=127`, higher first). */
	priority: number;
	/** When a reader asks for a new group: on a live join, on a {@link Subscriber.refresh}, or both. See {@link Refresh}. */
	refresh: Refresh;
	/** The original moq-transport publisher's draft, whose timestamps may rewind. @internal */
	ietf?: IetfVersion;
	/**
	 * Whether this copy's groups skip the rewind check, since its timestamps may be lite-05/06 send
	 * times. An {@link Info.ietf} track skips it too. Mirrors the Rust `track::Info::unchecked`.
	 * @internal
	 */
	unchecked?: boolean;
}

/**
 * When a reader of a track asks its publisher for a new group, which TRACK_INFO carries on
 * lite-07. Mirrors the Rust `track::Refresh`.
 *
 * A track opting in starts each group with an independent frame (a snapshot or a keyframe)
 * that the frames after it build on, so a new group is something the publisher can make on
 * request. A request asks for a group starting past the newest one; every hop judges it
 * against its own cache, which holds the same group, and requests made while one is pending
 * coalesce into it. The reader still receives the cached group while the new one is pending.
 *
 * A live join asks when the newest group outweighs {@link Refresh.ratio} (any join without one).
 * {@link Subscriber.refresh} asks regardless of the ratio on a track with an {@link Refresh.interval},
 * and like a join otherwise. Either request waits out the interval: it takes effect only once
 * the newest group arrived at least that long ago, so the interval caps how often the
 * publisher makes groups however many readers ask. A track with no group asks for its first at
 * once. Both `null` (the default) asks for nothing.
 */
export interface Refresh {
	/**
	 * How many times its first frame a group's later bytes may grow before a live join asks for
	 * a new group, or `null` to judge no ratio: then any join asks, if the track sets an interval.
	 * `0` asks on any join once a delta follows the first frame. An integer in `0..2^32`.
	 */
	ratio: number | null;
	/**
	 * How long after the newest group arrived a request may ask for another, or `null` for at
	 * once. Measured on each hop's own clock from when the group reached it (or, at the
	 * publisher, was created), as {@link Info.maxAge} expiry measures it. A request made sooner
	 * waits until then. Carried as milliseconds like {@link Info.maxAge}.
	 */
	interval: Duration | null;
}

/**
 * What an author supplies to create a track: any {@link Info} field. `compression` defaults to
 * `"none"`, `timescale` to `null`, `refresh` to never, `priority` to `0`, and `maxAge` to
 * zero. A received {@link Info} serves onward unchanged.
 */
export interface InfoInit extends Partial<Omit<Info, "refresh">> {
	/**
	 * When a reader asks for a new group; see {@link Info.refresh}. An omitted field is `null`.
	 * The interval is rounded up to a whole millisecond like {@link maxAge}.
	 */
	refresh?: Partial<Refresh>;
	/**
	 * The publisher max age; see {@link Info.maxAge}. Omitted is zero, so readers get only the
	 * live edge; `null` sets no publisher limit (the wire omits it). A track whose readers reach
	 * back for superseded groups sets it explicitly. Rounded up to a whole millisecond; a
	 * negative or non-finite value, or one past `Number.MAX_SAFE_INTEGER`, is refused.
	 */
	maxAge?: Duration | null;
}

/**
 * Resolve an author's {@link InfoInit} into the {@link Info} a producer commits, validating each field.
 * @internal
 */
export function authoredInfo(init: InfoInit = {}): Info {
	if (init.compression !== undefined && init.compression !== "none" && init.compression !== "deflate")
		throw new TypeError(`unknown compression: ${init.compression}`);
	const maxAge = init.maxAge === undefined ? Duration.ZERO : init.maxAge;
	return {
		compression: init.compression ?? "none",
		timescale: init.timescale == null ? null : Timescale(init.timescale),
		maxAge: maxAge === null ? null : wireMillis("maxAge", maxAge),
		priority: priorityValue(init.priority ?? DEFAULT_PRIORITY),
		refresh: authoredRefresh(init.refresh),
		...(init.ietf === undefined ? {} : { ietf: init.ietf }),
		...(init.unchecked ? { unchecked: true } : {}),
	};
}

// Normalize a duration for the wire, which carries it as an unsigned varint.
//
// Callers derive it from measurements (a jitter estimate scaled off RTT), so a fractional
// millisecond is expected; ceil rather than round, because a budget shortened by rounding
// skips a group the subscriber still wants. Anything that is not a duration is refused
// here, where the field is named, rather than deep in the encoder.
function wireMillis(field: string, value: Duration): Duration {
	const millis = Math.ceil(value.asMillis());
	if (!Number.isSafeInteger(millis)) {
		throw new RangeError(`${field} exceeds the safe integer millisecond range: ${value}`);
	}
	return Duration.millis(millis);
}

function priorityValue(value: number): number {
	if (!Number.isInteger(value) || value < -128 || value > 127) {
		throw new RangeError(`priority must be an integer in -128..=127: ${value}`);
	}
	return value;
}

function authoredRefresh(init: Partial<Refresh> = {}): Refresh {
	const ratio = init.ratio ?? null;
	if (ratio !== null && (!Number.isInteger(ratio) || ratio < 0 || ratio > 0xffff_ffff)) {
		throw new RangeError(`refresh ratio must be an integer in 0..2^32: ${ratio}`);
	}
	const interval = init.interval == null ? null : wireMillis("refresh interval", init.interval);
	return { ratio, interval };
}

// Whether a track asks for new groups at all.
function refreshes(refresh: Refresh): boolean {
	return refresh.ratio !== null || refresh.interval !== null;
}

/** An explicitly included or excluded group key. */
export type Bound = { included: bigint; excluded?: never } | { excluded: bigint; included?: never };

/** Group limits for {@link Subscriber.withRange}; an omitted endpoint is unbounded. */
export interface Groups {
	/** The first group boundary. */
	start?: Bound;
	/** The last group boundary. */
	end?: Bound;
}

/**
 * Where one end of a {@link Range} falls: a count of groups back from the latest one
 * (`relative`, counting the groups the serving hop holds, so `{ relative: 0n }` is the latest
 * group), or a time (`time`, in the track's timescale: the group containing it, the newest one
 * starting at or before it). Mirrors the Rust `Anchor`.
 *
 * Groups are keyed by their start, so a group's start names exactly it. On a track without a
 * timescale the time is a bare start, in the same units as the group starts.
 *
 * Every anchor names a whole group: a subscription always starts at a group's first frame. An
 * anchor resolves at the hop that serves it, when it starts serving and again whenever the
 * subscriber replaces its ranges. Before any group exists a relative anchor resolves at the first
 * one, so `{ start: { relative: 0n } }` starts there. One counting back past every group held
 * reaches back to the oldest as a start, and ends before them all as an end.
 *
 * A time start resolves from the starts of the groups the serving hop holds. When none starts at
 * or before it, a hop whose fill handler serves its history looks the start up there (see
 * {@link FillLookup}), reading nothing until it answers; otherwise, or when the handler can't
 * tell, it reaches back to the oldest group. While no group is held at all it waits for one,
 * reading only the groups from its time meanwhile.
 *
 * Older moq-lite versions carry only time anchors, as group IDs, the live start, and the oldest
 * start, refusing any other.
 */
export type Anchor = { relative: bigint; time?: never } | { time: bigint; relative?: never };

/**
 * A span of a track to deliver: from `start` through `end`. Mirrors the Rust `Range`.
 *
 * An omitted start is the oldest group still around (Rust's `Anchor::Oldest`), and an omitted end
 * keeps following the live edge. Both ends include their group: `{time: 2n}..{time: 6n}` delivers
 * every group from the one containing time 2 through the one containing time 6, and
 * `..{relative: 0n}` delivers through the latest group. A time end before its time start is empty.
 */
export interface Range {
	/** Where delivery starts, or the oldest group when omitted. */
	start?: Anchor;
	/** Where delivery stops, including its group, or onward when omitted. */
	end?: Anchor;
}

/**
 * How far behind the live edge a subscriber plays, in milliseconds: where it starts, and how
 * late it may fall. Mirrors the Rust `track::Delay`. Both default to zero, which starts at the
 * newest group and skips a superseded one at once. Each value is rounded up to a whole
 * millisecond, so one derived from a measurement is never shortened; a negative or non-finite
 * value, or one past `Number.MAX_SAFE_INTEGER` after rounding, is refused.
 */
export interface Delay {
	/**
	 * The target latency, the playout or jitter-buffer delay held behind the live edge. Omitted
	 * means `0`; `null` means not stated, which a peer whose wire cannot carry it (moq-lite
	 * before 07, moq-transport) sends. A media consumer also waits this long for a missing group,
	 * since past it the group would play late, and starts an unstated minimum as `0` but waits a
	 * missing group for {@link Delay.max} instead.
	 *
	 * A live join, `{ start: { relative: 0n } }` with no end, starts this far behind the live edge:
	 * at the newest group that began at or before the live edge minus this, so a player's buffer
	 * arrives full instead of filling in real time. The hop serving it measures from what it holds:
	 * in media time from the newest frame on a timed track, and by arrival on its own clock on a
	 * scale-less one. With no such group held it starts at the oldest one, and never fetches
	 * upstream for more. {@link Delay.max} still skips what has gone stale. Any other range, the
	 * default oldest start included, and a resumed subscription ignore it, and changing it later
	 * moves nothing already started.
	 *
	 * The aggregate across subscribers is the largest stated one, which a relay forwards upstream,
	 * so the copy it fills reaches back for the reader that buffers most; each reader's own
	 * minimum then bounds what it reads of that copy.
	 */
	min?: Duration | null;
	/**
	 * How long a non-latest group stays worth delivering, by the same two clocks as
	 * {@link Info.maxAge}: it is skipped once its successor arrived this long ago or starts this
	 * far behind the live edge, and never past the publisher's {@link Info.maxAge}. Omitted
	 * means `0`, the live edge only, timed or not. `null` sets no subscriber limit, so the
	 * publisher's {@link Info.maxAge} alone bounds it, which is what a reader reaching back for
	 * history wants; read in order, see {@link Ordered} for where that can block.
	 */
	max?: Duration | null;
}

/**
 * Per-subscription options, requested when a subscription opens and adjustable later via
 * {@link Subscriber.update}. Mirrors the Rust `Subscription`.
 * The obsolete `maxAge` and `maxDelay` fields are refused at runtime; use `delay`.
 */
export interface Subscription {
	/** Delivery priority relative to this session's other subscriptions (`-128..=127`, higher first). Defaults to `0`. */
	priority?: number;
	/** How far behind the live edge this subscriber plays; see {@link Delay}. Omitted is all zero. */
	delay?: Delay;
	/**
	 * The spans of the track to deliver; see {@link Range}.
	 *
	 * Defaults to `[{}]`: everything {@link Delay.max} still considers fresh.
	 * `[{ start: { relative: 0n } }]` starts at the latest group instead, and an empty list asks
	 * for nothing. {@link Delay.max} caps every range: a range names what may be delivered, and
	 * the budget skips what inside it has gone stale. A read whose ranges all end finishes once
	 * nothing in them is left to deliver, as in Rust, while one left open follows the live edge.
	 *
	 * A request to the publisher, aggregated across every live subscriber as the union of their
	 * ranges, and this subscriber's read filter: it reads only the groups its own ranges cover,
	 * whatever another subscriber widens the union to. {@link Subscriber.withRange} narrows the
	 * reads further.
	 */
	ranges?: Range[];
	/**
	 * Which pending group the publisher sends first. Defaults to `"descending"`.
	 *
	 * Carried by moq-lite 07 and moq-transport (fixed at subscribe time there); older
	 * moq-lite versions are served newest first whatever this says.
	 *
	 * Aggregated across every live subscriber: any descending subscriber makes the aggregate
	 * descending, so a relay fanning one upstream request out keeps its live subscribers
	 * fresh. Each subscriber's own publisher still serves it in its own order.
	 */
	order?: Order;
}

/**
 * Which of a subscription's pending groups the publisher sends first: `"descending"` (newest
 * first, so congestion sheds the backlog rather than the live edge) or `"ascending"` (oldest
 * first, draining a backlog in sequence order).
 *
 * A transmission order only: it never decides whether a group is delivered, which is
 * {@link Delay.max}'s job, so an ascending subscriber that wants a backlog needs a
 * budget that covers it.
 */
export type Order = "ascending" | "descending";

/** Constructors for {@link Subscription}. */
export const Subscription: {
	/**
	 * The default subscription, used when none is passed: priority `0`, descending order, and
	 * a {@link Delay} of zero that skips a superseded group at once. Mirrors the Rust
	 * `Subscription::default()`. Spread it to change one field and keep the rest:
	 * `{ ...Subscription.default(), priority: 2 }`.
	 */
	default(): Subscription;
} = {
	default: () => ({
		priority: DEFAULT_PRIORITY,
		delay: { min: Duration.ZERO, max: Duration.ZERO },
		order: "descending",
	}),
};

// Materialize the defaults at the model boundary so every layer observes a complete
// subscription rather than interpreting an omitted field differently.
function subscriptionDefaults(subscription: Subscription = Subscription.default()): Subscription {
	if ("maxAge" in subscription || "maxDelay" in subscription) {
		throw new TypeError("Subscriber maxAge and maxDelay are unsupported; use delay.max");
	}
	if ("groups" in subscription) throw new TypeError("Subscription groups is unsupported; use ranges");
	const delay = subscription.delay ?? {};
	return {
		priority: priorityValue(subscription.priority ?? DEFAULT_PRIORITY),
		delay: {
			min: delay.min === null ? null : wireMillis("delay.min", delay.min ?? Duration.ZERO),
			max:
				delay.max === null
					? Duration.millis(NO_MAX_DELAY)
					: wireMillis("delay.max", delay.max ?? Duration.ZERO),
		},
		order: orderOf(subscription.order),
		ranges: subscription.ranges === undefined ? [{}] : rangesOf(subscription.ranges),
	};
}

// A plain JS caller can pass anything, and silently serving a typo as descending would hide it.
function orderOf(order: Order | undefined): Order {
	if (order === undefined) return "descending";
	if (order !== "ascending" && order !== "descending") throw new TypeError(`unknown order: ${String(order)}`);
	return order;
}

// The larger of two minimums, ignoring one that went unstated (`null`).
function largest(a: Duration | null | undefined, b: Duration | null | undefined): Duration | null {
	if (a === null || a === undefined) return b ?? null;
	if (b === null || b === undefined) return a;
	return a.max(b);
}

// Aggregate the preferences of `states`, matching Rust's Subscription::poll_combined.
function combineSubscriptions(states: Iterable<TrackState>): Subscription | undefined {
	let combined: Subscription | undefined;
	for (const state of states) {
		const subscription = state.update.peek();
		if (!subscription) continue;
		if (!combined) {
			combined = { ...subscription };
			continue;
		}

		combined.priority = Math.max(combined.priority ?? 0, subscription.priority ?? 0);
		combined.delay = {
			max: Duration.millis(Math.max(maxDelayOf(combined), maxDelayOf(subscription))),
			// The largest stated, so an upstream starts far enough back for the reader that buffers most.
			min: largest(combined.delay?.min, subscription.delay?.min),
		};
		// One live subscriber held behind another's backlog is worse than a backlog sent late.
		combined.order =
			combined.order === "ascending" && subscription.order === "ascending" ? "ascending" : "descending";

		// A range above the live edge never hides the latest group from a merged live subscriber.
		combined.ranges = [...(combined.ranges ?? []), ...(subscription.ranges ?? [])];
	}
	if (combined) combined.ranges = union(combined.ranges ?? []);
	return combined;
}

/**
 * A request for a track the peer wants, delivered to the publishing wire layer.
 *
 * Created internally by the broadcast when a subscription (or info lookup) needs a track
 * served; answer it with {@link accept} or {@link reject}.
 *
 * @internal
 */
export class Request {
	/** The requested track name. */
	readonly name: string;

	#producer: Producer;
	#pending: Set<Request>;
	#lookups?: TrackRequestOptions["lookups"];
	#wanted?: Getter<boolean>;

	private constructor(options: TrackRequestOptions) {
		this.name = options.name;
		this.#producer = options.producer;
		this.#pending = options.pending;
		this.#lookups = options.lookups;
		this.#pending.add(this);
	}

	static {
		hooks.makeRequest = (options) => new Request(options);
		hooks.stageRequest = (request, info) => request.#stage(info);
		hooks.requestWanted = (request) => {
			const wanted = request.#producer.demand().wanted;
			request.#wanted ??= request.#lookups
				? new Derived([wanted, request.#lookups.pending], (subscribed, lookups) => subscribed || lookups > 0)
				: wanted;
			return request.#wanted;
		};
		hooks.keepRequest = (request) => {
			if (request.#lookups) request.#lookups.opened = false;
		};
	}

	/** Watch the subscribers waiting for this request. */
	demand(): Demand {
		return this.#producer.demand();
	}

	/** The aggregate subscription requested for this track, one-shot fetches included. */
	get subscription(): Readonly<Subscription> {
		return hooks.requested(this.#producer).peek() ?? subscriptionDefaults();
	}

	/** The subscriber's priority for this track. */
	get priority(): number {
		return this.subscription.priority ?? 0;
	}

	/**
	 * Accept the request, committing the track's immutable {@link Info}, resolved as
	 * {@link Producer.accept} does. Throws, rejecting the request, if `info` is invalid.
	 */
	accept(info: InfoInit = {}): Producer {
		return this.#stage(info).commit();
	}

	#stage(info: InfoInit): StagedTrack {
		this.#pending.delete(this);
		const producer = this.#producer;
		return {
			producer,
			commit: () => {
				try {
					return producer.accept(info);
				} catch (err) {
					// Refused, so its subscribers see the error instead of waiting on an open request.
					producer.abort(err instanceof Error ? err : new Error(String(err)));
					throw err;
				}
			},
		};
	}

	/** Reject the request with the error its subscribers receive. */
	reject(err: Error): void {
		this.#pending.delete(this);
		this.#producer.abort(err);
	}
}

/** Options for {@link Consumer.info}. */
export interface InfoOptions {
	/** Abandons the lookup, rejecting with the signal's reason; aborting after it resolves has no effect. */
	signal?: AbortSignal;
}

/** Options for {@link Subscriber.refresh}. */
export interface RefreshOptions {
	/**
	 * Withdraws this caller's request if it is not sent yet, rejecting with the signal's reason.
	 * An already-aborted signal rejects before anything is asked, and aborting after it resolves
	 * has no effect.
	 */
	signal?: AbortSignal;
}

/** Options for {@link Consumer.fetchGroup}. */
export interface FetchGroupOptions {
	/** Delivery priority for the fetch stream. Defaults to `0`. */
	priority?: number;

	/** Maximum age behind the live edge this caller accepts. Omission leaves the publisher's {@link Info.maxAge} as the only bound. */
	maxDelay?: Duration;

	/**
	 * Abandons this fetch, rejecting with the signal's reason. Concurrent fetches of the same
	 * group share one stream, cancelled only once every caller has left. An already-aborted
	 * signal rejects before anything is sent, and aborting after the group resolves has no
	 * effect; close the group instead.
	 */
	signal?: AbortSignal;
}

/**
 * A lazy handle to a track on a consumed broadcast.
 *
 * @public
 */
export class Consumer {
	/** The track name. */
	readonly name: string;

	#broadcast: BroadcastWire;

	private constructor(name: string, broadcast: BroadcastWire) {
		this.name = name;
		this.#broadcast = broadcast;
	}

	static {
		registerTrackConsumer((name, broadcast) => new Consumer(name, broadcast));
	}

	/**
	 * Open a live subscription to the track.
	 *
	 * It reads only the groups its {@link Subscription.ranges} cover.
	 * {@link Delay.max} is what asks for data: delivery skips everything in range that the budget
	 * convicts. Without options this is {@link Subscription.default}, whose budget of zero
	 * delivers only the latest group; a larger one reaches back over what it can still use, and
	 * `delay: { max: null }` reaches back over every group the publisher's {@link Info.maxAge} still
	 * considers fresh.
	 */
	subscribe(options?: Subscription): Subscriber {
		return this.#broadcast.subscribe(this.name, options);
	}

	/**
	 * Fetch the track's immutable publisher properties without subscribing. It waits for as long
	 * as the publisher takes to answer: abort `options.signal` to give up.
	 */
	info(options?: InfoOptions): Promise<Info> {
		const signal = options?.signal;
		if (!signal) return this.#broadcast.resolveTrackInfo(this.name);
		if (signal.aborted) return Promise.reject(signal.reason);
		// Held only until the answer, so the signal abandons the lookup but never keeps the track.
		const answered = new AbortController();
		return this.#broadcast
			.resolveTrackInfo(this.name, AbortSignal.any([signal, answered.signal]))
			.finally(() => answered.abort());
	}

	/** Fetch a single group by key without holding a live subscription. */
	fetchGroup(key: bigint, options?: FetchGroupOptions): Promise<GroupConsumer> {
		try {
			checkKey(key);
		} catch (err) {
			return Promise.reject(err);
		}
		if (options?.maxDelay !== undefined) {
			options = { ...options, maxDelay: wireMillis("maxDelay", options.maxDelay) };
		}
		if (!this.#broadcast.fetchGroup) {
			return Promise.reject(new Error("fetch group is not supported by this session's version"));
		}
		return this.#broadcast.fetchGroup(this.name, key, options);
	}
}

// The parts of sorted, disjoint `runs` inside `[start, end)`.
function intersect(runs: readonly Run[], start: bigint, end: bigint): Run[] {
	const found: Run[] = [];
	for (let i = runsFrom(runs, start); i < runs.length && runs[i].start < end; i++)
		found.push({ start: max(runs[i].start, start), end: min(runs[i].end, end) });
	return found;
}

// How many runs a sink's `lost` keeps apart before folding them into one span.
const LOST_KEPT = 16;

// Record that `sink`'s timeline lost group `key`, for its reader's fill probe.
function lose(sink: TrackState, key: bigint): void {
	const lost = sink.lost;
	if (!lost) return;
	insertRun(lost, key, key + 1n);
	if (lost.length > LOST_KEPT) lost.splice(0, lost.length, { start: lost[0].start, end: (lost.at(-1) as Run).end });
}

// Take a group's mirror out of `sink`, so its subscriber can no longer receive it.
function unlinkMirror(sink: TrackState, mirror: GroupConsumer): void {
	sink.groups.mutate((groups) => {
		const i = groups.indexOf(mirror);
		if (i >= 0) groups.splice(i, 1);
	});
	const index = timelineIndex(sink.timeline, mirror.start);
	if (sink.timeline[index] === mirror) sink.timeline.splice(index, 1);
	lose(sink, mirror.start);
}

// The index of the first timeline group at or after `key`.
function timelineIndex(timeline: readonly { start: bigint }[], key: bigint): number {
	let lo = 0;
	let hi = timeline.length;
	while (lo < hi) {
		const mid = (lo + hi) >>> 1;
		if (timeline[mid].start < key) lo = mid + 1;
		else hi = mid;
	}
	return lo;
}

// Add a group to a sorted timeline, replacing any group with the same key. Groups
// usually arrive in order, so the append is checked first.
function timelineInsert(timeline: GroupConsumer[], group: GroupConsumer): void {
	const last = timeline.at(-1);
	if (!last || last.start < group.start) {
		timeline.push(group);
		return;
	}
	const index = timelineIndex(timeline, group.start);
	if (timeline[index]?.start === group.start) timeline[index] = group;
	else timeline.splice(index, 0, group);
}

// Add a group to a sink's key-sorted unread buffer, after any group with the same
// key, as a stable sort would. Checked for the usual in-order append first, so neither a
// replayed cache nor a growing backlog re-sorts the buffer on every arrival.
function bufferInsert(groups: GroupConsumer[], group: GroupConsumer): void {
	const last = groups.at(-1);
	if (!last || last.start <= group.start) groups.push(group);
	else groups.splice(timelineIndex(groups, group.start + 1n), 0, group);
}

// How many keys just below `key` the publisher declared it never produced: the skip run that
// ends at `key`. Mirrors the Rust `TrackState::skipped_below`.
function skippedBelow(state: TrackState, key: bigint): bigint {
	if (key === 0n) return 0n;
	const run = state.drops.at(key - 1n);
	return run?.skip ? key - run.start : 0n;
}

// Where the run of keys from `key` that no group can arrive at ends: a skip, or with `dropped` a
// drop given up for good, undefined for neither. Mirrors the Rust `TrackState::hole_end`.
function holeEnd(drops: DropSet, key: bigint, dropped: boolean): bigint | undefined {
	const run = drops.at(key);
	if (!run) return undefined;
	return run.skip || (dropped && run.stale === undefined) ? run.end : undefined;
}

// The first key of `runs`, lowest first, that `end` says no hole covers: each hole it names is
// stepped past whole. Mirrors the Rust `first_open`.
function firstOpen(runs: readonly Run[], end: (key: bigint) => bigint | undefined): bigint | undefined {
	let floor = 0n;
	for (const run of runs) {
		let key = max(run.start, floor);
		while (key < run.end) {
			const past = end(key);
			if (past === undefined) return key;
			key = past;
		}
		floor = key;
	}
	return undefined;
}

// The shared state behind a Producer / Subscriber pair. Package-internal
// wiring, unexported so it never appears in the published type declarations.
class TrackState {
	/** The producer fanning into this sink, so a subscriber can mint a sibling of itself. */
	producer?: Producer;
	/** This sink's reader as the producer's fills know it, once it asked one. */
	asker?: Asker;
	/**
	 * Whether this sink's reader will still read group `key`: only a buffered group it will
	 * read keeps its source wanted. Set once the reader exists.
	 */
	reads?: (key: bigint) => boolean;
	groups = new Signal<GroupConsumer[]>([]);
	// How many groups were mirrored in, whatever this sink reads: one arriving after its reader
	// ran out is still in flight below the end.
	arrived = 0;
	// Keys the upstream publisher gave up, with why: an incoming SUBSCRIBE_SKIP or
	// SUBSCRIBE_DROP, or a producer cut. Kept apart from the cache so reclaim cannot
	// erase a disposition a downstream publisher still has to forward, and as runs so a wide
	// DROP costs one entry. A sink shares its producer's, which the pool's idle window forgets.
	drops = new DropSet();
	// The runs this sink's own fills refused, which it reads as given up beside `drops`.
	refused = new Signal<readonly DropRun[]>([]);
	// Every group still in the producer's replay cache, including groups this
	// subscriber already consumed, sorted by key. Drift anchors have the same
	// lifetime as content. Sorted so the latency guard, evaluated per group and per
	// arrival, searches it instead of scanning the whole retained window.
	timeline: GroupConsumer[] = [];
	// The groups `timeline` lost since this sink's reader last probed its fills (evicted, withheld,
	// or failed), which its probe judges again; undefined until it first probes. Past LOST_KEPT
	// runs they fold into the span covering them all, judged again whole.
	lost?: Run[];
	// First timestamps mutate group state rather than track state, so held groups
	// watch this revision as well as arrivals when enforcing latency after handoff.
	timelineChanged = new Signal(0);
	// A copy received over a wire without ranges: see `hooks.dropAnchors`.
	dropsAnchors = new Signal(false);
	// Frames wake the key cursor only while an owed hole holds it.
	waitingOrdered = false;
	// One-shot group reads need FETCH, even when metadata has not reported a live edge. A failed
	// fill answer is a fetch's outcome, as a Rust fetch hears its fill's, so it is not withheld.
	fetch = false;
	// Bumped when a wire subscription declares or resolves where its live feed starts, so a
	// publisher holding its first group for that answer wakes.
	startChanged = new Signal(0);
	latest?: bigint;
	// One past the highest group received, including evicted content.
	received = new Signal(0n);
	/**
	 * The declared end, by {@link Producer.finishAt}, {@link Producer.fail}, or a clean close at
	 * the head of the key after the highest produced, with the error the track ended with
	 * there. One value, so a wake on it sees both. Mirrors the Rust `End`.
	 */
	end = new Signal<End | undefined>(undefined);
	// A wire subscription's pending tail still owes a group below the end, so reaching the
	// end does not end readers yet. Mirrors the Rust `awaits_tail`.
	owed = new Signal(false);
	closed = new Once<Error | null>();
	// The producer's outcome, settled by its close or by a reached and settled end, whichever
	// is first. Only the producer's own state uses it.
	outcome = new Once<Error | null>();
	update: Signal<Subscription | undefined>;
	/** Resolved once the producer commits the immutable properties. */
	info = new Signal<Info | undefined>(undefined);
	/** Newest group head, frame, or location reported by the publisher. */
	largest = new Signal<Position | undefined>(undefined);
	// The group {@link largest} first named, where a relative anchor resolved before any group
	// existed starts.
	first?: bigint;
	// Set once {@link largest} can no longer advance while the track stays open.
	largestEnded = new Signal(false);
	// Whether {@link largest}'s group ended there: it closed cleanly, or its source said so. An
	// aborted one stays open, since it can be recreated or fetched.
	largestFinished = false;

	constructor(subscription?: Subscription) {
		this.update = new Signal(subscription === undefined ? undefined : subscriptionDefaults(subscription));
	}
}

// A track's declared end. Mirrors the Rust `End`.
type End = {
	// The exclusive end: no frame at or past it exists.
	at: Position;
	// The error the track ended with at `at`, if it did not end cleanly: a reader reaching the
	// end gets it instead of the end of stream, while everything below stays readable.
	error?: Error;
	// A received end, declared ahead of the group streams below it that QUIC may deliver in any
	// order: readers wait for a hole below it to fill, or for the track to close, rather than end
	// at the boundary with groups still to arrive. Once the subscription feeding the copy ends (see
	// `hooks.cacheTrack`), nothing is owed and readers end at the boundary. Mirrors the Rust
	// `End::tail`.
	tail: boolean;
};

// The wire subscription feeding a received copy, and what it declared. Ending the subscription
// drops it whole, so nothing it declared outlives it.
type Wire = {
	// How its start bounds the fills readers ask below it.
	starts: FillStarts;
	// The first group its live feed serves (see `hooks.startTrack`), unset until its START
	// resolves it, null for a resolved-empty range, and the floor its SUBSCRIBE requested.
	served?: { start?: bigint | null; requested?: bigint };
};

// Settle a track state's closed Once. Idempotent: Once.set throws on a second settle, and a
// Producer closing its sinks races the Subscriber closing itself (the sink is only removed from
// #sinks a microtask later, via the closed subscription below).
function closeTrackState(state: TrackState, abort?: Error): boolean {
	if (state.closed.peek() !== undefined) return false;
	state.closed.set(abort ?? null);
	return true;
}

// Resolve the track's immutable publisher properties, or reject if it closes first.
// On a producer this resolves once info is committed (at accept time); on a consumer
// once the wire layer commits the properties it received (TRACK_INFO on lite-05+,
// SUBSCRIBE_OK on moq-transport) or an untimed default (older lite drafts), so awaiting it
// never yields a placeholder.
async function resolveInfo(state: TrackState): Promise<Info> {
	for (;;) {
		const info = state.info.peek();
		if (info) return info;

		const closed = state.closed.peek();
		if (closed instanceof Error) throw closed;
		if (closed !== undefined) throw new Error("track closed before info was known");

		await Signal.race(state.info, state.closed);
	}
}

// Whether `a` is past `b`.
function after(a: Position, b: Position): boolean {
	return a.group > b.group || (a.group === b.group && a.frame > b.frame);
}

// A source group retained in the producer cache, with the mirror handed to each sink
// so eviction can drop them together, and its last access, which the cache pool ages it by.
// `arrived` is when the group joined this track, which its mirrors age a predecessor from.
// `bound` is its first frame's timestamp once checked to fit between its neighbors', which
// bounds theirs; `unverified` marks one accepted below the eviction watermark instead.
// `fetched` marks one a fill answered, which the feed never promised.
type CachedGroup = {
	group: GroupProducer;
	charge: cache.Charge;
	mirrors: Map<TrackState, GroupConsumer>;
	arrived: Instant;
	bound?: Timestamp;
	unverified?: boolean;
	fetched?: boolean;
};

// A group's checked start, kept by key.
interface Start {
	start: bigint;
	first: Timestamp;
}

// Insert group `key`'s start into `starts`, sorted by key.
function insertStart(starts: Start[], key: bigint, start: Timestamp): void {
	const last = starts.at(-1);
	if (!last || last.start < key) starts.push({ start: key, first: start });
	else starts.splice(timelineIndex(starts, key), 0, { start: key, first: start });
}

// Remove group `key`'s start from `starts`, if indexed.
function removeStart(starts: Start[], key: bigint): void {
	const index = timelineIndex(starts, key);
	if (starts[index]?.start === key) starts.splice(index, 1);
}

// The index of the first of the sorted `keys` at or after `key`.
function keyIndex(keys: readonly bigint[], key: bigint): number {
	let lo = 0;
	let hi = keys.length;
	while (lo < hi) {
		const mid = (lo + hi) >>> 1;
		if (keys[mid] < key) lo = mid + 1;
		else hi = mid;
	}
	return lo;
}

// Add `key` to the sorted `keys`, once.
function insertKey(keys: bigint[], key: bigint): void {
	const last = keys.at(-1);
	if (last === undefined || last < key) {
		keys.push(key);
		return;
	}
	const index = keyIndex(keys, key);
	if (keys[index] !== key) keys.splice(index, 0, key);
}

// Remove `key` from the sorted `keys`, if there.
function removeKey(keys: bigint[], key: bigint): void {
	const index = keyIndex(keys, key);
	if (keys[index] === key) keys.splice(index, 1);
}

// A fresh read handle on a group the producer still caches; assigned in its static block.
let cachedMirror: (producer: Producer, key: bigint, from: number) => GroupConsumer | undefined;
// Whether the producer still caches a group that has not failed; assigned in its static block.
let cachedHolds: (producer: Producer, key: bigint) => boolean;
// Whether group `key` is cached as a copy from frame 0 that has not written it yet, so its
// start may still land, as the Rust `Slot::may_start` says.
let cachedMayStart: (producer: Producer, key: bigint) => boolean;
// Whether the producer is a copy a session receives, whose handler fetches by group alone.
let receivedCopy: (producer: Producer) => boolean;
// The keys in `[start, end)` the producer caches, unsorted, costing the smaller of the span
// and the cache; assigned in its static block.
let cachedIn: (producer: Producer, start: bigint, end: bigint) => bigint[];
// Mirror the cached groups in `[start, end)` that `offer` accepts into a sink again, cut ones
// included as a fresh sink gets them, for a reader whose floor moved below them; assigned in
// the producer's static block.
let reofferCached: (
	producer: Producer,
	sink: TrackState,
	start: bigint,
	end: bigint,
	offer: (key: bigint) => boolean,
) => void;
// Mirror the cached groups at `keys` into a sink again, for a reader whose budget widened
// past the one it skipped them under; assigned in the producer's static block.
let reofferKeys: (producer: Producer, sink: TrackState, keys: readonly bigint[]) => void;
// Whether a group may still land on the producer's live feed; assigned in its static block.
let mayLand: (producer: Producer, land: Land) => boolean;
// Recompute the producer's aggregates now, for a sink that just became a fetch; assigned in its
// static block.
let resubscribe: (producer: Producer) => void;
// Where a session's live feed starts owing groups, if known: it owes none below.
let feedFloor: (producer: Producer) => bigint | undefined;
let owedRuns: (producer: Producer, start: bigint, end: bigint) => Iterable<Run & { folds?: Instant }>;
// Whether a wire subscription feeds the producer, so it owes every group it has not accounted for.
let wireFed: (producer: Producer) => boolean;
let owedGroup: (producer: Producer, start: bigint, end: bigint) => { key: bigint; folds?: Instant } | undefined;
let fillEdge: (producer: Producer) => bigint | undefined;
let sourceStart: (producer: Producer) => bigint | "pending" | undefined;
// The lowest group a reader naming `named` asks a fill for; see `Producer.#fillFloor`.
let fillFloor: (producer: Producer, named: bigint | undefined) => bigint | undefined;
// The keys of the groups the producer caches, sorted: what relative and time anchors resolve
// against, as the Rust `TrackState::lookup` keys; assigned in its static block.
let keysOf: (producer: Producer) => readonly bigint[];
// How a received copy resolves its readers' time anchors, as its wire tells it; assigned in the
// producer's static block. See `hooks.startsAtKeys` and `hooks.timeStarts`.
let timeAnchors: (producer: Producer) => {
	atKeys: boolean;
	starts: ReadonlyMap<bigint, bigint>;
	relative: ReadonlyMap<bigint, { start: bigint; latest?: bigint }>;
};

/** The groups a reader reads, and the exclusive end it stops at; undefined before its ranges resolve. */
type Coverage = { runs: readonly Run[]; cap?: bigint } | undefined;

/** Whether two coverages hold the same groups and cap. */
function sameCoverage(a: Coverage, b: Coverage): boolean {
	if (a === undefined || b === undefined) return a === b;
	return (
		a.cap === b.cap &&
		a.runs.length === b.runs.length &&
		a.runs.every((run, i) => run.start === b.runs[i].start && run.end === b.runs[i].end)
	);
}

// A run of the drop set and the pool tick it outlives the idle window at.
interface Born extends Run {
	expires: number;
}

// Whether `a` and `b` give a key up alike, however each was folded in.
function sameVerdict(a: DropRun, b: DropRun): boolean {
	return sameHole({ ...a, received: undefined }, { ...b, received: undefined });
}

// The runs of `drops` and a reader's `refused` overlapping `[start, end)`, lowest first. A drop
// that `settles` the reader, judged from where it meets `start`, hides the refusals it covers:
// the track's verdict wins, as in Rust, but a stale one the reader rejects is no verdict for it.
function givenUp(
	drops: DropSet,
	refused: readonly DropRun[],
	start: bigint,
	end: bigint,
	settles: (run: DropRun) => boolean,
): DropRun[] {
	const found = drops.overlapping(start, end);
	if (refused.length === 0) return found;
	const own = refused.filter((run) => run.start < end && start < run.end);
	const hiding = found.filter((run) => settles({ ...run, start: max(run.start, start) }));
	for (const run of subtractRuns(own, hiding)) found.push(run);
	return found.sort((a, b) => (a.start < b.start ? -1 : a.start > b.start ? 1 : 0));
}

// Whether the live edge reached `end`, counting a gap up to it that was wholly given up, or a
// received end's tail decides it instead: what it misses below is owed only while the feed may
// still deliver it. Mirrors the Rust `TrackState::reaches`.
function reaches(received: bigint, drops: DropSet, end: End, refused: readonly Run[] = []): boolean {
	if (end.tail) return true;
	const final = finalKey(end.at);
	if (received >= final) return true;
	const given: Run[] = [];
	for (const run of [...drops.overlapping(received, final), ...refused]) insertRun(given, run.start, run.end);
	return runGaps(given, received, final).length === 0;
}

/**
 * Whether `run` settles a reader judging under `budget` with `coverage`. Any run but a received
 * stale one does. A stale verdict holds for a reader no wider than the budget it was judged under
 * that reads every group above the run's start, past the live edge: its own successor and edge are
 * then no older than those the verdict was judged against, since a group's verdict looks only
 * above it. Mirrors the Rust `settles`.
 */
function settles(run: DropRun, budget: number, coverage: Coverage): boolean {
	if (run.stale === undefined) return true;
	if (budget > run.stale || coverage === undefined || coverage.cap !== undefined) return false;
	const above = run.start + 1n;
	const held = coverage.runs.find((run) => run.start <= above && above < run.end);
	return held !== undefined && held.end >= UNBOUNDED;
}
// Actual object metadata outlives cached payloads, unlike empty group heads in model largest.
let objectLargest: (producer: Producer) => Position | undefined;
// The fills serving the producer's misses; assigned in its static block.
let fillsOf: (producer: Producer) => FillState;

// The key-order cursor lives inside `Subscriber` (it shares the group buffer and the
// drift anchor with the arrival cursor), so `Ordered` reaches it through this bridge,
// assigned in the class's static block.
let makeOrdered: (subscriber: Subscriber) => Ordered;
let ordered_: {
	nextGroup(subscriber: Subscriber): Promise<GroupConsumer | undefined>;
	nextItem(subscriber: Subscriber): Promise<Item | undefined>;
	tryItem(subscriber: Subscriber): ItemRecv;
	ended(subscriber: Subscriber): undefined;
	readFrame(subscriber: Subscriber): Promise<({ group: bigint; frame: number } & Frame) | undefined>;
	readString(subscriber: Subscriber): Promise<string | undefined>;
	readJson(subscriber: Subscriber): Promise<unknown | undefined>;
};

// A pending request for a group past every key written. Mirrors the Rust `demand::Pending`.
interface PendingRefresh {
	// When it takes effect: at once, or once the track's refresh interval has passed.
	at: Instant;
	// {@link Subscriber.refresh} callers waiting for it to be sent. The last to abort before then
	// withdraws it, unless it is `kept`.
	callers: number;
	// A join, a shed read, or a peer asked too, and none of those withdraws.
	kept: boolean;
	// Whatever serves the track (an upstream session or the local publisher) read it.
	sent: boolean;
	// Settles each waiting caller: resolves it, or rejects it with the track's close.
	settles: Set<(err?: Error) => void>;
}

// A getter that reports each value read from it, so a pending refresh is sent once whatever
// serves the track reads it.
class Read<T> implements Getter<T> {
	#source: Getter<T>;
	#read: (value: T) => void;

	constructor(source: Getter<T>, read: (value: T) => void) {
		this.#source = source;
		this.#read = read;
	}

	peek(): T {
		const value = this.#source.peek();
		this.#read(value);
		return value;
	}

	changed(): Promise<T>;
	changed(fn: (value: T) => void): Dispose;
	changed(fn?: (value: T) => void): Promise<T> | Dispose {
		if (!fn) {
			return this.#source.changed().then((value) => {
				this.#read(value);
				return value;
			});
		}
		return this.#source.changed((value) => {
			this.#read(value);
			fn(value);
		});
	}

	subscribe(fn: (value: T) => void): Dispose {
		return this.#source.subscribe((value) => {
			this.#read(value);
			fn(value);
		});
	}
}

// Constructs a Demand from within this module; assigned in the class's static block.
let makeDemand: (name: string, signals: DemandSignals, state: TrackState) => Demand;

// What a producer reports through its {@link Demand}.
interface DemandSignals {
	wanted: Getter<boolean>;
	refresh: Getter<boolean>;
	asked: Getter<bigint | undefined>;
}

/**
 * A watch-only view of a track's subscriber demand, from {@link Producer.demand}. Mirrors the
 * Rust `Demand`.
 *
 * Lets a publisher gate work (on-demand capture or encoding) on whether anyone is subscribed,
 * and open a group when a reader asks for one, without the ability to write groups or close the
 * track. The consumer wire watches it to tear an idle upstream subscription down instead of
 * downloading to nobody.
 */
export class Demand {
	/** The track name. */
	readonly name: string;

	#signals: DemandSignals;
	#state: TrackState;

	private constructor(name: string, signals: DemandSignals, state: TrackState) {
		this.name = name;
		this.#signals = signals;
		this.#state = state;
	}

	static {
		makeDemand = (name, signals, state) => new Demand(name, signals, state);
		hooks.demandPriority = (demand) => demand.#state.info.peek()?.priority ?? DEFAULT_PRIORITY;
		hooks.demandReleased = (demand) => demand.#state.closed;
		hooks.demandAsked = (demand) => demand.#signals.asked;
	}

	/** Whether anyone subscribes to the open track. Watch it (`effect.get` / `.peek()`) to follow demand. */
	get wanted(): Getter<boolean> {
		return this.#signals.wanted;
	}

	/**
	 * Whether a subscriber asks for a new group: the first group of a track with none written, or
	 * a live join or {@link Subscriber.refresh} judged by {@link Info.refresh}, once its interval
	 * has passed. The next group appended clears it, so a publisher waits for it to turn true,
	 * opens a group (a keyframe or a snapshot), and waits again.
	 */
	get refresh(): Getter<boolean> {
		return this.#signals.refresh;
	}

	/**
	 * Settles once the track's outcome is final: `null` once its declared end is reached and
	 * settled (its error, if declared with one), or on a clean release, else the abort
	 * {@link Error}. Never because readers left.
	 */
	get closed(): GetPromise<Error | null> {
		return this.#state.outcome;
	}
}

// Constructs a Subscriber from within this module without exposing a public
// constructor that would leak the unexported TrackState. Assigned in the class's
// static block.
let makeSubscriber: (name: string, state: TrackState) => Subscriber;

/**
 * The write side of a track, mirroring the Rust `Producer`.
 *
 * A producer is a fan-out source: every {@link subscribe} (including each wire
 * subscription the publisher serves from it) gets an independent
 * {@link Subscriber} that receives a full copy of the groups, each with its own
 * read cursor. Groups are mirrored into every live subscriber and retained for the
 * idle cache window so a late subscriber can replay groups within its media budget.
 *
 * Obtained from `Broadcast.Producer.createTrack` or constructed directly for an in-process
 * track.
 */
export class Producer {
	/** The track name. */
	readonly name: string;

	// The producer's own state is the source of truth (info/closed); subscribers
	// read mirrored sinks, never this state directly.
	#state = new TrackState();
	// One past the highest group this producer received, like the Rust
	// `max_sequence`, and where the next append lands.
	#received = 0n;
	#subscribing = new Signal(false);
	// The aggregate across live subscribers only. A fetch's preferences judge just the group it
	// asks for, so a publisher sizing what it serves (such as how much retained media to replay)
	// must not see them. The wire's aggregate, `#state.update`, keeps them.
	#live = new Signal<Subscription | undefined>(undefined);

	// Recently written source groups, in arrival order, retained for replay to late subscribers
	// until the cache pool evicts them once idle. A set, so evicting a run of
	// them is linear. Each entry tracks the mirror it handed to every sink so eviction can drop
	// them too: otherwise a slow consumer that never reads would pin old groups (and their frame
	// bytes) forever.
	#cache = new Set<CachedGroup>();
	// The same entries by key, so a write finds a duplicate without a scan.
	#cached = new Map<bigint, CachedGroup>();
	// The cached groups' checked starts, sorted by key, so a first frame finds its
	// neighbors' with a search.
	#bounds: Start[] = [];
	// The keys of the cached groups, sorted: what relative and time anchors resolve against, as
	// the Rust `TrackState::lookup` keys them.
	#keys: bigint[] = [];
	// A received copy's readers start a time anchor at the group starting there, held or not:
	// its wire's publisher starts one at the first group at or after it, and a copy holding part
	// of the track cannot tell which earlier group contains the time. Mirrors the Rust
	// `TrackState::starts_at_keys`.
	#startsAtKeys = false;
	// Where the upstream publisher started each of a lite-07 copy's live wire ranges, by the time
	// it asked from. Mirrors the Rust `TrackState::time_starts`.
	#timeStarts = new Map<bigint, bigint>();
	// On a lite-07 copy, where the publisher started each live range counting back, by its count.
	#relativeStarts = new Map<bigint, { start: bigint; latest?: bigint }>();
	// The highest-key checked start evicted. Eviction is not a key prefix, so a
	// group above it is checked against it as well as its cached neighbors; one at or below
	// it may have lost its lower bound. One entry, so nothing grows with the cache.
	#evicted?: Start;
	// This track's account against its broadcast's cache pool (a pool of its own when it has no
	// broadcast), which evicts the cached groups once idle.
	#account: cache.Track<CachedGroup>;
	// The pool tick past which each run of the drop set outlives the pool's idle window, as of when
	// it was last given up; empty when the pool has no idle window. See `#setDrops`.
	#dropsBorn = new Runs<Born>((a, b) => a.expires === b.expires);
	// At most the lowest tick in `#dropsBorn`, so an idle scan finds nothing to forget without
	// walking it.
	#dropsOldest?: number;

	// One independent downstream state per live subscriber.
	#sinks = new Set<TrackState>();

	// Whether any subscriber is attached, watched through {@link demand}.
	#wanted = new Signal(false);
	// Whether one asks for a new group now; see {@link Demand.refresh}.
	#refreshing = new Signal(false);
	// The group start asked for while it is past every key written; see `hooks.demandAsked`.
	#asked = new Signal<bigint | undefined>(undefined);
	// The lowest group they want while no refresh is pending, never lowered.
	#floor = 0n;
	// A request for a group past every key written, until a group answers it or its callers
	// withdraw it. Read against `#received` rather than stored as a key, so a key written
	// without answering it can never leave the ask behind.
	#pending?: PendingRefresh;
	// The last pending refresh {@link #refreshing} notified as due.
	#announced?: PendingRefresh;
	// Wakes a deferred refresh once it comes due.
	#refreshTimer?: { at: Instant; abort: AbortController };
	#demand: Demand;
	// Only real objects or upstream metadata advance this, never the head of an empty group.
	#objectLargest?: Position;

	// The wire subscription feeding this copy, from `hooks.receiveTrack` until `hooks.cacheTrack`
	// ends it: only a copy one feeds owes missing groups. Unset on a track's own publisher.
	#wire?: Wire;
	// The misses its readers asked of a {@link Requests} handle.
	#fills: FillState;
	// The keys the live feed delivered, as a group or a drop, and the stale runs a reclaim
	// forgot: what a pending tail is judged by. Eviction never touches it. A gap folds away a
	// subscription's grace after a later arrival opened it, as the session's tail does, so this
	// holds only the gaps still worth waiting for.
	#arrivals = new Ledger();
	// What this track's own publisher produced since its last append, which skips every
	// key below it that never arrived and was not given up: the producer moved on. The
	// arrivals cannot say so, since a gap folds into them once nobody waits for it. Only
	// keys at or above `#appended` are kept. Mirrors the Rust `produced`.
	#produced: Run[] = [];
	// One past the last append: every key below it arrived, was given up, or was skipped.
	#appended?: bigint;

	constructor(name: string) {
		this.name = name;
		// Whatever serves the track reading a request sends it.
		this.#demand = makeDemand(
			name,
			{
				wanted: this.#wanted,
				refresh: new Read(this.#refreshing, (asking) => asking && this.#sent()),
				asked: new Read(this.#asked, (asked) => asked !== undefined && this.#sent()),
			},
			this.#state,
		);
		this.#fills = new FillState({
			name,
			accept: (key, info) => {
				if (info !== undefined && this.#state.info.peek() === undefined) this.accept(info);
				return this.#createGroup(key, 0, true);
			},
			drop: (start, end, hole) => {
				this.#drop(start, end, hole);
			},
			holds: (key) => {
				const entry = this.#cached.get(key);
				if (entry === undefined || entry.group.closed.peek() instanceof Error) return false;
				return hooks.groupHeadless(entry.group) ? "headless" : true;
			},
			closed: () => this.#state.closed.peek() !== undefined,
			released: this.#state.closed,
		});
		this.#account = this.#openAccount(new cache.Pool({ expiry: cache.DEFAULT_EXPIRY }));
	}

	// Open this track's account against `pool`, evicting through this producer's cache.
	#openAccount(pool: cache.Pool): cache.Track<CachedGroup> {
		return new cache.Track(pool, {
			protects: (entry) => this.#protects(entry),
			evict: (entry) => {
				this.#evict(entry);
				this.#uncache(entry, true);
			},
			settle: () => this.#settle(),
			expireDrops: (now) => this.#expireDrops(now),
			holdsDrops: () => (this.#state.drops.all[0]?.start ?? UNBOUNDED) < this.#keptDrops(),
		});
	}

	// Give up `[start, end)` as `hole` says, or forget it without one, dating each run against the
	// pool's idle window. Mirrors the Rust
	// `TrackState::change_drops`.
	#setDrops(start: bigint, end: bigint, hole?: Hole): void {
		this.#state.drops.set(start, end, hole);
		const pool = this.#account.pool;
		const window = pool.expiryTicks;
		if (!hole) this.#dropsBorn.set(start, end);
		else if (window !== undefined) {
			// Rounded up to a quarter of the window, so runs given up together stay one entry and a
			// run is forgotten no sooner than the window after it was given up.
			const quarter = Math.max(1, Math.floor(window / 4));
			const expires = (Math.floor(pool.now() / quarter) + 1) * quarter + window;
			this.#dropsBorn.set(start, end, { start, end, expires });
			this.#dropsOldest = Math.min(this.#dropsOldest ?? expires, expires);
		}
		this.#account.dropsChanged();
	}

	// Forget the runs given up longer than the pool's idle window before tick `now`; walks the set
	// only once its oldest run is due, which a quarter of the window separates from the next.
	#expireDrops(now: number): boolean {
		if (this.#dropsOldest === undefined || now < this.#dropsOldest) return false;
		const kept = this.#keptDrops();
		const expired = this.#dropsBorn.all
			.filter((run) => now >= run.expires && run.start < kept)
			.map(({ start, end }) => ({ start, end: min(end, kept) }));
		// In one pass: runs given up in the same quarter of the window expire together.
		if (expired.length > 0) {
			this.#state.drops.clear(expired);
			this.#dropsBorn.clear(expired);
			this.#forgot(expired);
		}
		this.#dropsOldest = undefined;
		for (const run of this.#dropsBorn.all) {
			if (run.start < kept) this.#dropsOldest = Math.min(this.#dropsOldest ?? run.expires, run.expires);
		}
		return expired.length > 0;
	}

	// Account `forgotten`, runs a reclaim took out of the drop set, in the arrivals as an evicted
	// group stays: the publisher accounted for them, so neither the tail nor a reader waits on
	// them again, and a reader that wants one asks a fill or judges it by its budget. A reclaim
	// never adds a run to the arrivals: a run touching no accounted one takes the gap below it
	// too, given up early as a fold would. Nothing below the tail's floor is accounted, since
	// accounting it would lower the floor onto gaps nobody waited for. Mirrors the Rust
	// `TrackState::forget_drops`.
	#forgot(forgotten: readonly Run[]): void {
		const feed = this.#feedStart();
		for (const run of forgotten) {
			const floor = (feed === "pending" ? this.#wire?.served?.requested : feed) ?? this.#arrivals.first() ?? 0n;
			let start = max(run.start, floor);
			if (start >= run.end) continue;
			// The highest run starting at or below the run's end touches it when it ends at or past
			// its start; otherwise it is the run below.
			const below = this.#arrivals.endBelow(run.end);
			if (below === undefined || below < start) start = below === undefined ? floor : max(below, floor);
			this.#arrive(start, run.end);
		}
	}

	// Where the runs a reclaim may forget end: every run on a received copy, but only those below an
	// open local track's edge, since one above it is what lets its declared end be reached (see
	// `reaches`). A local producer gives up only what it chose to. Mirrors the Rust `kept_drops`,
	// except that a closed track's runs all go here too: Rust frees them with the state's last
	// handle, but here the pool holds the account until the sweep forgets them.
	#keptDrops(): bigint {
		return this.#wire || this.#state.closed.peek() !== undefined ? UNBOUNDED : this.#received;
	}

	// The runs from `from` up to `#keptDrops` were kept and are now reclaimable: date the oldest so
	// the next idle scan finds it, and have the sweep visit the track for them.
	#rearmDrops(from: bigint): void {
		const runs = this.#dropsBorn.overlapping(from, this.#keptDrops());
		for (const run of runs) this.#dropsOldest = Math.min(this.#dropsOldest ?? run.expires, run.expires);
		if (this.#state.drops.overlapping(from, this.#keptDrops()).length > 0)
			this.#account.pool.register(this.#account);
	}

	static {
		hooks.poolTrack = (producer, pool) => {
			if (!producer.#account.empty || producer.#state.drops.size > 0)
				throw new Error("track already caches groups");
			producer.#account = producer.#openAccount(pool);
		};
		hooks.subscribing = (producer) => producer.#subscribing;
		hooks.requested = (producer) => producer.#state.update;
		resubscribe = (producer) => producer.#updateSubscription();
		hooks.cacheTrack = (producer) => {
			producer.#wire = undefined;
			// Nothing feeds the tail anymore, so it owes nothing and readers end at the boundary.
			for (const state of [producer.#state, ...producer.#sinks]) state.owed.set(false);
			producer.#endLargest();
		};
		hooks.refuseLive = (producer, edge, error) => {
			for (const sink of producer.#sinks) {
				if (sink.fetch) continue;
				const ranges = sink.update.peek()?.ranges ?? [{}];
				const span = wireSpan(ranges);
				if (bounded(ranges) && span.end !== undefined && span.end <= edge) continue;
				closeTrackState(sink, error);
			}
		};
		fillEdge = (producer) =>
			!producer.#wire && producer.#state.info.peek()?.ietf !== undefined
				? producer.#state.largest.peek()?.group
				: undefined;
		hooks.receiveTrack = (producer, starts = {}) => {
			producer.#wire = { starts, served: producer.#wire?.served };
		};
		hooks.receiveGroup = (producer, { start: sequence, offset: start, fill }) =>
			producer.#createGroup(sequence, start, fill);
		hooks.arrivingGroup = (producer, key) => {
			const group = producer.#cached.get(key)?.group;
			if (!group || group.closed.peek() !== undefined) return undefined;
			return { settled: group.closed, release: hooks.holdGroup(group) };
		};
		hooks.arrivingIn = (producer, start, end) =>
			cachedIn(producer, start, end).filter(
				(key) => producer.#cached.get(key)?.group.closed.peek() === undefined,
			);
		sourceStart = (producer) => producer.#wire?.served && producer.#feedStart();
		fillFloor = (producer, named) => producer.#fillFloor(named);
		keysOf = (producer) => producer.#keys;
		timeAnchors = (producer) => ({
			atKeys: producer.#startsAtKeys,
			starts: producer.#timeStarts,
			relative: producer.#relativeStarts,
		});
		hooks.startsAtKeys = (producer) => {
			producer.#startsAtKeys = true;
		};
		hooks.timeStarts = (producer, starts) => {
			producer.#timeStarts = new Map(starts);
		};
		hooks.relativeStarts = (producer, starts) => {
			producer.#relativeStarts = new Map([...starts].map(([back, start, latest]) => [back, { start, latest }]));
		};
		hooks.producerLargest = (producer) => producer.#state.largest;
		hooks.askProducer = (producer, ask) => producer.#ask(ask);
		hooks.requestProducer = (producer, signal) => producer.#request(signal);
		hooks.endTrack = (producer, end, error) => producer.#endPending(end, error);
		hooks.tailGrace = (producer) => producer.#grace();
		hooks.releaseTrack = (producer) => producer.#close(undefined);
		hooks.trackReleased = (producer) => producer.#state.closed;
		hooks.trackFinished = (producer) => producer.#state.end.peek() !== undefined;
		hooks.dropTrackGroups = (producer, start, end, hole) => producer.#drop(start, end, hole, true);
		hooks.skipTrackGroups = (producer, start, end) => producer.#skipReceived(start, end);
		hooks.appendNext = (producer) => producer.#appendAt(producer.#received);
		hooks.supersedeStale = (producer) => {
			const held = producer.#state.drops.all.filter((run) => run.stale !== undefined);
			for (const run of held) producer.#setDrops(run.start, run.end);
		};
		hooks.dropAnchors = (producer) => {
			for (const state of [producer.#state, ...producer.#sinks]) state.dropsAnchors.set(true);
		};
		hooks.startTrack = (producer, served) => {
			if (!producer.#wire) throw new Error("only a wire subscription declares a start");
			producer.#settle();
			producer.#wire.served = served;
			producer.#owe();
			for (const state of [producer.#state, ...producer.#sinks]) state.startChanged.update((rev) => rev + 1);
		};
		hooks.cachedLargest = (producer) => producer.#cachedLargest();
		hooks.reportedLargest = (producer, largest, finished) => {
			producer.#advanceObject(largest);
			const current = producer.#state.largest.peek();
			if (finished && current && current.group === largest?.group && current.frame === largest.frame)
				producer.#finishLargest(largest.group);
		};
		hooks.reportedGroup = (producer, group) => producer.#advanceLargest({ group, frame: 0 });
		objectLargest = (producer) => producer.#objectLargest;
		fillsOf = (producer) => producer.#fills;
		cachedHolds = (producer, key) => {
			const entry = producer.#cached.get(key);
			return entry !== undefined && !(entry.group.closed.peek() instanceof Error);
		};
		cachedMayStart = (producer, key) => {
			const entry = producer.#cached.get(key);
			return entry !== undefined && hooks.groupMayStart(entry.group);
		};
		receivedCopy = (producer) => producer.#wire !== undefined;
		cachedIn = (producer, start, end) => {
			if (end - start <= BigInt(producer.#cached.size)) {
				const found: bigint[] = [];
				for (let key = start; key < end; key++) if (producer.#cached.has(key)) found.push(key);
				return found;
			}
			return [...producer.#cached.keys()].filter((key) => key >= start && key < end);
		};
		cachedMirror = (producer, key, from) => {
			const entry = producer.#cached.get(key);
			if (!entry || entry.group.closed.peek() instanceof Error) return undefined;
			const mirror = entry.group.mirror();
			if (hooks.groupIndex(mirror) > from) {
				mirror.close();
				return undefined;
			}
			hooks.arriveGroup(mirror, entry.arrived);
			// A cache hit is an access, restarting the group's idle window.
			entry.charge.touch();
			return mirror;
		};
		reofferCached = (producer, sink, start, end, offer) => {
			const held = new Set(sink.groups.peek().map((group) => group.start));
			for (const entry of producer.#cache) {
				const key = entry.group.start;
				if (key < start || key >= end || held.has(key) || !offer(key)) continue;
				producer.#mirror(entry, sink);
			}
		};
		reofferKeys = (producer, sink, keys) => {
			const held = new Set(sink.groups.peek().map((group) => group.start));
			for (const key of keys) {
				const entry = producer.#cached.get(key);
				if (entry && !held.has(key)) producer.#mirror(entry, sink);
			}
		};
		mayLand = (producer, land) => producer.#landable(land) !== undefined;
		wireFed = (producer) => producer.#wire !== undefined;
		feedFloor = (producer) => {
			if (!producer.#wire) return undefined;
			const feed = producer.#feedStart();
			return feed === "pending" ? producer.#wire.served?.requested : feed;
		};
		owedRuns = (producer, start, end) => {
			if (!producer.#wire) return [];
			const floor = feedFloor(producer);
			return producer.#landing({
				start: max(start, floor ?? 0n),
				end,
				floor: floor !== undefined,
				grace: producer.#grace(),
			});
		};
		owedGroup = (producer, start, end) => {
			for (const run of owedRuns(producer, start, end)) return { key: run.start, folds: run.folds };
			return undefined;
		};
	}

	// The first group that may still land from the live feed, by the rule a pending tail is judged
	// by; see {@link Land}. Mirrors the Rust `TrackState::may_land`.
	#landable(land: Land): { key: bigint; folds?: Instant } | undefined {
		for (const run of this.#landing(land)) {
			if (!land.delivered?.covers(run.start, run.end)) return { key: run.start, folds: run.folds };
		}
		return undefined;
	}

	*#landing(land: Land): Generator<Run & { folds?: Instant }> {
		if (this.#state.closed.peek() !== undefined) return;
		let start = land.start;
		if (!land.floor) {
			const first = this.#arrivals.first();
			if (first === undefined) return;
			start = max(start, first);
		}
		// A START bounds the feed only while the demand keeps the floor it answered: a lowered floor
		// asks the feed for the groups below it, which a fill must not duplicate.
		const feed = this.#feedStart();
		if (typeof feed === "bigint") start = max(start, feed);
		const declared = this.#state.end.peek()?.at;
		const end = declared === undefined ? land.end : min(land.end, finalKey(declared));
		const at = now();
		const limit = land.grace;
		const drops = this.#state.drops;
		for (const gap of this.#arrivals.gaps(start, end)) {
			const folds = gap.since?.saturatingAdd(limit);
			if (folds !== undefined && at.ge(folds)) continue;
			for (const open of runGaps(drops.overlapping(gap.start, gap.end), gap.start, gap.end)) {
				yield { ...open, folds };
			}
		}
	}

	// Record `[start, end)` given up as `hole` says, returning the runs recorded. A skip is final
	// and the most authoritative verdict, so a drop never replaces one. An upstream drop (`open`)
	// also ends the groups still arriving in it with its error, so their readers settle now
	// rather than once the stream's reset lands. A skip names nothing produced.
	#drop(start: bigint, end: bigint, hole: Hole, open = false): Run[] {
		if (start >= end) return [];
		const skipped = hole.skip ? [] : this.#state.drops.overlapping(start, end).filter((run) => run.skip);
		let runs = runGaps(skipped, start, end);
		const arriving: GroupProducer[] = [];
		if (open && !hole.skip) {
			for (const [key, entry] of this.#cached) {
				const closed = entry.group.closed.peek();
				if (key < start || key >= end || closed instanceof Error) continue;
				// A final drop ends a group still arriving, but one that finished here stays whole, as a
				// re-sent copy the subscriber refused leaves it: naming it given up would hide it from
				// every reader. A stale drop settles only the readers its budget covers, so it leaves a
				// group arriving too, which another subscription sharing the copy may be delivering.
				if (hole.stale === undefined && closed === undefined) arriving.push(entry.group);
				else runs = removeRun(runs, key, key + 1n);
			}
		}
		// A fill no longer owes what the track gave up for good. A stale verdict settles only
		// the readers it covers, so a fill asked by any other is still owed.
		if (hole.stale === undefined) this.#fills.dropped(start, end);
		for (const run of runs) this.#setDrops(run.start, run.end, hole);
		for (const group of arriving) group.abort(hole.error);
		// A pending tail owes nothing the publisher gave up. A stale verdict is not receipt: a new
		// upstream subscription supersedes it, leaving those groups unknown again, as in Rust.
		if (hole.stale === undefined) this.#arrive(start, end);
		// Giving up the last keys below the end reaches it, as writing them would.
		this.#endLargest();
		return runs;
	}

	// A publisher may re-create a group it cut, and a group a reader's fill refused may still
	// arrive; the key is no longer given up.
	#undrop(key: bigint): void {
		if (this.#state.drops.at(key)) this.#setDrops(key, key + 1n);
		for (const sink of this.#sinks) {
			const refused = sink.refused.peek();
			if (dropAt(refused, key)) sink.refused.set(removeRun(refused, key, key + 1n));
		}
	}

	/**
	 * Resolve this track's immutable publisher properties, committed at accept time.
	 * Rejects if the track is closed before the properties are known.
	 */
	info(): Promise<Info> {
		return resolveInfo(this.#state);
	}

	/**
	 * Publisher priority from the committed {@link Info}, or the default before {@link accept}.
	 *
	 * Higher is served first. Hang publishers set this from `Catalog.PRIORITY` so
	 * audio outranks video on the wire and in the bandwidth allocator.
	 */
	get priority(): number {
		return this.#state.info.peek()?.priority ?? DEFAULT_PRIORITY;
	}

	/** The timescale from the committed {@link Info}: `null` when scale-less, undefined before {@link accept}. */
	get timescale(): Timescale | null | undefined {
		return this.#state.info.peek()?.timescale;
	}

	/**
	 * Settles once the track's outcome is final: `null` once its declared end is reached and
	 * settled (every group below it finished; its error, if declared with one) or on a clean
	 * release, else the abort {@link Error}. The producer stays open after a settled end, serving its cache.
	 * Peek it synchronously (`undefined` while open), observe it reactively, or `await` it.
	 */
	get closed(): GetPromise<Error | null> {
		return this.#state.outcome;
	}

	/** The aggregate subscription across live subscribers, or `undefined` when there are none. */
	get subscription(): Getter<Subscription | undefined> {
		return this.#live;
	}

	/**
	 * Commit the immutable publisher properties, resolving {@link info}. An omitted `maxAge` is
	 * zero, the live edge only; see {@link InfoInit.maxAge}. Returns `this`.
	 */
	accept(info: InfoInit = {}): this {
		if (this.#state.closed.peek() !== undefined) return this;
		const resolved = authoredInfo(info);
		// Anything written before accept() must match too. A mismatch closes the track, so
		// nothing waiting on info() hangs on a half-bound cache.
		try {
			for (const { group } of this.#cache) {
				hooks.bindGroupTimed(group, resolved);
				hooks.bindGroupCompression(group, resolved.compression === "deflate");
			}
		} catch (err) {
			this.abort(err instanceof Error ? err : new Error(String(err)));
			throw err;
		}
		this.#state.info.set(resolved);
		// Propagate to any sink handed out before accept (the on-demand path).
		for (const sink of this.#sinks) sink.info.set(resolved);
		this.#updateSubscription();
		return this;
	}

	/**
	 * An independent {@link Subscriber} reading this track's groups.
	 *
	 * It reads only the groups its {@link Subscription.ranges} cover.
	 * {@link Delay.max} is what asks for data: delivery skips everything in range that the budget
	 * convicts. Without options this is {@link Subscription.default}, whose budget of zero
	 * delivers only the latest group; a larger one reaches back over what it can still use, and
	 * `delay: { max: null }` reaches back over every group the publisher's {@link Info.maxAge} still
	 * considers fresh.
	 */
	subscribe(options: Subscription = Subscription.default()): Subscriber {
		const sink = new TrackState(options);
		this.#addSink(sink);
		return makeSubscriber(this.name, sink);
	}

	/** A watch-only view of this track's subscriber demand. */
	demand(): Demand {
		return this.#demand;
	}

	/**
	 * Open a {@link Requests} handle serving the groups readers miss: ones their ranges name that
	 * the cache no longer holds, or never did, below the newest group written. Most producers
	 * never need one; a publisher backed by storage opens one to serve its history. Mirrors the
	 * Rust `Producer::requests`.
	 *
	 * While one is open, each run of misses queues a {@link Fill}, which readers missing the same
	 * groups share; with none open, a miss is given up and {@link Consumer.fetchGroup} of an
	 * uncached group fails with `NotFound`. A reader only asks for what its budget still wants,
	 * so a reader at the live edge never asks. The track stays one track: answered groups land
	 * in its cache, and every reader of it sees them.
	 */
	requests(): Requests {
		return requestsOf(this.#fills);
	}

	// Ask for a group past every key written, judged by the track's refresh: a live join only
	// when the newest group outweighs the ratio, and any request only once the newest group is
	// at least the interval old. A request made sooner is deferred until then, and one made
	// while a request is pending coalesces into it, the earliest taking effect.
	#ask(ask: Ask): void {
		const at = this.#asks(ask);
		if (at !== undefined) this.#pend(at, true);
	}

	// {@link #ask} for a {@link Subscriber.refresh} caller, resolving once the request is sent or
	// a group answers it. Aborting `signal` before then withdraws this caller's request.
	#request(signal?: AbortSignal): Promise<void> {
		if (signal?.aborted) return Promise.reject(signal.reason);
		// A closed track never sends another request, so the caller ends as a pending one would.
		const closed = this.#state.closed.peek();
		if (closed !== undefined) return Promise.reject(closed ?? this.#closedBeforeSent());
		// A caller joining a request already sent has nothing left to wait for.
		if (this.#pending?.sent) return Promise.resolve();
		const at = this.#asks("refresh");
		if (at === undefined) return Promise.resolve();
		const pending = this.#pend(at, false);
		return new Promise((resolve, reject) => {
			const abort = () => {
				pending.settles.delete(settle);
				this.#withdraw(pending);
				reject(signal?.reason);
			};
			const settle = (err?: Error) => {
				signal?.removeEventListener("abort", abort);
				if (err) reject(err);
				else resolve();
			};
			pending.settles.add(settle);
			signal?.addEventListener("abort", abort, { once: true });
		});
	}

	// Coalesce a request taking effect at `at` into the pending one, the earliest taking effect.
	#pend(at: Instant, kept: boolean): PendingRefresh {
		const current = this.#pending;
		const pending = current ?? { at, callers: 0, kept: false, sent: false, settles: new Set() };
		this.#pending = pending;
		pending.kept ||= kept;
		if (!kept) pending.callers++;
		// Only a new request, or an earlier one, changes what is asked for.
		if (current?.at.le(at)) return pending;
		pending.at = at;
		this.#reportWanted();
		return pending;
	}

	// A caller of `pending` gave up: the last caller withdraws a request not yet sent, unless
	// something else that never withdraws asked too.
	#withdraw(pending: PendingRefresh): void {
		if (this.#pending !== pending || pending.sent) return;
		pending.callers--;
		if (pending.callers > 0 || pending.kept) return;
		this.#endRefresh();
		this.#reportWanted();
	}

	// Whatever serves the track read the group asked for: a due refresh is sent, resolving its
	// callers.
	#sent(): void {
		const pending = this.#pending;
		if (!pending || pending.sent || !pending.at.le(now())) return;
		pending.sent = true;
		for (const settle of pending.settles) settle();
		pending.settles.clear();
	}

	// The pending refresh ended, answered or withdrawn, or the track closed with `err`.
	#endRefresh(err?: Error): void {
		const pending = this.#pending;
		this.#pending = undefined;
		for (const settle of pending?.settles ?? []) settle(err);
	}

	// What a refresh caller sees when the track closes cleanly before its request is sent.
	#closedBeforeSent(): Error {
		return new StreamError(StreamCode.Cancel, { message: `track ${this.name} closed before the refresh was sent` });
	}

	// When a request as `ask` takes effect, if the track's refresh lets it ask at all.
	#asks(ask: Ask): Instant | undefined {
		const refresh = this.#state.info.peek()?.refresh;
		const latest = this.#state.latest;
		if (!refresh || !refreshes(refresh) || latest === undefined) return undefined;
		const entry = this.#cached.get(latest);
		// The interval alone judges a refresh on a track that declares one, and a shed read
		// cannot decode on whatever the ratio says.
		const ratio = ask === "join" || (ask === "refresh" && refresh.interval === null) ? refresh.ratio : null;
		if (ratio !== null && !(entry && hooks.overRefresh(entry.group, ratio))) return undefined;
		const current = now();
		if (refresh.interval === null || !entry) return current;
		// `undefined` when no instant reaches it.
		return entry.arrived.checkedAdd(refresh.interval)?.max(current);
	}

	// Report the demand: whether anyone subscribes, and the group start asked for while it is
	// past every key written, which is the next free key once a pending refresh is due. A
	// deferred one arms a wakeup to report again when it comes due.
	#reportWanted(): void {
		const current = now();
		const request = this.#pending;
		const pending = request?.at;
		const due = pending?.le(current) === true;
		const start = due ? max(this.#received, this.#floor) : this.#floor;
		const asked = start >= this.#received ? start : undefined;
		const wanted = this.#sinks.size > 0;
		this.#asked.set(asked);
		this.#wanted.set(wanted);
		const refreshing = wanted && asked !== undefined;
		// A request due in the batch that ended the last one leaves the level unchanged, so it
		// notifies anyway: the publisher must read it to send it.
		if (refreshing && request !== undefined && due && request !== this.#announced) {
			this.#announced = request;
			this.#refreshing.set(true, true);
		} else {
			this.#refreshing.set(refreshing);
		}

		const armed = this.#refreshTimer;
		if (pending !== undefined && !due && armed?.at.eq(pending)) return;
		armed?.abort.abort();
		this.#refreshTimer = undefined;
		if (pending === undefined || due || this.#state.closed.peek() !== undefined) return;
		const timer = { at: pending, abort: new AbortController() };
		this.#refreshTimer = timer;
		// A deferred refresh is never a reason to hold a Node/Bun process open.
		void sleepUntil(pending, { signal: timer.abort.signal, ref: false }).then(
			() => {
				if (this.#refreshTimer !== timer) return;
				this.#refreshTimer = undefined;
				this.#reportWanted();
			},
			() => {},
		);
	}

	// Register a downstream sink: seed its info, replay the retained window, and (while
	// the track is open) mirror future groups into it. A late subscriber to a closed
	// track still drains the buffered groups before seeing the end.
	#addSink(sink: TrackState): void {
		sink.producer = this;
		sink.drops = this.#state.drops;
		const info = this.#state.info.peek();
		if (info) sink.info.set(info);
		sink.largest.set(this.#state.largest.peek());
		sink.first = this.#state.largest.peek()?.group;
		sink.largestEnded.set(this.#state.largestEnded.peek());
		sink.largestFinished = this.#state.largestFinished;
		sink.dropsAnchors.set(this.#state.dropsAnchors.peek());

		const closed = this.#state.closed.peek();
		if (closed === undefined) {
			if (joinsLive(sink.update.peek()?.ranges)) this.#ask("join");
			this.#sinks.add(sink);
			this.#reportWanted();

			// Forward subscription updates from the sink's Subscriber to the producer's own
			// state, which the wire layer (or the serving application) watches.
			const forward = sink.update.subscribe(() => this.#updateSubscription());
			this.#updateSubscription();

			// Drop the sink once its consumer goes away, closing its mirrors so source
			// groups stop teeing into them, so a long-lived producer doesn't leak. This
			// covers mirrors already handed out via recvGroup (no longer in sink.groups)
			// by closing them through the cache's per-sink tracking.
			const dispose = sink.closed.subscribe((c) => {
				if (c === undefined) return;
				const abort = c instanceof Error ? c : undefined;
				forward();
				this.#sinks.delete(sink);
				if (sink.asker) this.#fills.leave(sink.asker);
				this.#updateSubscription();
				// Update demand: once the last subscriber leaves, the consumer wire (watching
				// {@link Demand.wanted}) tears the upstream down instead of downloading to nobody.
				this.#reportWanted();
				// A closed track leaves a sink's buffered mirrors readable; they age out
				// with the cache, so a stale consumer can't pin them.
				if (this.#state.closed.peek() !== undefined) {
					dispose();
					return;
				}
				for (const entry of this.#cache) {
					const mirror = entry.mirrors.get(sink);
					if (mirror) {
						// A group its recovery moved to another copy no longer reads this one.
						if (hooks.readsMirrored(mirror)) hooks.releaseGroupReader(mirror, abort);
						entry.mirrors.delete(sink);
					}
				}
				for (const group of sink.groups.peek()) hooks.releaseGroupReader(group, abort);
				dispose();
			});
		}

		// Full, unlike a write's bounded scan, so the subscriber gets no group already idled out.
		this.#evictExpired(true);
		for (const entry of this.#cache) this.#mirror(entry, sink);

		sink.received.set(this.#received);
		sink.end.set(this.#state.end.peek());
		sink.owed.set(this.#state.owed.peek());
		if (closed !== undefined) {
			closeTrackState(sink, closed instanceof Error ? closed : undefined);
		}
	}

	// Recompute from every sink because an update or close can narrow as well as widen the
	// aggregates. The wire layer observes `#state.update` and emits SUBSCRIBE_UPDATE.
	//
	// The max delay is the subscribers' own, not clamped to the track's maxAge: the publisher
	// applies its own, as each reader's budget does, so the demand asked upstream is the same
	// before and after the info lands.
	#updateSubscription(): void {
		const live = [...this.#sinks].filter((sink) => !sink.fetch);
		this.#subscribing.set(live.some((sink) => sink.closed.peek() === undefined));
		const combined = combineSubscriptions(this.#sinks);
		this.#settle();
		this.#state.update.set(combined);
		this.#live.set(combineSubscriptions(live));
		// The requested floor bounds what a pending tail owes.
		this.#owe();
	}

	// Mirror a cached source group into a sink. The mirror fills synchronously as the
	// source is written and keeps its own read cursor; frame bytes are shared by
	// reference. Tracked on the entry so eviction can drop it from the sink.
	#mirror(entry: CachedGroup, sink: TrackState): void {
		if (entry.fetched && entry.group.closed.peek() instanceof Error) return;
		const dst = entry.group.mirror();
		hooks.arriveGroup(dst, entry.arrived);
		if (entry.unverified) hooks.unverifyGroup(dst);
		entry.mirrors.set(sink, dst);
		if (sink.reads?.(dst.start) === false) hooks.groupReads(dst, false);
		timelineInsert(sink.timeline, dst);
		sink.arrived++;
		void dst.readable().then(() => sink.timelineChanged.update((revision) => revision + 1));
		sink.latest = max(sink.latest ?? 0n, dst.start);
		sink.received.set(max(sink.received.peek(), dst.start + 1n));
		sink.groups.mutate((groups) => bufferInsert(groups, dst));
	}

	// Drop a cached group's mirror from every sink so no consumer can pin it, the cache pool
	// having reclaimed it for idling, or a new copy having replaced it.
	#evict(entry: CachedGroup): void {
		// Reclaiming a group the publisher never closed ends it as the gap it is, before
		// the mirrors below would otherwise report a clean finish to a reader that had
		// drained it. The usual case, an already-closed group aging out, keeps its own
		// terminal state.
		const lost = new TooFarBehind();
		EVICTED.add(lost);
		if (entry.group.closed.peek() === undefined) entry.group.abort(lost);
		const mirrors = [...entry.mirrors.values()].filter((mirror) => hooks.evictGroup(mirror, lost));
		this.#unlink(entry);
		for (const mirror of mirrors) mirror.close();
	}

	// Take a failed fill answer out of every sink that has not read it, as Rust serves no failed
	// fetch-only slot; #mirror gives it to no new sink either. The entry stays cached and
	// reclaimable, as Rust keeps a spent slot until its scan reclaims it, so a reader already
	// holding it keeps its frames and the fill's own failure.
	#withhold(entry: CachedGroup): void {
		for (const [sink, mirror] of entry.mirrors) {
			// A fetch's own outcome.
			if (sink.fetch) continue;
			const index = sink.groups.peek().indexOf(mirror);
			if (index < 0) continue;
			sink.groups.mutate((groups) => {
				groups.splice(index, 1);
			});
			const at = timelineIndex(sink.timeline, mirror.start);
			if (sink.timeline[at] === mirror) sink.timeline.splice(at, 1);
			lose(sink, mirror.start);
			entry.mirrors.delete(sink);
			hooks.releaseGroupReader(mirror, undefined);
		}
	}

	// Take a cached group's mirrors out of every sink, so a subscriber can no longer
	// receive them. A reader already holding one keeps it as is.
	#unlink(entry: CachedGroup): void {
		for (const [sink, mirror] of entry.mirrors) unlinkMirror(sink, mirror);
		entry.mirrors.clear();
	}

	// Take a cached group out of the cache lookups and its track's account.
	#uncache(entry: CachedGroup, evicted = false): void {
		this.#cache.delete(entry);
		if (this.#cached.get(entry.group.start) === entry) {
			this.#cached.delete(entry.group.start);
			removeKey(this.#keys, entry.group.start);
		}
		this.#unbound(entry, evicted);
		this.#account.remove(entry);
		hooks.chargeGroup(entry.group, undefined);
	}

	// The starts a first frame of group `key` must fall between: the nearest checked
	// starts cached on either side, since groups land out of key order, and the eviction
	// watermark. Throws {@link TimestampMismatch} on a rewind or a tie: starts strictly increase.
	// Returns whether the group is late, at or below the watermark, where its floor may be gone,
	// so the watermark caps it instead and it is accepted unverified. Mirrors the Rust
	// `Neighbors::admit`.
	#admit(key: bigint, start: Timestamp): boolean {
		const bounds = this.#bounds;
		const index = timelineIndex(bounds, key);
		let floor = bounds[index - 1]?.first;
		let ceiling = bounds[bounds[index]?.start === key ? index + 1 : index]?.first;
		const evicted = this.#evicted;
		const late = evicted !== undefined && key <= evicted.start;
		// The evicted group itself, fetched again, starts where it did: only its neighbors bound it.
		if (evicted && key < evicted.start) {
			if (ceiling === undefined || evicted.first.isBefore(ceiling)) ceiling = evicted.first;
		} else if (evicted && !late) {
			if (floor === undefined || floor.isBefore(evicted.first)) floor = evicted.first;
		}
		if ((floor && !floor.isBefore(start)) || (ceiling && !start.isBefore(ceiling))) throw new TimestampMismatch();
		return late;
	}

	// Record a cached group's checked start: a bound for its neighbors, or unverified. Either
	// indexes where it starts, an unverified one only where a local handler serves the history,
	// as the Rust `index_start` does.
	#record(entry: CachedGroup, start: Timestamp, late: boolean): void {
		const key = entry.group.start;
		if (late) {
			entry.unverified = true;
			for (const mirror of entry.mirrors.values()) hooks.unverifyGroup(mirror);
			return;
		}
		entry.bound = start;
		insertStart(this.#bounds, key, start);
	}

	// Drop a group's checked start as it leaves the cache, raising the watermark to it when
	// it was evicted.
	#unbound(entry: CachedGroup, evicted = false): void {
		const key = entry.group.start;
		const start = entry.bound;
		if (start === undefined) return;
		removeStart(this.#bounds, key);
		if (evicted && (this.#evicted === undefined || key > this.#evicted.start)) {
			this.#evicted = { start: key, first: start };
		}
	}

	// The cached group at the newest key written, if still cached.
	#edgeEntry(): CachedGroup | undefined {
		const latest = this.#state.latest;
		return latest === undefined ? undefined : this.#cached.get(latest);
	}

	// The newest group remains replayable while the track is live, even after the group
	// closes: a catalog can publish one snapshot for the broadcast's whole lifetime. An
	// aborted one holds nothing worth replaying, so it ages out like any other.
	#protects(entry: CachedGroup): boolean {
		return (
			this.#state.closed.peek() === undefined &&
			entry.group.start === this.#state.latest &&
			!(entry.group.closed.peek() instanceof Error)
		);
	}

	// The newest frame in the cache, as {@link Subscriber.largest} reads it from a sink:
	// unknown once the newest group is evicted, since what is left cannot say where the edge is.
	#cachedLargest(): Position | undefined {
		const latest = this.#state.latest;
		if (latest === undefined || !this.#cached.has(latest)) return undefined;

		let largest: Position | undefined;
		for (const { group } of this.#cache) {
			const count = hooks.groupFrameCount(group);
			if (count > hooks.groupOffset(group) && (largest === undefined || group.start > largest.group)) {
				largest = { group: group.start, frame: count - 1 };
			}
		}
		return largest;
	}

	#advanceLargest(largest: Position | undefined): void {
		if (!largest) return;
		const current = this.#state.largest.peek();
		if (current && !after(largest, current)) return;
		for (const state of [this.#state, ...this.#sinks]) {
			state.first ??= largest.group;
			state.largest.set(largest);
			state.largestFinished = false;
		}
	}

	#advanceObject(largest: Position | undefined): void {
		if (largest && (!this.#objectLargest || after(largest, this.#objectLargest))) this.#objectLargest = largest;
		this.#advanceLargest(largest);
	}

	/** The track's largest location: its newest group at the newest frame, or undefined before any. */
	largest(): Position | undefined {
		return this.#state.largest.peek();
	}

	// Evict the groups idle past the pool's window: every one when `full`, else a bounded scan.
	#evictExpired(full: boolean): void {
		if (this.#account.evictExpired(full)) this.#settle();
	}

	// Retain a source group and fan it out to every live sink; its frame 0 is checked
	// against this track when it lands.
	// `fetched` marks a group a fill answered, which the live feed never delivered: it accounts
	// for nothing the feed owes, as the Rust `insert` records arrivals only for visible groups.
	// `succeeds` is a failed copy of the same key this one takes over: its mirrors held open
	// for a duplicate continue on this one, and each sink in `kept` keeps its mirror.
	#publish(
		group: GroupProducer,
		fetched = false,
		succeeds?: { failed: GroupProducer; kept?: Map<TrackState, GroupConsumer> },
	): void {
		this.#settle();
		const entry: CachedGroup = {
			group,
			charge: this.#account.charge(),
			mirrors: new Map<TrackState, GroupConsumer>(),
			arrived: now(),
			fetched: fetched || undefined,
		};
		hooks.chargeGroup(group, entry.charge);
		if (succeeds) hooks.succeedGroup(group, succeeds.failed);
		this.#undrop(group.start);
		// No reader is handed a failed fill answer (see #withhold), withheld as it aborts so a
		// read that follows at once cannot take it: its key is unknown again, owed while the
		// feed owes it, else judged by budget or filled again.
		if (fetched) {
			hooks.onGroupAbort(group, (error) => {
				// An eviction takes the mirrors out itself.
				if (!EVICTED.has(error) && this.#cached.get(group.start) === entry) this.#withhold(entry);
			});
		}
		group.closed.then((closed) => {
			// An aborted live edge is no longer protected, so it may expire like any other.
			if (closed instanceof Error) this.#account.demote(entry);
			if (!(closed instanceof Error)) return;
			// A re-created group already replaced this one.
			const current = this.#cached.get(group.start);
			if (current && current.group !== group) return;
			// A failed copy holds nothing to read, so each reader's probe misses it again.
			for (const sink of entry.mirrors.keys()) lose(sink, group.start);
			// A failed fill answer says nothing of the feed, so it gives nothing up: the track's drops
			// and arrivals hold only the feed's verdicts, as Rust records nothing for a failed fill.
			if (fetched) return;
			if (HOLES.has(toStreamCode(closed))) return;
			this.#drop(group.start, group.start + 1n, { error: closed });
		});
		const key = group.start;
		if (this.#unchecked()) {
			// Its timestamps may rewind, so they are not checked.
		} else {
			hooks.admitGroupStart(group, (start) => {
				const late = this.#admit(key, start);
				return () => this.#record(entry, start, late);
			});
		}
		this.#cache.add(entry);
		this.#cached.set(group.start, entry);
		insertKey(this.#keys, group.start);
		this.#fills.arrived(group.start);
		this.#advance(group.start, !fetched);
		if (!fetched) this.#arrive(group.start);
		this.#produce(group.start);
		// A newer group demotes the live edge to an evictable one.
		const edge = this.#edgeEntry();
		this.#state.latest = max(this.#state.latest ?? 0n, group.start);
		if (edge && edge !== entry && !this.#protects(edge)) this.#account.demote(edge);
		this.#account.insert(entry);
		for (const sink of this.#sinks) {
			const mirror = succeeds?.kept?.get(sink);
			// Continued, it stays the sink's copy of the group, so the sink never holds two.
			if (mirror && mirror.closed.peek() === undefined) {
				entry.mirrors.set(sink, mirror);
				continue;
			}
			if (mirror) unlinkMirror(sink, mirror);
			this.#mirror(entry, sink);
		}
		// Follow the group's frames as they land, past the head `#advance` already named.
		const stop = hooks.groupProgress(group, (total) => {
			if (total > hooks.groupOffset(group)) this.#advanceObject({ group: group.start, frame: total - 1 });
			// A frame can change an owed hole's reach or drift edge without adding a group.
			for (const sink of this.#sinks)
				if (sink.waitingOrdered) sink.timelineChanged.update((revision) => revision + 1);
		});
		void group.closed.then((closed) => {
			stop();
			for (const sink of this.#sinks)
				if (sink.waitingOrdered) sink.timelineChanged.update((revision) => revision + 1);
			if (closed === null) this.#finishLargest(group.start);
			// A failed fill answer holds no end open (see #settled), so the end may settle now, as the
			// Rust `poll_outcome` judges again once any copy closes.
			else if (fetched) this.#settle();
		});
		// Give held mirrors the new live edge before pruning their timeline entry,
		// so their latency guard can preserve a terminal expiry verdict.
		this.#evictExpired(false);
	}

	// Record a written key. It raises the largest too, at the head of an empty group, so
	// a later group below it never changes it. A group (`answers`) at or past the next free
	// key answers a pending refresh; anything else written there moves the key it
	// asks for.
	#advance(key: bigint, answers: boolean): void {
		const next = this.#received;
		const kept = this.#keptDrops();
		this.#received = max(next, key + 1n);
		if (this.#keptDrops() > kept) this.#rearmDrops(kept);
		this.#advanceLargest({ group: key, frame: 0 });
		this.#endLargest();
		if (this.#pending && key >= next && answers) {
			this.#endRefresh();
			this.#floor = max(this.#floor, key);
		}
		// Even unanswered, the first key written ends the ask for the first group.
		this.#reportWanted();
	}

	// `start` as a group key: its value at the track's timescale, rounded down, or the key of an
	// unscaled timestamp on a scale-less track. A timed start before the track's info is accepted
	// has no timescale to key it in.
	#keyOf(start: Timestamp): bigint {
		const info = this.#state.info.peek();
		if (info === undefined && start.scale !== undefined)
			throw new Error("a timed group start needs the track's timescale: accept() it first");
		const rescaled = info === undefined ? start : rescale(info, start);
		return rescaled.key ?? BigInt(rescaled.value);
	}

	// Hold a group to this track's timedness, once the track has committed it.
	#bind(group: GroupProducer): void {
		const info = this.#state.info.peek();
		if (info) {
			hooks.bindGroupTimed(group, info);
			hooks.bindGroupCompression(group, info.compression === "deflate");
		}
	}

	// Refuse a write once the track is closed, or at or past its declared end.
	#writable(key: bigint): void {
		if (this.#state.closed.peek() !== undefined) throw new Error("track is closed");
		const end = this.#state.end.peek()?.at;
		if (end !== undefined && key >= finalKey(end)) {
			throw new Error(`sequence ${key} is at or past the track's end ${end.group}/${end.frame}`);
		}
	}

	// Cap a group the end cuts, so it never holds a frame past the end.
	#cap(group: GroupProducer): void {
		const end = this.#state.end.peek()?.at;
		if (end !== undefined && end.frame !== 0 && end.group === group.start) hooks.limitGroup(group, end.frame);
	}

	/**
	 * Create the next group, starting at `start`: after every group this track holds. Mirrors the
	 * Rust `append_group`.
	 *
	 * A timed track converts `start` into its timescale, rounding down, as {@link writeFrame}
	 * does; a scale-less track takes {@link Timestamp.unscaled} at the group's key. A `start` that
	 * doesn't match the track, or at or before a group this track holds, throws
	 * {@link TimestampMismatch}, like a rewound frame.
	 *
	 * Appending says the producer will not go back, so it skips every start below the new group
	 * that it never produced, from just past its previous append (before the first, from the
	 * lowest group it created), as {@link skipGroups} would, replacing any drop there:
	 * consecutive appends name the span between their starts. Those starts are then final:
	 * creating one throws {@link Skipped}.
	 *
	 * The same broadcast, track, and group start always name the same content, and nothing
	 * checks it: the broadcast remembers nothing of an earlier producer of the name. A recreated
	 * track, or a restarted publisher keeping its instance, keeps its starts moving forward, as
	 * its media time does, or uses a new track name or instance. Reusing a start only corrupts
	 * this broadcast for its readers. Datagram keys name no cached content and are exempt.
	 *
	 * Its frames must match the track's {@link Info.timescale}: a scaled timestamp on every frame
	 * of a timed track, and on a scale-less one {@link Timestamp.unscaled} at the group's start,
	 * or the write throws {@link TimestampMismatch}. So does a first frame that starts at or
	 * before the group below it, or at or after the one above it: starts strictly increase.
	 */
	appendGroup(start: Timestamp): GroupProducer {
		return this.#appendAt(this.#keyOf(start));
	}

	// Append a group at `key`, past every group held, skipping what it leaves below.
	#appendAt(key: bigint): GroupProducer {
		if (key < this.#received) throw new TimestampMismatch();
		this.#writable(key);
		this.#refuseSkipped(key);
		const group = new GroupProducer(key);
		this.#bind(group);
		this.#cap(group);
		this.#publish(group);
		this.#settleAppend(key);

		return group;
	}

	/**
	 * Declare that this producer will never produce `[start, end)`: none will arrive, so a reader
	 * waiting on one moves past it, the declared end does not wait for them, and a relay serving
	 * this track forwards the skip (lite-07 SUBSCRIBE_SKIP). Unlike a drop, it ends no reader.
	 *
	 * This and {@link appendGroup} are the only sources of a skip: a gap between created groups
	 * says nothing, since groups may be created out of order, so only the producer knows a
	 * key will never exist. A skip is final: creating a group at a skipped key throws
	 * {@link Skipped}, and a later drop of it is ignored.
	 *
	 * A skip replaces any drop of the same keys, whatever its error: a reader the drop
	 * already ended stays ended, and every later reader and relay sees the skip. A group already
	 * cached keeps what it is, since it was produced after all. Mirrors the Rust `skip_groups`.
	 */
	skipGroups(start: bigint, end: bigint): void {
		checkKey(start);
		checkEnd(end);
		this.#skipReceived(start, end);
	}

	// `skipGroups`, returning the cached keys it left alone: a skip naming a group that
	// arrived contradicts it, which a subscriber reports.
	#skipReceived(start: bigint, end: bigint): Run[] {
		if (this.#state.closed.peek() !== undefined) return [];
		const cached: Run[] = [];
		for (const key of this.#cached.keys()) {
			if (key >= start && key < end) insertRun(cached, key, key + 1n);
		}
		this.#skipGaps(cached, start, end);
		return cached;
	}

	// Skip what of `[start, end)` neither `kept` nor an earlier skip names, replacing any drop.
	#skipGaps(kept: readonly Run[], start: bigint, end: bigint): void {
		const skipped = this.#state.drops.overlapping(start, end).filter((run) => run.skip);
		for (const gap of runGaps(kept, start, end)) {
			for (const open of runGaps(skipped, gap.start, gap.end)) {
				this.#drop(open.start, open.end, skipHole());
			}
		}
	}

	// Record a key this track's own publisher produced, for the next append to skip around.
	#produce(key: bigint): void {
		if (this.#wire || (this.#appended !== undefined && key < this.#appended)) return;
		insertRun(this.#produced, key, key + 1n);
	}

	// Settle an append at `key`: skip every key below it this track's own publisher
	// never produced, from just past its previous append (before the first, from the lowest it
	// produced), since appending says it will not go back. The skip replaces a drop given there,
	// as `skipGroups` does.
	#settleAppend(key: bigint): void {
		const from = this.#appended ?? this.#produced[0]?.start;
		if (from !== undefined) {
			const kept = [...this.#produced];
			for (const cached of this.#cached.keys()) {
				if (cached >= from && cached < key) insertRun(kept, cached, cached + 1n);
			}
			this.#skipGaps(kept, from, key);
		}
		this.#produced = [];
		this.#appended = key + 1n;
	}

	// Refuse a key the publisher declared skipped: a skip is final.
	#refuseSkipped(key: bigint): void {
		if (this.#state.drops.at(key)?.skip) throw new Skipped(key);
	}

	/**
	 * Create a new group with the given sequence number.
	 *
	 * It says nothing about the keys around it: groups may be created out of order, and an
	 * aborted or evicted key created again. A gap it leaves is skipped only once the
	 * producer says it will not fill it, with {@link skipGroups} or {@link appendGroup}. A
	 * key already skipped throws {@link Skipped}, and the reserved key 2^64-1 a
	 * `RangeError`.
	 *
	 * Throws on a key that is still cached: a live duplicate would fan out to every
	 * subscriber twice. An aborted incarnation is evicted so a fresh group can serve the
	 * key again. Best effort (mirrors Rust): nothing remembers a key whose cache
	 * entry is already gone, so a long-evicted key is accepted as new. Its frames are
	 * checked like {@link appendGroup}'s.
	 */
	createGroup(key: bigint): GroupProducer {
		return this.#createGroup(key, 0);
	}

	#createGroup(key: bigint, start: number, fetched = false): GroupProducer {
		this.#writable(key);
		// A skip says the key will never be produced, so it is final. A drop is not: a
		// publisher may re-create a group it cut or aborted.
		this.#refuseSkipped(key);
		const existing = this.#cached.get(key);
		let kept: Map<TrackState, GroupConsumer> | undefined;
		const failed = existing?.group.closed.peek() instanceof Error ? existing.group : undefined;
		if (existing && !failed && start === 0 && hooks.groupHeadless(existing.group)) {
			// A copy from frame 0, a fill a bounded reader asked or the feed's own, replaces one that
			// can't serve its head. One still arriving keeps arriving for the readers already holding
			// it, outside the cache.
			this.#unlink(existing);
			this.#uncache(existing);
		} else if (existing) {
			if (!failed) {
				throw new Error(`duplicate group: key=${key}`);
			}
			// A mirror the failed copy kept open, held for a duplicate, is the new copy's to continue.
			for (const [sink, mirror] of existing.mirrors) {
				if (mirror.closed.peek() !== undefined || !hooks.readsMirrored(mirror)) continue;
				kept ??= new Map();
				kept.set(sink, mirror);
				existing.mirrors.delete(sink);
			}
			// Subscribers may have seen the aborted incarnation's start, so it stays a watermark.
			this.#evict(existing);
			this.#uncache(existing, true);
		}

		const group = new GroupProducer(key);
		// Mirrors copy the absolute indices, so set them before publishing to any sink.
		hooks.startGroup(group, start);
		this.#bind(group);
		this.#cap(group);
		this.#publish(group, fetched, failed && { failed, kept });
		return group;
	}

	// Whether this track's timestamps may rewind (see {@link Info.unchecked} and {@link Info.ietf}).
	#unchecked(): boolean {
		const info = this.#state.info.peek();
		return info?.unchecked === true || info?.ietf !== undefined;
	}

	/**
	 * Declare the track's exclusive end, possibly ahead of the live edge, mirroring the Rust
	 * `finish_at`.
	 *
	 * `end` is the first position that will never be produced: a track whose last group is
	 * 89 finishes at `{ group: 90, frame: 0 }`, and `{ group: 89, frame: 5 }` also caps group
	 * 89 at frames 0 to 4. Groups and frames below it are still accepted; anything
	 * at or past it is refused. Unlike {@link close} it is not terminal: call `close()` once
	 * the remaining groups are written. Readers end once the live edge reaches the end's
	 * group, independently of closure. Throws if the track is closed, already has an end, or
	 * `end` would orphan a group or frame already produced.
	 *
	 * Always a clean end; {@link fail} ends the track in error.
	 */
	finishAt(end: Position): void {
		if (this.#state.closed.peek() !== undefined) throw new Error("track is closed");
		const declared = this.#state.end.peek()?.at;
		if (declared !== undefined) throw new Error(`track already ends at ${declared.group}/${declared.frame}`);
		this.#declareEnd(end, undefined, false);
	}

	// The wire's end: repeating it is a no-op, and a mid-group end may refine the group
	// boundary it falls just before. Either must carry the same error code. Mirrors the Rust
	// `finish_at` on a `received()` copy. Only a wire subscription owes a tail below the end: a
	// FETCH's end leaves nothing in flight, as the Rust `set_end` a fill declares. An end that
	// lands after the track closed, such as a viewer releasing it while the marker was in
	// flight, has nothing left to end.
	#endPending(end: Position, error?: Error): void {
		const declared = this.#state.end.peek();
		const code = (err?: Error) => (err === undefined ? undefined : toStreamCode(err));
		const tail = this.#wire !== undefined;
		if (declared === undefined) {
			if (this.#state.closed.peek() !== undefined) return;
			this.#declareEnd(end, error, tail);
		} else if (code(declared.error) !== code(error)) {
			throw new ProtocolViolation(`conflicting track end error: ${error}`);
		} else if (declared.at.group !== end.group || declared.at.frame !== end.frame) {
			if (declared.at.frame !== 0 || end.frame === 0 || finalKey(end) !== declared.at.group) {
				throw new ProtocolViolation(`conflicting track end: ${end.group}/${end.frame}`);
			}
			this.#declareEnd(end, error, declared.tail || tail);
		}
		this.#owe();
	}

	// Record that the live feed accounted for `[start, end)`, then fold the gaps that aged out.
	#arrive(start: bigint, end = start + 1n): void {
		const at = now();
		this.#arrivals.account(start, end, at);
		// Only a gap folds, so an in-order feed never reads the subscription.
		if (this.#arrivals.hasGaps()) this.#arrivals.expire(at, this.#grace());
		this.#owe();
	}

	// The grace the session's tail uses, from the max delay the track's subscribers ask for,
	// bounded by how long the publisher keeps a superseded group fresh. The demand asked upstream is not.
	#grace(): Duration {
		const update = this.#state.update.peek();
		const maxDelay = Duration.millis(update ? maxDelayOf(update) : 0);
		const maxAge = this.#state.info.peek()?.maxAge;
		return grace(maxAge == null ? maxDelay : maxDelay.min(maxAge));
	}

	// Tell every reader whether the pending tail still owes a group below the end.
	#owe(): void {
		const end = this.#state.end.peek();
		if (!end?.tail) return;
		const owed = this.#owes(finalKey(end.at));
		for (const state of [this.#state, ...this.#sinks]) state.owed.set(owed);
		this.#endLargest();
	}

	// Settle the outcome cleanly once the declared end is reached and settled. Also run before a
	// write that could reopen it (a late group below the end, a fresh grace for the tail): that
	// write is still served, but the clean outcome stays final. Mirrors the Rust `latch_settled`.
	#settle(): void {
		if (this.#state.outcome.peek() === undefined && this.#settled())
			this.#state.outcome.set(this.#state.end.peek()?.error ?? null);
	}

	// Whether the pending tail misses a key from where the feed starts up to `final`: one
	// no live group delivered, in a gap younger than the grace. Judged by the
	// arrivals, not the cache, so an evicted group counts as delivered.
	//
	// The feed starts at the declared start while the demand keeps the floor its SUBSCRIBE asked
	// for, since an update gets no fresh START: a moved floor stands on its own, and moving
	// back restores the START. Without either, the first arrival stands in for it.
	// Until the START arrives, the feed start is pending; the requested floor still bounds it.
	#feedStart(): bigint | "pending" | undefined {
		const ranges = this.#state.update.peek()?.ranges;
		const requested = ranges && wireSpan(ranges).start;
		const served = this.#wire?.served;
		if (!served || served.requested !== requested) return requested;
		if (served.start === null) return requested;
		if (served.start !== undefined) return served.start;
		// A closed track never gets its START.
		return this.#state.closed.peek() === undefined ? "pending" : requested;
	}

	// The lowest group a reader whose ranges name `named` (undefined for one reaching back to
	// the oldest group) asks a fill for, or undefined for no bound. Below a start resolved from
	// the budget, each reader fills what its own budget reaches; otherwise only what its ranges
	// name; and never on a wire whose fills cannot judge a group by that budget. Mirrors the
	// Rust `Misses::judge`. Fetches are never bounded by it.
	#fillFloor(named: bigint | undefined): bigint | undefined {
		const wire = this.#wire;
		if (!wire) return undefined;
		const feed = this.#feedStart();
		const start = feed === "pending" ? wire.served?.requested : feed;
		if (start === undefined || start === 0n) return undefined;
		if (wire.starts.fillsFromStart) return start;
		// The START applies only while the demand keeps the floor it answered, as `#feedStart`.
		const ranges = this.#state.update.peek()?.ranges;
		const served = wire.served;
		const resolved =
			wire.starts.resolvesStart === true &&
			typeof served?.start === "bigint" &&
			served.requested === (ranges && wireSpan(ranges).start);
		if (resolved) return undefined;
		return named === undefined || named >= start ? start : undefined;
	}

	#owes(final: bigint): boolean {
		if (!this.#wire || !this.#state.end.peek()?.tail) return false;
		const feed = this.#feedStart();
		const floor = (feed === "pending" ? this.#wire?.served?.requested : feed) ?? this.#arrivals.first();
		if (floor === undefined) return final > 0n;
		return !this.#arrivals.covers(min(floor, final), final);
	}

	#declareEnd(end: Position, error: Error | undefined, tail: boolean): void {
		const { group, frame } = end;
		if (typeof group !== "bigint" || group < 0n || !Number.isSafeInteger(frame) || frame < 0) {
			throw new RangeError(`invalid track end: ${group}/${frame}`);
		}
		const final = finalKey(end);
		if (final < this.#received) {
			throw new ProtocolViolation(`track end ${group}/${frame} is below the next sequence ${this.#received}`);
		}
		const cut = frame === 0 ? undefined : this.#cached.get(group)?.group;
		if (cut) hooks.limitGroup(cut, frame);
		const declared: End = { at: { group, frame }, error, tail };
		for (const state of [this.#state, ...this.#sinks]) state.end.set(declared);
		this.#endLargest();
	}

	// Group `key` holds every frame it will have: it closed cleanly, its stream finished.
	// A group name fixes its content, so no legal write raises the largest past it.
	#finishLargest(key: bigint): void {
		if (this.#state.largest.peek()?.group === key)
			for (const state of [this.#state, ...this.#sinks]) state.largestFinished = true;
		this.#endLargest();
	}

	// The largest group may still grow until it closes cleanly; an aborted one can be recreated.
	get #largestOpen(): boolean {
		return this.#state.largest.peek() !== undefined && !this.#state.largestFinished;
	}

	// End the largest stream while the track stays open, once it can no longer advance: the
	// declared end is reached, and the largest group's content is final. Every key below
	// the end raised it already, so neither a late group below it nor a recreated copy of it
	// moves it after this.
	#endLargest(): void {
		this.#settle();
		if (this.#state.largestEnded.peek() || this.#largestOpen) return;
		const end = this.#state.end.peek();
		if (end === undefined || !reaches(this.#received, this.#state.drops, end) || this.#owes(finalKey(end.at)))
			return;
		for (const state of [this.#state, ...this.#sinks]) state.largestEnded.set(true);
	}

	/**
	 * Declare the track's end after the highest key written, returning it as {@link Finished}.
	 * A no-op once an end is declared or the track closed, so a cleanup path can finish whatever
	 * the track's state. A track that closed first keeps its outcome and declares no end, so its
	 * readers' `finished()` rejects.
	 *
	 * Not terminal: groups below the end may still be written. Readers end once the live edge
	 * reaches it.
	 */
	finish(): Finished {
		if (this.#state.end.peek() === undefined && this.#state.closed.peek() === undefined)
			this.finishAt({ group: this.#received, frame: 0 });
		return this;
	}

	/**
	 * End the track with `error` after the highest key written, the end {@link finish}
	 * declares, returning it as {@link Finished}. Mirrors the Rust `fail`.
	 *
	 * Unlike {@link abort}, the track keeps its history: readers still get every group below the
	 * end, and a reader reaching the end gets `error` instead of the end of stream. Like
	 * {@link finish}, a no-op once an end is declared or the track closed: the outcome already
	 * given stands, so a cleanup path can fail whatever the track's state.
	 */
	fail(error: Error): Finished {
		if (this.#state.end.peek() === undefined && this.#state.closed.peek() === undefined)
			this.#declareEnd({ group: this.#received, frame: 0 }, error, false);
		return this;
	}

	/**
	 * Abort the track and every subscriber, mirroring the error to their open groups. Terminal
	 * and idempotent.
	 *
	 * Subscribers still draining get the finished groups first, then the error. An abort after
	 * the declared end settled (reached, with every group below it finished) only releases the
	 * track: readers end cleanly. The groups left behind still age out on the idle cache
	 * window, so a stale subscriber can't pin them.
	 */
	abort(error: Error): void {
		this.#close(error);
	}

	/**
	 * Release this producer, like dropping the last Rust producer. Idempotent.
	 *
	 * With an end declared ({@link finish}, {@link finishAt}, or {@link fail}), the track ends
	 * there and its subscribers drain to it. Without one, the track aborts: nothing finished it.
	 */
	close(): void {
		if (this.#state.end.peek() !== undefined) this.#close(undefined);
		else this.#close(new StreamError(StreamCode.Cancel, { message: `track ${this.name} closed without finish` }));
	}

	// `abort` is undefined for a clean close, which keeps whatever end was declared. A received
	// copy released because nobody wants it closes cleanly without one: a dead route, not finished
	// content.
	#close(abort: Error | undefined) {
		if (this.#state.closed.peek() !== undefined) return;
		// A gap past the grace was lost before the abort rather than cut off by it.
		if (abort && this.#state.end.peek()?.tail) this.#arrivals.expire(now(), this.#grace());
		if (abort && (this.#state.outcome.peek() === null || this.#settled())) abort = undefined;
		// Nobody will finish these, so a subscriber that has not taken one yet never sees it.
		// Not evicted: a reader already holding one keeps its frames and sees the abort.
		const open = [...this.#cache].filter((entry) => entry.group.closed.peek() === undefined);
		const kept = this.#keptDrops();
		this.#refreshTimer?.abort.abort();
		this.#refreshTimer = undefined;
		this.#endRefresh(abort ?? this.#closedBeforeSent());
		closeTrackState(this.#state, abort);
		if (this.#keptDrops() > kept) this.#rearmDrops(kept);
		if (this.#state.outcome.peek() === undefined)
			this.#state.outcome.set(abort ?? this.#state.end.peek()?.error ?? null);
		// Closing a copy never supplies the missing tail of a group. Already finished groups
		// ignore the abort and stay readable; unfinished ones retain their truncation cause.
		const groupError =
			abort ??
			new StreamError(StreamCode.Cancel, { message: `track ${this.name} closed with unfinished groups` });
		for (const { group } of this.#cache) group.abort(groupError);
		for (const entry of open) {
			this.#unlink(entry);
			this.#uncache(entry);
		}
		for (const sink of this.#sinks) {
			for (const group of sink.groups.peek()) hooks.releaseGroupReader(group, abort);
			closeTrackState(sink, abort);
		}
		this.#sinks.clear();
		// The former live edge becomes evictable too, its idle window restarted, so the close
		// never reclaims a group its subscribers have yet to drain.
		const edge = this.#edgeEntry();
		if (edge) {
			edge.charge.touch();
			this.#account.demote(edge);
		}
		this.#evictExpired(true);
	}

	// Whether the declared end was reached, the pending tail owes nothing below it, and every
	// cached group below it finished, so the track already holds everything it promised. An
	// abort over a group the tail still owed cut it off, so readers get the error rather than
	// a clean end without it. A failed fill answer holds nothing open, since the feed never
	// promised it. Mirrors the Rust `is_settled`.
	#settled(): boolean {
		const end = this.#state.end.peek();
		if (end === undefined) return false;
		const final = finalKey(end.at);
		if (!reaches(this.#received, this.#state.drops, end) || this.#owes(final)) return false;
		for (const { group, fetched } of this.#cache) {
			if (group.start >= final) continue;
			const closed = group.closed.peek();
			if (closed !== null && !(fetched && closed instanceof Error)) return false;
		}
		return true;
	}

	/**
	 * Append a group holding one frame at its timestamp, which is where the group starts, as
	 * {@link appendGroup} does: on a scale-less track, {@link Timestamp.unscaled} at the group's
	 * key. A timestamp at or before a group this track holds, or that doesn't match the track,
	 * throws {@link TimestampMismatch} before any group exists. The group closes after its
	 * frame, since nothing says where it ends. Mirrors the Rust `write_frame`.
	 */
	writeFrame(frame: Frame) {
		const group = this.appendGroup(frame.timestamp);
		try {
			group.writeFrame(frame);
		} catch (err) {
			// Abort the group so no subscriber waits on a group that will never get its frame.
			group.abort(err instanceof Error ? err : new Error(String(err)));
			throw err;
		}
		group.finish();
	}
}

// One read pass's view of the budget and the live edge; see Subscriber.#drift.
type Drift = {
	budget: number;
	// The budget is the track's max age, which no widening passes: its verdicts are final.
	bounded: boolean;
	// The live edge: its group, and the newest time it presents, in milliseconds. A scale-less
	// track's bare starts count as milliseconds, as the Rust `age` reads them.
	presentation?: { key: bigint; at: number };
	end?: bigint;
};

// An ordered read's state, mirroring the Rust `Ordered`: a group read and held back because a gap
// below it goes first, the gaps found and not returned yet (sorted and disjoint), and one past
// the highest key returned, as a group or the end of a gap.
type Items = { held?: GroupConsumer; gaps: DropRun[]; at: bigint };

// A group that landed after an ordered read passed its key.
function late(): Hole {
	return { error: new StreamError(StreamCode.Old, { message: "group landed after the read passed it" }) };
}

// A group missing its head that no fill could serve.
function headless(): Hole {
	return { error: new StreamError(StreamCode.NotFound, { message: "group head never served" }) };
}

// The successor a handed-out group was last judged by, so the group stays judgeable once the
// cache evicts both. Each reader keeps its own, so the memory is bounded by the groups held.
type Superseded = { successor?: Successor };

// A successor, when it arrived, and its copy's fate (see `hooks.groupFate`). Not the consumer:
// a handed-out successor's own record would retain the next one, chaining every later group to
// the oldest held.
type Successor = { key: bigint; arrived: Instant; fate: () => Error | undefined };

/**
 * How a reader reported a run, and what its wire was told. Mirrors the Rust `record::Report`:
 * `settled` is a group served, or a drop on a version whose drops are all final, and a skip
 * replaces a drop.
 *
 * @internal
 */
export interface Reported extends Run {
	kind: "settled" | "dropped" | "skip";
}

const sameReport = (a: Reported, b: Reported) => a.kind === b.kind;

// Whether `reported` stops `hole` going out over it: only a skip goes out over a drop, which it
// replaces. Mirrors the Rust `Report::blocks`.
function blocksHole(reported: Reported, hole: Hole): boolean {
	return reported.kind !== "dropped" || !hole.skip;
}

/**
 * What a lite publisher told its wire about a reader's keys, kept in the reader's own
 * record so the cursor's drops and the publisher's groups consult one map. Mirrors the
 * publisher half of the Rust `record::Record`.
 *
 * @internal
 */
export interface WireRecord {
	/** Whether the wire was told of `key`: served, or given up. */
	told(key: bigint): boolean;
	/** The parts of `[start, end)` the wire was told nothing of. */
	untold(start: bigint, end: bigint): Run[];
	/** Record that the wire was told `report` of `[start, end)`. */
	tell(start: bigint, end: bigint, report: Omit<Reported, "start" | "end">): void;
	/** Forget what was recorded of `[start, end)` without going out on the wire. */
	untell(start: bigint, end: bigint): void;
}

/**
 * A track its producer finished: its end is declared unless it closed first, and it can still
 * take an older group below that end, be read, or be aborted or closed, but not append. {@link Producer.finish} returns it,
 * mirroring the Rust `track::Finished`; the producer itself still refuses a group at or past the
 * end.
 *
 * @public
 */
export interface Finished {
	/** The track name. */
	readonly name: string;
	/** Settles once the track's outcome is final; see {@link Producer.closed}. */
	readonly closed: GetPromise<Error | null>;
	/** The track's info, once accepted. */
	info(): Promise<Info>;
	/** Create an older group below the end, which arrived late; see {@link Producer.createGroup}. */
	createGroup(key: bigint): GroupProducer;
	/** Serve the groups readers miss below the end; see {@link Producer.requests}. */
	requests(): Requests;
	/** Subscribe to this in-process track; see {@link Producer.subscribe}. */
	subscribe(options?: Subscription): Subscriber;
	/** Abort the track; see {@link Producer.abort}. */
	abort(error: Error): void;
	/** Release the track, like dropping the last Rust handle; see {@link Producer.close}. */
	close(): void;
}

/**
 * The read side of a live track subscription, mirroring the Rust `Subscriber`.
 *
 * Obtained from `Broadcast.Consumer.subscribe` / `Track.Consumer.subscribe`, or from
 * {@link Producer.subscribe} for an in-process track. Reads the groups a
 * {@link Producer} on the same underlying state writes.
 */
export class Subscriber {
	/** The track name. */
	readonly name: string;

	#state: TrackState;
	// Reads its routes' copies instead of `#state`'s buffer: the counterpart of the Rust
	// `Inner::Resume`. `#state` then only carries the reader's own preferences, info, and close.
	#resume?: ResumeSource;
	// Every group key this reader handed out, so a re-created group or one a second route
	// delivers is handed out once.
	#delivered = new Delivered();
	// Cached groups the arrival cursor skipped as stale, each with the budget it was judged
	// under, so a wider budget offers them again. Only a widening reverses a stale verdict, and
	// an entry is kept only while its group is cached, since nothing could offer it again.
	#skipped = new Map<bigint, number>();
	// At most the narrowest budget in #skipped, so a read whose budget did not widen skips the scan.
	#narrowest = Number.POSITIVE_INFINITY;
	// This reader closed itself, so lowering its floor offers nothing: a producer's clean close
	// alone leaves the cache readable, but the reader leaving must stay terminal. Leaving also
	// settles the shared `#state.closed`, so a guard for either close reads that alone.
	#left = false;
	// What {@link largest} last reported, and whether it ended: moving the bounds afterwards
	// does not bring it back.
	#largestSeen?: Position;
	#largestDone = false;
	#nextKey = 0n;
	// A wire publisher's reader parks past the end of a bounded window rather than end there:
	// its peer would read the end of the stream as the track's. Set through `hooks.parked`.
	#parks = false;
	// The local bounds {@link withRange} set.
	#local: { start: bigint; end?: bigint } = { start: 0n };
	// This reader's own ranges resolved to the groups it reads, whatever another subscriber
	// widens the aggregate to. Assigned in the constructor.
	#window!: Window;
	// Bumped whenever the window moves, so the drop scan knows to look again.
	#revision = 0;
	// The hull of what this reader reads now: its local bounds narrowed to the window's floor
	// and end. A group inside it is read only if the window contains it too.
	#cursor = new Signal<{ start: bigint; end?: bigint }>({ start: 0n });
	// A FETCH's own budget in place of the subscription's; unlike SUBSCRIBE's zero
	// default, an omitted one is unbounded, leaving the publisher's maxAge as the only bound.
	#fetchBudget?: number;
	// The first frame a fetch asks of its group: a copy starting past it lacks its head.
	#fetchFrom = 0;
	// Which cursor owns this subscription. Both cursors draw from one buffer, so the
	// first group read commits the track to arrival order and {@link ordered} commits
	// it to sequence order; whichever wins is the only one allowed from here on.
	#mode?: "arrival" | "ordered";
	// What this cursor reports as drops: keys given up and not reported yet,
	// the track's drops last folded in, and every key already reported. Drops are
	// persistent track state, so they are tracked here rather than removed for everyone.
	#pending: DropRun[] = [];
	#dropsSeen?: {
		// The track's drops revision folded in: later changes fold in alone.
		drops: number;
		refused: readonly DropRun[];
		start: bigint;
		end?: bigint;
		revision: number;
		budget: number;
		coverage?: Coverage;
	};
	#reported = new Runs<Reported>(sameReport);
	// Everything below was settled for good and folded out of #reported: handed out, told, or
	// passed, so nothing there is reported again. Mirrors the Rust `Record::floor`, which folds
	// under pool pressure; a key-order read folds at where it is instead (see #fold).
	#floor = 0n;
	// A wire publisher reads #reported as what its wire was told, so it never folds.
	#wired = false;
	// What an ordered read owes before its next group; unset before its first read. See #tryItem.
	#items?: Items;
	// The group the frame-level helpers are currently draining, acquired through the
	// key cursor so frame reads and {@link Ordered.nextGroup} share one floor.
	#frameGroup?: GroupConsumer;
	// The earliest wall deadline a held read armed, and its timer, which bumps `revision`
	// when it comes due. See #wakeAt. Kept apart from the subscriber so a pending timer
	// retains only this, never the subscriber or its track state, once no read waits.
	#wall: { revision: Signal<number>; at: number; timer?: AbortController } = {
		revision: new Signal(0),
		at: Number.POSITIVE_INFINITY,
	};
	// When each group the timeline held at close arrived, failed ones left out. A group handed
	// out earlier can outlive this subscriber, as one a resumed reader still writes from a copy
	// it let go of, and its budget still runs from its successor's arrival.
	#arrivals?: { key: bigint; arrived: Instant }[];
	// The misses this reader asked a fill for, each judged once, and those asked again after the
	// answer was evicted before this reader took it, which is never repeated: a slow reader's
	// refills cannot evict each other without bound. Mirrors the Rust `Misses`.
	#asked: Run[] = [];
	#refilled: Run[] = [];
	// Fill-owed runs this reader gave up once its budget convicted them: never wanted again, so
	// the fills withdraw them and their handlers hear, as the Rust `Misses` gives up a stale run.
	#givenUp: Run[] = [];
	// The heads this reader asked a fill for, each once: one met again with no fill bringing it
	// is given up, as the Rust `Heads` gives up a head whose fetch ended without one. Clipped to
	// what it still reads, so a widening back over one asks it afresh.
	#heads: Run[] = [];
	// The keys the fill probe has yet to judge, and the terms it last judged under: each is
	// judged once, then again only once a change reopens it (see #reopen), as the Rust `Misses`
	// keeps `judged`. A run the live feed still owes stays unjudged until the feed gives it up.
	#unjudged: Run[] = [{ start: 0n, end: UNBOUNDED }];
	#judgedAt?: {
		producer: Producer;
		budget: number;
		coverage: Coverage;
		opened: number;
		drops: number;
		edge: bigint;
	};
	// The received stale runs that do not settle this reader, and the terms they were judged
	// under; see #unknownRuns.
	#unknown?: {
		drops: number;
		budget: number;
		coverage: Coverage;
		runs: DropRun[];
	};
	// The last read waited on a fill, so the staleness deadline it armed wakes the next one: a
	// fill that never answers cannot pin a reader in either order.
	#fillWait = false;
	// Bumped whenever the fills this reader joined are rejudged, which may withdraw groups it reads
	// again by the time it looks (a narrowing undone in one turn moves no signal), so it asks anew.
	#fillMoves = new Signal(0);
	// The time starts this reader asked its fills to look up, each with its fill, absent when no
	// handler could take it.
	#lookups: { time: bigint; lookup?: Lookup }[] = [];

	#drift(): Drift {
		// A fetch judges its group against the whole track, as the Rust `fetch_group` does: a
		// newer group outside its one-group window still supersedes it.
		const end = this.#fetchBudget === undefined ? this.#cursor.peek().end : undefined;
		const timeline = this.#state.timeline;
		let presentation: Drift["presentation"];
		const info = this.#state.info.peek();
		// The edge wants the newest content that exists, so it takes the newest stamped group's
		// latest frame: walk back from the cap to the first one. A scale-less track's frames carry
		// no timestamp, so its edge is the newest group's start.
		const scaleless = info !== undefined && info.timescale == null;
		for (let i = (end === undefined ? timeline.length : timelineIndex(timeline, end)) - 1; i >= 0; i--) {
			const group = timeline[i];
			if (hooks.groupFailed(group) || !this.#judges(group.start)) continue;
			if (scaleless) {
				if (!hooks.groupStarted(group)) continue;
				presentation = { key: group.start, at: Number(group.start) };
				break;
			}
			const timestamp = hooks.groupTimestamp(group);
			if (timestamp === undefined) continue;
			presentation = { key: group.start, at: (hooks.groupLatest(group) ?? timestamp).asMillis() };
			break;
		}

		const update = this.#state.update.peek();
		const requested = this.#fetchBudget ?? (update ? maxDelayOf(update) : 0);
		const fresh = info?.maxAge?.asMillis();
		return {
			budget: fresh === undefined ? requested : Math.min(requested, fresh),
			bounded: fresh !== undefined && requested >= fresh,
			presentation,
			end,
		};
	}

	// The group after `key` that bounds it: the first servable one below `end` that
	// counts as content (its first frame landed, or it ended), stamped or not.
	//
	// Its first timestamp is the furthest presentation time `key` could still reach.
	// An upper bound, deliberately: a frame's duration is not on the wire, so a group's own
	// last timestamp says where it starts presenting, not where it ends. A group whose header
	// landed without a frame is not content yet, so the next one is the bound; an unstamped
	// successor leaves the reach unbounded.
	#successor(key: bigint, end?: bigint): GroupConsumer | undefined {
		return this.#after(key, end, hooks.groupStarted);
	}

	// The group after `key` that starts its wall clock: like {@link #successor}, but
	// counting a group before its first frame. Its header arrived, so a relay ages the group
	// before it as the origin does, even if its first frame never comes.
	#arrival(key: bigint, end?: bigint): GroupConsumer | undefined {
		return this.#after(key, end, () => true);
	}

	// Whether group `key` counts toward the drift edge: inside the window, or anywhere for a
	// fetch.
	#judges(key: bigint): boolean {
		return this.#fetchBudget !== undefined || this.#window.contains(key);
	}

	// The first servable group after `key` below `end` that `counts`.
	#after(key: bigint, end: bigint | undefined, counts: (group: GroupConsumer) => boolean): GroupConsumer | undefined {
		const timeline = this.#state.timeline;
		for (let i = timelineIndex(timeline, key + 1n); i < timeline.length; i++) {
			const successor = timeline[i];
			if (end !== undefined && successor.start >= end) break;
			if (hooks.groupFailed(successor) || !this.#judges(successor.start)) continue;
			if (counts(successor)) return successor;
		}
		return undefined;
	}

	// Whether the drift budget says to give up on `group`.
	//
	// Presentation time measures a group by how far it could still reach, not by how far
	// behind it started. Being behind is survivable: priority transmits newer groups first,
	// so a backlog bursts at whatever rate is left over and closes the gap faster than the
	// live edge advances. A group is abandoned only once everything it could still present
	// falls outside the budget.
	//
	// A group's reach is bounded by its nearest successor: it cannot present past where the
	// next group begins. Its own frames say nothing, since a frame's duration is not on the
	// wire, and the candidate needs no timestamp of its own: an empty group is bounded by
	// its stamped successor the same way. The bound is exclusive, so the comparison is `>=`:
	// the freshest frame a group could still hold sits just below its reach, so an age equal
	// to the budget already puts every frame in it strictly past the budget. A zero budget
	// falls out for free.
	//
	// The wall clock judges it too: a group is also stale once its successor arrived a full
	// budget ago, so a peer whose timestamps stop advancing, an untimed track, or an empty
	// successor that never presents cannot pin a superseded group. The successor is re-read,
	// not latched, like the reach, except by a reader holding the group: it passes `held`, so
	// the wall clock outlives the successor's eviction (see {@link #supersededAt}). The newest
	// group has no successor, so neither clock ever convicts it. `wake` arms a timer for the
	// wall deadline, for a held read that waits on it.
	//
	// Judged by its successor alone, so the group need not be in this track: evicted, held
	// from another copy, or outliving this subscriber. Once closed, only the wall clock judges
	// it, from the arrivals kept at close.
	#staleAt(key: bigint, drift: Drift, wake = false, held?: Superseded): boolean {
		const arrival = this.#arrivals ? undefined : this.#arrival(key, drift.end);
		let arrived = this.#arrivals
			? this.#keptSupersededAt(key, drift, held)
			: this.#supersededAt(arrival, drift, held);
		// A group finished whole ages from its own end without a successor.
		if (arrived === undefined && !this.#arrivals) arrived = this.#endedAt(key);
		if (arrived === undefined) return false;
		// Plain arithmetic: an unenforced budget is infinite, which no duration brand holds.
		const deadline = arrived.since(Instant.ZERO).asNanos() + drift.budget * 1_000_000;
		if (now().since(Instant.ZERO).asNanos() >= deadline) return true;
		if (wake) this.#wakeAt(deadline);
		// A closed subscriber keeps no timeline to bound the reach with.
		if (this.#arrivals) return false;

		const reach = this.#reach(key, drift.end);
		return (
			drift.presentation !== undefined &&
			drift.presentation.key > key &&
			reach !== undefined &&
			drift.presentation.at - reach >= drift.budget
		);
	}

	// The cached group starting at `key`, failed or not.
	#held(key: bigint): GroupConsumer | undefined {
		const timeline = this.#state.timeline;
		const group = timeline[timelineIndex(timeline, key)];
		return group?.start === key ? group : undefined;
	}

	// A start or a group's end, in milliseconds of presentation time: a scale-less track's bare
	// units count as milliseconds. Mirrors the Rust `TrackState::presented` and `age`.
	#presented(value: bigint): number | undefined {
		const info = this.#state.info.peek();
		if (info === undefined) return undefined;
		const scale = info.timescale;
		return scale == null ? Number(value) : (Number(value) * 1000) / scale;
	}

	// The furthest presentation time, in milliseconds, group `key` could still reach: its own
	// end once finished whole, else where its immediate successor below `end` begins, undefined
	// when nothing proves where it stops. Mirrors the Rust `TrackState::reach`.
	#reach(key: bigint, end: bigint | undefined): number | undefined {
		const own = this.#held(key)?.end;
		if (own !== undefined) return this.#presented(own);
		const successor = this.#successor(key, end);
		if (!successor) return undefined;
		const info = this.#state.info.peek();
		if (info !== undefined && info.timescale == null) return this.#presented(successor.start);
		return hooks.groupTimestamp(successor)?.asMillis();
	}

	// When group `key`, finished whole, stops presenting on the wall clock: its arrival plus
	// its span, which stands in for a successor arriving, so the last group of a paused or
	// finished track ages out without one. Mirrors the Rust `TrackState::ended_at`.
	#endedAt(key: bigint): Instant | undefined {
		const group = this.#held(key);
		const end = group?.end;
		if (group === undefined || end === undefined || end <= key) return undefined;
		const span = this.#presented(end - key);
		return span === undefined ? undefined : hooks.groupArrived(group).checkedAdd(Duration.millis(span));
	}

	// When the wall clock starts: the cached `arrival`'s arrival, or that of the one `held`
	// recorded, unless a group landed between them since. The record outlives the successor's
	// eviction: otherwise an evicted successor restarts the clock at whichever newer group the
	// cache still holds, which a budget past the cache window never reaches. A recorded
	// successor that failed hands the clock on, as a re-read does, whether or not the cache
	// has evicted it yet.
	#supersededAt(arrival: GroupConsumer | undefined, drift: Drift, held?: Superseded): Instant | undefined {
		const arrived = arrival && hooks.groupArrived(arrival);
		if (!held) return arrived;
		const recorded = held.successor;
		if (
			recorded &&
			!failed(recorded) &&
			(drift.end === undefined || recorded.key < drift.end) &&
			(!arrival || arrival.start > recorded.key)
		) {
			return recorded.arrived;
		}
		if (!arrival || arrived === undefined) {
			held.successor = undefined;
		} else if (recorded?.key !== arrival.start || !recorded.arrived.eq(arrived) || failed(recorded)) {
			// Re-recorded only on a change, so judging the same successor again allocates nothing.
			held.successor = { key: arrival.start, arrived, fate: hooks.groupFate(arrival) };
		}
		return arrived;
	}

	// {@link #supersededAt} for a closed subscriber, with the arrivals kept at close standing in
	// for the timeline it cleared.
	#keptSupersededAt(key: bigint, drift: Drift, held?: Superseded): Instant | undefined {
		const covered = (candidate: bigint) => drift.end === undefined || candidate < drift.end;
		const kept = this.#arrivals?.find((entry) => entry.key > key && covered(entry.key));
		const recorded = held?.successor;
		if (recorded && !failed(recorded) && covered(recorded.key) && (!kept || kept.key > recorded.key)) {
			return recorded.arrived;
		}
		return kept?.arrived;
	}

	// Bump {@link #wall} at `at`, unless a wakeup is already armed sooner. Each held read
	// re-arms on the bump, so one timer covers every deadline this subscriber waits on.
	#wakeAt(at: number): void {
		const wall = this.#wall;
		if (!Number.isFinite(at) || at >= wall.at) return;
		wall.timer?.abort();
		const timer = new AbortController();
		// A staleness wakeup is never a reason to hold a Node/Bun process open.
		void sleepUntil(Instant.ZERO.saturatingAdd(Duration.nanos(at)), { signal: timer.signal, ref: false }).then(
			() => {
				if (timer.signal.aborted) return;
				wall.timer = undefined;
				wall.at = Number.POSITIVE_INFINITY;
				wall.revision.update((revision) => revision + 1);
			},
			() => {},
		);
		wall.timer = timer;
		wall.at = at;
	}

	#guard(group: GroupConsumer): GroupConsumer {
		// Handed out, it is read, whatever this reader judged it while buffered. Taking it is the
		// cache access buffering it is not, as in Rust: it restarts the group's idle window.
		hooks.groupReads(group, true);
		hooks.refreshGroup(group);
		const held: Superseded = {};
		hooks.onShedRead(group, () => {
			if (this.#state.latest === group.start && this.#state.producer)
				hooks.askProducer(this.#state.producer, "shed");
		});
		hooks.expireGroup(group, {
			expired: () => this.#staleAt(group.start, this.#drift(), true, held),
			changed: this.#expiryChanged(),
		});
		return group;
	}

	// What can change a drift verdict.
	#expiryChanged(): Getter<unknown>[] {
		return [
			this.#state.groups,
			this.#state.timelineChanged,
			this.#wall.revision,
			this.#state.update,
			this.#state.info,
			this.#cursor,
			this.#state.closed,
		];
	}

	// Record `key` as handed out. An ordered read never goes back, and any reader stops
	// expecting what is past its budget, so only the gaps within it cost memory.
	#handOut(key: bigint, ordered: boolean): void {
		const floor = this.#delivered.runEnd(0n) ?? 0n;
		if (ordered) this.#delivered.forgetBelow(key);
		this.#delivered.add(key);
		this.#skipped.delete(key);
		// Another copy of it buffered here is never handed out.
		this.#judge(key, key + 1n);
		if (this.#resume) this.#resume.forgetPastBudget(this.#delivered);
		else this.#forgetPastBudget(this.#delivered);
		// What it hands out, and forgets below it, it no longer wants from a fill: tell the fills
		// owing it there, so their handlers stop serving it.
		const forgot = this.#delivered.runEnd(0n) ?? 0n;
		if (forgot > floor) this.#stoppedReading(floor, forgot);
		if (key >= forgot) this.#stoppedReading(key, key + 1n);
		if (ordered) this.#fold(forgot);
	}

	// This reader stopped reading `[start, end)` for good: clip what it asked there, and judge
	// again only the fills owing it there, so a hand-out costs what it touches, not every fill.
	#stoppedReading(start: bigint, end: bigint): void {
		const asker = this.#state.asker;
		const producer = this.#state.producer;
		if (!asker || !producer) return;
		this.#reclip(this.#givenUp, start, end, (run) => this.#reads(run.start, run.end));
		this.#reclip(this.#heads, start, end, (run) => this.#reads(run.start, run.end));
		this.#reclip(this.#asked, start, end, (run) => this.#wants(run.start, run.end));
		this.#reclip(this.#refilled, start, end, (run) => this.#wants(run.start, run.end));
		fillsOf(producer).moved(asker, start, end);
		this.#fillMoves.update((moves) => moves + 1);
	}

	// Replace the `runs` overlapping `[start, end)` with what `keep` leaves of them.
	#reclip(runs: Run[], start: bigint, end: bigint, keep: (run: Run) => Run[]): void {
		const from = runsFrom(runs, start);
		let to = from;
		while (to < runs.length && runs[to].start < end) to++;
		if (from < to) runs.splice(from, to - from, ...runs.slice(from, to).flatMap(keep));
	}

	// What this reader wants from the fills it joined may have narrowed: rejudge them, so their
	// handlers hear it.
	#fillsMoved(given?: readonly Run[]): void {
		const producer = this.#state.producer;
		if (!this.#state.asker || !producer) return;
		const fills = fillsOf(producer);
		// Runs given up are never wanted again, unless an update retries them, which asks afresh:
		// only the fills owing them are judged again.
		if (given) {
			for (const run of given) fills.moved(this.#state.asker, run.start, run.end);
		} else {
			// At once, before the fills withdraw what it no longer reads: a widening back asks those
			// groups afresh, never through the one refill an evicted answer is allowed.
			this.#clipAsked();
			fills.moved(this.#state.asker);
		}
		this.#fillMoves.update((moves) => moves + 1);
	}

	// Keep what this reader asked within what it still reads, as the Rust `Misses` clips what it
	// judged to the window: a group outside the window is never asked again, so these stay within the live span. A window moving back over a
	// clipped group asks it once more, as in Rust.
	#clipAsked(): void {
		if (this.#givenUp.length > 0) this.#givenUp = this.#givenUp.flatMap((run) => this.#reads(run.start, run.end));
		if (this.#heads.length > 0) this.#heads = this.#heads.flatMap((run) => this.#reads(run.start, run.end));
		if (this.#asked.length > 0) this.#asked = this.#asked.flatMap((run) => this.#wants(run.start, run.end));
		if (this.#refilled.length > 0) {
			this.#refilled = this.#refilled.flatMap((run) => this.#wants(run.start, run.end));
		}
	}

	// Raise the key cursor to `floor`: what lies below it is passed for good.
	#raiseNext(floor: bigint | undefined): void {
		if (floor === undefined || floor <= this.#nextKey) return;
		const passed = this.#nextKey;
		this.#nextKey = floor;
		this.#judge(passed, floor);
		this.#stoppedReading(passed, floor);
	}

	// Forget the gaps in `delivered` below each group past this subscriber's budget now, whether
	// this track holds it or not: one landing there would be written off. A group skipped as
	// stale is not forgotten, since a wider budget offers it again.
	#forgetPastBudget(delivered: Delivered): void {
		if (delivered.aboveGap() === undefined) return;
		// One edge for every group judged, so forgetting N gaps never resolves it N times.
		const drift = this.#drift();
		// Found once a gap is stale, so a hand-out that forgets nothing never scans #skipped. A
		// skip whose group left the cache can never be offered again, so it is forgotten here
		// rather than holding the gaps above it forever.
		let skipped: bigint | undefined;
		let scanned = false;
		let forgot: bigint | undefined;
		for (let oldest = delivered.aboveGap(); oldest !== undefined; oldest = delivered.aboveGap()) {
			if (!this.#staleAt(oldest, drift)) break;
			if (!scanned) {
				scanned = true;
				const producer = this.#state.producer;
				for (const key of this.#skipped.keys()) {
					if (producer && !cachedHolds(producer, key)) this.#skipped.delete(key);
					else skipped = min(skipped ?? key, key);
				}
			}
			if (skipped !== undefined && skipped < oldest) {
				delivered.forgetBelow(skipped);
				forgot = skipped;
				break;
			}
			delivered.forgetBelow(oldest);
			forgot = oldest;
		}
		// A buffered group in a gap forgotten here is never handed out.
		if (forgot !== undefined && delivered === this.#delivered) this.#judge(0n, forgot);
	}

	// Record `key` as skipped stale under `budget`, so a wider one offers it again.
	#skip(key: bigint, budget: number): void {
		this.#skipped.set(key, budget);
		this.#narrowest = Math.min(this.#narrowest, budget);
		// Each kept entry holds a distinct cached group, so pruning once they outnumber the
		// cache twice over costs O(1) per skip amortized.
		const producer = this.#state.producer;
		if (!producer || this.#skipped.size <= 2 * this.#state.timeline.length) return;
		for (const skipped of this.#skipped.keys()) {
			if (!cachedHolds(producer, skipped)) this.#skipped.delete(skipped);
		}
	}

	// Offer again the cached groups skipped as stale under a budget narrower than `budget`, now
	// that it widened. Each stays recorded, judged under `budget`, until it is handed out or
	// skipped again, so a newer one handed out first never folds it into #delivered. Costs the
	// skipped groups, never the cache.
	#reoffer(budget: number): void {
		if (this.#skipped.size === 0 || budget <= this.#narrowest) return;
		const keys: bigint[] = [];
		let narrowest = Number.POSITIVE_INFINITY;
		for (const [key, judged] of this.#skipped) {
			if (judged >= budget) {
				narrowest = Math.min(narrowest, judged);
				continue;
			}
			this.#skipped.set(key, budget);
			narrowest = budget;
			keys.push(key);
			// Judged again, so a stale drop not reported yet is withdrawn.
			if (dropAt(this.#pending, key) instanceof Error) {
				this.#pending = this.#pending.flatMap((run) =>
					run.start <= key && key < run.end && run.stale !== undefined
						? removeRun([run], key, key + 1n)
						: [run],
				);
			}
		}
		this.#narrowest = narrowest;
		const producer = this.#state.producer;
		if (producer && !this.#left) reofferKeys(producer, this.#state, keys);
	}

	// The publisher's view of this reader's record.
	#wireRecord(): WireRecord {
		this.#wired = true;
		const reported = this.#reported;
		return {
			told: (key) => reported.at(key) !== undefined,
			untold: (start, end) => (start < end ? runGaps(reported.overlapping(start, end), start, end) : []),
			tell: (start, end, report) => {
				if (start < end) reported.set(start, end, { ...report, start, end });
			},
			untell: (start, end) => {
				if (start < end) reported.set(start, end);
			},
		};
	}

	private constructor(name: string, state: TrackState) {
		this.name = name;
		this.#state = state;
		state.reads = (key) => this.#reading(key);
		// Each reader filters locally even when another reader widens upstream demand.
		this.#resolve();
	}

	// The latest group to count relative anchors back from. A reader following route copies
	// leaves them to the copies, which hold the groups.
	#latestGroup(): bigint | undefined {
		return this.#resume ? undefined : this.#state.largest.peek()?.group;
	}

	// Resolve this reader's ranges again, as when they are replaced.
	#resolve(): void {
		const update = this.#state.update.peek();
		this.#window = new Window(
			update?.ranges ?? [{}],
			this.#latestGroup(),
			this.#holdings(),
			update?.delay?.min ?? undefined,
		);
		// Leaves the lookups the old ranges asked, which a settled window never reads again.
		if (this.#lookups.length > 0) this.#lookUp();
		this.#moved();
	}

	// The window moved: narrow the bounds to it.
	#moved(): void {
		this.#revision++;
		this.#bound();
	}

	// The groups this reader's track holds, by start, for resolving its relative and time anchors:
	// the producer's cache. A reader following route copies leaves them to the copies, which hold
	// the groups. Mirrors the Rust `Holdings for TrackState`.
	#holdings(): Holdings {
		const state = this.#state;
		const producer = state.producer;
		if (this.#resume || !producer) return UNHELD;
		const keys = keysOf(producer);
		// Whether drops of a kind `kind` accepts cover every key in `[start, end)`. An empty run is.
		const covered = (start: bigint, end: bigint, kind: (run: DropRun) => boolean) => {
			if (start >= end) return true;
			let at = start;
			for (const run of state.drops.overlapping(start, end)) {
				if (!kind(run) || run.start > at) return false;
				at = run.end;
			}
			return at >= end;
		};
		const anchors = timeAnchors(producer);
		return {
			resolves: () => true,
			containing: (time) => {
				// A fetch names its group by start: the one starting there, never an earlier one
				// containing it, as a Rust fetch reads by key.
				if (anchors.atKeys || state.fetch) return { group: time };
				const index = keyIndex(keys, time + 1n);
				const below = index > 0 ? keys[index - 1] : undefined;
				// A lite-07 copy holds only part of the track: a group held before `time` contains it
				// only once nothing between them is still to arrive. Mirrors the Rust
				// `TrackState::containing_received`.
				if (receivedCopy(producer) && !state.dropsAnchors.peek()) {
					if (below === time) return { group: time };
					const start = anchors.starts.get(time);
					// The publisher serves nothing at or before it, as for a time older than every group.
					if (start !== undefined) return start <= time ? { group: start } : "before";
					if (below !== undefined && (covered(below + 1n, time + 1n, () => true) || index < keys.length)) {
						return { group: below };
					}
					if (below === undefined && covered(0n, time + 1n, () => true)) return "before";
					return "unknown";
				}
				if (below !== undefined) return { group: below };
				// Every key at or before it is gone, so whichever group held it will never arrive.
				if (covered(0n, time + 1n, () => true)) return "before";
				return keys.length === 0 ? "unknown" : "before";
			},
			nextStart: (from) => keys[keyIndex(keys, from)],
			back: (latest, back) => {
				// A lite-07 copy holds only part of the track, so it counts back as its publisher did,
				// from the latest group the publisher counted from: a newer one moved the live edge.
				const reported = receivedCopy(producer) ? anchors.relative.get(back) : undefined;
				if (reported !== undefined && reported.latest === latest) return reported.start;
				if (back === 0n) return latest;
				// Bounded by the cache: a huge count reaches back to the oldest.
				const index = BigInt(keyIndex(keys, latest)) - back;
				return index < 0n ? undefined : keys[Number(index)];
			},
			// Only once the producer is gone: a group held below a reached end may still start.
			complete: () => state.closed.peek() !== undefined,
			// A received copy's handler is its session, which fetches by group alone.
			looksUp: () => !receivedCopy(producer) && fillsOf(producer).serving,
			reachBack: (_latest, min) => this.#reachBack(min),
			// Skip runs coalesce, so a gap between two held groups is one run unless the cache
			// evicted a group inside it.
			neverProduced: (start, end) => covered(start, end, (run) => run.skip === true),
		};
	}

	// Where a live join holding `min` behind the live edge starts, as {@link Holdings.reachBack}
	// says: in media time from the newest frame held, or on a scale-less track, by arrival on this
	// hop's clock, as expiry measures it. Always a group held: the newest one starting by the
	// target, or the oldest one when none does, so nothing below what this hop holds is asked for.
	// Groups keyed by their start are found by key; on a copy whose wire keys groups otherwise, by
	// when each one's first frame presented. Mirrors the Rust `TrackState::reach_back`.
	#reachBack(min: Duration): { anchor?: Anchor } | undefined {
		const timeline = this.#state.timeline;
		// The newest group held for which `f` holds, failed ones aside.
		const newest = (f: (group: GroupConsumer) => boolean) =>
			timeline.findLast((g) => !hooks.groupFailed(g) && f(g));
		const held = (group: GroupConsumer | undefined) => {
			const at = group ?? timeline.find((g) => !hooks.groupFailed(g));
			return at && { anchor: { time: at.start } };
		};
		const info = this.#state.info.peek();
		// Until the track is accepted, whether it is timed at all is unknown.
		if (info === undefined) return undefined;
		const scale = info.timescale;
		if (scale == null) {
			const at = now();
			return held(newest((group) => at.since(hooks.groupArrived(group)).ge(min)));
		}
		const stamped = newest((group) => hooks.groupTimestamp(group) !== undefined);
		const started = stamped && hooks.groupTimestamp(stamped);
		// In the track's own scale, which holds every timestamp it accepted. Floored exactly, as
		// Rust's `Timestamp::convert`: an accepted timestamp may sit between ticks.
		const edge = started && BigInt(floorAt(hooks.groupLatest(stamped) ?? started, scale));
		// The Min Age rounds up to whole ticks, so the start is never newer than it allows.
		const back = (BigInt(Math.ceil(min.asNanos())) * BigInt(scale) + 999_999_999n) / 1_000_000_000n;
		const producer = this.#state.producer;
		if (!this.#state.dropsAnchors.peek() && !(producer && timeAnchors(producer).atKeys)) {
			const last = newest(() => true)?.start;
			const top = edge === undefined ? last : last === undefined ? edge : max(edge, last);
			if (top === undefined) return undefined;
			const target = top > back ? top - back : 0n;
			return held(newest((group) => group.start <= target));
		}
		if (edge === undefined) return undefined;
		const target = edge > back ? edge - back : 0n;
		return held(
			newest((group) => {
				const start = hooks.groupTimestamp(group);
				return start !== undefined && BigInt(floorAt(start, scale)) <= target;
			}),
		);
	}

	// Resolve what the groups now held settle: a relative anchor waiting on a first group once
	// one exists, and a time start as groups arrive. Returns the refusal of ranges this copy cannot
	// resolve, which every read reports.
	#settle(): Error | undefined {
		const ranges = this.#window.ranges;
		if (this.#state.dropsAnchors.peek() && !ranges.every(isWire)) {
			return new Error("a relative or time anchor other than the live start needs moq-lite-07");
		}
		if (this.#resume || this.#window.settled) return undefined;
		// The live feed had no group when the ranges resolved, so the newest one then was the first.
		const latest = this.#window.pending ? this.#state.first : this.#latestGroup();
		const looked = this.#lookUp();
		if (this.#window.settle(latest, this.#holdings()) || looked) this.#moved();
		return undefined;
	}

	// Look up each time start the window can't resolve from the cache, once, and take in the
	// answers. Returns whether the groups in range moved. Mirrors the Rust `Cursor::look_up`.
	#lookUp(): boolean {
		const wanted = this.#window.lookups();
		if (wanted.length === 0 && this.#lookups.length === 0) return false;
		const producer = this.#state.producer;
		// The window stops looking once replaced.
		this.#lookups = this.#lookups.filter((lookup) => {
			if (wanted.some((want) => want.time === lookup.time)) return true;
			lookup.lookup?.leave();
			return false;
		});
		for (const { time, below } of wanted) {
			if (this.#lookups.some((lookup) => lookup.time === time)) continue;
			// With no handler open any more, nothing can answer.
			const lookup = producer && fillsOf(producer).lookup(time, below, this.#askerOf());
			this.#lookups.push({ time, lookup });
		}
		let moved = false;
		for (const lookup of [...this.#lookups]) {
			const answer = lookup.lookup?.answer();
			if (answer === undefined && lookup.lookup && !lookup.lookup.done()) continue;
			// The answer counts once held.
			const holdings = this.#holdings();
			if (
				answer !== undefined &&
				producer &&
				holdings.nextStart(answer) !== answer &&
				cachedMayStart(producer, answer)
			)
				continue;
			this.#lookups.splice(this.#lookups.indexOf(lookup), 1);
			lookup.lookup?.leave();
			if (this.#window.found(lookup.time, answer, holdings)) moved = true;
		}
		return moved;
	}

	// Narrow the local bounds to the window into the hull every read starts from.
	#bound(): void {
		const end = this.#window.end();
		const local = this.#local;
		this.#cursor.set({
			start: max(local.start, this.#window.floor() ?? 0n),
			end: end === undefined ? local.end : min(local.end ?? end, end),
		});
		this.#judge(0n, UNBOUNDED);
		// What it reads changed, so its fill probe judges everything again.
		this.#judgedAt = undefined;
		// A fill this reader alone waited on for a group it no longer reads is withdrawn.
		this.#fillsMoved();
	}

	// The parts of `[start, end)` this reader still reads: inside its cursor and window, not
	// handed out, and not passed by the key cursor, which never goes back.
	#reads(start: bigint, end: bigint): Run[] {
		if (this.#state.closed.peek() !== undefined) return [];
		return this.#unread(start, end);
	}

	// The parts of `[start, end)` this reader's bounds and cursor still leave unread, even once
	// the track closed, since what it buffered stays readable.
	#unread(start: bigint, end: bigint): Run[] {
		const cursor = this.#cursor.peek();
		const from = max(max(start, cursor.start), this.#nextKey);
		const to = cursor.end === undefined ? end : min(end, cursor.end);
		return this.#window.runs(from, to).flatMap((run) => this.#delivered.gaps(run.start, run.end));
	}

	// Tell each group buffered in `[start, end)` whether this reader will still read it, as the
	// Rust `Join` holds a run until its reader takes it, passes it, or stops reading it. A budget
	// judges a group only once a read reaches it, as there.
	#judge(start: bigint, end: bigint): void {
		const groups = this.#state.groups.peek();
		for (let i = timelineIndex(groups, start); i < groups.length && groups[i].start < end; i++) {
			hooks.groupReads(groups[i], this.#reading(groups[i].start));
		}
	}

	// Whether this reader will still read group `key` once buffered: what it wants from a
	// fill, judged even once the track closed.
	#reading(key: bigint): boolean {
		if (this.#left) return false;
		const unread = this.#unread(key, key + 1n);
		return unread.length > 0 && runGaps(this.#givenUp, key, key + 1n).length > 0;
	}

	// What this reader still wants from a fill: what it reads, less what it gave up as stale.
	#wants(start: bigint, end: bigint): Run[] {
		const reads = this.#reads(start, end);
		if (this.#givenUp.length === 0) return reads;
		return reads.flatMap((run) => runGaps(this.#givenUp, run.start, run.end));
	}

	// This reader as the producer's fills know it.
	#askerOf(): Asker {
		this.#state.asker ??= {
			wants: (start, end) => this.#wants(start, end),
			priority: () => this.#state.update.peek()?.priority ?? 0,
			maxDelay: () => this.#drift().budget,
			order: () => this.#state.update.peek()?.order ?? "descending",
			refuse: (start, end, error, stale) => {
				// A budget's verdict is no eviction: a widening asks it afresh, never through the
				// one refill an evicted answer is allowed, as the Rust `Misses::resolve` retries it.
				if (stale !== undefined) {
					this.#asked = removeRun(this.#asked, start, end);
					this.#refilled = removeRun(this.#refilled, start, end);
					insertRun(this.#unjudged, start, end);
				}
				this.#state.refused.update((runs) => insertDrop(runs, start, end, { error, stale }));
			},
		};
		return this.#state.asker;
	}

	// Ask the producer's fills for the misses in `[start, end)`: groups this reader reads below
	// the newest one received that the cache does not hold and nobody gave up. A run whose newest
	// group the budget already convicts is not asked, so a reader at the live edge never asks.
	// Only what is still unjudged is looked at (see #unjudged), so a probe costs what changed
	// since the last. Returns the first key a fill still owes this reader whose budget still
	// wants it, arming the wakeup for when that changes. Mirrors the Rust `Misses::judge`.
	#askFills(start: bigint, end: bigint, drift?: Drift): bigint | undefined {
		const producer = this.#state.producer;
		if (this.#resume || !producer || this.#left) return undefined;
		const fills = fillsOf(producer);
		// A fill already out can be joined even once its handler's handle closed.
		if (fills.serving || fills.active) {
			drift ??= this.#drift();
			const asker = this.#askerOf();
			// A moving read floor retires only its prefix: what lies above it stays asked.
			const read = max(this.#cursor.peek().start, this.#nextKey);
			for (const runs of [this.#asked, this.#refilled, this.#givenUp, this.#heads]) cutRun(runs, 0n, read);
			const budget = drift.budget;
			const coverage = this.#coverage();
			this.#reopen(producer, fills, budget, coverage);
			const timeline = this.#state.timeline;
			// Below where the feed starts, only the history the copy's start rule leaves fillable.
			const floor = fillFloor(producer, this.#window.start());
			const { drops, refused } = this.#state;
			const low = floor === undefined ? start : max(start, floor);
			const edge = min(end, max(this.#state.received.peek(), (fillEdge(producer) ?? -1n) + 1n));
			const above = max(low, edge);
			const unknown = this.#unknownRuns(budget, coverage);
			const seen: Run[] = [];
			const unjudged = this.#unjudged;
			for (let at = runsFrom(unjudged, low); at < unjudged.length && unjudged[at].start < end; at++) {
				const from = max(unjudged[at].start, low);
				const to = min(unjudged[at].end, end);
				// Reordered streams still owed by the live feed are not cache misses, and stay unjudged
				// until the feed gives them up.
				const accounted: Run[] = [];
				for (const run of owedRuns(producer, from, to)) {
					insertRun(accounted, run.start, run.end);
					if (run.folds !== undefined) this.#wakeAt(run.folds.since(Instant.ZERO).asNanos());
				}
				for (const run of runGaps(accounted, from, to)) seen.push(run);
				const spans: Run[] = from < min(to, edge) ? [{ start: from, end: min(to, edge) }] : [];
				// A received stale run that does not settle this reader is unknown to it, so it is
				// still asked for, as the Rust `Misses::unclaimed` judges it: the upstream accounted
				// for it, so the live feed brings none of it unless an update supersedes it. Only the
				// unknown runs overlapping what is unjudged are walked.
				const over = max(from, above);
				for (let i = runsFrom(unknown, over); i < unknown.length && unknown[i].start < to; i++) {
					const dropped = unknown[i];
					const span = { ...dropped, start: max(dropped.start, over), end: min(dropped.end, to) };
					if (span.start < span.end && !settles(span, budget, coverage)) spans.push(span);
				}
				// Only what the window lets a miss ask for: none below the oldest group held for a time
				// start reaching back past it.
				const missable = spans.flatMap((span) => this.#window.missable(span.start, span.end));
				for (const run of missable.flatMap((span) => this.#wants(span.start, span.end))) {
					const gone = [...accounted];
					for (const dropped of givenUp(drops, refused.peek(), run.start, run.end, (run) =>
						settles(run, budget, coverage),
					)) {
						// Judged from where it meets the asked run, as the reader would read it.
						const clipped = { ...dropped, start: max(dropped.start, run.start) };
						if (settles(clipped, budget, coverage)) insertRun(gone, clipped.start, dropped.end);
					}
					for (const open of runGaps(gone, run.start, run.end)) {
						let next = open.start;
						for (let i = timelineIndex(timeline, open.start); i < timeline.length; i++) {
							const group = timeline[i];
							if (group.start >= open.end) break;
							// A failed copy holds nothing to read, so it is missed like an evicted one.
							if (hooks.groupFailed(group)) continue;
							this.#askMiss(next, group.start, drift, asker);
							next = group.start + 1n;
						}
						this.#askMiss(next, open.end, drift, asker);
					}
				}
			}
			for (const run of seen) cutRun(this.#unjudged, run.start, run.end);
		}
		const asker = this.#state.asker;
		if (!asker) return undefined;
		// Every group of an owed run waits on the same successor, so its newest judges the run;
		// a stale run is given up, and gives way to a later one the budget still wants.
		let owed: bigint | undefined;
		const stale: Run[] = [];
		for (const run of fills.covered(start, end, asker)) {
			drift ??= this.#drift();
			if (this.#staleAt(run.end - 1n, drift, true)) {
				stale.push(run);
				continue;
			}
			this.#fillWait = true;
			owed = run.start;
			break;
		}
		if (stale.length > 0) {
			for (const run of stale) {
				insertRun(this.#givenUp, run.start, run.end);
				this.#judge(run.start, run.end);
			}
			this.#fillsMoved(stale);
		}
		return owed;
	}

	// Take what changed since the last probe as unjudged again: every key once the terms it
	// was judged under change, else the drop runs changed or forgotten, the groups the timeline
	// lost with the gaps they were successor to, and what the received edge rose over, which
	// arrives live no more.
	#reopen(producer: Producer, fills: FillState, budget: number, coverage: Coverage): void {
		const state = this.#state;
		const at = this.#judgedAt;
		const edge = max(state.received.peek(), (fillEdge(producer) ?? -1n) + 1n);
		const opened = fills.opened.peek();
		const removed = at && state.drops.removedSince(at.drops);
		if (
			!at ||
			!removed ||
			!state.lost ||
			at.producer !== producer ||
			at.budget !== budget ||
			!sameCoverage(at.coverage, coverage) ||
			at.opened !== opened
		) {
			this.#unjudged = [{ start: 0n, end: UNBOUNDED }];
		} else {
			for (const run of [...removed, ...state.drops.since(at.drops)])
				insertRun(this.#unjudged, run.start, run.end);
			// A lost group was the successor the gap below it was judged stale by, back to the
			// group below that #after would count instead.
			const timeline = state.timeline;
			for (const run of state.lost) {
				let i = timelineIndex(timeline, run.start) - 1;
				while (
					i >= 0 &&
					(hooks.groupFailed(timeline[i]) ||
						!this.#judges(timeline[i].start) ||
						!hooks.groupStarted(timeline[i]))
				)
					i--;
				insertRun(this.#unjudged, i < 0 ? 0n : timeline[i].start + 1n, run.end);
			}
			if (edge > at.edge) insertRun(this.#unjudged, at.edge, edge);
		}
		state.lost = [];
		this.#judgedAt = {
			producer,
			budget,
			coverage,
			opened,
			drops: state.drops.revision,
			edge,
		};
	}

	// The received stale runs that do not settle this reader under `budget` and `coverage`,
	// lowest first. While the terms hold, only what changed since is judged again: a run the
	// track replaced or forgot leaves the memo, and the runs the change left are judged anew, so a
	// drop costs the memo the runs it touches. Once the set no longer remembers what it removed
	// that far back, the memo is rebuilt, at most once per a live set's worth of removals. A later start only narrows what a run asks the
	// reader to read, so a run that settles here settles clipped too. Mirrors the Rust
	// `Misses::unknown`.
	#unknownRuns(budget: number, coverage: Coverage): readonly DropRun[] {
		const { drops } = this.#state;
		const unsettled = (run: DropRun) => !settles(run, budget, coverage);
		const memo = this.#unknown;
		const removed =
			memo && memo.budget === budget && sameCoverage(memo.coverage, coverage) && drops.removedSince(memo.drops);
		if (memo && removed) {
			if (memo.drops === drops.revision) return memo.runs;
			// Updated in place: its one caller is done with the last answer.
			const runs = memo.runs;
			for (const gone of removed) {
				const from = runsFrom(runs, gone.start);
				let to = from;
				while (to < runs.length && runs[to].start < gone.end) to++;
				runs.splice(from, to - from);
			}
			for (const changed of drops.since(memo.drops)) {
				for (const run of drops.overlapping(changed.start, changed.end)) {
					const at = runsFrom(runs, run.start);
					if (unsettled(run) && runs[at] !== run) runs.splice(at, 0, run);
				}
			}
			memo.drops = drops.revision;
			return runs;
		}
		const runs = drops.all.filter(unsettled);
		this.#unknown = { drops: drops.revision, budget, coverage, runs };
		return runs;
	}

	// Ask a fill for a fetch's group at once, written yet or not, as the Rust `fetch_group` queues
	// one; a group the live feed writes meanwhile answers it. `false` with no handler to ask; `"pending"` until the track is accepted.
	#askFetch(key: bigint): boolean | "pending" {
		const producer = this.#state.producer;
		if (!producer) return false;
		if (this.#state.info.peek() === undefined && this.#state.closed.peek() === undefined) return "pending";
		if (cachedHolds(producer, key)) return true;
		// Joins a fill already bringing it, handler or not; asks a new one only while one is open.
		const asked = fillsOf(producer).ask(key, key + 1n, this.#askerOf());
		if (asked.length === 0) return false;
		insertRun(this.#asked, key, key + 1n);
		return true;
	}

	// The lowest group a fill still brings this reader, asking for what it misses first.
	#filling(): bigint | undefined {
		const producer = this.#state.producer;
		if (this.#resume || !producer) return undefined;
		const fills = fillsOf(producer);
		if (!fills.serving && !fills.active) return undefined;
		const drift = this.#drift();
		return this.#askFills(this.#cursor.peek().start, UNBOUNDED, drift);
	}

	// Ask a fill for the missed run `[start, end)`, unless its newest group is stale: what was
	// never asked, and once more what was asked but is missed again.
	#askMiss(start: bigint, end: bigint, drift: Drift, asker: Asker): void {
		if (start >= end || this.#staleAt(end - 1n, drift)) return;
		const fills = fillsOf(this.#state.producer as Producer);
		// Asked before and missed again, unless a fill still brings it: its answer was evicted
		// before this reader took it.
		const again = intersect(this.#asked, start, end).flatMap((run) =>
			runGaps(this.#refilled, run.start, run.end).flatMap((refill) =>
				runGaps([...fills.covered(refill.start, refill.end, asker)], refill.start, refill.end),
			),
		);
		// Only what a fill took counts as asked: with no handler open, a miss no fill covers is
		// left to ask once one opens.
		for (const gap of runGaps(this.#asked, start, end)) {
			for (const run of fills.ask(gap.start, gap.end, asker)) insertRun(this.#asked, run.start, run.end);
		}
		// Refilled and missed again, unless a fill still brings it: given up and named, never
		// asked a third time, as the Rust `Misses` gives up a refill evicted again.
		const lost = intersect(this.#refilled, start, end).flatMap((run) =>
			runGaps([...fills.covered(run.start, run.end, asker)], run.start, run.end),
		);
		for (const refill of again) {
			for (const run of fills.ask(refill.start, refill.end, asker)) insertRun(this.#refilled, run.start, run.end);
		}
		for (const run of lost) {
			insertRun(this.#givenUp, run.start, run.end);
			asker.refuse(run.start, run.end, new StreamError(StreamCode.NotFound, { message: "group evicted again" }));
		}
		if (lost.length > 0) this.#fillsMoved(lost);
	}

	// Whether `group` is one this reader hands out only from its first frame, which it lacks: the
	// window is bounded, so it reads whole groups as a fetch would, from the frame a fetch asks.
	// Mirrors the Rust `Cursor::lacks_head`, and a fetch's `poll_fetch_cached` frame bounds.
	#lacksHead(group: GroupConsumer): boolean {
		return this.#window.end() !== undefined && hooks.groupFirstFrame(group) > this.#fetchFrom;
	}

	// Ask a fill for the head of group `key` once, as a fetch from frame 0 would, unless one
	// is already out. Returns whether one still owes it; false once it is given up, as with no
	// handler to ask, or a fill that ended without a copy from frame 0 (dropped, or closed
	// unanswered), which refuses it to this reader as a miss no fill can serve. Mirrors the Rust
	// `Cursor::ask_head` and `give_up_heads`.
	#askHead(key: bigint): boolean {
		const end = key + 1n;
		if (dropAt(this.#state.refused.peek(), key) || runGaps(this.#givenUp, key, end).length === 0) return false;
		const producer = this.#state.producer;
		if (!producer) return false;
		const fills = fillsOf(producer);
		const asker = this.#askerOf();
		if (!fills.covered(key, end, asker).next().done) return true;
		if (runGaps(this.#heads, key, end).length > 0 && fills.ask(key, end, asker).length > 0) {
			insertRun(this.#heads, key, end);
			return true;
		}
		// A head the reader found missing belonged to a group that existed.
		asker.refuse(key, end, new StreamError(StreamCode.NotFound, { message: `group ${key} has no head` }));
		return false;
	}

	// Whether a held group waits on the local cap rising: one past it may still be read. A
	// group outside the window alone never holds a finished track open.
	#parked(groups: readonly GroupConsumer[]): boolean {
		const cap = this.#local.end;
		return cap !== undefined && (groups.at(-1)?.start ?? -1n) >= cap;
	}

	// Whether a held group from `from` on may still start, and so settle a time anchor the window
	// waits on, which a reached end does not decide: until it starts or the track closes, as
	// {@link Holdings.complete} says, the window may still take it in.
	#starting(groups: readonly GroupConsumer[], from = 0n): boolean {
		if (this.#window.settled || this.#state.closed.peek() !== undefined) return false;
		return groups.some((group) => group.start >= from && !hooks.groupStarted(group));
	}

	// Take the group at `index` off the ordered buffer, closing the out-of-range ones below it,
	// which sequence order passes for good.
	#pass(groups: GroupConsumer[], index: number): void {
		const [group] = groups.splice(index, 1);
		for (const passed of groups.splice(0, index)) passed.close();
		this.#nextKey = group.start + 1n;
		this.#judge(group.start, this.#nextKey);
	}

	// Whether this read ends because its bounded window has nothing left to deliver; see
	// {@link #drained}. A wire publisher's reader never ends there.
	#exhausted(): boolean {
		if (this.#parks) return false;
		if (this.#resume) return this.#resume.exhausted(this.#delivered, this.#window);
		return this.#drained([this.#delivered]);
	}

	// Whether a bounded window has nothing left to deliver: every group in its ranges was handed
	// out (in any of `taken`), dropped, or never will be, as one skipped as stale, one below where
	// the feed started, or a hole the feed no longer owes. An open window always has more, and so
	// does one the cap keeps a group out of, since the reader may raise it again. Mirrors the
	// Rust `Cursor::is_exhausted`.
	#drained(taken: readonly Delivered[]): boolean {
		const end = this.#window.end();
		if (end === undefined) return false;
		// A fill still owes a group it reads.
		if (this.#askFills(this.#window.floor() ?? 0n, end) !== undefined) return false;
		const held = this.#state.groups.peek();
		const producer = this.#state.producer;
		const received = max(this.#state.received.peek(), ((producer && fillEdge(producer)) ?? -1n) + 1n);
		// A received stale run that does not settle this reader is unknown to it (see `settles`):
		// the reader still wants those groups, however the feed accounted for them.
		const budget = this.#drift().budget;
		const coverage = this.#coverage();
		const { drops: all, refused } = this.#state;
		for (const run of this.#window.runs(this.#window.floor() ?? 0n, end)) {
			let gaps = [run];
			for (const delivered of taken) gaps = gaps.flatMap((gap) => delivered.gaps(gap.start, gap.end));
			for (const gap of gaps) {
				// Each run judged from where it meets the gap, as the reader would read it.
				const overlapping = givenUp(all, refused.peek(), gap.start, gap.end, (run) =>
					settles(run, budget, coverage),
				).map((run) => ({
					...run,
					start: max(run.start, gap.start),
					end: min(run.end, gap.end),
				}));
				const drops = overlapping.filter((run) => settles(run, budget, coverage));
				const unknown = overlapping.filter((run) => !settles(run, budget, coverage));
				for (const open of runGaps(drops, gap.start, gap.end)) {
					if (unknown.some((run) => run.start < open.end && open.start < run.end)) return false;
					// Not received yet, so it may still arrive.
					if (open.end > received) return false;
					// Held, so a read still hands it out, skips it, or waits for the cap to rise.
					const index = timelineIndex(held, open.start);
					if (held[index] && held[index].start < open.end) return false;
					const owed = producer && owedGroup(producer, open.start, open.end);
					if (owed) {
						if (owed.folds !== undefined) this.#wakeAt(owed.folds.since(Instant.ZERO).asNanos());
						return false;
					}
				}
			}
		}
		return true;
	}

	static {
		makeSubscriber = (name, state) => new Subscriber(name, state);
		hooks.subscriberEnd = (subscriber) =>
			subscriber.#resume ? subscriber.#resume.end() : subscriber.#state.end.peek()?.at;
		hooks.subscriberEndError = (subscriber) =>
			subscriber.#resume ? subscriber.#resume.endError() : subscriber.#state.end.peek()?.error;
		hooks.askSubscriber = (subscriber, ask) => {
			if (subscriber.#resume) subscriber.#resume.ask(ask);
			else if (subscriber.#state.producer) hooks.askProducer(subscriber.#state.producer, ask);
		};
		hooks.skippedBelow = (subscriber, key) =>
			subscriber.#resume ? subscriber.#resume.skippedBelow(key) : skippedBelow(subscriber.#state, key);
		hooks.sourceStart = (subscriber) => {
			if (subscriber.#resume) return subscriber.#resume.start();
			// A relative anchor still pending reads every group, so a fill asked below would claim
			// the history it never names. Mirrors the Rust `cursor.sync()` in `poll_start`.
			subscriber.#settle();
			const producer = subscriber.#state.producer;
			const start = producer && sourceStart(producer);
			if (start === "pending") return start;
			const floor = start === undefined ? undefined : max(start, subscriber.#cursor.peek().start);
			// The lowest group a fill brings this reader starts it too, as the Rust `poll_start`
			// has it: a group arriving first must not give up the history being filled below it.
			const filling = subscriber.#filling();
			if (filling === undefined) return floor;
			return floor === undefined ? filling : min(floor, filling);
		};
		hooks.tryRecvGroup = (subscriber, dispositions) => subscriber.#tryRecvGroup(dispositions);
		hooks.parked = (subscriber) => {
			subscriber.#parks = true;
			return subscriber;
		};
		// A route copy judged for the logical reader that handed out `delivered`: what the copy
		// handed out itself counts too. A copy that is itself a front judges across its own copies.
		hooks.exhaustedFor = (subscriber, delivered) =>
			subscriber.#resume
				? subscriber.#resume.exhausted(subscriber.#delivered.union(delivered), subscriber.#window)
				: subscriber.#drained([subscriber.#delivered, delivered]);
		hooks.tryDrop = (subscriber) => subscriber.#tryDrop();
		// A reader following route copies leaves its ranges to the serving copy, which resolves
		// their anchors against the groups it holds, then narrows them to its own local bounds.
		hooks.servingRuns = (subscriber, start, end) => {
			const cursor = subscriber.#cursor.peek();
			const from = max(start, cursor.start);
			const to = min(end, cursor.end ?? end);
			return subscriber.#resume?.runs(from, to) ?? subscriber.#window.runs(from, to);
		};
		hooks.servingEnd = (subscriber) => {
			const cursor = subscriber.#cursor.peek();
			if (!subscriber.#resume) return subscriber.#window.end() === undefined ? undefined : cursor.end;
			const end = subscriber.#resume.servingEnd();
			return end === undefined ? undefined : min(end, cursor.end ?? end);
		};
		hooks.servingStarts = (subscriber, from) => {
			const resumed = subscriber.#resume?.starts(from);
			if (resumed) return resumed;
			subscriber.#settle();
			return subscriber.#window.starts(from);
		};
		hooks.servingResolved = (subscriber) => {
			if (subscriber.#resume) return subscriber.#resume.resolved();
			subscriber.#settle();
			return subscriber.#window.resolved();
		};
		hooks.resolveRanges = (subscriber, subscription) =>
			new Window(
				subscription.ranges ?? [{}],
				subscriber.#latestGroup(),
				subscriber.#holdings(),
				subscription.delay?.min ?? undefined,
			).resolved();
		hooks.tryNextGroup = (subscriber, floor) => {
			subscriber.#raiseNext(floor);
			return subscriber.#tryNextGroup();
		};
		hooks.peekGroup = (subscriber, ordered, floor) => {
			if (ordered) subscriber.#raiseNext(floor);
			return subscriber.#peekGroup(ordered);
		};
		hooks.forgetPastBudget = (subscriber, delivered) => subscriber.#forgetPastBudget(delivered);
		hooks.fold = (subscriber, at) => subscriber.#fold(at);
		hooks.wire = (subscriber) => subscriber.#wireRecord();
		hooks.deliveredRuns = (subscriber) => subscriber.#delivered.runs;
		hooks.reportedRuns = (subscriber) => subscriber.#reported.size;
		hooks.handedOut = (subscriber, key) => subscriber.#delivered.has(key);
		hooks.hasUnreadGroup = (subscriber, start, end, delivered) =>
			subscriber.#state.groups
				.peek()
				.some(
					(group) =>
						group.start >= start &&
						(end === undefined || group.start < end) &&
						!delivered.has(group.start) &&
						!hooks.groupFailed(group),
				);
		hooks.spent = (subscriber, start, end, delivered) =>
			(subscriber.#state.closed.peek() !== undefined || subscriber.#complete()) &&
			!hooks.hasUnreadGroup(subscriber, start, end, delivered);
		hooks.awaitsFill = (subscriber) => subscriber.#fillWait;
		hooks.staleAt = (subscriber, key) => {
			const held: Superseded = {};
			return {
				expired: () => subscriber.#staleAt(key, subscriber.#drift(), true, held),
				changed: subscriber.#expiryChanged(),
			};
		};
		hooks.cachedGroup = (subscriber, key, from = 0) => {
			if (subscriber.#resume) return subscriber.#resume.cachedGroup(key, from);
			const producer = subscriber.#state.producer;
			return producer && cachedMirror(producer, key, from);
		};
		hooks.holdsGroup = (subscriber, key) => {
			const producer = subscriber.#state.producer;
			return producer !== undefined && cachedHolds(producer, key);
		};
		hooks.holeEnd = (subscriber, key, dropped) => holeEnd(subscriber.#state.drops, key, dropped);
		hooks.previousStart = (subscriber, start) => {
			// A reader following route copies knows only what the serving copy holds, which this
			// one does not: unknown, so the skips below go out on their own.
			if (subscriber.#resume || start === 0n) return undefined;
			const below = start - 1n;
			const run = subscriber.#state.drops.at(below);
			if (run?.skip) return run.start > 0n ? run.start - 1n : undefined;
			const producer = subscriber.#state.producer;
			return producer !== undefined && cachedIn(producer, below, start).length > 0 ? below : undefined;
		};
		hooks.staleHeld = (subscriber, start, end) => {
			const held: Run[] = [];
			for (const run of subscriber.#state.drops.overlapping(start, end)) {
				if (run.stale !== undefined) insertRun(held, max(run.start, start), min(run.end, end));
			}
			return held;
		};
		hooks.givenUp = (subscriber, key) => {
			const { drops, refused } = subscriber.#state;
			const drop = drops.at(key);
			const own = dropAt(refused.peek(), key);
			// As in `givenUp`: only a drop that settles the reader hides its own refusal.
			const wins =
				drop &&
				(own === undefined ||
					settles({ ...drop, start: key }, subscriber.#drift().budget, subscriber.#coverage()));
			return {
				error: wins ? drop.error : own,
				changed: [drops.version, refused],
			};
		};
		hooks.mayLand = (subscriber, land) => {
			const producer = subscriber.#state.producer;
			return producer !== undefined && mayLand(producer, land);
		};
		hooks.resumeSubscriber = (name, subscription, source) => {
			const state = new TrackState(subscription ?? Subscription.default());
			const subscriber = new Subscriber(name, state);
			subscriber.#resume = source({
				subscription: state.update,
				cursor: subscriber.#cursor,
				closed: state.closed,
				info: () => state.info.peek(),
				commit: (info) => {
					if (state.info.peek() === undefined) state.info.set(info);
				},
				close: (abort) => {
					closeTrackState(state, abort);
				},
			});
			return subscriber;
		};
		hooks.watchSubscriberError = (subscriber, fn) => {
			if (subscriber.#resume) return subscriber.#resume.watchError(fn);
			const check = (error: Error | null | undefined) => {
				if (error instanceof Error) fn(error);
			};
			const stop = subscriber.closed.subscribe(check);
			check(subscriber.closed.peek());
			return stop;
		};
		hooks.latestKey = (subscriber) => (subscriber.#resume ? subscriber.#resume.latest() : subscriber.#state.latest);
		hooks.arrived = (subscriber) => (subscriber.#resume ? 0 : subscriber.#state.arrived);
		hooks.bufferedLargest = (subscriber) => subscriber.#bufferedLargest();
		hooks.objectLargest = (subscriber) => {
			if (subscriber.#resume) return subscriber.#resume.objectLargest();
			const producer = subscriber.#state.producer;
			return producer && objectLargest(producer);
		};
		hooks.largestFinished = (subscriber, position) => {
			if (subscriber.#resume) return subscriber.#resume.largestFinished(position);
			const state = subscriber.#state;
			const largest = state.largest.peek();
			return state.largestFinished && largest?.group === position.group && largest.frame === position.frame;
		};
		hooks.peekLargest = (subscriber) => {
			if (subscriber.#resume) return subscriber.#resume.peekLargest();
			const state = subscriber.#state;
			return {
				largest: state.largest.peek(),
				ended: state.closed.peek() !== undefined || state.largestEnded.peek(),
			};
		};
		hooks.largestChanged = (subscriber, fn) => {
			const state = subscriber.#state;
			let fired = false;
			const once = () => {
				if (fired) return;
				dispose();
				fn();
			};
			const disposes = [
				state.largest.changed(once),
				state.largestEnded.changed(once),
				state.closed.changed(once),
			];
			const dispose = () => {
				fired = true;
				for (const close of disposes) close();
			};
			return dispose;
		};
		hooks.groupChanged = (subscriber, fn) => subscriber.#groupChanged(fn);
		hooks.askFetch = (subscriber, key) => subscriber.#askFetch(key);
		hooks.fetchBudget = (subscriber, maxDelay, frameStart = 0) => {
			subscriber.#fetchBudget = maxDelay?.asMillis() ?? Number.POSITIVE_INFINITY;
			subscriber.#fetchFrom = frameStart;
			subscriber.#state.fetch = true;
			// Its group resolves by start now; see `#holdings`.
			subscriber.#resolve();
			// The sink joined as a live subscriber; leave the live aggregate before anyone sees it.
			if (subscriber.#state.producer) resubscribe(subscriber.#state.producer);
			const update = subscriber.#state.update.peek();
			if (update) subscriber.#state.update.set(update, true);
		};
		// The key cursor lives here (it shares the buffer and the drift anchor with
		// the arrival cursor); `Ordered` is the handle that reaches it.
		ordered_ = {
			nextGroup: (subscriber) => subscriber.#nextGroup(),
			nextItem: (subscriber) => subscriber.#nextItem(),
			tryItem: (subscriber) => subscriber.#tryItem(),
			ended: (subscriber) => subscriber.#ended(),
			readFrame: (subscriber) => subscriber.#readFrame(),
			readString: (subscriber) => subscriber.#readString(),
			readJson: (subscriber) => subscriber.#readJson(),
		};
	}

	// Refuse a read on this handle once `ordered()` has taken the subscription.
	#live() {
		if (this.#mode === "ordered") throw new Error("track is read in sequence order; use the Ordered handle");
	}

	/**
	 * Resolve this track's immutable publisher properties.
	 *
	 * Resolves once the wire layer commits the properties it received (TRACK_INFO on
	 * lite-05+, SUBSCRIBE_OK on moq-transport) or an untimed default (older lite drafts),
	 * so awaiting it never yields a placeholder. Rejects if
	 * the track is closed before the properties are known (e.g. a rejected subscription).
	 */
	info(): Promise<Info> {
		return resolveInfo(this.#state);
	}

	/** Settles once the track closes; see {@link Producer.closed}. */
	get closed(): GetPromise<Error | null> {
		return this.#state.closed;
	}

	/** This subscriber's current options, including defaults and the last {@link update}. */
	get subscription(): Getter<Subscription | undefined> {
		return this.#state.update;
	}

	/**
	 * Resolve with the track's exclusive end once it is known, mirroring the Rust `finished`.
	 *
	 * Resolves as soon as the end is declared, which may be ahead of the live edge, so it
	 * says nothing about every group having arrived: read until the cursor returns
	 * `undefined` for that. Rejects with the abort, or if the track closes without an end.
	 */
	async finished(): Promise<Position> {
		if (this.#resume) return this.#resume.finished();
		for (;;) {
			const end = this.#state.end.peek()?.at;
			if (end !== undefined) return end;

			const closed = this.#state.closed.peek();
			if (closed instanceof Error) throw closed;
			if (closed !== undefined) throw new Error("track closed before its end was known");

			await Signal.race(this.#state.end, this.#state.closed);
		}
	}

	/**
	 * The lowest group this subscriber's ranges name, as resolved here, raised to the start of
	 * {@link withRange}: undefined while one reaches back to the oldest group still around, or a
	 * relative start still waits on the first group from the live feed.
	 *
	 * Where a reader that waits on each group in key starts, rather than at whatever arrives
	 * first, which newest-first delivery makes the latest. Mirrors the Rust `Subscriber::floor`.
	 */
	floor(): bigint | undefined {
		let start: bigint | undefined;
		if (this.#resume) {
			for (const range of this.#resume.resolved()) {
				if (!range?.floored) return undefined;
				start = start === undefined ? range.start : min(start, range.start);
			}
		} else {
			this.#settle();
			start = this.#window.start();
		}
		return start === undefined ? undefined : max(start, this.#local.start);
	}

	/**
	 * Where the source's feed starts once its publisher resolved it, raised to this reader's own
	 * floor: the first group a publisher tells its peer it serves, or lower where a fill this
	 * reader waits on starts. Undefined when the source declares none and no fill is out.
	 *
	 * Where a reader that waits on each group in key starts when {@link floor} names none:
	 * groups arrive newest first, so the first to arrive is not the oldest served. A copy says
	 * only once subscribed upstream, so ask once {@link info} resolves or a group arrives. Mirrors
	 * the Rust `Subscriber::poll_start`.
	 */
	async start(): Promise<bigint | undefined> {
		for (;;) {
			const start = hooks.sourceStart(this);
			if (start !== "pending") return start;
			if (this.#state.closed.peek() !== undefined) return undefined;
			await this.#wait();
		}
	}

	/**
	 * The lowest group at or above `from` this subscriber's ranges and {@link withRange} floor
	 * cover, past the starts the publisher declared skipped (see {@link Producer.appendGroup}) and
	 * those given up for good, undefined when none does: the groups between are never read, so a
	 * reader waiting on each group in key need not wait for them. A wire before lite-07 forwards
	 * a skip as such a drop. `from` itself while a relative anchor waits on the first group, which
	 * can't say yet. Mirrors the Rust `Subscriber::next_in_range`.
	 */
	nextInRange(from: bigint): bigint | undefined {
		from = max(from, this.#local.start);
		if (this.#resume) {
			// Without a serving copy nothing says yet.
			const runs = this.#resume.runs(from, UNBOUNDED);
			const resume = this.#resume;
			return runs ? firstOpen(runs, (key) => resume.holeEnd(key, true)) : from;
		}
		this.#settle();
		const drops = this.#state.drops;
		return firstOpen(this.#window.runs(from), (key) => holeEnd(drops, key, true));
	}

	/**
	 * Whether the publisher declared every start in `[start, end)` skipped, never to be produced
	 * (which {@link Producer.appendGroup} does between groups), so no group can arrive there: true
	 * for an empty range. Unlike {@link nextInRange}, this ignores the subscriber's ranges. A copy
	 * knows only the skips its wire forwarded.
	 *
	 * What tells a reader that nothing lies between two groups, or before a track's first. It
	 * knows only the skips recorded so far, so `false` means "not known skipped": a skip still in
	 * flight reads `false`, as does one a local producer's cache expired below its edge. Mirrors
	 * the Rust `Subscriber::is_skipped`.
	 */
	isSkipped(start: bigint, end: bigint): boolean {
		if (end <= start) return true;
		const run = [{ start, end }];
		if (this.#resume) {
			const resume = this.#resume;
			return resume.serving() && firstOpen(run, (key) => resume.holeEnd(key, false)) === undefined;
		}
		const drops = this.#state.drops;
		return firstOpen(run, (key) => holeEnd(drops, key, false)) === undefined;
	}

	/**
	 * The largest location within {@link withRange}: the current one at first, then each newer
	 * one (skipping any in between), and `undefined` once the track ends.
	 *
	 * The largest is the track's newest group at the newest frame (0 while empty), as
	 * the publisher reported it and its feed delivered since, so it can name a frame before its
	 * group arrives. A largest past the cap or below the floor reports nothing: the cache does not
	 * stand in for it, so a cap moved below the largest stays pending. Moving the bounds wakes a
	 * pending read, which reports the largest once it is within them and newer than the last report.
	 */
	async largest(): Promise<Position | undefined> {
		if (this.#largestDone) return undefined;
		if (this.#resume) {
			const largest = await this.#resume.largest(this.#largestSeen);
			if (largest) this.#largestSeen = largest;
			else this.#largestDone = true;
			return largest;
		}
		for (;;) {
			const { start, end } = this.#cursor.peek();
			const largest = this.#state.largest.peek();
			const within =
				largest && largest.group >= start && (end === undefined || largest.group < end) ? largest : undefined;
			const seen = this.#largestSeen;
			if (within && (!seen || after(within, seen))) {
				this.#largestSeen = within;
				return within;
			}
			if (this.#state.closed.peek() !== undefined || this.#state.largestEnded.peek()) {
				this.#largestDone = true;
				return undefined;
			}
			await Signal.race(this.#state.largest, this.#state.closed, this.#state.largestEnded, this.#cursor);
		}
	}

	// The newest frame buffered for this subscriber, which a wire answer names as its edge:
	// a newest group with no frames yet falls back to the newest older group that does, and it
	// reads as undefined once the newest group was evicted or received, since what is left can
	// no longer say where the edge is.
	#bufferedLargest(): Position | undefined {
		if (this.#resume) return this.#resume.bufferedLargest();
		const latest = this.#state.latest;
		if (latest === undefined) return undefined;

		const groups = this.#state.groups.peek();
		const newest = groups[groups.length - 1];
		if (!newest || newest.start !== latest) return undefined;

		for (let i = groups.length - 1; i >= 0; i--) {
			const count = groups[i].frameCount;
			if (count > 0) return { group: groups[i].start, frame: count - 1 };
		}
		return undefined;
	}

	/**
	 * An independent subscriber to the same track, with its own cursor and its own replay of
	 * the retained window.
	 *
	 * Reads here do not consume anything this one would deliver, which is what lets a
	 * publisher read the cache alongside the cursor it is serving from. Resolving the track
	 * again through the broadcast would not do: a dynamic serve is one request per peer
	 * subscription, so that mints a second producer the application has to answer separately.
	 * A routed track's fork follows the same logical track across route swaps.
	 *
	 * @internal Wire layers only.
	 */
	fork(options?: Subscription): Subscriber {
		if (this.#resume) return this.#resume.fork(options);
		const producer = this.#state.producer;
		if (!producer) throw new Error("track has no producer to fork from");
		return producer.subscribe(options);
	}

	/**
	 * Limit subsequent reads to these groups and return this reader for chaining. Mirrors the
	 * Rust `with_range`.
	 *
	 * Both bounds are assigned, in either direction: an omitted start is group 0 and an
	 * omitted end removes the cap. They narrow the subscription's ranges, never widen them.
	 * Lowering the floor or raising the cap offers the groups it newly covers that the track
	 * still caches and this reader never handed out; a group is never handed out twice. A local
	 * filter, not a request: it leaves the wire subscription alone, so to change what is asked
	 * for, replace {@link Subscription.ranges} with {@link update}. One exception: on a track
	 * served across route changes, lowering the floor asks the serving route again for the groups
	 * it newly covers within the subscription's ranges, since the route that delivered them may
	 * be gone. {@link largest} reports within these bounds.
	 */
	withRange(groups: Groups): this {
		const before = this.#cursor.peek();
		this.#local = groupBounds(groups);
		this.#bound();
		const { start } = this.#cursor.peek();
		const producer = this.#state.producer;
		if (!this.#resume && !this.#left && producer && start < before.start) {
			// The ordered cursor let go of what it passed below the old floor; the arrival
			// cursor keeps it buffered, which this skips.
			reofferCached(
				producer,
				this.#state,
				start,
				before.start,
				(key) => !this.#delivered.has(key) && this.#window.contains(key),
			);
		}
		return this;
	}

	/** Close the track (optionally with an error), closing any pending groups. Idempotent. */
	close(abort?: Error) {
		if (!this.#left) {
			const arrivals = this.#state.timeline.filter((group) => !hooks.groupFailed(group));
			this.#arrivals = arrivals.map((group) => ({
				key: group.start,
				arrived: hooks.groupArrived(group),
			}));
		}
		this.#left = true;
		this.#state.waitingOrdered = false;
		// Settle if we're first (the producer may already have); either way drop anything
		// still buffered. Groups parked at the withRange cap deliberately outlive a clean
		// producer close, so the subscriber leaving is what must release them: closing
		// and clearing wakes a pending read to observe the end instead of hanging.
		closeTrackState(this.#state, abort);
		this.#resume?.close(abort);
		this.#state.groups.mutate((groups) => {
			for (const group of groups) hooks.releaseGroupReader(group, abort);
			groups.length = 0;
		});
		if (this.#frameGroup) hooks.releaseGroupReader(this.#frameGroup, abort);
		this.#frameGroup = undefined;
		// A group an ordered read holds behind a gap is buffered too, so the read ends without it.
		if (this.#items?.held) hooks.releaseGroupReader(this.#items.held, abort);
		if (this.#items) this.#items.held = undefined;
		this.#state.timeline.length = 0;
		this.#wall.timer?.abort();
		this.#wall.timer = undefined;
		// A deadline kept in #arrivals may still arm a wakeup.
		this.#wall.at = Number.POSITIVE_INFINITY;
	}

	/**
	 * Receive every group on this track exactly once, as it becomes available.
	 *
	 * Groups may arrive out of order or with gaps due to network conditions; unlike
	 * {@link Ordered.nextGroup}, one that arrives after a newer group was already returned
	 * is still delivered. When several groups are buffered, the lowest key is
	 * returned first.
	 *
	 * Honors the range set by {@link withRange}: a group
	 * beyond the cap stays buffered (not dropped) and is offered once the cap rises, even
	 * after a clean close, without blocking in-range groups that arrive behind it.
	 * Once the track closes cleanly, a reader with nothing left in range finishes. So does a
	 * read whose {@link Subscription.ranges} all end, once every group in them was handed out,
	 * dropped, skipped as stale, or will never arrive, while the track goes on, as in Rust;
	 * later reads return `undefined` immediately too, and widening the ranges with {@link update}
	 * reads on without replaying groups already returned. A cap keeping a group out holds it
	 * open, since the cap may rise.
	 * A group whose presentation time is further behind the live edge than this
	 * subscriber's `delay.max` is skipped. The default subscription's zero takes the live edge.
	 * The budget remains attached after return, so a pending frame read rejects if a stalled
	 * group becomes stale while newer data advances.
	 *
	 * The first call commits this track to arrival order: {@link ordered} throws afterwards.
	 */
	async recvGroup(): Promise<GroupConsumer | undefined> {
		this.#live();
		this.#mode = "arrival";
		for (;;) {
			const recv = this.#tryRecvGroup();
			switch (recv.kind) {
				case "group":
					return recv.group;
				case "drop":
					continue;
				case "done":
					return this.#ended();
				case "error":
					throw recv.error;
			}

			// While open, wait for more groups or a cap raise.
			await this.#wait();
		}
	}

	// A drained read: the end of stream, or the error the track ended with once this reader's
	// range reaches that end. A range short of the end never reaches the error.
	#ended(): undefined {
		const error = hooks.subscriberEndError(this);
		if (error === undefined) return undefined;
		const end = hooks.subscriberEnd(this);
		const cap = this.#cursor.peek().end;
		if (end !== undefined && cap !== undefined && cap < finalKey(end)) return undefined;
		throw error;
	}

	// Park until a group read may go differently.
	#wait(): Promise<void> {
		return new Promise((resolve) => {
			const dispose = this.#groupChanged(() => {
				dispose();
				resolve();
			});
		});
	}

	// Package-internal synchronous half of recvGroup. The lite publisher uses this so applying
	// control state, popping the group, and positioning its frames are one JavaScript turn.
	#tryRecvGroup(dispositions = false): Recv {
		if (this.#resume) return this.#tryResumed(false, dispositions);
		const refused = this.#settle();
		if (refused) return { kind: "error", error: refused };
		this.#fillWait = false;
		const groups = this.#state.groups.peek();
		const { start, end } = this.#cursor.peek();

		if (dispositions) {
			const dropped = this.#tryDrop();
			if (dropped) return dropped;
		}
		const drift = this.#drift();
		this.#reoffer(drift.budget);
		// Keep unread groups below the floor until cache eviction: replacing the
		// subscription can lower it, just as raising the cap re-offers held groups. Found
		// after the re-offer, which may insert below the floor.
		let index = timelineIndex(groups, start);

		for (;;) {
			// The buffer is key-sorted, so an in-range group that arrives behind a
			// beyond-cap one sorts in front of it and is never blocked by it.
			const group = groups[index];
			if (!group || (end !== undefined && group.start >= end)) break;
			// Kept, like a group below the floor: replacing the ranges can take it in.
			if (!this.#window.contains(group.start)) {
				index++;
				continue;
			}
			groups.splice(index, 1);
			const failed = hooks.groupFailed(group);
			if (dispositions && failed) {
				// A group expired under this subscription's budget is a stale verdict.
				const hole =
					toStreamCode(failed) === StreamCode.Old
						? { ...staleHole(drift.budget), error: failed }
						: { error: failed };
				const dropped = this.#report(group.start, group.start + 1n, hole);
				if (dropped) return dropped;
				continue;
			}
			if (this.#delivered.has(group.start)) {
				group.close();
				continue;
			}
			if (this.#staleAt(group.start, drift)) {
				group.close();
				// A re-offered group stale at the bound is final.
				if (drift.bounded) this.#skipped.delete(group.start);
				else this.#skip(group.start, drift.budget);
				if (dispositions) {
					const dropped = this.#report(group.start, group.start + 1n, staleHole(drift.budget));
					if (dropped) return dropped;
				} else if (this.#dropsSeen) {
					// A cursor #tryDrop reads reports it there.
					this.#pending = insertDrop(this.#pending, group.start, group.start + 1n, staleHole(drift.budget));
				}
				continue;
			}
			// A group missing its head comes from the fill asked for it, in its turn.
			if (this.#lacksHead(group)) {
				if (this.#askHead(group.start)) {
					groups.splice(index++, 0, group);
					continue;
				}
				group.close();
				continue;
			}
			this.#handOut(group.start, false);
			return { kind: "group", group: this.#guard(group) };
		}

		const closed = this.#state.closed.peek();
		if (closed instanceof Error) return { kind: "error", error: closed };
		// A clean close completes this range, retaining groups a later cap raise can read.
		if (closed !== undefined) return { kind: "done" };
		// Nothing buffered: ask a fill for what this reader misses, and wait while one owes it.
		if (this.#askFills(start, end ?? UNBOUNDED, drift) !== undefined) return { kind: "idle" };
		// A time start still looked up may yet take in a group held already.
		if (this.#window.lookups().length > 0) return { kind: "idle" };
		// A bounded window ends once nothing in it is left to deliver, as in Rust.
		if (this.#exhausted()) return { kind: "done" };
		if (!this.#complete() || this.#starting(groups)) return { kind: "idle" };
		// An open track can still raise the cap. Keep the groups beyond it.
		return this.#parked(groups) ? { kind: "boundary" } : { kind: "done" };
	}

	// The first part of `[from, to)` this cursor has not reported yet within its window, now
	// reported, with why.
	#report(from: bigint, to: bigint, hole: Hole): (Recv & { kind: "drop" }) | undefined {
		const { start, end } = this.#cursor.peek();
		for (const run of this.#window.runs(max(from, start), min(to, end ?? to))) {
			const dropped = this.#reportRun(run.start, run.end, hole);
			if (dropped) return dropped;
		}
		return undefined;
	}

	// The first part of `[from, to)` nothing already reported blocks, now reported, with why,
	// replacing what it goes out over. Mirrors the Rust `Record::report`.
	#reportRun(from: bigint, to: bigint, hole: Hole): (Recv & { kind: "drop" }) | undefined {
		const start = max(from, this.#floor);
		if (start >= to) return undefined;
		const blocked = this.#reported.overlapping(start, to).filter((reported) => blocksHole(reported, hole));
		const [gap] = runGaps(blocked, start, to);
		if (!gap) return undefined;
		this.#reported.set(gap.start, gap.end, { ...gap, kind: hole.skip ? "skip" : "dropped" });
		return { kind: "drop", ...gap, error: hole.error, skip: hole.skip, stale: hole.stale };
	}

	// The next run this cursor gave up and has not reported, without taking a group. From the
	// first call on, a read without dispositions keeps its budget skips for it too. A resumed
	// one reports what its routes' copies gave up.
	#tryDrop(): (Recv & { kind: "drop" }) | undefined {
		if (this.#resume) {
			for (;;) {
				const queued = this.#nextResumedDrop();
				if (queued) return queued;
				const next = this.#resume.tryDrop(this.#delivered);
				if (!next) return undefined;
				this.#queueResumedDrop(next);
			}
		}
		this.#settle();
		const { start, end } = this.#cursor.peek();
		// The whole span is folded in again only after the window, the budget, or what settles a
		// stale run moved, since out-of-window and unsettling runs were skipped and may be owed
		// now; otherwise only the runs that changed are, so a drop costs each reader the runs it
		// touches.
		const { drops, refused } = this.#state;
		const revision = this.#revision;
		const { budget } = this.#drift();
		const coverage = this.#coverage();
		const seen = this.#dropsSeen;
		const full =
			!seen ||
			seen.refused !== refused.peek() ||
			seen.start !== start ||
			seen.end !== end ||
			seen.revision !== revision ||
			seen.budget !== budget ||
			!sameCoverage(seen.coverage, coverage);
		const changed: Run[] = full ? [{ start, end: end ?? UNBOUNDED }] : drops.since(seen.drops);
		if (full) this.#pending = this.#pending.filter((run) => !run.received);
		this.#dropsSeen = {
			drops: drops.revision,
			refused: refused.peek(),
			start,
			end,
			revision,
			budget,
			coverage,
		};
		for (const span of changed) {
			const from = max(span.start, start);
			const to = end === undefined ? span.end : min(span.end, end);
			if (from >= to) continue;
			if (!full) {
				this.#pending = this.#pending.flatMap((run) => (run.received ? removeRun([run], from, to) : [run]));
			}
			// The reader's own refusals too: clearing the span above took out those it covered.
			const folded = givenUp(drops, refused.peek(), from, to, (run) => settles(run, budget, coverage));
			const received: DropRun[] = [];
			for (const dropped of folded) {
				// Judged from where it meets this cursor's span, as Rust clips it.
				const run = { ...dropped, start: max(dropped.start, from), end: min(dropped.end, to) };
				if (run.start >= run.end) continue;
				// A received stale run that does not settle this reader is unknown to it.
				if (!settles(run, budget, coverage)) continue;
				received.push({ ...run, received: true });
			}
			// Sorted and disjoint: the settling drops and the refusals they leave never overlap.
			this.#pending = insertDrops(this.#pending, received);
		}
		for (let run = this.#pending.shift(); run; run = this.#pending.shift()) {
			// A received run the track has since forgotten, expired or superseded, is unknown again:
			// only the part it still holds as it was folded in is owed.
			if (run.received) {
				const folded = run;
				const held = givenUp(drops, refused.peek(), run.start, run.end, (run) =>
					settles(run, budget, coverage),
				).find((given) => sameVerdict(given, folded));
				if (!held) continue;
				if (held.end < run.end) this.#pending.unshift({ ...run, start: held.end });
				run = { ...run, start: max(run.start, held.start), end: min(run.end, held.end) };
			}
			const dropped = this.#report(run.start, run.end, run);
			if (!dropped) continue;
			if (dropped.end < run.end) this.#pending.unshift({ ...run, start: dropped.end });
			return dropped;
		}
		return undefined;
	}

	// The groups this reader reads, and its cap; see `Coverage`.
	#coverage(): Coverage {
		if (this.#window.pending) return undefined;
		return { runs: this.#window.runs(), cap: this.#cursor.peek().end };
	}

	// A group from the routes' copies; see #resume.
	#tryResumed(ordered: boolean, dispositions = false): Recv {
		const resume = this.#resume as ResumeSource;
		// A clean end leaves what the copies hold readable, as a direct reader's buffer is.
		const closed = this.#state.closed.peek();
		if (closed instanceof Error) return { kind: "error", error: closed };
		for (;;) {
			if (dispositions) {
				const queued = this.#nextResumedDrop();
				if (queued) return queued;
			}
			const recv = resume.tryGroup(ordered, this.#delivered, dispositions);
			if (recv.kind === "drop") {
				this.#queueResumedDrop(recv);
				continue;
			}
			if (recv.kind === "group") {
				// Passing the keys below it hands them out as far as #delivered knows, so first take what
				// the copies gave up there: a session-fed copy reports a key it passed to take this group.
				if (ordered) {
					const key = recv.group.start;
					for (
						let drop = resume.tryDrop(this.#delivered, key);
						drop;
						drop = resume.tryDrop(this.#delivered, key)
					) {
						this.#queueResumedDrop(drop);
					}
				}
				this.#handOut(recv.group.start, ordered);
			} else if (recv.kind === "idle" && this.#exhausted()) return { kind: "done" };
			return recv;
		}
	}

	// Queue a run a resumed reader's copy gave up. Each copy reports to its own route; this
	// reader's record keeps one route's run from going out over what another's already settled.
	#queueResumedDrop(dropped: Recv & { kind: "drop" }): void {
		const { start, end, error, skip, stale } = dropped;
		this.#pending.push({ start, end, error, skip, stale });
	}

	// The lowest queued run's first part nothing already reported blocks, now reported; the rest
	// of that run waits its turn. Mirrors the Rust `Record::next_pending`.
	#nextResumedDrop(): (Recv & { kind: "drop" }) | undefined {
		for (let run = this.#pending.shift(); run; run = this.#pending.shift()) {
			const dropped = this.#reportRun(run.start, run.end, run);
			if (!dropped) continue;
			if (dropped.end < run.end) this.#pending.unshift({ ...run, start: dropped.end });
			return dropped;
		}
		return undefined;
	}

	// The live edge reached the end, with no hole a wire subscription's pending tail still
	// owes below it. Reader completion does not settle the tail: open streams remain
	// accounted for by the wire layer, and an abort still wins on the next read.
	#complete(): boolean {
		const end = this.#state.end.peek();
		return (
			end !== undefined &&
			reaches(this.#state.received.peek(), this.#state.drops, end, this.#state.refused.peek()) &&
			!this.#state.owed.peek()
		);
	}

	// Package-internal readiness half of recvGroup. Each registration fires at most once, and
	// the caller disposes the losers after whichever source wakes it. A declared end wakes it
	// too, so a publisher can forward the end before the live edge reaches it.
	#groupChanged(fn: () => void): Dispose {
		if (this.#resume) {
			const resumed = [this.#resume.changed(fn), this.#cursor.changed(fn), this.#state.closed.changed(fn)];
			return () => {
				for (const close of resumed) close();
			};
		}
		const producer = this.#state.producer;
		const dispose = [
			this.#state.groups.changed(fn),
			this.#state.drops.version.changed(fn),
			this.#state.refused.changed(fn),
			this.#fillMoves.changed(fn),
			...this.#lookups.flatMap((lookup) => (lookup.lookup ? [lookup.lookup.changed.changed(fn)] : [])),
			// A handler opening takes a miss nobody could ask before.
			...(producer ? [fillsOf(producer).opened.changed(fn)] : []),
			this.#state.startChanged.changed(fn),
			// A group starting may settle a time end, or move the cap it holds reads back with.
			...(this.#window.settled ? [] : [this.#state.timelineChanged.changed(fn)]),
			// Accepting the track may refuse a time anchor, with no group arriving to say so.
			this.#state.info.changed(fn),
			this.#state.dropsAnchors.changed(fn),
			this.#cursor.changed(fn),
			// A widened budget re-offers the groups it skipped as stale.
			this.#state.update.changed(fn),
			this.#state.closed.changed(fn),
			this.#state.end.changed(fn),
			this.#state.received.changed(fn),
			this.#state.owed.changed(fn),
		];
		if (this.#state.waitingOrdered)
			dispose.push(this.#state.timelineChanged.changed(fn), this.#wall.revision.changed(fn));
		// A bounded window ends once the feed stops owing its last hole, which only time decides,
		// and a fill's hole stops holding a read once the budget convicts it.
		else if (this.#window.end() !== undefined || this.#fillWait) dispose.push(this.#wall.revision.changed(fn));
		return () => {
			for (const close of dispose) close();
		};
	}

	/**
	 * Take the next buffered group without blocking, honoring the same cursor bounds as
	 * {@link recvGroup}.
	 *
	 * Returns `undefined` when nothing is deliverable right now, which is not by itself the
	 * end of the track: a group may still arrive, or one may be parked beyond the
	 * {@link withRange} cap. Use it to drain what the retained window already holds, where
	 * waiting for a key nothing will republish would park forever.
	 *
	 * @internal
	 */
	tryRecvGroup(): GroupConsumer | undefined {
		this.#live();
		this.#mode = "arrival";
		return taken(
			() => this.#tryRecvGroup(),
			() => this.#ended(),
		);
	}

	// The key cursor behind {@link Ordered}, which owns the only public door to it.
	async #nextItem(): Promise<Item | undefined> {
		for (;;) {
			const recv = this.#tryItem();
			switch (recv.kind) {
				case "group":
				case "gap":
					return recv;
				case "done":
					return this.#ended();
				case "error":
					throw recv.error;
			}
			await this.#wait();
		}
	}

	// The next group in key order, past any gap.
	async #nextGroup(): Promise<GroupConsumer | undefined> {
		for (;;) {
			const item = await this.#nextItem();
			if (!item || item.kind === "group") return item?.group;
		}
	}

	// The synchronous half of #nextItem: the next group, or a gap the read gave up on below it,
	// as the Rust `Ordered::poll_item` merges them. Every gap goes out before the end.
	#tryItem(): ItemRecv {
		let items = this.#items;
		if (!items) {
			items = this.#items = { gaps: [], at: 0n };
			// The drop record starts before the first read, so the cursor keeps what it gives up.
			this.#findGaps(items);
		}
		const read = this.#readHeld(items);
		this.#findGaps(items);
		switch (read.kind) {
			case "group": {
				const key = read.group.start;
				const lowest = this.#lowestGap(items);
				// The gap lies wholly below it, or around it: the part below goes first.
				if (lowest && lowest.span.start < key) {
					// What lies above the group, or above the run when it ends before it, waits.
					const resume = lowest.span.end <= key ? lowest.span.end : key + 1n;
					const below = { start: lowest.span.start, end: min(lowest.span.end, key) };
					items.held = read.group;
					return this.#takeGap(items, lowest.index, below, resume);
				}
				// A group handed out is no gap, even one a drop also named, as a cut group is.
				if (lowest && lowest.span.start === key) this.#cutGap(items, lowest.index, key, key + 1n);
				items.at = max(items.at, key + 1n);
				this.#fold(this.#delivered.runEnd(0n) ?? 0n);
				return read;
			}
			case "done":
			case "error": {
				const lowest = this.#lowestGap(items);
				if (lowest) return this.#takeGap(items, lowest.index, lowest.span, lowest.span.end);
				return read;
			}
			case "drop":
				// Only a read with dispositions reports a drop, which this one finds through the record.
				throw new Error("an ordered read took a drop");
			default: {
				// What the read still owes lies above the gap, so the gap goes first.
				const lowest = this.#lowestGap(items);
				if (!lowest) return { kind: "idle" };
				const owed = this.nextInRange(items.at);
				const ready = lowest.span.start <= items.at || owed === undefined || owed >= lowest.span.start;
				return ready ? this.#takeGap(items, lowest.index, lowest.span, lowest.span.end) : { kind: "idle" };
			}
		}
	}

	// The group held behind a gap, while the bounds still cover it, else the cursor's next. A group
	// read is settled, so no later cut or drop of it is a gap.
	#readHeld(items: Items): Recv {
		const held = items.held;
		if (held) {
			items.held = undefined;
			if (this.#covered(held.start, held.start + 1n).length > 0) return { kind: "group", group: held };
			// The bounds moved off it. With nothing they cover above, it waits for them to widen again,
			// still polling the cursor so the read sees the track or itself end; otherwise the read
			// passes it.
			if (this.#covered(held.start, UNBOUNDED).length === 0) {
				const read = this.#tryNextGroup();
				// A clean end keeps it, as the buffer keeps a group past the cap, for a raise to read.
				if (read.kind === "error") held.close();
				else items.held = held;
				return read.kind === "done" || read.kind === "error" ? read : { kind: "idle" };
			}
			held.close();
		}
		const read = this.#tryNextGroup();
		if (read.kind === "group") this.#settled(read.group.start, read.group.start + 1n);
		return read;
	}

	// The parts of `[start, end)` this reader's ranges and bounds cover now.
	#covered(start: bigint, end: bigint): Run[] {
		const cursor = this.#cursor.peek();
		const from = max(start, cursor.start);
		const to = cursor.end === undefined ? end : min(end, cursor.end);
		if (from >= to) return [];
		if (this.#resume) return this.#resume.runs(from, to) ?? [{ start: from, end: to }];
		return this.#window.runs(from, to);
	}

	// Take every run the record gave up into the gaps waiting, in whatever order they come. A drop
	// replaces what it overlaps, and a skip was never a group, so it removes them. Runs come lowest
	// first, so one past every gap waiting is appended without copying the rest.
	#findGaps(items: Items): void {
		for (let dropped = this.#tryDrop(); dropped; dropped = this.#tryDrop()) {
			const last = items.gaps.at(-1);
			if (!last || last.end <= dropped.start) {
				if (dropped.skip) continue;
				const gap = { start: dropped.start, end: dropped.end, error: dropped.error };
				if (last && last.end === gap.start && sameHole(last, gap)) last.end = gap.end;
				else items.gaps.push(gap);
				continue;
			}
			items.gaps = dropped.skip
				? removeRun(items.gaps, dropped.start, dropped.end)
				: insertDrop(items.gaps, dropped.start, dropped.end, { error: dropped.error });
		}
	}

	// The lowest gap waiting that the bounds cover, as its index and the first part of it they cover,
	// since they may have moved since it was found. A gap they exclude waits, in case they widen.
	#lowestGap(items: Items): { index: number; span: Run } | undefined {
		for (let index = 0; index < items.gaps.length; index++) {
			const gap = items.gaps[index];
			const [span] = this.#covered(gap.start, gap.end);
			if (span) return { index, span };
		}
		return undefined;
	}

	// Take `given` from the gap at `index` as an item, which the read has now moved past. What lies
	// below `given`, which the bounds leave out, and from `resume` on stay gaps.
	#takeGap(items: Items, index: number, given: Run, resume: bigint): Item {
		const error = this.#cutGap(items, index, given.start, resume);
		items.at = max(items.at, given.end);
		// Told once: no later drop of these keys is a gap again.
		this.#settled(given.start, given.end);
		// Passed for good, as a group handed out is: an ordered read never goes back, so no group
		// below its end is handed out after it, whether it lands late here or on a resumed
		// reader's copy, and a fill no longer owes it there.
		const floor = this.#delivered.runEnd(0n) ?? 0n;
		this.#delivered.forgetBelow(given.end);
		const forgot = this.#delivered.runEnd(0n) ?? 0n;
		if (forgot > floor) this.#stoppedReading(floor, forgot);
		if (!this.#resume) this.#nextKey = max(this.#nextKey, given.end);
		this.#fold(forgot);
		return { kind: "gap", start: given.start, end: given.end, error };
	}

	// Remove `[from, resume)` from the gap at `index`, returning its error.
	#cutGap(items: Items, index: number, from: bigint, resume: bigint): Error {
		const gap = items.gaps[index];
		const kept: DropRun[] = [];
		if (gap.start < from) kept.push({ ...gap, end: from });
		if (gap.end > resume) kept.push({ ...gap, start: resume });
		items.gaps.splice(index, 1, ...kept);
		return gap.error;
	}

	// Record `[start, end)` as settled: handed out, or told as a gap.
	#settled(start: bigint, end: bigint): void {
		start = max(start, this.#floor);
		if (start < end) this.#reported.set(start, end, { start, end, kind: "settled" });
	}

	// Fold the record below `at`, where a key-order read is, into the floor, held below every run
	// still to report there: a run given up and not reported yet, and a gap found and not returned
	// that the read may still reach.
	// The record then holds only what lies ahead of the read, bounded however long it plays. A
	// session-fed read loses nothing, since every key it passed was handed out or reported; a group
	// a local track produces below the floor is told as nothing, where Rust, folding only under pool
	// pressure, would report it Old. A wire publisher's record is what its wire was told, which it
	// keeps.
	#fold(at: bigint): void {
		if (this.#wired) return;
		const items = this.#items;
		if (items) {
			// A gap the bounds left out below where the read is can never come in key order, and an
			// ordered read never goes back, so it is dropped rather than held for a widening.
			if ((items.gaps[0]?.start ?? items.at) < items.at) items.gaps = removeRun(items.gaps, 0n, items.at);
			at = min(at, items.at);
			for (const gap of items.gaps) at = min(at, gap.start);
		}
		for (const run of this.#pending) if (run.end > this.#floor) at = min(at, run.start);
		if (at <= this.#floor) return;
		this.#floor = at;
		this.#reported.set(0n, at);
	}

	// The key cursor passes `[start, end)` to hand out the group at `end`. Each key there that the
	// track owed and no group or drop accounts for was given up, whether the budget convicted it or
	// the tail's grace folded it, so it is stale. A wire-fed copy's feed owes every key from where it
	// starts, and a fill what this reader asked it for. Below the first group passed is where the
	// read joined, which is no gap.
	#passOwed(start: bigint, end: bigint, drift: Drift): void {
		const producer = this.#state.producer;
		if (start >= end || this.#nextKey === 0n || !this.#dropsSeen || !producer) return;
		const owed = [...intersect(this.#asked, start, end), ...intersect(this.#givenUp, start, end)];
		if (wireFed(producer)) owed.push(...this.#window.runs(max(start, feedFloor(producer) ?? 0n), end));
		// Only a drop that settles this reader reports the key; a stale verdict under a tighter budget
		// does not, so the key is still this reader's to give up.
		const { drops, refused } = this.#state;
		const coverage = this.#coverage();
		for (const run of owed) {
			const settled = givenUp(drops, refused.peek(), run.start, run.end, (drop) =>
				settles(drop, drift.budget, coverage),
			).filter((drop) => settles({ ...drop, start: max(drop.start, run.start) }, drift.budget, coverage));
			for (const passed of runGaps(settled, run.start, run.end)) {
				this.#giveUp(passed, staleHole(drift.budget));
			}
		}
	}

	// Keep a run the key cursor gave up, once the drop record started, so a read reports it.
	#giveUp(run: Run, hole: Hole): void {
		if (this.#dropsSeen) this.#pending = insertDrop(this.#pending, run.start, run.end, hole);
	}

	// A missing group the subscription still owes holds sequence order until it lands,
	// is accounted for, or the same drift budget as a buffered group gives up on it.
	#waitOwed(start: bigint, end: bigint, drift: Drift): boolean {
		const producer = this.#state.producer;
		if (start >= end || !producer) return false;
		// Only a hole in this reader's ranges is owed to it.
		let owed: { key: bigint; folds?: Instant } | undefined;
		for (const run of this.#window.runs(start, end)) {
			owed = owedGroup(producer, run.start, run.end);
			if (owed) break;
		}
		if (!owed) {
			// A fill still owes a group below, which lands as a group does. Its staleness deadline
			// wakes the cursor too, so a fill that never answers cannot pin it.
			if (this.#askFills(start, end, drift) === undefined) return false;
			this.#state.waitingOrdered = true;
			return true;
		}
		if (this.#staleAt(owed.key, drift, true)) return false;
		if (owed.folds !== undefined) this.#wakeAt(owed.folds.since(Instant.ZERO).asNanos());
		this.#state.waitingOrdered = true;
		return true;
	}

	// The lowest buffered candidate in range that the budget spares. An ordered take may
	// still wait on an owed hole below it, so a resumed reader must keep its place.
	#peekGroup(ordered: boolean): bigint | undefined {
		this.#settle();
		const cursor = this.#cursor.peek();
		const start = ordered ? max(cursor.start, this.#nextKey) : cursor.start;
		const drift = this.#drift();
		for (const group of this.#state.groups.peek()) {
			if (group.start < start || !this.#window.contains(group.start)) continue;
			if (cursor.end !== undefined && group.start >= cursor.end) return undefined;
			if (!this.#delivered.has(group.start) && !this.#staleAt(group.start, drift)) {
				return group.start;
			}
		}
		return undefined;
	}

	// The synchronous half of #nextGroup.
	#tryNextGroup(): Recv {
		const recv = this.#resume ? this.#tryResumed(true) : this.#tryNextBuffered();
		// One cursor: the frame helpers must not keep draining a group this read just
		// moved past, or interleaved reads would run backwards.
		if (recv.kind === "group" && this.#frameGroup && this.#frameGroup.start < recv.group.start) {
			this.#frameGroup.close();
			this.#frameGroup = undefined;
		}
		return recv;
	}

	#tryNextBuffered(): Recv {
		const refused = this.#settle();
		if (refused) return { kind: "error", error: refused };
		this.#state.waitingOrdered = false;
		this.#fillWait = false;
		const groups = this.#state.groups.peek();
		const cursor = this.#cursor.peek();
		let start = max(cursor.start, this.#nextKey);
		// A group landing after the read passed its key is old: the read never goes back.
		for (let group = groups[0]; group && group.start < start; group = groups[0]) {
			groups.shift();
			group.close();
			// The record tells nothing twice, so a key already handed out or given up stays told.
			this.#giveUp({ start: group.start, end: group.start + 1n }, late());
		}
		// One anchor for the whole pass, so walking a backlog off stays linear.
		const drift = this.#drift();

		for (let index = 0; ; ) {
			const group = groups[index];
			if (!group || (cursor.end !== undefined && group.start >= cursor.end)) break;
			// Out of range for now, but kept until the cursor passes it: an anchor still
			// resolving, or an update widening the ranges, may take it in.
			if (!this.#window.contains(group.start)) {
				index++;
				continue;
			}
			// Every frame this group could still hold is past the budget, so keep
			// scanning: one pass walks a whole backlog off rather than replaying it.
			if (this.#staleAt(group.start, drift)) {
				// Passing it passes the owed keys below it too, which the budget gives up with it.
				this.#passOwed(start, group.start, drift);
				this.#giveUp({ start: group.start, end: group.start + 1n }, staleHole(drift.budget));
				const passed = this.#nextKey;
				this.#pass(groups, index);
				// The cursor passed what lies below it, which a fill no longer owes this reader.
				this.#stoppedReading(passed, this.#nextKey);
				// Holes below a skipped group are undeliverable, even ones the wall clock
				// still spares because their successor arrived late.
				start = this.#nextKey;
				index = 0;
				group.close();
				continue;
			}
			if (this.#waitOwed(start, group.start, drift)) return { kind: "idle" };
			// A group missing its head comes from the fill asked for it, in its turn.
			if (this.#lacksHead(group)) {
				if (this.#askHead(group.start)) {
					// Its staleness deadline wakes the cursor, as for any owed hole.
					this.#fillWait = true;
					return { kind: "idle" };
				}
				// Given up: past it, as past any group no fill can serve, and the owed keys below it.
				this.#passOwed(start, group.start, drift);
				this.#giveUp({ start: group.start, end: group.start + 1n }, headless());
				const passed = this.#nextKey;
				this.#pass(groups, index);
				this.#stoppedReading(passed, this.#nextKey);
				start = this.#nextKey;
				index = 0;
				group.close();
				continue;
			}
			this.#passOwed(start, group.start, drift);
			this.#pass(groups, index);
			this.#handOut(group.start, true);
			return { kind: "group", group: this.#guard(group) };
		}

		const closed = this.#state.closed.peek();
		if (closed instanceof Error) return { kind: "error", error: closed };
		// A clean close completes this range, retaining groups a later cap raise can read.
		if (closed !== undefined) return { kind: "done" };
		// Nothing buffered: ask a fill for what this reader misses, and wait while one owes it.
		if (this.#askFills(start, cursor.end ?? UNBOUNDED, drift) !== undefined) {
			// Its staleness deadline wakes the cursor, as for any owed hole.
			this.#state.waitingOrdered = true;
			return { kind: "idle" };
		}
		const end = this.#state.end.peek()?.at;
		// A time start still looked up may yet take in a group held already.
		if (this.#window.lookups().length > 0) return { kind: "idle" };
		if (this.#exhausted()) return { kind: "done" };
		if (
			!this.#parked(groups) &&
			!this.#starting(groups, start) &&
			(this.#complete() || (end !== undefined && start >= finalKey(end)))
		) {
			return { kind: "done" };
		}
		return { kind: "idle" };
	}

	/**
	 * Reads the next frame across groups, in sequence order, with its group and frame numbers.
	 * Treat the returned frame bytes as read-only; they are shared with other consumers.
	 *
	 * Groups are acquired through the same key cursor as {@link Ordered.nextGroup},
	 * so frames never run backwards: a late lower-key group is skipped, and so is
	 * one every frame of which `delay.max` proves is too old. A group the budget abandons
	 * mid-stall ends cleanly and the cursor resyncs from the next group; a gap inside a
	 * group still surfaces as `Evicted`, {@link TooFarBehind}, or {@link GroupTooLarge}.
	 */
	async #readFrame(): Promise<({ group: bigint; frame: number } & Frame) | undefined> {
		for (;;) {
			if (!this.#frameGroup) {
				this.#frameGroup = await this.#nextGroup();
				if (!this.#frameGroup) return undefined;
			}
			const group = this.#frameGroup;

			let next: ({ index: number } & Frame) | undefined;
			try {
				next = await group.readFrameIndex();
			} catch (err) {
				// The group failed underneath us: resync from the next one, surfacing
				// only what the caller can act on (a gap, or the track's own abort).
				this.#frameGroup = undefined;
				group.close();
				if (
					err instanceof TooFarBehind ||
					err instanceof GroupTooLarge ||
					(err instanceof StreamError && err.code === StreamCode.Evicted)
				)
					throw err;
				const closed = this.#state.closed.peek();
				if (closed instanceof Error) throw closed;
				continue;
			}

			if (next) {
				return {
					group: group.start,
					frame: next.index,
					payload: next.payload,
					timestamp: next.timestamp,
				};
			}

			// The group is exhausted (or the budget abandoned its stall); move on.
			this.#frameGroup = undefined;
			group.close();
		}
	}

	/** Reads the next frame and decodes it as a UTF-8 string. */
	async #readString(): Promise<string | undefined> {
		const next = await this.#readFrame();
		if (!next) return undefined;
		return new TextDecoder().decode(next.payload);
	}

	/** Reads the next frame and parses it as JSON. */
	async #readJson(): Promise<unknown | undefined> {
		const next = await this.#readString();
		if (next === undefined) return undefined;
		return JSON.parse(next);
	}

	/**
	 * Read this track's groups in sequence order instead of arrival order.
	 *
	 * Both cursors draw from the same buffer, so a track is read one way or the other: this
	 * hands the subscription to the returned {@link Ordered} and leaves this handle inert.
	 * {@link recvGroup} throw afterwards. Throws once a
	 * {@link recvGroup} call has already committed the track to arrival order.
	 *
	 */
	ordered(): Ordered {
		if (this.#mode === "ordered") throw new Error("track is already read in sequence order");
		if (this.#mode === "arrival") throw new Error("track is already read in arrival order");
		this.#mode = "ordered";
		return makeOrdered(this);
	}

	/**
	 * Ask the publisher for a group starting after the newest one this node holds, judged by the
	 * track's {@link Info.refresh}, resolving once the request is sent. Mirrors the Rust
	 * `Subscriber::refresh`.
	 *
	 * The request is made now. It is sent once the newest group is old enough, when whatever
	 * serves this node's copy reads it: the session forwarding it upstream, or a local publisher
	 * watching {@link Demand.refresh}. Concurrent requests coalesce into one, and all of them
	 * resolve when it is sent. Aborting `options.signal` before then withdraws this caller's
	 * request and rejects with the signal's reason; one already sent stays sent. It resolves at
	 * once when there is nothing to send: a track that does not refresh, a ratio the newest group
	 * is under, a track with no group yet (whose first is already asked for), or a request
	 * already sent. It also resolves when a newer group lands first, which answers it. It rejects
	 * if the track closes before then. A resumed reader asks the copy serving it, and asks the next
	 * one if that copy closes first.
	 *
	 * The answer arrives like any other group: the first group this subscriber receives that
	 * starts after the newest held when it was called. A group already on its way when the
	 * request reached the publisher answers it, so across hops "made after this call" is best
	 * effort; a reader that needs that puts its own marker in the frame.
	 */
	refresh(options?: RefreshOptions): Promise<void> {
		if (this.#resume) return this.#resume.refresh(options?.signal);
		const producer = this.#state.producer;
		if (options?.signal?.aborted) return Promise.reject(options.signal.reason);
		return producer ? hooks.requestProducer(producer, options?.signal) : Promise.resolve();
	}

	/**
	 * Update this subscription's options (e.g. priority), triggering a SUBSCRIBE_UPDATE to the
	 * publisher. Replacing the ranges resolves them again, without redelivering groups already
	 * read; an arrival reader can receive the unread groups it still holds that the new ranges
	 * take in.
	 * Mirrors the Rust `Subscriber::update`.
	 */
	update(options: Subscription) {
		const subscription = subscriptionDefaults(options);
		const replaced = !sameRanges(subscription.ranges ?? [], this.#window.ranges);
		const reordered = subscription.order !== this.#state.update.peek()?.order;
		this.#state.update.set(subscription);
		if (replaced) this.#resolve();
		// A live join not yet started reaches back by the Min Age stated when it starts.
		else this.#window.min = subscription.delay?.min ?? undefined;
		// A fill's order follows its readers, so its handler wakes to serve the new first group.
		if (!replaced && reordered) this.#fillsMoved();
	}
}

/**
 * A {@link Subscriber} that reads groups in sequence order.
 *
 * Created by {@link Subscriber.ordered}, which takes the subscription over: the two
 * cursors draw from one buffer, so a track is read one way or the other and never both.
 * Every group this returns has a higher key than the last, so a late arrival is
 * reported as a gap rather than delivered out of turn, as is every group the read gives up on
 * (see {@link Ordered.nextGroup}).
 *
 * `delay.max` applies as this cursor reads, exactly as it does on the arrival cursor: a
 * group is skipped once its reach, where its immediate successor begins, is that far
 * behind the newest frame on the track. Nothing weaker convicts it, since the reach is
 * the only proof that every frame it could still hold is past the budget, so a backlog
 * inside the budget is still delivered whole, as a burst in order. The budget follows a
 * group already handed out too: a stalled group's pending frame read rejects once newer
 * content has pulled that far ahead.
 *
 * A history or VOD read sets `delay: { max: null }` (no limit). A subscribed track waits for a
 * missing group the feed still owes, within the budget, or until it resolves without one.
 * Local tracks skip missing groups. An unlimited or long budget
 * only avoids blocking where the feed signals its gaps: moq-lite, whose publisher sends
 * `SUBSCRIBE_DROP` for every group it won't deliver, or an ordered moq-transport FETCH.
 * Over a moq-transport SUBSCRIBE, which signals no gaps, that wait can last until the
 * track ends.
 */
export class Ordered {
	/** The track name. */
	readonly name: string;

	#subscriber: Subscriber;

	private constructor(subscriber: Subscriber) {
		this.name = subscriber.name;
		this.#subscriber = subscriber;
	}

	static {
		makeOrdered = (subscriber) => new Ordered(subscriber);
	}

	/** Resolve this track's immutable publisher properties; see {@link Subscriber.info}. */
	info(): Promise<Info> {
		return this.#subscriber.info();
	}

	/** Settles once the track closes; see {@link Producer.closed}. */
	get closed(): GetPromise<Error | null> {
		return this.#subscriber.closed;
	}

	/** This subscriber's current options, including defaults and the last {@link update}. */
	get subscription(): Getter<Subscription | undefined> {
		return this.#subscriber.subscription;
	}

	/** The largest location within {@link withRange}; see {@link Subscriber.largest}. */
	largest(): Promise<Position | undefined> {
		return this.#subscriber.largest();
	}

	/** Resolve with the track's exclusive end once known; see {@link Subscriber.finished}. */
	finished(): Promise<Position> {
		return this.#subscriber.finished();
	}

	/** Whether the publisher declared every start in `[start, end)` skipped; see {@link Subscriber.isSkipped}. */
	isSkipped(start: bigint, end: bigint): boolean {
		return this.#subscriber.isSkipped(start, end);
	}

	/**
	 * Limit subsequent reads to these groups and return this reader for chaining; see
	 * {@link Subscriber.withRange}. An ordered read never goes back, so lowering the floor
	 * below a group it already returned offers nothing, not even a gap it passed while the
	 * range left it out.
	 */
	withRange(groups: Groups): this {
		this.#subscriber.withRange(groups);
		return this;
	}

	/** Update this subscription's options; see {@link Subscriber.update}. */
	update(options: Subscription) {
		this.#subscriber.update(options);
	}

	/** Ask for a group newer than the newest held; see {@link Subscriber.refresh}. */
	refresh(options?: RefreshOptions): Promise<void> {
		return this.#subscriber.refresh(options);
	}

	/** Close the track (optionally with an error), closing any pending groups. Idempotent. */
	close(abort?: Error) {
		this.#subscriber.close(abort);
	}

	/**
	 * Return the next group with a strictly-greater sequence number than the last returned,
	 * passing any gap {@link nextItem} would report below it. Mirrors the Rust `next_group`.
	 *
	 * Late arrivals (key at or below the last returned) are silently skipped, as is a
	 * group whose every frame is further behind the live edge than `delay.max` (the default
	 * subscription's zero keeps only what nothing newer has superseded). Honors the bounds set by
	 * {@link withRange}. Like {@link Subscriber.recvGroup}, a read whose ranges all end
	 * finishes once nothing in them is left to deliver. Later reads return `undefined`
	 * immediately too; widening the ranges with {@link update} reads on above the last key returned.
	 */
	nextGroup(): Promise<GroupConsumer | undefined> {
		return ordered_.nextGroup(this.#subscriber);
	}

	/**
	 * Take the next group as {@link nextGroup} would, without waiting: `undefined` when nothing
	 * is deliverable right now, which is not by itself the end of the track. Mirrors the Rust
	 * `poll_next_group`.
	 */
	tryNextGroup(): GroupConsumer | undefined {
		for (;;) {
			const item = this.tryNextItem();
			if (item?.kind !== "gap") return item?.group;
		}
	}

	/**
	 * The next group, as {@link nextGroup} returns it, or a gap this reader gave up on below it.
	 * Mirrors the Rust `next_item` and its `Item`.
	 *
	 * A gap is every group the publisher or a relay dropped, the transport evicted, `delay.max`
	 * aged out, a fill could not find, or that landed after the read passed its key. It carries
	 * the keys it spans and is returned once, after any group the read still owes below it and
	 * before the next group above it, or when the track ends. Keys the publisher declared skipped
	 * were never groups, so they are no gap, and nor is a key a local track never produced.
	 * Unlike Rust, nor is a group a local track produces after the read moved past its key: the
	 * read keeps no record below where it is, so its memory stays bounded. A session-fed track
	 * reports such a key at the latest when the read passes it.
	 */
	nextItem(): Promise<Item | undefined> {
		return ordered_.nextItem(this.#subscriber);
	}

	/**
	 * Take the next item as {@link nextItem} would, without waiting: `undefined` when nothing
	 * is ready right now, which is not by itself the end of the track. Mirrors the Rust
	 * `poll_next_item`.
	 */
	tryNextItem(): Item | undefined {
		const recv = ordered_.tryItem(this.#subscriber);
		switch (recv.kind) {
			case "group":
			case "gap":
				return recv;
			case "error":
				throw recv.error;
			case "done":
				return ordered_.ended(this.#subscriber);
			default:
				return undefined;
		}
	}

	/**
	 * Read the next frame across groups, in sequence order, with its group and frame numbers.
	 *
	 * Rides the same cursor as {@link nextGroup} and shares this handle's contract: a
	 * buffered backlog is drained in full up to the point `delay.max` proves it useless.
	 * Treat the returned frame bytes as read-only; they are shared with other consumers.
	 */
	readFrame(): Promise<({ group: bigint; frame: number } & Frame) | undefined> {
		return ordered_.readFrame(this.#subscriber);
	}

	/** Read the next frame and decode it as a UTF-8 string. */
	readString(): Promise<string | undefined> {
		return ordered_.readString(this.#subscriber);
	}

	/** Read the next frame and parse it as JSON. */
	readJson(): Promise<unknown | undefined> {
		return ordered_.readJson(this.#subscriber);
	}
}

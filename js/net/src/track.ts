/**
 * Track role handles: a live stream of groups (and best-effort datagrams) within a broadcast.
 *
 * @module
 */

import { Instant, Milli, Nano, now, sleepUntil } from "@moq/clock";
import { type Dispose, type GetPromise, type Getter, Once, Signal } from "@moq/signals";
import type { Datagram } from "./datagram.ts";
import { Delivered } from "./delivered.ts";
import {
	GroupTooLarge,
	ProtocolViolation,
	StreamCode,
	StreamError,
	TimestampMismatch,
	TooFarBehind,
	toStreamCode,
} from "./error.ts";
import { datagramSlice, type Stored, sliceView } from "./flate/group.ts";
import { type Frame, type Consumer as GroupConsumer, Producer as GroupProducer } from "./group.ts";
import type { IetfVersion } from "./ietf/version.ts";
import {
	type DropRun,
	dropAt,
	finalSequence,
	groupBounds,
	hooks,
	insertDrop,
	insertRun,
	type Land,
	MAX_DATAGRAMS,
	NO_MAX_DELAY,
	type Recv,
	type RecvDatagram,
	type ResumeSource,
	type Run,
	removeRun,
	runGaps,
	type StagedTrack,
	type TrackRequestOptions,
} from "./internal.ts";
import { grace, Ledger } from "./ledger.ts";
import { Timescale, type Timestamp } from "./time.ts";
import { type Broadcast as BroadcastWire, registerTrackConsumer } from "./wire.ts";

export type { Datagram } from "./datagram.ts";

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

// Wall-clock bound for idle cached content, matching Rust cache::DEFAULT_EXPIRY.
// Media maxDelay only bounds how far behind the live edge a reader may fall.
const CACHE_WINDOW = Nano.fromMilli(Milli(30_000));

// The cache scans at most this many times per idle cache window.
const PRUNE_SLICES = 8;

// The unset priority, the midpoint of the signed range: moq-lite carries it as 128 and
// moq-transport as 127, one urgency on both.
const DEFAULT_PRIORITY = 0;

/**
 * Sanity cap on a datagram payload: the QUIC DATAGRAM frame ceiling. The real limit is
 * per-hop (the negotiated transport datagram size minus a small header) and oversize
 * datagrams are dropped there; a payload above this cap could never fit anywhere.
 */
const MAX_DATAGRAM_BYTES = 65535;

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
 * A position within a track: a group's sequence and a frame's index inside it.
 *
 * As a track's end it is exclusive: `{ group: 90, frame: 0 }` ends before group 90, and
 * `{ group: 89, frame: 5 }` ends group 89 after its frames 0 to 4. Mirrors the Rust
 * `track::Position`.
 */
export interface Position {
	/** The group's sequence number within the track. */
	group: number;
	/** The frame's index within that group. */
	frame: number;
}

/**
 * A track's immutable publisher properties, fixed for the lifetime of the track.
 *
 * A producer declares these once (via {@link Request.accept} or
 * {@link Producer.accept}); a consumer awaits them via {@link Subscriber.info}
 * (resolved from the wire TRACK_INFO on lite-05+). They map 1:1 onto TRACK_INFO.
 */
export interface Info {
	/** Compress frame payloads per group on lite-07; other sessions receive plain frames. Defaults to false. */
	compress?: boolean;
	/**
	 * Units per second for this track's frame timestamps, or omitted (`undefined` or `null`,
	 * which mean the same) for an untimed track, whose frames and datagrams carry no timestamp.
	 *
	 * There is no default: a timed track declares its scale, such as {@link Timescale | Timescale.MILLI},
	 * or {@link Timescale | Timescale.MICRO} for media that needs sub-millisecond timing. Reported in
	 * TRACK_INFO on Lite05+ and as the TIMESCALE property on moq-transport. A track received
	 * without one (moq-lite before Lite05, or moq-transport without TIMESCALE) is untimed,
	 * and stays untimed when served onward.
	 */
	timescale?: Timescale | null;
	/**
	 * Publisher Max Age: how long, in milliseconds, a superseded group stays fresh. It is
	 * stale once either clock reaches the budget: the wall-clock time since its successor
	 * arrived at this hop, or how far its successor's first timestamp trails the live edge.
	 * The newest group is never stale, and an untimed track is judged by the wall clock alone.
	 * Reported in TRACK_INFO (Lite05+) so relays re-serve with the same bound. Omission sets
	 * no publisher limit; zero serves only the live edge. This clamps each subscriber budget.
	 * Rounded up to a whole millisecond by {@link infoDefaults}, which refuses a negative
	 * or non-finite value and a result past `Number.MAX_SAFE_INTEGER`.
	 */
	maxAge?: Milli;
	/** Tie-break priority between subscriptions of equal subscriber priority (`-128..=127`, higher first). Defaults to `0`. */
	priority: number;
	/**
	 * How many times its first frame a group's later stored bytes may grow before a joining
	 * subscriber asks for a new group, or omitted to never ask. A join against a newest group
	 * whose bytes after its first frame exceed this many times the first frame moves
	 * {@link Demand.wanted} past it. The joiner still receives the cached group while the new
	 * one is pending. `0` asks on any join after a delta. An integer in `0..2^32`.
	 */
	refresh?: number;
	/** The original moq-transport publisher's draft, whose timestamps may rewind. @internal */
	ietf?: IetfVersion;
	/**
	 * Whether this copy's groups skip the rewind check, since its timestamps may be lite-05/06 send
	 * times. An {@link Info.ietf} track skips it too. Mirrors the Rust `track::Info::unchecked`.
	 * @internal
	 */
	unchecked?: boolean;
}

// Normalize a duration for the wire, which carries it as an unsigned varint.
//
// Callers derive it from measurements (a jitter estimate scaled off RTT), so a fractional
// millisecond is expected; ceil rather than round, because a budget shortened by rounding
// skips a group the subscriber still wants. Anything that is not a duration is refused
// here, where the field is named, rather than deep in the encoder.
function wireMillis(field: string, value: Milli): Milli {
	if (!Number.isFinite(value) || value < 0) {
		throw new RangeError(`${field} must be a non-negative number of milliseconds: ${value}`);
	}
	const millis = Math.ceil(value);
	if (!Number.isSafeInteger(millis)) {
		throw new RangeError(`${field} exceeds the safe integer millisecond range: ${value}`);
	}
	return Milli(millis);
}

function priorityValue(value: number): number {
	if (!Number.isInteger(value) || value < -128 || value > 127) {
		throw new RangeError(`priority must be an integer in -128..=127: ${value}`);
	}
	return value;
}

/** Fill in any unset {@link Info} fields with their defaults. */
export function infoDefaults(info: Partial<Info> = {}): Info {
	if (info.compress !== undefined && typeof info.compress !== "boolean")
		throw new TypeError("compress must be a boolean");
	const timescale = info.timescale == null ? undefined : Timescale(info.timescale);
	return {
		compress: info.compress ?? false,
		timescale,
		maxAge: info.maxAge === undefined ? undefined : wireMillis("maxAge", info.maxAge),
		priority: priorityValue(info.priority ?? DEFAULT_PRIORITY),
		refresh: info.refresh == null ? undefined : refreshRatio(info.refresh),
		...(info.ietf === undefined ? {} : { ietf: info.ietf }),
		...(info.unchecked ? { unchecked: true } : {}),
	};
}

function refreshRatio(value: number): number {
	if (!Number.isInteger(value) || value < 0 || value > 0xffff_ffff) {
		throw new RangeError(`refresh must be an integer in 0..2^32: ${value}`);
	}
	return value;
}

/** An explicitly included or excluded group sequence. */
export type Bound = { included: number; excluded?: never } | { excluded: number; included?: never };

/** Group limits; an omitted endpoint is unbounded. */
export interface Groups {
	/** The first group boundary. */
	start?: Bound;
	/** The last group boundary. */
	end?: Bound;
}

/**
 * Per-subscription options, requested when a subscription opens and adjustable later via
 * {@link Subscriber.update}. Mirrors the Rust `Subscription`.
 * The obsolete subscriber `maxAge` field is refused at runtime; use `maxDelay`.
 */
export interface Subscription {
	/** Delivery priority relative to this session's other subscriptions (`-128..=127`, higher first). Defaults to `0`. */
	priority?: number;
	/**
	 * How long (milliseconds) a non-latest group stays worth delivering, by the same two
	 * clocks as {@link Info.maxAge}: it is skipped once its successor arrived this long ago
	 * or starts this far behind the live edge, and never past the publisher's
	 * {@link Info.maxAge}. Omitted (or `undefined`) means `0`, the live edge only, timed or
	 * not, as in {@link Subscription.default}. `null` sets no subscriber limit, so the
	 * publisher's {@link Info.maxAge} alone bounds it, which is what a reader reaching back
	 * for history wants; read in order, see {@link Ordered} for where that can block.
	 * Rounded up to a whole millisecond, so a value derived from a measurement is never
	 * shortened. A negative or non-finite value, or one past `Number.MAX_SAFE_INTEGER`
	 * after rounding, is refused.
	 */
	maxDelay?: Milli | null;
	/**
	 * The groups this reader may receive, with an optional floor and cap.
	 *
	 * Only {@link maxDelay} asks for data; these bounds limit which groups it may reach.
	 * An omitted start means 0, and an omitted end leaves the range open. A floor above
	 * the live edge waits there (a resumed subscription naming where it left off).
	 * The latency budget uses the latest eligible group within these bounds.
	 */
	groups?: Groups;
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
 * {@link Subscription.maxDelay}'s job, so an ascending subscriber that wants a backlog needs a
 * budget that covers it.
 */
export type Order = "ascending" | "descending";

/** Constructors for {@link Subscription}. */
export const Subscription: {
	/**
	 * The default subscription, used when none is passed: priority `0`, descending order, and
	 * a `maxDelay` of zero that skips a superseded group at once. Mirrors the Rust
	 * `Subscription::default()`. Spread it to change one field and keep the rest:
	 * `{ ...Subscription.default(), priority: 2 }`.
	 */
	default(): Subscription;
} = {
	default: () => ({ priority: DEFAULT_PRIORITY, maxDelay: Milli.zero, order: "descending" }),
};

// Materialize the defaults at the model boundary so every layer observes a complete
// subscription rather than interpreting an omitted field differently.
function subscriptionDefaults(subscription: Subscription = Subscription.default()): Subscription {
	if ("maxAge" in subscription) throw new TypeError("Subscriber maxAge is unsupported; use maxDelay");
	const bounds = groupBounds(subscription.groups ?? {});
	return {
		priority: priorityValue(subscription.priority ?? DEFAULT_PRIORITY),
		maxDelay:
			subscription.maxDelay === null ? NO_MAX_DELAY : wireMillis("maxDelay", subscription.maxDelay ?? Milli.zero),
		order: orderOf(subscription.order),
		groups: {
			start: subscription.groups?.start === undefined ? undefined : { included: bounds.start },
			end: bounds.end === undefined ? undefined : { excluded: bounds.end },
		},
	};
}

// A plain JS caller can pass anything, and silently serving a typo as descending would hide it.
function orderOf(order: Order | undefined): Order {
	if (order === undefined) return "descending";
	if (order !== "ascending" && order !== "descending") throw new TypeError(`unknown order: ${String(order)}`);
	return order;
}

// Aggregate the preferences of every live subscriber, matching Rust's Subscription::poll_combined.
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
		combined.maxDelay = Milli(Math.max(combined.maxDelay ?? Milli.zero, subscription.maxDelay ?? Milli.zero));
		// One live subscriber held behind another's backlog is worse than a backlog sent late.
		combined.order =
			combined.order === "ascending" && subscription.order === "ascending" ? "ascending" : "descending";

		// A floor only restricts, so a subscriber without one clears the aggregate:
		// its budget may reach below any floor the others set.
		const a = groupBounds(combined.groups ?? {});
		const b = groupBounds(subscription.groups ?? {});
		combined.groups = {
			start:
				combined.groups?.start === undefined || subscription.groups?.start === undefined
					? undefined
					: { included: Math.min(a.start, b.start) },
			end: a.end === undefined || b.end === undefined ? undefined : { excluded: Math.max(a.end, b.end) },
		};
	}
	return combined;
}

/**
 * A request for a track the peer wants, delivered to the publishing wire layer.
 *
 * Created internally by the broadcast when a subscription (or info lookup) needs a track
 * served; answer it with {@link accept} or {@link reject}.
 *
 * @public
 */
export class Request {
	/** The requested track name. */
	readonly name: string;

	#producer: Producer;
	#pending: Set<Request>;

	private constructor(options: TrackRequestOptions) {
		this.name = options.name;
		this.#producer = options.producer;
		this.#pending = options.pending;
		this.#pending.add(this);
	}

	static {
		hooks.makeRequest = (options) => new Request(options);
		hooks.stageRequest = (request, info) => request.#stage(info);
	}

	/** Watch the subscribers waiting for this request. */
	demand(): Demand {
		return this.#producer.demand();
	}

	/** The aggregate subscription requested for this track. */
	get subscription(): Readonly<Subscription> {
		return this.#producer.subscription.peek() ?? subscriptionDefaults();
	}

	/** The subscriber's priority for this track. */
	get priority(): number {
		return this.subscription.priority ?? 0;
	}

	/**
	 * Accept the request, committing the track's immutable {@link Info}. Throws, rejecting the
	 * request, if `info` is invalid.
	 */
	accept(info: Partial<Info> = {}): Producer {
		return this.#stage(info).commit();
	}

	#stage(info: Partial<Info>): StagedTrack {
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

/** Options for {@link Consumer.fetchGroup}. */
export interface FetchGroupOptions {
	/** Delivery priority for the fetch stream. Defaults to `0`. */
	priority?: number;

	/** Maximum age behind the live edge this caller accepts. Omission uses publisher retention alone. */
	maxDelay?: Milli;

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
	 * The cursor starts at the group the subscription named (its floor), or 0.
	 * {@link Subscription.maxDelay} is what asks for data: delivery skips everything above
	 * the floor that the budget convicts. Without options this is {@link Subscription.default},
	 * whose budget of zero delivers only the latest group; a larger one reaches back over what
	 * it can still use, and an options object without `maxDelay` reaches back as far as the
	 * publisher retains.
	 */
	subscribe(options?: Subscription): Subscriber {
		return this.#broadcast.subscribe(this.name, options);
	}

	/** Fetch the track's immutable publisher properties without subscribing. */
	info(): Promise<Info> {
		return this.#broadcast.resolveTrackInfo(this.name);
	}

	/** Fetch a single group by sequence without holding a live subscription. */
	fetchGroup(sequence: number, options?: FetchGroupOptions): Promise<GroupConsumer> {
		if (options?.maxDelay !== undefined) {
			options = { ...options, maxDelay: wireMillis("maxDelay", options.maxDelay) };
		}
		if (!this.#broadcast.fetchGroup) {
			return Promise.reject(new Error("fetch group is not supported by this session's version"));
		}
		return this.#broadcast.fetchGroup(this.name, sequence, options);
	}
}

// The index of the first timeline group at or after `sequence`.
function timelineIndex(timeline: readonly { sequence: number }[], sequence: number): number {
	let lo = 0;
	let hi = timeline.length;
	while (lo < hi) {
		const mid = (lo + hi) >>> 1;
		if (timeline[mid].sequence < sequence) lo = mid + 1;
		else hi = mid;
	}
	return lo;
}

// Add a group to a sorted timeline, replacing any group with the same sequence. Groups
// usually arrive in order, so the append is checked first.
function timelineInsert(timeline: GroupConsumer[], group: GroupConsumer): void {
	const last = timeline.at(-1);
	if (!last || last.sequence < group.sequence) {
		timeline.push(group);
		return;
	}
	const index = timelineIndex(timeline, group.sequence);
	if (timeline[index]?.sequence === group.sequence) timeline[index] = group;
	else timeline.splice(index, 0, group);
}

// The shared state behind a Producer / Subscriber pair. Package-internal
// wiring, unexported so it never appears in the published type declarations.
class TrackState {
	/** The producer fanning into this sink, so a subscriber can mint a sibling of itself. */
	producer?: Producer;
	groups = new Signal<GroupConsumer[]>([]);
	// Sequences the upstream publisher gave up, with the error each was given up with: an
	// incoming SUBSCRIBE_DROP or a producer cut. Kept apart from the cache so reclaim cannot
	// erase a disposition a downstream publisher still has to forward, and as runs so a wide
	// DROP costs one entry. Replaced rather than mutated, so a cursor can tell it changed by
	// identity.
	drops = new Signal<readonly DropRun[]>([]);
	// Every group still in the producer's replay cache, including groups this
	// subscriber already consumed, sorted by sequence. Drift anchors have the same
	// lifetime as content. Sorted so the latency guard, evaluated per group and per
	// arrival, searches it instead of scanning the whole retained window.
	timeline: GroupConsumer[] = [];
	// First timestamps mutate group state rather than track state, so held groups
	// watch this revision as well as arrivals when enforcing latency after handoff.
	timelineChanged = new Signal(0);
	// Bumped when a wire subscription declares or resolves where its live feed starts, so a
	// publisher holding its first group for that answer wakes.
	startChanged = new Signal(0);
	/** Best-effort datagram channel, parallel to {@link groups}; a bounded send buffer per subscriber. */
	datagrams = new Signal<Datagram[]>([]);
	latest?: number;
	// One past the highest group or datagram received, including evicted content.
	received = new Signal(0);
	/**
	 * The exclusive end, declared by {@link Producer.finishAt} or stamped by a clean close
	 * at the head of the sequence after the highest produced. Groups and datagrams share
	 * the namespace, so it can sit past `latest + 1` (which only tracks groups). Mirrors
	 * the Rust `end`.
	 */
	end = new Signal<Position | undefined>(undefined);
	// The error the track ended with at `end`, if it did not end cleanly: a reader reaching the
	// end gets it instead of the end of stream. Set before `end`, so its wake sees it.
	endError?: Error;
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
	/** Newest group head, datagram, frame, or location reported by the publisher. */
	largest = new Signal<Position | undefined>(undefined);
	// Set once {@link largest} can no longer advance while the track stays open.
	largestEnded = new Signal(false);

	constructor(subscription?: Subscription) {
		this.update = new Signal(subscription === undefined ? undefined : subscriptionDefaults(subscription));
	}
}

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
// so eviction can drop them together.
// `arrived` is when the group joined this track, which its mirrors age a predecessor from.
// `bound` is its first frame's timestamp once checked to fit between its neighbors', which
// bounds theirs; `unverified` marks one accepted below the eviction watermark instead.
type CachedGroup = {
	group: GroupProducer;
	mirrors: Map<TrackState, GroupConsumer>;
	arrived: Instant;
	bound?: Timestamp;
	unverified?: boolean;
};

// A group's checked start, kept by sequence.
type Start = { sequence: number; start: Timestamp };

// A fresh read handle on a group the producer still caches; assigned in its static block.
let cachedMirror: (producer: Producer, sequence: number, from: number) => GroupConsumer | undefined;
// Whether the producer still caches a group that has not failed; assigned in its static block.
let cachedHolds: (producer: Producer, sequence: number) => boolean;
// Mirror the cached groups in `[start, end)` that `offer` accepts into a sink again, cut ones
// included as a fresh sink gets them, for a reader whose floor moved below them; assigned in
// the producer's static block.
let reofferCached: (
	producer: Producer,
	sink: TrackState,
	start: number,
	end: number,
	offer: (sequence: number) => boolean,
) => void;
// Mirror the cached groups at `sequences` into a sink again, for a reader whose budget widened
// past the one it skipped them under; assigned in the producer's static block.
let reofferSequences: (producer: Producer, sink: TrackState, sequences: readonly number[]) => void;
// Whether a group may still land on the producer's live feed; assigned in its static block.
let mayLand: (producer: Producer, land: Land) => boolean;
let sourceStart: (producer: Producer) => number | "pending" | undefined;
// Actual object metadata outlives cached payloads, unlike empty group heads in model largest.
let objectLargest: (producer: Producer) => Position | undefined;

// The sequence-order cursor lives inside `Subscriber` (it shares the group buffer and the
// drift anchor with the arrival cursor), so `Ordered` reaches it through this bridge,
// assigned in the class's static block.
let makeOrdered: (subscriber: Subscriber) => Ordered;
let ordered_: {
	nextGroup(subscriber: Subscriber): Promise<GroupConsumer | undefined>;
	tryNextGroup(subscriber: Subscriber): Recv;
	ended(subscriber: Subscriber): undefined;
	readFrame(subscriber: Subscriber): Promise<({ group: number; frame: number } & Frame) | undefined>;
	readString(subscriber: Subscriber): Promise<string | undefined>;
	readJson(subscriber: Subscriber): Promise<unknown | undefined>;
	readBool(subscriber: Subscriber): Promise<boolean | undefined>;
	recvDatagram(subscriber: Subscriber): Promise<Datagram | undefined>;
};

// Whether a subscription joins at the live edge rather than resuming from a fixed group.
function joinsLive(subscription: Subscription | undefined): boolean {
	return subscription?.groups?.start === undefined && subscription?.groups?.end === undefined;
}

// Constructs a Demand from within this module; assigned in the class's static block.
let makeDemand: (name: string, wanted: Getter<number | undefined>, state: TrackState) => Demand;

/**
 * A watch-only view of a track's subscriber demand, from {@link Producer.demand} or {@link Request.demand}. Mirrors the
 * Rust `Demand`.
 *
 * Lets a publisher gate work (on-demand capture or encoding) on whether anyone is subscribed,
 * without the ability to write groups or close the track. The consumer wire watches it to tear an
 * idle upstream subscription down instead of downloading to nobody.
 */
export class Demand {
	/** The track name. */
	readonly name: string;

	#wanted: Getter<number | undefined>;
	#state: TrackState;

	private constructor(name: string, wanted: Getter<number | undefined>, state: TrackState) {
		this.name = name;
		this.#wanted = wanted;
		this.#state = state;
	}

	static {
		makeDemand = (name, wanted, state) => new Demand(name, wanted, state);
		hooks.demandPriority = (demand) => demand.#state.info.peek()?.priority ?? DEFAULT_PRIORITY;
		hooks.demandReleased = (demand) => demand.#state.closed;
	}

	/**
	 * The group subscribers want: `undefined` while nobody subscribes, otherwise the sequence
	 * the next group should have at least. A publisher opens a group whenever it passes its
	 * largest. The first subscriber of a track with no group wants `0`, and a join judged by
	 * {@link Info.refresh} wants the next free sequence until a group lands there, so a
	 * datagram taking it moves the ask past. It never goes back. Watch it
	 * (`effect.get` / `.peek()`) to follow demand.
	 */
	get wanted(): Getter<number | undefined> {
		return this.#wanted;
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
 * Obtained from {@link Request.accept} (the wire asks the application for a track to
 * serve) or constructed directly for an in-process track.
 */
export class Producer {
	/** The track name. */
	readonly name: string;

	// The producer's own state is the source of truth (info/closed); subscribers
	// read mirrored sinks, never this state directly.
	#state = new TrackState();
	// One past the highest group or datagram this producer received, like the Rust
	// `max_sequence`, and where the next append lands.
	#received = 0;

	// Recently written source groups, retained for replay to late subscribers and
	// pruned once idle for longer than the cache window. Each entry tracks the mirror
	// it handed to every sink so eviction can drop them too: otherwise a slow consumer
	// that never reads would pin old groups (and their frame bytes) forever.
	#cache: CachedGroup[] = [];
	// The same entries by sequence, so a write finds a duplicate without a scan.
	#cached = new Map<number, CachedGroup>();
	// The cached groups' checked starts, sorted by sequence, so a first frame finds its
	// neighbors' with a search.
	#bounds: Start[] = [];
	// The highest-sequence checked start evicted. Eviction is not a sequence prefix, so a
	// group above it is checked against it as well as its cached neighbors; one at or below
	// it may have lost its lower bound. One entry, so nothing grows with the cache.
	#evicted?: Start;
	// When the cache was last scanned. See #prune.
	#pruned = Number.NEGATIVE_INFINITY;
	// When the track closed, which restarts the former newest group's idle window. See #prune.
	#closedAt = Number.NEGATIVE_INFINITY;

	// Wakeup for the next entry due to age out. Writes settle retention inline, but a
	// publisher that stalls stops writing, so without this an abandoned group (and any
	// reader parked in it) would wait for a write that never comes.
	#pruneTimer?: AbortController;
	#pruneTimerAt = 0;

	// One independent downstream state per live subscriber.
	#sinks = new Set<TrackState>();

	// The group subscribers want while any is attached, watched through {@link demand}.
	#wanted = new Signal<number | undefined>(undefined);
	// The lowest group they want while no refresh is pending, never lowered.
	#floor = 0;
	// A join over the refresh ratio asked for a group past every sequence written, and none
	// has landed yet. Read against `#received` rather than stored as a number, so a datagram
	// taking the next sequence can never leave the ask behind.
	#refresh = false;
	#demand: Demand;
	// The largest group's content may still grow: it has not closed cleanly, and it is not a
	// datagram. An aborted one stays open, since it can be recreated.
	#largestOpen = false;
	// Only real objects or upstream metadata advance this, never the head of an empty group.
	#objectLargest?: Position;

	// A wire subscription declared the end ahead of the group streams below it, which QUIC
	// may deliver in any order: readers wait for a hole below it to fill, or for the track
	// to close, rather than end at the boundary with groups still to arrive.
	#tailPending = false;
	// The first group the wire subscription's live feed serves (see `hooks.startTrack`), unset
	// until its START resolves it, null for a resolved-empty range, and the requested floor.
	#served?: { start?: number | null; requested?: number };
	// The sequences the live feed delivered, as a group or a datagram: what a pending tail is
	// judged by. Eviction never touches it. A gap folds away a subscription's grace after a
	// later arrival opened it, as the session's tail does, so this holds only the gaps still
	// worth waiting for.
	#arrivals = new Ledger();

	constructor(name: string) {
		this.name = name;
		this.#demand = makeDemand(name, this.#wanted, this.#state);
	}

	static {
		hooks.receiveGroup = (producer, { sequence, start }) => producer.#createGroup(sequence, start);
		hooks.writeDatagram = (producer, datagram, inflated) => producer.#insertDatagram(datagram, inflated);
		sourceStart = (producer) => producer.#served && producer.#feedStart();
		hooks.producerLargest = (producer) => producer.#state.largest;
		hooks.joinProducer = (producer) => producer.#join();
		hooks.endTrack = (producer, end, error) => producer.#endPending(end, error);
		hooks.releaseTrack = (producer) => producer.#close(undefined);
		hooks.trackReleased = (producer) => producer.#state.closed;
		hooks.trackFinished = (producer) => producer.#state.end.peek() !== undefined;
		hooks.dropTrackGroups = (producer, start, end, error) => producer.#drop(start, end, error, true);
		hooks.startTrack = (producer, served) => {
			producer.#settle();
			producer.#served = served;
			producer.#owe();
			for (const state of [producer.#state, ...producer.#sinks]) state.startChanged.update((rev) => rev + 1);
		};
		hooks.cachedLargest = (producer) => producer.#cachedLargest();
		hooks.reportedLargest = (producer, largest) => producer.#advanceObject(largest);
		hooks.reportedGroup = (producer, group) => producer.#advanceLargest({ group, frame: 0 });
		objectLargest = (producer) => producer.#objectLargest;
		cachedHolds = (producer, sequence) => {
			const entry = producer.#cached.get(sequence);
			return entry !== undefined && !(entry.group.closed.peek() instanceof Error);
		};
		cachedMirror = (producer, sequence, from) => {
			const entry = producer.#cached.get(sequence);
			if (!entry || entry.group.closed.peek() instanceof Error) return undefined;
			const mirror = entry.group.mirror();
			if (hooks.groupIndex(mirror) > from) {
				mirror.close();
				return undefined;
			}
			hooks.arriveGroup(mirror, entry.arrived);
			return mirror;
		};
		reofferCached = (producer, sink, start, end, offer) => {
			const held = new Set(sink.groups.peek().map((group) => group.sequence));
			for (const entry of producer.#cache) {
				const sequence = entry.group.sequence;
				if (sequence < start || sequence >= end || held.has(sequence) || !offer(sequence)) continue;
				producer.#mirror(entry, sink);
			}
		};
		reofferSequences = (producer, sink, sequences) => {
			const held = new Set(sink.groups.peek().map((group) => group.sequence));
			for (const sequence of sequences) {
				const entry = producer.#cached.get(sequence);
				if (entry && !held.has(sequence)) producer.#mirror(entry, sink);
			}
		};
		mayLand = (producer, land) => producer.#mayLand(land);
	}

	// Whether a group may still land from the live feed, by the rule a pending tail is judged
	// by; see {@link Land}. Mirrors the Rust `TrackState::may_land`.
	#mayLand(land: Land): boolean {
		if (this.#state.closed.peek() !== undefined) return false;
		let start = land.start;
		if (!land.floor) {
			const first = this.#arrivals.first();
			if (first === undefined) return false;
			start = Math.max(start, first);
		}
		start = Math.max(start, this.#served?.start ?? 0);
		const declared = this.#state.end.peek();
		const end = declared === undefined ? land.end : Math.min(land.end, finalSequence(declared));
		const at = now();
		const limit = Nano.fromMilli(land.grace);
		const drops = this.#state.drops.peek();
		return this.#arrivals
			.gaps(start, end)
			.some(
				(gap) =>
					(gap.since === undefined || Instant.durationSince(at, gap.since) < limit) &&
					runGaps(drops, gap.start, gap.end).some((open) => !land.delivered?.covers(open.start, open.end)),
			);
	}

	// Record `[start, end)` given up with `error`. An upstream drop (`open`) also ends the
	// groups still arriving in it with that error, so their readers settle now rather than
	// once the stream's reset lands; one that finished here stays whole.
	#drop(start: number, end: number, error: Error, open = false): void {
		if (start >= end) return;
		for (const state of [this.#state, ...this.#sinks]) {
			state.drops.update((drops) => insertDrop(drops, start, end, error));
		}
		if (open) {
			for (const [sequence, entry] of this.#cached) {
				if (sequence >= start && sequence < end && entry.group.closed.peek() === undefined) {
					entry.group.abort(error);
				}
			}
		}
		// A pending tail owes nothing the publisher gave up.
		this.#arrive(start, end);
	}

	// A publisher may re-create a group it cut; the sequence is no longer given up.
	#undrop(sequence: number): void {
		for (const state of [this.#state, ...this.#sinks]) {
			const drops = state.drops.peek();
			if (runGaps(drops, sequence, sequence + 1).length === 0) {
				state.drops.set(removeRun(drops, sequence, sequence + 1));
			}
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

	/**
	 * Settles once the track's outcome is final: `null` once its declared end is reached and
	 * settled (every group below it finished; its error, if declared with one) or on a clean
	 * release, else the abort {@link Error}. The producer stays open after a settled end, serving its cache.
	 * Peek it synchronously (`undefined` while open), observe it reactively, or `await` it.
	 */
	get closed(): GetPromise<Error | null> {
		return this.#state.outcome;
	}

	/**
	 * The aggregate subscription across live subscribers, or `undefined` when there are none.
	 * The wire layer watches this to emit SUBSCRIBE_UPDATE.
	 */
	get subscription(): Getter<Subscription | undefined> {
		return this.#state.update;
	}

	/** Commit the immutable publisher properties, resolving {@link info}. Returns `this`. */
	accept(info: Partial<Info> = {}): this {
		if (this.#state.closed.peek() !== undefined) return this;
		const resolved = infoDefaults(info);
		const timed = resolved.timescale != null;
		// Anything written before accept() must match too. A mismatch closes the track, so
		// nothing waiting on info() hangs on a half-bound cache.
		try {
			for (const { group } of this.#cache) {
				hooks.bindGroupTimed(group, timed);
				hooks.bindGroupCompression(group, resolved.compress ?? false);
			}
			for (const sink of this.#sinks) {
				for (const datagram of sink.datagrams.peek()) {
					if ((datagram.timestamp !== undefined) !== timed) throw new TimestampMismatch();
				}
			}
		} catch (err) {
			this.abort(err instanceof Error ? err : new Error(String(err)));
			throw err;
		}
		this.#state.info.set(resolved);
		// Propagate to any sink handed out before accept (the on-demand path).
		const compressed = new Map<Datagram, Stored>();
		for (const sink of this.#sinks) {
			if (resolved.compress) {
				sink.datagrams.mutate((datagrams) => {
					for (let index = 0; index < datagrams.length; index++) {
						const { sequence, timestamp, payload } = datagrams[index];
						let slice = compressed.get(datagrams[index]);
						if (!slice) {
							slice = datagramSlice(payload);
							compressed.set(datagrams[index], slice);
						}
						datagrams[index] = Object.assign(sliceView(timestamp, slice), { sequence });
					}
				});
			}
			sink.info.set(resolved);
		}
		this.#updateSubscription();
		return this;
	}

	/**
	 * An independent {@link Subscriber} reading this track's groups.
	 *
	 * Its cursor starts at the group the subscription named (its floor), or 0.
	 * {@link Subscription.maxDelay} is what asks for data: delivery skips everything above
	 * the floor that the budget convicts. Without options this is {@link Subscription.default},
	 * whose budget of zero delivers only the latest group; a larger one reaches back over what
	 * it can still use, and an options object without `maxDelay` reaches back as far as the
	 * publisher retains.
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

	// A subscriber joined at the live edge: ask for a group past every sequence written when
	// the newest group's bytes after its first frame exceed the refresh ratio times the first
	// frame.
	#join(): void {
		const ratio = this.#state.info.peek()?.refresh;
		const latest = this.#state.latest;
		if (ratio === undefined || latest === undefined) return;
		const group = this.#cached.get(latest)?.group;
		if (!group || !hooks.overRefresh(group, ratio)) return;
		this.#refresh = true;
		this.#reportWanted();
	}

	// Report the wanted group while anyone subscribes: the next free sequence while a refresh
	// is pending.
	#reportWanted(): void {
		const wanted = this.#refresh ? Math.max(this.#received, this.#floor) : this.#floor;
		this.#wanted.set(this.#sinks.size > 0 ? wanted : undefined);
	}

	// Register a downstream sink: seed its info, replay the retained window, and (while
	// the track is open) mirror future groups into it. A late subscriber to a closed
	// track still drains the buffered groups before seeing the end.
	#addSink(sink: TrackState): void {
		sink.producer = this;
		const info = this.#state.info.peek();
		if (info) sink.info.set(info);
		sink.largest.set(this.#state.largest.peek());
		sink.largestEnded.set(this.#state.largestEnded.peek());

		const closed = this.#state.closed.peek();
		if (closed === undefined) {
			if (joinsLive(sink.update.peek())) this.#join();
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

		this.#prune();
		for (const entry of this.#cache) this.#mirror(entry, sink);
		sink.drops.set(this.#state.drops.peek());

		sink.received.set(this.#received);
		sink.endError = this.#state.endError;
		sink.end.set(this.#state.end.peek());
		sink.owed.set(this.#state.owed.peek());
		if (closed !== undefined) {
			closeTrackState(sink, closed instanceof Error ? closed : undefined);
		}
	}

	// Recompute from every live sink because an update or close can narrow as well as widen
	// the aggregate. The wire layer observes this signal and emits SUBSCRIBE_UPDATE.
	#updateSubscription(): void {
		const combined = combineSubscriptions(this.#sinks);
		this.#settle();
		const retained = this.#state.info.peek()?.maxAge;
		if (combined && retained !== undefined)
			combined.maxDelay = Milli(Math.min(combined.maxDelay ?? Milli.zero, retained));
		this.#state.update.set(combined);
		// The requested floor bounds what a pending tail owes.
		this.#owe();
	}

	// Mirror a cached source group into a sink. The mirror fills synchronously as the
	// source is written and keeps its own read cursor; frame bytes are shared by
	// reference. Tracked on the entry so eviction can drop it from the sink.
	#mirror(entry: CachedGroup, sink: TrackState): void {
		const dst = entry.group.mirror();
		hooks.arriveGroup(dst, entry.arrived);
		if (entry.unverified) hooks.unverifyGroup(dst);
		entry.mirrors.set(sink, dst);
		timelineInsert(sink.timeline, dst);
		void dst.readable().then(() => sink.timelineChanged.update((revision) => revision + 1));
		sink.latest = Math.max(sink.latest ?? 0, dst.sequence);
		sink.received.set(Math.max(sink.received.peek(), dst.sequence + 1));
		sink.groups.mutate((groups) => {
			groups.push(dst);
			groups.sort((a, b) => a.sequence - b.sequence);
		});
	}

	// Drop a cached group's mirror from every sink so no consumer can pin it.
	#evict(entry: CachedGroup): void {
		// Reclaiming a group the publisher never closed ends it as the gap it is, before
		// the mirrors below would otherwise report a clean finish to a reader that had
		// drained it. The usual case, an already-closed group aging out, keeps its own
		// terminal state.
		if (!entry.group.isClosed) {
			const evicted = new TooFarBehind();
			EVICTED.add(evicted);
			entry.group.abort(evicted);
		}
		const mirrors = [...entry.mirrors.values()].filter((mirror) => hooks.evictGroup(mirror));
		this.#unlink(entry);
		for (const mirror of mirrors) mirror.close();
	}

	// Take a cached group's mirrors out of every sink, so a subscriber can no longer
	// receive them. A reader already holding one keeps it as is.
	#unlink(entry: CachedGroup): void {
		for (const [sink, mirror] of entry.mirrors) {
			sink.groups.mutate((groups) => {
				const i = groups.indexOf(mirror);
				if (i >= 0) groups.splice(i, 1);
			});
			const index = timelineIndex(sink.timeline, mirror.sequence);
			if (sink.timeline[index] === mirror) sink.timeline.splice(index, 1);
		}
		entry.mirrors.clear();
	}

	// Take a cached group out of the cache lookups.
	#uncache(entry: CachedGroup, evicted = false): void {
		this.#cache.splice(this.#cache.indexOf(entry), 1);
		this.#cached.delete(entry.group.sequence);
		this.#unbound(entry, evicted);
	}

	// The starts a first frame of group `sequence` must fall between: the nearest checked
	// starts cached on either side, since groups land out of sequence order, and the eviction
	// watermark. Throws {@link TimestampMismatch} on a rewind; an equal start is allowed. Returns
	// whether the group is late, at or below the watermark, where its floor may be gone, so
	// the watermark caps it instead and it is accepted unverified.
	#admit(sequence: number, start: Timestamp): boolean {
		const bounds = this.#bounds;
		const index = timelineIndex(bounds, sequence);
		let floor = bounds[index - 1]?.start;
		let ceiling = bounds[bounds[index]?.sequence === sequence ? index + 1 : index]?.start;
		const evicted = this.#evicted;
		const late = evicted !== undefined && sequence <= evicted.sequence;
		if (evicted && late) {
			if (ceiling === undefined || evicted.start.isBefore(ceiling)) ceiling = evicted.start;
		} else if (evicted) {
			if (floor === undefined || floor.isBefore(evicted.start)) floor = evicted.start;
		}
		if ((floor && start.isBefore(floor)) || ceiling?.isBefore(start)) throw new TimestampMismatch();
		return late;
	}

	// Record a cached group's checked start: a bound for its neighbors, or unverified.
	#record(entry: CachedGroup, start: Timestamp, late: boolean): void {
		if (late) {
			entry.unverified = true;
			for (const mirror of entry.mirrors.values()) hooks.unverifyGroup(mirror);
			return;
		}
		entry.bound = start;
		const sequence = entry.group.sequence;
		const last = this.#bounds.at(-1);
		if (!last || last.sequence < sequence) this.#bounds.push({ sequence, start });
		else this.#bounds.splice(timelineIndex(this.#bounds, sequence), 0, { sequence, start });
	}

	// Drop a group's checked start as it leaves the cache, raising the watermark to it when
	// it was evicted.
	#unbound(entry: CachedGroup, evicted = false): void {
		const start = entry.bound;
		if (start === undefined) return;
		const sequence = entry.group.sequence;
		const index = timelineIndex(this.#bounds, sequence);
		if (this.#bounds[index]?.sequence === sequence) this.#bounds.splice(index, 1);
		if (evicted && (this.#evicted === undefined || sequence > this.#evicted.sequence)) {
			this.#evicted = { sequence, start };
		}
	}

	// The newest group remains replayable while the track is live, even after the group
	// closes: a catalog can publish one snapshot for the broadcast's whole lifetime. An
	// aborted one holds nothing worth replaying, so it ages out like any other.
	#edge(): GroupProducer | undefined {
		const latest = this.#state.latest;
		if (latest === undefined) return undefined;
		const group = this.#cached.get(latest)?.group;
		return group?.closed.peek() instanceof Error ? undefined : group;
	}

	// The newest frame in the cache, as {@link Subscriber.largest} reads it from a sink:
	// unknown once the newest group is evicted, since what is left cannot say where the edge is.
	#cachedLargest(): Position | undefined {
		const latest = this.#state.latest;
		if (latest === undefined || !this.#cached.has(latest)) return undefined;

		let largest: Position | undefined;
		for (const { group } of this.#cache) {
			const count = hooks.groupFrameCount(group);
			if (count > hooks.groupOffset(group) && (largest === undefined || group.sequence > largest.group)) {
				largest = { group: group.sequence, frame: count - 1 };
			}
		}
		return largest;
	}

	#advanceLargest(largest: Position | undefined): void {
		if (!largest) return;
		const current = this.#state.largest.peek();
		if (current && !after(largest, current)) return;
		this.#largestOpen = true;
		this.#state.largest.set(largest);
		for (const sink of this.#sinks) sink.largest.set(largest);
	}

	#advanceObject(largest: Position | undefined): void {
		if (largest && (!this.#objectLargest || after(largest, this.#objectLargest))) this.#objectLargest = largest;
		this.#advanceLargest(largest);
	}

	/** The track's largest location: its newest group or datagram at the newest frame, or undefined before any. */
	largest(): Position | undefined {
		return this.#state.largest.peek();
	}

	// Evict cached groups idle for longer than the cache window. Idle means nothing
	// written, so an abandoned open group ages out instead of pinning its buffer (and
	// any reader parked in it) forever.
	//
	// Scans at most once per slice of the window, so a track publishing faster than that
	// evicts a run of groups per scan instead of scanning everything to evict one per
	// write. A group can outlive the window by up to one slice.
	#prune(): void {
		const at = now();
		const slice = CACHE_WINDOW / PRUNE_SLICES;
		if (at < this.#pruned + slice) {
			// Something may have come due since the last scan, so make sure another follows.
			this.#wake(this.#pruned + slice);
			return;
		}
		this.#pruned = at;

		const cutoff = at - CACHE_WINDOW;
		const edge = this.#edge();
		const live = this.#state.closed.peek() === undefined;

		let oldest: number | undefined;
		const retained: CachedGroup[] = [];
		for (const entry of this.#cache) {
			// Once the track closes, the former edge ages from the close instead, so close()
			// never reclaims a group its subscribers have yet to drain.
			const activity =
				entry.group === edge ? Math.max(entry.group.activity, this.#closedAt) : entry.group.activity;
			if (entry.group === edge && live) {
				retained.push(entry);
			} else if (activity >= cutoff) {
				retained.push(entry);
				if (oldest === undefined || activity < oldest) oldest = activity;
			} else {
				this.#cached.delete(entry.group.sequence);
				this.#unbound(entry, true);
				this.#evict(entry);
			}
		}
		const evicted = retained.length < this.#cache.length;
		this.#cache = retained;
		// An evicted group below the end no longer holds it open, as in Rust.
		if (evicted) this.#settle();

		// Replace the wakeup with one for the next entry due to age out. Writes settle
		// retention inline, so this only has to cover the case no write follows. Cheap to
		// over-arm: an entry written since is retained and re-armed.
		this.#pruneTimer?.abort();
		this.#pruneTimer = undefined;
		if (oldest !== undefined) this.#wake(Math.max(oldest + CACHE_WINDOW, at + slice));
	}

	// Arm the prune wakeup for `at`, unless one is already armed sooner. Kept after a close,
	// until the cache empties, so what the track left behind still ages out.
	#wake(at: number): void {
		if (this.#pruneTimer !== undefined && this.#pruneTimerAt <= at) return;
		this.#pruneTimer?.abort();

		const timer = new AbortController();
		// A cache prune is never a reason to hold a Node/Bun process open.
		void sleepUntil(Instant(at), { signal: timer.signal, ref: false }).then(
			() => {
				// The wakeup can already be queued when a sooner one replaces it.
				if (timer.signal.aborted) return;
				this.#pruneTimer = undefined;
				this.#prune();
			},
			() => {},
		);
		this.#pruneTimer = timer;
		this.#pruneTimerAt = at;
	}

	// Retain a source group and fan it out to every live sink; its frame 0 is checked
	// against this track when it lands.
	#publish(group: GroupProducer): void {
		this.#settle();
		const entry: CachedGroup = { group, mirrors: new Map<TrackState, GroupConsumer>(), arrived: now() };
		this.#undrop(group.sequence);
		group.closed.then((closed) => {
			if (!(closed instanceof Error) || HOLES.has(toStreamCode(closed))) return;
			// A re-created group already replaced this one.
			const current = this.#cached.get(group.sequence);
			if (current && current.group !== group) return;
			this.#drop(group.sequence, group.sequence + 1, closed);
		});
		const sequence = group.sequence;
		if (this.#unchecked()) {
			// Its timestamps may rewind, so they are not checked.
		} else {
			hooks.admitGroupStart(group, (start) => {
				const late = this.#admit(sequence, start);
				return () => this.#record(entry, start, late);
			});
		}
		this.#cache.push(entry);
		this.#cached.set(group.sequence, entry);
		this.#advance(group.sequence, true);
		this.#arrive(group.sequence);
		this.#state.latest = Math.max(this.#state.latest ?? 0, group.sequence);
		for (const sink of this.#sinks) this.#mirror(entry, sink);
		// Follow the group's frames as they land, past the head `#advance` already named.
		const stop = hooks.groupProgress(group, (total) => {
			if (total > hooks.groupOffset(group)) this.#advanceObject({ group: group.sequence, frame: total - 1 });
		});
		void group.closed.then((closed) => {
			stop();
			if (closed === null) this.#finishLargest(group.sequence);
		});
		// Give held mirrors the new live edge before pruning their timeline entry,
		// so their latency guard can preserve a terminal expiry verdict.
		this.#prune();
	}

	// Record a written sequence. It raises the largest too, at the head of an empty group, so
	// a later group below it never changes it. A group (`answers`) at or past the next free
	// sequence answers a pending refresh; anything else written there moves the sequence it
	// asks for.
	#advance(sequence: number, answers: boolean): void {
		const next = this.#received;
		this.#received = Math.max(next, sequence + 1);
		this.#advanceLargest({ group: sequence, frame: 0 });
		this.#endLargest();
		if (!this.#refresh || sequence < next) return;
		if (answers) {
			this.#refresh = false;
			this.#floor = Math.max(this.#floor, sequence);
		}
		this.#reportWanted();
	}

	// The sequence the next append takes: one past this producer's own writes.
	#nextSequence(): number {
		const next = this.#received;
		this.#writable(next);
		return next;
	}

	// Hold a group to this track's timedness, once the track has committed it.
	#bind(group: GroupProducer): void {
		const info = this.#state.info.peek();
		if (info) {
			hooks.bindGroupTimed(group, info.timescale != null);
			hooks.bindGroupCompression(group, info.compress ?? false);
		}
	}

	// Refuse a datagram whose timedness disagrees with this track's.
	#checkDatagram(timestamp: Timestamp | undefined): void {
		const info = this.#state.info.peek();
		if (info && (info.timescale != null) !== (timestamp !== undefined)) throw new TimestampMismatch();
	}

	// Refuse a write once the track is closed, or at or past its declared end.
	#writable(sequence: number): void {
		if (this.#state.closed.peek() !== undefined) throw new Error("track is closed");
		const end = this.#state.end.peek();
		if (end !== undefined && sequence >= finalSequence(end)) {
			throw new Error(`sequence ${sequence} is at or past the track's end ${end.group}/${end.frame}`);
		}
	}

	// Cap a group the end cuts, so it never holds a frame past the end.
	#cap(group: GroupProducer): void {
		const end = this.#state.end.peek();
		if (end !== undefined && end.frame !== 0 && end.group === group.sequence) hooks.limitGroup(group, end.frame);
	}

	/**
	 * Append a new group with the next sequence number: one past this producer's own writes.
	 *
	 * The broadcast remembers nothing of an earlier producer of the name, so a replacement
	 * starts at 0. Since a name always means the same content, a replacement continues past
	 * its predecessor's groups with {@link createGroup}.
	 *
	 * Its frames must match the track's {@link Info.timescale}: a timestamp on every frame of
	 * a timed track and none on an untimed one, or the write throws {@link TimestampMismatch}.
	 * So does a first frame that starts before the group below it or after the one above it.
	 */
	appendGroup(): GroupProducer {
		const group = new GroupProducer(this.#nextSequence());
		this.#bind(group);
		this.#cap(group);
		this.#publish(group);

		return group;
	}

	/**
	 * Create a new group with the given sequence number.
	 *
	 * Throws on a sequence that is still cached: a live duplicate would fan out to every
	 * subscriber twice. An aborted incarnation is evicted so a fresh group can serve the
	 * sequence again. Best effort (mirrors Rust): nothing remembers a sequence whose cache
	 * entry is already gone, so a long-evicted sequence is accepted as new. Its frames are
	 * checked like {@link appendGroup}'s.
	 */
	createGroup(sequence: number): GroupProducer {
		return this.#createGroup(sequence, 0);
	}

	#createGroup(sequence: number, start: number): GroupProducer {
		this.#writable(sequence);
		const existing = this.#cached.get(sequence);
		if (existing) {
			if (!(existing.group.closed.peek() instanceof Error)) {
				throw new Error(`duplicate group: sequence=${sequence}`);
			}
			// Subscribers may have seen the aborted incarnation's start, so it stays a watermark.
			this.#evict(existing);
			this.#uncache(existing, true);
		}

		const group = new GroupProducer(sequence);
		// Mirrors copy the absolute indices, so set them before publishing to any sink.
		hooks.startGroup(group, start);
		this.#bind(group);
		this.#cap(group);
		this.#publish(group);
		return group;
	}

	// Whether this track's timestamps may rewind (see {@link Info.unchecked} and {@link Info.ietf}).
	#unchecked(): boolean {
		const info = this.#state.info.peek();
		return info?.unchecked === true || info?.ietf !== undefined;
	}

	// Fan a datagram out to every live subscriber, dropping the oldest once the ring is full.
	// Late subscribers do NOT replay old datagrams (best-effort, unlike the group cache). A
	// compressed track's rings share only `slice`: each holds its own lazy view of it.
	#publishDatagram(datagram: Datagram, slice?: Stored): void {
		this.#settle();
		const { sequence, timestamp } = datagram;
		this.#advanceObject({ group: sequence, frame: 0 });
		this.#advance(sequence, false);
		this.#arrive(sequence);
		this.#finishLargest(sequence);
		for (const sink of this.#sinks) {
			sink.received.set(this.#received);
			const queued = slice ? Object.assign(sliceView(timestamp, slice), { sequence }) : datagram;
			sink.datagrams.mutate((list) => {
				if (list.length === MAX_DATAGRAMS) list.shift();
				list.push(queued);
			});
		}
	}

	/**
	 * Append a datagram with the next sequence number, returning the assigned sequence.
	 *
	 * A datagram is delivered best-effort over a single QUIC datagram, parallel to the track's
	 * groups but drawing from the same sequence namespace (interleaving with {@link appendGroup}
	 * never reuses a number). The payload must fit the negotiated transport datagram size minus
	 * a small header; an oversize payload is dropped at each hop (there is no group fallback), so
	 * keep datagram payloads small (e.g. a single audio frame). Datagrams are never delivered
	 * over stream-only transports (the WebSocket fallback). A payload over
	 * 65535 bytes (the QUIC datagram frame ceiling) throws. An origin publisher uses this; a
	 * relay preserving upstream numbering uses {@link insertDatagram}.
	 *
	 * Pass `undefined` for the timestamp on an untimed track; a timestamp whose presence
	 * disagrees with the track's {@link Info.timescale} throws {@link TimestampMismatch}.
	 */
	appendDatagram(timestamp: Timestamp | undefined, payload: Uint8Array): number {
		const sequence = this.#nextSequence();
		if (payload.byteLength > MAX_DATAGRAM_BYTES) throw new Error("datagram payload too large");
		this.#checkDatagram(timestamp);

		this.#insertDatagram({ sequence, timestamp, payload });
		return sequence;
	}

	/**
	 * Insert a datagram with an explicit sequence number.
	 *
	 * Preserves the supplied sequence (later appends continue past it) so a relay can
	 * forward a datagram without renumbering it. The size and timestamp rules of
	 * {@link appendDatagram} apply. Most origin publishers want {@link appendDatagram} instead.
	 */
	insertDatagram(sequence: number, timestamp: Timestamp | undefined, payload: Uint8Array) {
		this.#insertDatagram({ sequence, timestamp, payload });
	}

	#insertDatagram(datagram: Datagram, inflated?: number): void {
		const { sequence, timestamp, payload } = datagram;
		this.#writable(sequence);
		this.#checkDatagram(timestamp);
		if (payload.length > MAX_DATAGRAM_BYTES || (inflated ?? 0) > MAX_DATAGRAM_BYTES)
			throw new Error("datagram payload too large");
		if (
			inflated !== undefined &&
			(!Number.isSafeInteger(inflated) || inflated < 0 || !this.#state.info.peek()?.compress)
		)
			throw new ProtocolViolation("unexpected inflated datagram size");
		// A bad best-effort datagram is dropped on arrival, before any reader receives it.
		const slice = this.#state.info.peek()?.compress ? datagramSlice(payload, inflated) : undefined;
		this.#publishDatagram(datagram, slice);
	}

	/**
	 * Declare the track's exclusive end, possibly ahead of the live edge, mirroring the Rust
	 * `finish_at`.
	 *
	 * `end` is the first position that will never be produced: a track whose last group is
	 * 89 finishes at `{ group: 90, frame: 0 }`, and `{ group: 89, frame: 5 }` also caps group
	 * 89 at frames 0 to 4. Groups, frames, and datagrams below it are still accepted; anything
	 * at or past it is refused. Unlike {@link close} it is not terminal: call `close()` once
	 * the remaining groups are written. Readers end once the live edge reaches the end's
	 * group, independently of closure. Throws if the track is closed, already has an end, or
	 * `end` would orphan a group or frame already produced.
	 *
	 * With an `error`, the track ended that way rather than cleanly, yet keeps its history, unlike
	 * {@link abort}: readers still get every group below the end, and a reader reaching the end
	 * gets the error instead of the end of stream.
	 */
	finishAt(end: Position, error?: Error): void {
		if (this.#state.closed.peek() !== undefined) throw new Error("track is closed");
		const declared = this.#state.end.peek();
		if (declared !== undefined) throw new Error(`track already ends at ${declared.group}/${declared.frame}`);
		this.#declareEnd(end, error);
	}

	// The wire's end: repeating it is a no-op, and a mid-group end may refine the group
	// boundary it falls just before. Either must carry the same error code. Mirrors the Rust
	// `finish_at` on a `received()` copy.
	#endPending(end: Position, error?: Error): void {
		// Before the end lands, so settling it already counts what the tail still owes.
		this.#tailPending = true;
		const declared = this.#state.end.peek();
		const code = (err?: Error) => (err === undefined ? undefined : toStreamCode(err));
		if (declared === undefined) {
			this.finishAt(end, error);
		} else if (code(this.#state.endError) !== code(error)) {
			throw new ProtocolViolation(`conflicting track end error: ${error}`);
		} else if (declared.group !== end.group || declared.frame !== end.frame) {
			if (declared.frame !== 0 || end.frame === 0 || finalSequence(end) !== declared.group) {
				throw new ProtocolViolation(`conflicting track end: ${end.group}/${end.frame}`);
			}
			this.#declareEnd(end, error);
		}
		this.#owe();
	}

	// Record that the live feed accounted for `[start, end)`, then fold the gaps that aged out.
	#arrive(start: number, end = start + 1): void {
		const at = now();
		this.#arrivals.account(start, end, at);
		// Only a gap folds, so an in-order feed never reads the subscription.
		if (this.#arrivals.hasGaps()) this.#arrivals.expire(at, this.#grace());
		this.#owe();
	}

	// The grace the session's tail uses, from the max delay the track's subscribers ask for.
	#grace(): Milli {
		return grace(this.#state.update.peek()?.maxDelay ?? Milli.zero);
	}

	// Tell every reader whether the pending tail still owes a group below the end.
	#owe(): void {
		if (!this.#tailPending) return;
		const end = this.#state.end.peek();
		const owed = end !== undefined && this.#owes(finalSequence(end));
		for (const state of [this.#state, ...this.#sinks]) state.owed.set(owed);
		this.#settle();
	}

	// Settle the outcome cleanly once the declared end is reached and settled. Also run before a
	// write that could reopen it (a late group below the end, a fresh grace for the tail): that
	// write is still served, but the clean outcome stays final. Mirrors the Rust `latch_settled`.
	#settle(): void {
		if (this.#state.outcome.peek() === undefined && this.#settled())
			this.#state.outcome.set(this.#state.endError ?? null);
	}

	// Whether the pending tail misses a sequence from where the feed starts up to `final`: one
	// no live group or datagram delivered, in a gap younger than the grace. Judged by the
	// arrivals, not the cache, so an evicted group or a datagram counts as delivered.
	//
	// The feed starts at the declared start while the demand keeps the floor its SUBSCRIBE asked
	// for, since an update gets no fresh START: a moved floor stands on its own, and moving
	// back restores the START. Without either, the first arrival stands in for it.
	// Until the START arrives, the feed start is pending; the requested floor still bounds it.
	#feedStart(): number | "pending" | undefined {
		const groups = this.#state.update.peek()?.groups;
		const requested = groups?.start === undefined ? undefined : groupBounds(groups).start;
		const served = this.#served;
		if (!served || served.requested !== requested) return requested;
		if (served.start === null) return requested;
		if (served.start !== undefined) return served.start;
		// A closed track never gets its START.
		return this.#state.closed.peek() === undefined ? "pending" : requested;
	}

	#owes(final: number): boolean {
		if (!this.#tailPending) return false;
		const feed = this.#feedStart();
		const floor = (feed === "pending" ? this.#served?.requested : feed) ?? this.#arrivals.first();
		if (floor === undefined) return final > 0;
		return !this.#arrivals.covers(Math.min(floor, final), final);
	}

	#declareEnd(end: Position, error: Error | undefined): void {
		const { group, frame } = end;
		if (!Number.isSafeInteger(group) || group < 0 || !Number.isSafeInteger(frame) || frame < 0) {
			throw new RangeError(`invalid track end: ${group}/${frame}`);
		}
		const final = finalSequence(end);
		if (final < this.#received) {
			throw new ProtocolViolation(`track end ${group}/${frame} is below the next sequence ${this.#received}`);
		}
		const cut = frame === 0 ? undefined : this.#cached.get(group)?.group;
		if (cut) hooks.limitGroup(cut, frame);
		end = { group, frame };
		for (const state of [this.#state, ...this.#sinks]) {
			state.endError = error;
			state.end.set(end);
		}
		this.#endLargest();
	}

	// Group `sequence` holds every frame it will have: it closed cleanly, or it is a datagram.
	// A group name fixes its content, so no legal write raises the largest past it.
	#finishLargest(sequence: number): void {
		if (this.#state.largest.peek()?.group === sequence) this.#largestOpen = false;
		this.#endLargest();
	}

	// End the largest stream while the track stays open, once it can no longer advance: the
	// declared end is reached, and the largest group's content is final. Every sequence below
	// the end raised it already, so neither a late group below it nor a recreated copy of it
	// moves it after this.
	#endLargest(): void {
		this.#settle();
		if (this.#state.largestEnded.peek() || this.#largestOpen) return;
		const end = this.#state.end.peek();
		if (end === undefined || this.#received < finalSequence(end)) return;
		for (const state of [this.#state, ...this.#sinks]) state.largestEnded.set(true);
	}

	/**
	 * Declare the track's end after the highest sequence written. A no-op once an end is declared or the track closed,
	 * so a cleanup path can finish whatever the track's state.
	 *
	 * Not terminal: groups below the end may still be written. Readers end once the live edge
	 * reaches it.
	 */
	finish(): void {
		if (this.#state.end.peek() !== undefined || this.#state.closed.peek() !== undefined) return;
		this.finishAt({ group: this.#received, frame: 0 });
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
	 * With an end declared ({@link finish} or {@link finishAt}), the track ends there and its
	 * subscribers drain to it. Without one, the track aborts: nothing finished it.
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
		if (abort && this.#tailPending) this.#arrivals.expire(now(), this.#grace());
		if (abort && (this.#state.outcome.peek() === null || this.#settled())) abort = undefined;
		// Nobody will finish these, so a subscriber that has not taken one yet never sees it.
		// Not evicted: a reader already holding one keeps its frames and sees the abort.
		const open = this.#cache.filter((entry) => entry.group.closed.peek() === undefined);
		closeTrackState(this.#state, abort);
		if (this.#state.outcome.peek() === undefined) this.#state.outcome.set(abort ?? this.#state.endError ?? null);
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
		this.#closedAt = now();
		this.#prune();
	}

	// Whether the declared end was reached, the pending tail owes nothing below it, and every
	// cached group below it finished, so the track already holds everything it promised. An
	// abort over a group the tail still owed cut it off, so readers get the error rather than
	// a clean end without it. Mirrors the Rust `is_settled`.
	#settled(): boolean {
		const end = this.#state.end.peek();
		if (end === undefined) return false;
		const final = finalSequence(end);
		if (this.#received < final || this.#owes(final)) return false;
		return this.#cache.every(({ group }) => group.sequence >= final || group.closed.peek() === null);
	}

	/** Append a frame as its own single-frame group. */
	writeFrame(frame: Frame) {
		const group = this.appendGroup();
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
	presentation?: { sequence: number; timestamp: Timestamp };
	end?: number;
};

// The successor a handed-out group was last judged by, so the group stays judgeable once the
// cache evicts both. Each reader keeps its own, so the memory is bounded by the groups held.
type Superseded = { successor?: Successor };

// A successor, when it arrived, and its copy's fate (see `hooks.groupFate`). Not the consumer:
// a handed-out successor's own record would retain the next one, chaining every later group to
// the oldest held.
type Successor = { sequence: number; arrived: Instant; fate: () => Error | undefined };

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
	// Every group sequence this reader handed out, so a re-created group or one a second route
	// delivers is handed out once.
	#delivered = new Delivered();
	// Cached groups the arrival cursor skipped as stale, each with the budget it was judged
	// under, so a wider budget offers them again. Only a widening reverses a stale verdict, and
	// an entry is kept only while its group is cached, since nothing could offer it again.
	#skipped = new Map<number, number>();
	// At most the narrowest budget in #skipped, so a read whose budget did not widen skips the scan.
	#narrowest = Number.POSITIVE_INFINITY;
	// This reader closed itself, so lowering its floor offers nothing: a producer's clean close
	// alone leaves the cache readable, but the reader leaving must stay terminal.
	#closed = false;
	// What {@link largest} last reported, and whether it ended: moving the bounds afterwards
	// does not bring it back.
	#largestSeen?: Position;
	#largestDone = false;
	#nextSequence = 0;
	#cursor = new Signal<{ start: number; end?: number }>({ start: 0 });
	// A FETCH's own budget in place of the subscription's; unlike SUBSCRIBE's zero
	// default, an omitted one is unbounded, leaving publisher retention alone.
	#fetchBudget?: number;
	// Which cursor owns this subscription. Both cursors draw from one buffer, so the
	// first group read commits the track to arrival order and {@link ordered} commits
	// it to sequence order; whichever wins is the only one allowed from here on.
	// Datagrams are a separate cursor and never commit.
	#mode?: "arrival" | "ordered";
	// What this cursor reports as drops: sequences given up and not reported yet,
	// the track's drops last folded in, and every sequence already reported. Drops are
	// persistent track state, so they are tracked here rather than removed for everyone.
	#pending: DropRun[] = [];
	#dropsSeen?: { drops: readonly DropRun[]; start: number; end?: number };
	#reported: Run[] = [];
	// The group the frame-level helpers are currently draining, acquired through the
	// sequence cursor so frame reads and {@link Ordered.nextGroup} share one floor.
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
	#arrivals?: { sequence: number; arrived: Instant }[];

	#drift(): Drift {
		const { end } = this.#cursor.peek();
		const timeline = this.#state.timeline;
		let presentation: { sequence: number; timestamp: Timestamp } | undefined;
		const info = this.#state.info.peek();
		// An accepted untimed track cannot contain a presentation timestamp.
		if (info === undefined || info.timescale != null) {
			// The edge wants the newest content that exists, so it takes the newest
			// stamped group's latest frame: walk back from the cap to the first one.
			for (let i = (end === undefined ? timeline.length : timelineIndex(timeline, end)) - 1; i >= 0; i--) {
				const group = timeline[i];
				if (group.closed.peek() instanceof Error) continue;
				const timestamp = hooks.groupTimestamp(group);
				if (timestamp === undefined) continue;
				presentation = { sequence: group.sequence, timestamp: hooks.groupLatest(group) ?? timestamp };
				break;
			}
		}

		const requested = this.#fetchBudget ?? this.#state.update.peek()?.maxDelay ?? 0;
		const retained = info?.maxAge;
		return {
			budget: retained === undefined ? requested : Math.min(requested, retained),
			bounded: retained !== undefined && requested >= retained,
			presentation,
			end,
		};
	}

	// The group after `sequence` that bounds it: the first servable one below `end` that
	// counts as content (its first frame landed, or it ended), stamped or not.
	//
	// Its first timestamp is the furthest presentation time `sequence` could still reach.
	// An upper bound, deliberately: a frame's duration is not on the wire, so a group's own
	// last timestamp says where it starts presenting, not where it ends. A group whose header
	// landed without a frame is not content yet, so the next one is the bound; an unstamped
	// successor leaves the reach unbounded.
	#successor(sequence: number, end?: number): GroupConsumer | undefined {
		return this.#after(sequence, end, hooks.groupStarted);
	}

	// The group after `sequence` that starts its wall clock: like {@link #successor}, but
	// counting a group before its first frame. Its header arrived, so a relay ages the group
	// before it as the origin does, even if its first frame never comes.
	#arrival(sequence: number, end?: number): GroupConsumer | undefined {
		return this.#after(sequence, end, () => true);
	}

	// The first servable group after `sequence` below `end` that `counts`.
	#after(
		sequence: number,
		end: number | undefined,
		counts: (group: GroupConsumer) => boolean,
	): GroupConsumer | undefined {
		const timeline = this.#state.timeline;
		for (let i = timelineIndex(timeline, sequence + 1); i < timeline.length; i++) {
			const successor = timeline[i];
			if (end !== undefined && successor.sequence >= end) break;
			if (successor.closed.peek() instanceof Error) continue;
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
	#staleAt(sequence: number, drift: Drift, wake = false, held?: Superseded): boolean {
		const arrived = this.#arrivals
			? this.#keptSupersededAt(sequence, drift, held)
			: this.#supersededAt(this.#arrival(sequence, drift.end), drift, held);
		if (arrived === undefined) return false;
		// Plain arithmetic: an unenforced budget is infinite, which no duration brand holds.
		const deadline = arrived + drift.budget * 1_000_000;
		if (now() >= deadline) return true;
		if (wake) this.#wakeAt(deadline);
		// A closed subscriber keeps no timeline to bound the reach with.
		if (this.#arrivals) return false;

		const successor = this.#successor(sequence, drift.end);
		if (!successor) return false;
		const reach = hooks.groupTimestamp(successor)?.asMillis();
		return (
			drift.presentation !== undefined &&
			drift.presentation.sequence > sequence &&
			reach !== undefined &&
			drift.presentation.timestamp.asMillis() - reach >= drift.budget
		);
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
			(drift.end === undefined || recorded.sequence < drift.end) &&
			(!arrival || arrival.sequence > recorded.sequence)
		) {
			return recorded.arrived;
		}
		if (!arrival || arrived === undefined) {
			held.successor = undefined;
		} else if (recorded?.sequence !== arrival.sequence || recorded.arrived !== arrived || failed(recorded)) {
			// Re-recorded only on a change, so judging the same successor again allocates nothing.
			held.successor = { sequence: arrival.sequence, arrived, fate: hooks.groupFate(arrival) };
		}
		return arrived;
	}

	// {@link #supersededAt} for a closed subscriber, with the arrivals kept at close standing in
	// for the timeline it cleared.
	#keptSupersededAt(sequence: number, drift: Drift, held?: Superseded): Instant | undefined {
		const covered = (candidate: number) => drift.end === undefined || candidate < drift.end;
		const kept = this.#arrivals?.find((entry) => entry.sequence > sequence && covered(entry.sequence));
		const recorded = held?.successor;
		if (
			recorded &&
			!failed(recorded) &&
			covered(recorded.sequence) &&
			(!kept || kept.sequence > recorded.sequence)
		) {
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
		void sleepUntil(Instant(at), { signal: timer.signal, ref: false }).then(
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
		const held: Superseded = {};
		hooks.expireGroup(group, {
			expired: () => this.#staleAt(group.sequence, this.#drift(), true, held),
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

	// Record `sequence` as handed out. An ordered read never goes back, and any reader stops
	// expecting what is past its budget, so only the gaps within it cost memory.
	#handOut(sequence: number, ordered: boolean): void {
		if (ordered) this.#delivered.forgetBelow(sequence);
		this.#delivered.add(sequence);
		this.#skipped.delete(sequence);
		if (this.#resume) this.#resume.forgetPastBudget(this.#delivered);
		else this.#forgetPastBudget(this.#delivered);
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
		let skipped: number | undefined;
		let scanned = false;
		for (let oldest = delivered.aboveGap(); oldest !== undefined; oldest = delivered.aboveGap()) {
			if (!this.#staleAt(oldest, drift)) break;
			if (!scanned) {
				scanned = true;
				const producer = this.#state.producer;
				for (const sequence of this.#skipped.keys()) {
					if (producer && !cachedHolds(producer, sequence)) this.#skipped.delete(sequence);
					else skipped = Math.min(skipped ?? sequence, sequence);
				}
			}
			if (skipped !== undefined && skipped < oldest) {
				delivered.forgetBelow(skipped);
				break;
			}
			delivered.forgetBelow(oldest);
		}
	}

	// Record `sequence` as skipped stale under `budget`, so a wider one offers it again.
	#skip(sequence: number, budget: number): void {
		this.#skipped.set(sequence, budget);
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
		const sequences: number[] = [];
		let narrowest = Number.POSITIVE_INFINITY;
		for (const [sequence, judged] of this.#skipped) {
			if (judged >= budget) {
				narrowest = Math.min(narrowest, judged);
				continue;
			}
			this.#skipped.set(sequence, budget);
			narrowest = budget;
			sequences.push(sequence);
			// Judged again, so a stale drop not reported yet is withdrawn.
			if (dropAt(this.#pending, sequence) instanceof Error) {
				this.#pending = this.#pending.flatMap((run) =>
					run.start <= sequence && sequence < run.end && toStreamCode(run.error) === StreamCode.Old
						? removeRun([run], sequence, sequence + 1)
						: [run],
				);
			}
		}
		this.#narrowest = narrowest;
		const producer = this.#state.producer;
		if (producer && !this.#closed) reofferSequences(producer, this.#state, sequences);
	}

	private constructor(name: string, state: TrackState) {
		this.name = name;
		this.#state = state;
		// Each reader filters locally even when another reader widens upstream demand.
		this.#cursor.set(groupBounds(state.update.peek()?.groups));
	}

	static {
		makeSubscriber = (name, state) => new Subscriber(name, state);
		hooks.subscriberEnd = (subscriber) =>
			subscriber.#resume ? subscriber.#resume.end() : subscriber.#state.end.peek();
		hooks.subscriberEndError = (subscriber) =>
			subscriber.#resume ? subscriber.#resume.endError() : subscriber.#state.endError;
		hooks.joinSubscriber = (subscriber) => {
			if (subscriber.#resume) subscriber.#resume.join();
			else if (subscriber.#state.producer) hooks.joinProducer(subscriber.#state.producer);
		};
		hooks.sourceStart = (subscriber) => {
			if (subscriber.#resume) return subscriber.#resume.start();
			const producer = subscriber.#state.producer;
			const start = producer && sourceStart(producer);
			if (start === undefined || start === "pending") return start;
			return Math.max(start, subscriber.#cursor.peek().start);
		};
		hooks.tryRecvGroup = (subscriber, dispositions) => subscriber.#tryRecvGroup(dispositions);
		hooks.tryDrop = (subscriber) => subscriber.#tryDrop();
		hooks.tryNextGroup = (subscriber) => subscriber.#tryNextGroup();
		hooks.peekGroup = (subscriber, ordered) => subscriber.#peekGroup(ordered);
		hooks.forgetPastBudget = (subscriber, delivered) => subscriber.#forgetPastBudget(delivered);
		hooks.unreport = (subscriber, start, end) => {
			subscriber.#reported = removeRun(subscriber.#reported, start, end);
		};
		hooks.tryRecvDatagram = (subscriber) => subscriber.#tryDatagram();
		hooks.deliveredRuns = (subscriber) => subscriber.#delivered.runs;
		hooks.handedOut = (subscriber, sequence) => subscriber.#delivered.has(sequence);
		hooks.datagramChanged = (subscriber, fn) => {
			if (subscriber.#resume) return subscriber.#groupChanged(fn);
			const state = subscriber.#state;
			const dispose = [
				state.datagrams.changed(fn),
				state.closed.changed(fn),
				state.end.changed(fn),
				state.received.changed(fn),
				state.owed.changed(fn),
			];
			return () => {
				for (const close of dispose) close();
			};
		};
		hooks.hasUnreadGroup = (subscriber, start, end, delivered) =>
			subscriber.#state.groups
				.peek()
				.some(
					(group) =>
						group.sequence >= start &&
						(end === undefined || group.sequence < end) &&
						!delivered.has(group.sequence) &&
						!(group.closed.peek() instanceof Error),
				);
		hooks.spent = (subscriber, start, end, delivered) => {
			const state = subscriber.#state;
			// A datagram outside the cursor is gone for good, as a read would discard it.
			const cursor = subscriber.#cursor.peek();
			const readable = state.datagrams
				.peek()
				.some(
					(datagram) =>
						datagram.sequence >= cursor.start &&
						(cursor.end === undefined || datagram.sequence < cursor.end),
				);
			return (
				(state.closed.peek() !== undefined || subscriber.#complete()) &&
				!readable &&
				!hooks.hasUnreadGroup(subscriber, start, end, delivered)
			);
		};
		hooks.staleAt = (subscriber, sequence) => {
			const held: Superseded = {};
			return {
				expired: () => subscriber.#staleAt(sequence, subscriber.#drift(), true, held),
				changed: subscriber.#expiryChanged(),
			};
		};
		hooks.cachedGroup = (subscriber, sequence, from = 0) => {
			if (subscriber.#resume) return subscriber.#resume.cachedGroup(sequence, from);
			const producer = subscriber.#state.producer;
			return producer && cachedMirror(producer, sequence, from);
		};
		hooks.holdsGroup = (subscriber, sequence) => {
			const producer = subscriber.#state.producer;
			return producer !== undefined && cachedHolds(producer, sequence);
		};
		hooks.givenUp = (subscriber, sequence) => {
			const drops = subscriber.#state.drops;
			return { error: dropAt(drops.peek(), sequence), changed: drops };
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
		hooks.latestSequence = (subscriber) =>
			subscriber.#resume ? subscriber.#resume.latest() : subscriber.#state.latest;
		hooks.bufferedLargest = (subscriber) => subscriber.#bufferedLargest();
		hooks.objectLargest = (subscriber) => {
			if (subscriber.#resume) return subscriber.#resume.objectLargest();
			const producer = subscriber.#state.producer;
			return producer && objectLargest(producer);
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
		hooks.fetchBudget = (subscriber, maxDelay) => {
			subscriber.#fetchBudget = maxDelay ?? Number.POSITIVE_INFINITY;
		};
		// The sequence cursor lives here (it shares the buffer and the drift anchor with
		// the arrival cursor); `Ordered` is the handle that reaches it.
		ordered_ = {
			nextGroup: (subscriber) => subscriber.#nextGroup(),
			tryNextGroup: (subscriber) => subscriber.#tryNextGroup(),
			ended: (subscriber) => subscriber.#ended(),
			readFrame: (subscriber) => subscriber.#readFrame(),
			readString: (subscriber) => subscriber.#readString(),
			readJson: (subscriber) => subscriber.#readJson(),
			readBool: (subscriber) => subscriber.#readBool(),
			// Unordered either way, so both handles reach the same cursor.
			recvDatagram: (subscriber) => subscriber.#recvDatagram(),
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
			const end = this.#state.end.peek();
			if (end !== undefined) return end;

			const closed = this.#state.closed.peek();
			if (closed instanceof Error) throw closed;
			if (closed !== undefined) throw new Error("track closed before its end was known");

			await Signal.race(this.#state.end, this.#state.closed);
		}
	}

	/**
	 * The largest location within {@link withRange}: the current one at first, then each newer
	 * one (skipping any in between), and `undefined` once the track ends.
	 *
	 * The largest is the track's newest group or datagram at the newest frame (0 while empty), as
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
		if (!newest || newest.sequence !== latest) return undefined;

		for (let i = groups.length - 1; i >= 0; i--) {
			const count = groups[i].frameCount;
			if (count > 0) return { group: groups[i].sequence, frame: count - 1 };
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
	 * omitted end removes the cap. Lowering the floor or raising the cap offers the groups it
	 * newly covers that the track still caches and this reader never handed out; a group is
	 * never handed out twice. A local filter, not a request: it leaves the wire subscription
	 * alone. One exception: on a track served across route changes, lowering the floor asks the
	 * serving route again for the groups it newly covers within the subscription's groups, since
	 * the route that delivered them may be gone. {@link largest} reports within these bounds.
	 */
	withRange(groups: Groups): this {
		const { start, end } = groupBounds(groups);
		const before = this.#cursor.peek();
		this.#cursor.set({ start, end });
		const producer = this.#state.producer;
		if (!this.#resume && !this.#closed && producer && start < before.start) {
			// The ordered cursor let go of what it passed below the old floor; the arrival
			// cursor keeps it buffered, which this skips.
			reofferCached(producer, this.#state, start, before.start, (sequence) => !this.#delivered.has(sequence));
		}
		return this;
	}

	/** Close the track (optionally with an error), closing any pending groups. Idempotent. */
	close(abort?: Error) {
		if (!this.#closed) {
			const arrivals = this.#state.timeline.filter((group) => !(group.closed.peek() instanceof Error));
			this.#arrivals = arrivals.map((group) => ({
				sequence: group.sequence,
				arrived: hooks.groupArrived(group),
			}));
		}
		this.#closed = true;
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
	 * is still delivered. When several groups are buffered, the lowest sequence is
	 * returned first.
	 *
	 * Honors the range set by {@link withRange}: a group
	 * beyond the cap stays buffered (not dropped) and is offered once the cap rises, even
	 * after a clean close, without blocking in-range groups that arrive behind it.
	 * A group whose presentation time is further behind the live edge than this
	 * subscriber's `maxDelay` is skipped. The default subscription's zero takes the live edge.
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

			// Idle, or parked at the boundary waiting for the cap to rise.
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
		if (end !== undefined && cap !== undefined && cap < finalSequence(end)) return undefined;
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
		const groups = this.#state.groups.peek();
		const { start, end } = this.#cursor.peek();
		const old = () => new StreamError(StreamCode.Old, { message: "group is past the budget" });

		if (dispositions) {
			const dropped = this.#tryDrop();
			if (dropped) return dropped;
		}
		const drift = this.#drift();
		this.#reoffer(drift.budget);
		// Keep unread groups below the floor until cache eviction: replacing the
		// subscription can lower it, just as raising the cap re-offers held groups. Found
		// after the re-offer, which may insert below the floor.
		const index = timelineIndex(groups, start);

		for (;;) {
			// The buffer is sequence-sorted, so an in-range group that arrives behind a
			// beyond-cap one sorts in front of it and is never blocked by it.
			const group = groups[index];
			if (!group || (end !== undefined && group.sequence >= end)) break;
			groups.splice(index, 1);
			const failed = group.closed.peek();
			if (dispositions && failed instanceof Error) {
				const dropped = this.#report(group.sequence, group.sequence + 1, failed);
				if (dropped) return dropped;
				continue;
			}
			if (this.#delivered.has(group.sequence)) {
				group.close();
				continue;
			}
			if (this.#staleAt(group.sequence, drift)) {
				group.close();
				// A re-offered group stale at the bound is final.
				if (drift.bounded) this.#skipped.delete(group.sequence);
				else this.#skip(group.sequence, drift.budget);
				if (dispositions) {
					const dropped = this.#report(group.sequence, group.sequence + 1, old());
					if (dropped) return dropped;
				} else if (this.#dropsSeen) {
					// A cursor #tryDrop reads reports it there.
					this.#pending = insertDrop(this.#pending, group.sequence, group.sequence + 1, old());
				}
				continue;
			}
			this.#handOut(group.sequence, false);
			return { kind: "group", group: this.#guard(group) };
		}

		const group = groups[index];
		const closed = this.#state.closed.peek();
		if (closed instanceof Error) return { kind: "error", error: closed };
		if (closed === undefined && !this.#complete()) return { kind: "idle" };
		// A group beyond the cap outlives a clean close: it becomes deliverable if
		// the cap rises, so the track isn't over while any are held.
		return group ? { kind: "boundary" } : { kind: "done" };
	}

	// The first part of `[from, to)` this cursor has not reported yet, now reported, with why.
	#report(from: number, to: number, error: Error): (Recv & { kind: "drop" }) | undefined {
		const { start, end } = this.#cursor.peek();
		const [gap] = runGaps(this.#reported, Math.max(from, start), Math.min(to, end ?? to));
		if (!gap) return undefined;
		insertRun(this.#reported, gap.start, gap.end);
		return { kind: "drop", ...gap, error };
	}

	// The next run this cursor gave up and has not reported, without taking a group. From the
	// first call on, a read without dispositions keeps its budget skips for it too. A resumed
	// one reports what its routes' copies gave up.
	#tryDrop(): (Recv & { kind: "drop" }) | undefined {
		if (this.#resume) {
			return this.#resume.tryDrop(this.#delivered);
		}
		const { start, end } = this.#cursor.peek();
		// Rescanned only after it changed, so an idle poll costs nothing per drop held.
		// Out-of-window runs were skipped, so a moved window rescans too.
		const drops = this.#state.drops.peek();
		const seen = this.#dropsSeen;
		if (!seen || seen.drops !== drops || seen.start !== start || seen.end !== end) {
			this.#dropsSeen = { drops, start, end };
			for (const run of drops) this.#pending = insertDrop(this.#pending, run.start, run.end, run.error);
		}
		for (let run = this.#pending.shift(); run; run = this.#pending.shift()) {
			const dropped = this.#report(run.start, run.end, run.error);
			if (!dropped) continue;
			if (dropped.end < run.end) this.#pending.unshift({ start: dropped.end, end: run.end, error: run.error });
			return dropped;
		}
		return undefined;
	}

	// A group from the routes' copies; see #resume.
	#tryResumed(ordered: boolean, dispositions = false): Recv {
		const resume = this.#resume as ResumeSource;
		// A clean end leaves what the copies hold readable, as a direct reader's buffer is.
		const closed = this.#state.closed.peek();
		if (closed instanceof Error) return { kind: "error", error: closed };
		const recv = resume.tryGroup(ordered, this.#delivered, dispositions);
		if (recv.kind === "group") this.#handOut(recv.group.sequence, ordered);
		return recv;
	}

	// The live edge reached the end, with no hole a wire subscription's pending tail still
	// owes below it. Reader completion does not settle the tail: open streams remain
	// accounted for by the wire layer, and an abort still wins on the next read.
	#complete(): boolean {
		const end = this.#state.end.peek();
		return end !== undefined && this.#state.received.peek() >= finalSequence(end) && !this.#state.owed.peek();
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
		const dispose = [
			this.#state.groups.changed(fn),
			this.#state.drops.changed(fn),
			this.#state.startChanged.changed(fn),
			this.#cursor.changed(fn),
			// A widened budget re-offers the groups it skipped as stale.
			this.#state.update.changed(fn),
			this.#state.closed.changed(fn),
			this.#state.end.changed(fn),
			this.#state.received.changed(fn),
			this.#state.owed.changed(fn),
		];
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
	 * waiting for a sequence nothing will republish would park forever.
	 */
	tryRecvGroup(): GroupConsumer | undefined {
		this.#live();
		this.#mode = "arrival";
		return taken(
			() => this.#tryRecvGroup(),
			() => this.#ended(),
		);
	}

	/**
	 * Receive the next datagram in arrival order.
	 *
	 * Datagrams are a separate best-effort channel from groups (see
	 * {@link Producer.appendDatagram}); they share only the sequence namespace. Those outside
	 * the group range are skipped. A consumer that falls too far behind silently loses the
	 * oldest datagrams. Read this alongside
	 * {@link recvGroup} (e.g. in a separate loop) to receive both channels concurrently.
	 * The two cursors are independent: a datagram never moves the group cursor.
	 */
	async recvDatagram(): Promise<Datagram | undefined> {
		this.#live();
		return this.#recvDatagram();
	}

	// The datagram cursor, reachable from either handle: unordered by construction, so the
	// choice of group order says nothing about it.
	async #recvDatagram(): Promise<Datagram | undefined> {
		for (;;) {
			const recv = this.#tryDatagram();
			switch (recv.kind) {
				case "datagram":
					return recv.datagram;
				case "done":
					return this.#ended();
				case "error":
					throw recv.error;
			}
			if (this.#resume) await this.#wait();
			else
				await Signal.race(
					this.#state.datagrams,
					this.#state.closed,
					this.#state.end,
					this.#state.received,
					this.#state.owed,
				);
		}
	}

	// The synchronous half of #recvDatagram.
	#tryDatagram(): RecvDatagram {
		const closed = this.#state.closed.peek();
		if (this.#resume && closed instanceof Error) return { kind: "error", error: closed };
		// The group range bounds datagrams too, judged when each is read. One outside it is
		// gone for good: nothing holds it for a later raise, unlike a group past the cap.
		const { start, end } = this.#cursor.peek();
		const inRange = (datagram: Datagram) =>
			datagram.sequence >= start && (end === undefined || datagram.sequence < end);
		if (this.#resume) {
			for (;;) {
				const recv = this.#resume.tryDatagram(this.#delivered);
				if (recv.kind !== "datagram" || inRange(recv.datagram)) return recv;
			}
		}
		const datagrams = this.#state.datagrams.peek();
		for (let datagram = datagrams.shift(); datagram; datagram = datagrams.shift()) {
			if (inRange(datagram)) return { kind: "datagram", datagram };
		}
		if (closed instanceof Error) return { kind: "error", error: closed };
		if (closed !== undefined || this.#complete()) return { kind: "done" };
		return { kind: "idle" };
	}

	// The sequence cursor behind {@link Ordered}, which owns the only public door to it.
	async #nextGroup(): Promise<GroupConsumer | undefined> {
		for (;;) {
			const recv = this.#tryNextGroup();
			switch (recv.kind) {
				case "group":
					return recv.group;
				case "done":
					return this.#ended();
				case "error":
					throw recv.error;
			}
			await this.#wait();
		}
	}

	// The sequence #tryNextGroup (`ordered`) or #tryRecvGroup would take next: the lowest buffered
	// group in range, not yet handed out, that the budget spares.
	#peekGroup(ordered: boolean): number | undefined {
		const cursor = this.#cursor.peek();
		const start = ordered ? Math.max(cursor.start, this.#nextSequence) : cursor.start;
		const drift = this.#drift();
		for (const group of this.#state.groups.peek()) {
			if (group.sequence < start) continue;
			if (cursor.end !== undefined && group.sequence >= cursor.end) return undefined;
			if (!this.#delivered.has(group.sequence) && !this.#staleAt(group.sequence, drift)) return group.sequence;
		}
		return undefined;
	}

	// The synchronous half of #nextGroup.
	#tryNextGroup(): Recv {
		const recv = this.#resume ? this.#tryResumed(true) : this.#tryNextBuffered();
		// One cursor: the frame helpers must not keep draining a group this read just
		// moved past, or interleaved reads would run backwards.
		if (recv.kind === "group" && this.#frameGroup && this.#frameGroup.sequence < recv.group.sequence) {
			this.#frameGroup.close();
			this.#frameGroup = undefined;
		}
		return recv;
	}

	#tryNextBuffered(): Recv {
		const groups = this.#state.groups.peek();
		const cursor = this.#cursor.peek();
		const start = Math.max(cursor.start, this.#nextSequence);
		while (groups.length > 0 && groups[0].sequence < start) groups.shift()?.close();
		// One anchor for the whole pass, so walking a backlog off stays linear.
		const drift = this.#drift();

		for (;;) {
			const group = groups[0];
			if (!group || (cursor.end !== undefined && group.sequence >= cursor.end)) break;
			groups.shift();
			this.#nextSequence = group.sequence + 1;
			// Every frame this group could still hold is past the budget, so keep
			// scanning: one pass walks a whole backlog off rather than replaying it.
			if (this.#staleAt(group.sequence, drift)) {
				group.close();
				continue;
			}
			this.#handOut(group.sequence, true);
			return { kind: "group", group: this.#guard(group) };
		}

		const closed = this.#state.closed.peek();
		if (closed instanceof Error) return { kind: "error", error: closed };
		// A group parked above the cap stays deliverable even after a clean close
		// (its frames remain buffered), so keep waiting for a cap raise. Only a
		// drained track reports finished.
		const end = this.#state.end.peek();
		if (
			!groups[0] &&
			(closed !== undefined || this.#complete() || (end !== undefined && start >= finalSequence(end)))
		) {
			return { kind: "done" };
		}
		return { kind: "idle" };
	}

	/**
	 * Reads the next frame across groups, in sequence order, with its group and frame numbers.
	 * Treat the returned frame bytes as read-only; they are shared with other consumers.
	 *
	 * Groups are acquired through the same sequence cursor as {@link Ordered.nextGroup},
	 * so frames never run backwards: a late lower-sequence group is skipped, and so is
	 * one every frame of which `maxDelay` proves is too old. A group the budget abandons
	 * mid-stall ends cleanly and the cursor resyncs from the next group; a gap inside a
	 * group still surfaces as {@link TooFarBehind} or {@link GroupTooLarge}.
	 */
	async #readFrame(): Promise<({ group: number; frame: number } & Frame) | undefined> {
		for (;;) {
			if (!this.#frameGroup) {
				this.#frameGroup = await this.#nextGroup();
				if (!this.#frameGroup) return undefined;
			}
			const group = this.#frameGroup;

			let next: ({ sequence: number } & Frame) | undefined;
			try {
				next = await group.readFrameSequence();
			} catch (err) {
				// The group failed underneath us: resync from the next one, surfacing
				// only what the caller can act on (a gap, or the track's own abort).
				this.#frameGroup = undefined;
				group.close();
				if (err instanceof TooFarBehind || err instanceof GroupTooLarge) throw err;
				const closed = this.#state.closed.peek();
				if (closed instanceof Error) throw closed;
				continue;
			}

			if (next) {
				return {
					group: group.sequence,
					frame: next.sequence,
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

	/** Reads the next frame and decodes it as a one-byte boolean, throwing on a malformed frame. */
	async #readBool(): Promise<boolean | undefined> {
		const next = await this.#readFrame();
		if (!next) return undefined;
		const payload = next.payload;
		if (payload.byteLength !== 1 || !(payload[0] === 0 || payload[0] === 1)) throw new Error("invalid bool frame");
		return payload[0] === 1;
	}

	/**
	 * Read this track's groups in sequence order instead of arrival order.
	 *
	 * Both cursors draw from the same buffer, so a track is read one way or the other: this
	 * hands the subscription to the returned {@link Ordered} and leaves this handle inert.
	 * {@link recvGroup} and {@link recvDatagram} throw afterwards. Throws once a
	 * {@link recvGroup} call has already committed the track to arrival order.
	 *
	 * Datagrams come along: they are a separate cursor either way, so the choice of group
	 * order says nothing about them, and reading them commits nothing.
	 */
	ordered(): Ordered {
		if (this.#mode === "ordered") throw new Error("track is already read in sequence order");
		if (this.#mode === "arrival") throw new Error("track is already read in arrival order");
		this.#mode = "ordered";
		return makeOrdered(this);
	}

	/**
	 * Update this subscription's options (e.g. priority), triggering a SUBSCRIBE_UPDATE to the
	 * publisher. Replaces this reader's group bounds without redelivering groups already
	 * read; an arrival reader can receive unread cached groups below its previous floor.
	 * Mirrors the Rust `Subscriber::update`.
	 */
	update(options: Subscription) {
		const subscription = subscriptionDefaults(options);
		this.#cursor.set(groupBounds(subscription.groups));
		this.#state.update.set(subscription);
	}
}

/**
 * A {@link Subscriber} that reads groups in sequence order.
 *
 * Created by {@link Subscriber.ordered}, which takes the subscription over: the two
 * cursors draw from one buffer, so a track is read one way or the other and never both.
 * Every group this returns has a higher sequence than the last, so a late arrival is
 * skipped rather than delivered out of turn.
 *
 * `maxDelay` applies as this cursor reads, exactly as it does on the arrival cursor: a
 * group is skipped once its reach, where its immediate successor begins, is that far
 * behind the newest frame on the track. Nothing weaker convicts it, since the reach is
 * the only proof that every frame it could still hold is past the budget, so a backlog
 * inside the budget is still delivered whole, as a burst in order. The budget follows a
 * group already handed out too: a stalled group's pending frame read rejects once newer
 * content has pulled that far ahead.
 *
 * A history or VOD read sets `maxDelay: null` (no limit). This reader skips a missing
 * group rather than waiting for it, but the Rust reader waits for one a subscription still
 * owes, within the budget, or until it resolves without one. An unlimited or long budget
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

	/**
	 * Limit subsequent reads to these groups and return this reader for chaining; see
	 * {@link Subscriber.withRange}. An ordered read never goes back, so lowering the floor
	 * below a group it already returned offers nothing.
	 */
	withRange(groups: Groups): this {
		this.#subscriber.withRange(groups);
		return this;
	}

	/** Update this subscription's options; see {@link Subscriber.update}. */
	update(options: Subscription) {
		this.#subscriber.update(options);
	}

	/** Close the track (optionally with an error), closing any pending groups. Idempotent. */
	close(abort?: Error) {
		this.#subscriber.close(abort);
	}

	/**
	 * Return the next group with a strictly-greater sequence number than the last returned.
	 *
	 * Late arrivals (sequence at or below the last returned) are silently skipped, as is a
	 * group whose every frame is further behind the live edge than `maxDelay` (the default
	 * subscription's zero keeps only what nothing newer has superseded). Honors the bounds set by
	 * {@link withRange}.
	 */
	nextGroup(): Promise<GroupConsumer | undefined> {
		return ordered_.nextGroup(this.#subscriber);
	}

	/**
	 * Take the next group as {@link nextGroup} would, without waiting; see
	 * {@link Subscriber.tryRecvGroup}. Mirrors the Rust `poll_next_group`.
	 */
	tryNextGroup(): GroupConsumer | undefined {
		return taken(
			() => ordered_.tryNextGroup(this.#subscriber),
			() => ordered_.ended(this.#subscriber),
		);
	}

	/**
	 * Read the next frame across groups, in sequence order, with its group and frame numbers.
	 *
	 * Rides the same cursor as {@link nextGroup} and shares this handle's contract: a
	 * buffered backlog is drained in full up to the point `maxDelay` proves it useless.
	 * Treat the returned frame bytes as read-only; they are shared with other consumers.
	 */
	readFrame(): Promise<({ group: number; frame: number } & Frame) | undefined> {
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

	/** Read the next frame and decode it as a one-byte boolean, throwing on a malformed frame. */
	readBool(): Promise<boolean | undefined> {
		return ordered_.readBool(this.#subscriber);
	}

	/**
	 * Receive the next datagram in arrival order.
	 *
	 * Datagrams are a separate best-effort channel from groups (see
	 * {@link Producer.appendDatagram}); they share only the sequence namespace, and
	 * neither cursor moves the other. Unordered by construction, so this behaves
	 * identically on either handle; it is here so a track carrying both channels needs
	 * one subscription rather than two.
	 */
	recvDatagram(): Promise<Datagram | undefined> {
		return ordered_.recvDatagram(this.#subscriber);
	}
}

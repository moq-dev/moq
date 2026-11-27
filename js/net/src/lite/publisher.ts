import { Duration, type Instant, now, sleep, yieldNow } from "@moq/clock";
import { type Dispose, type Getter, Once, race, Signal } from "@moq/signals";
import type { Grant } from "../auth.ts";
import { Gate } from "../auth_gate.ts";
import type { AuthSession } from "../auth_session.ts";
import { enforceGrant } from "../auth_session.ts";
import type * as broadcast from "../broadcast.ts";
import { statsOf } from "../connection/stats.ts";
import { Withdrawal } from "../connection/withdrawal.ts";
import { Counts } from "../cover.ts";
import type * as datagram from "../datagram.ts";
import * as DatagramStream from "../datagram_stream.ts";
import { Runs } from "../drops.ts";
import {
	error,
	NotFound,
	ProtocolViolation,
	reason,
	StreamCode,
	StreamError,
	TimestampMismatch,
	toStreamCode,
	unauthorized,
} from "../error.ts";
import type * as group from "../group.ts";
import { Cost, type Hop, routesEqual, type WireRoute } from "../hop.ts";
import type * as Identity from "../identity.ts";
import {
	type DropRun,
	datagramTrackInfo,
	finalKey,
	type Hole,
	hiddenBelow,
	hooks,
	insertRun,
	NO_MAX_DELAY,
	presented,
	type Run,
	runGaps,
	sameHole,
	staleHole,
} from "../internal.ts";
import type { Consumer as OriginConsumer } from "../origin.ts";
import * as Path from "../path.ts";
import { readSize, sendPayload } from "../payload.ts";
import { Priority, type Ranking, sendOrder } from "../priority.ts";
import { concat, encodeVarint, type Reader, type Stream, type StreamVersion, Writer } from "../stream.ts";
import { floorAt, type Timescale, type Timestamp } from "../time.ts";
import type * as track from "../track.ts";
import type { Reported } from "../track.ts";
import { untilAborted } from "../util/abort.ts";
import { nextChange } from "../util/change.ts";
import { GivenUp } from "../util/given_up.ts";
import { max, min } from "../util/key.ts";
import { Budget } from "../util/yield.ts";
import { isEmpty, type Resolved, rangeKey } from "../window.ts";
import { type Advertised, type Advertisements, sameIdentity, wireOf } from "../wire.ts";
import { type Add, AnnounceInit, AnnounceOk, type AnnounceRequest, encodeAnnounceBroadcast } from "./announce.ts";
import { AnnounceStream } from "./announce_stream.ts";
import { Answer } from "./answer.ts";
import { Datagram as DatagramMessage, type Datagrams, DatagramsInfo, DatagramsUpdate } from "./datagram.ts";
import { closeViolation } from "./error.ts";
import type { Fetch } from "./fetch.ts";
import { Group as GroupMessage } from "./group.ts";
import { Probe } from "./probe.ts";
import {
	type Applied,
	type Edge,
	encodeSubscribeResponse,
	groupPolicy,
	LiveRanges,
	legacyStart,
	policyOf,
	positionFrames,
	positionStops,
	rangeBounds,
	rangesOf,
	requestedPlaced,
	requestedRange,
	requestedRanges,
	type Subscribe,
	SubscribeDrop,
	SubscribeEnd,
	SubscribeOk,
	type SubscribeResponse,
	SubscribeStart,
	SubscribeUpdate,
	spanBetween,
	subscriptionPolicy,
	type WireRange,
} from "./subscribe.ts";
import { TrackInfo as TrackInfoMessage, type Track as TrackMessage } from "./track.ts";
import { encodeTracksEvent, type Tracks as TracksMessage, type TracksEvent as TracksMessageEvent } from "./tracks.ts";
import {
	hasAnnounceId,
	hasAnnounceOk,
	hasCompression,
	hasDatagrams,
	hasDatagramTracks,
	hasEndError,
	hasGroupEnd,
	hasLargest,
	hasLengthFirst,
	hasProbeRtt,
	hasRanges,
	hasReliableReset,
	hasRouteCost,
	hasRoutes,
	hasSkip,
	hasTrackStream,
	keepsSubscribeOpen,
	resolvesStart,
	Version,
	waitsForSubscriberFin,
	wireTimescale,
} from "./version.ts";

const PROBE_INTERVAL = 100; // ms
const PROBE_MAX_AGE = 10_000; // ms
const PROBE_MAX_DELTA = 0.25;
const PROBE_RTT_DELTA = 0.25;
const datagramBudget = new Budget();

/** Map a signed delta to an unsigned zigzag varint value (mirrors Rust `varint::zigzag`). */
function zigzag(delta: bigint): bigint {
	return delta >= 0n ? delta << 1n : (-delta << 1n) - 1n;
}

/**
 * A frame's fields ahead of its payload, built in memory for one write: a stream write is far
 * costlier than a varint, and a backlog pays it for every frame. The timestamp `delta` is present
 * when the track is stamped, and `inflated` when the frame was decompressed. Lite-07 leads with the
 * size plus one, leaving 0 for GROUP_END; earlier versions end the header with the size.
 */
export function frameHeader(
	version: StreamVersion,
	delta: bigint | undefined,
	inflated: number | undefined,
	size: number,
	lengthFirst: boolean,
) {
	const parts: Uint8Array[] = [];
	if (lengthFirst) parts.push(encodeVarint(size + 1, version));
	if (delta !== undefined) parts.push(encodeVarint(zigzag(delta), version));
	if (inflated !== undefined) parts.push(encodeVarint(inflated, version));
	if (!lengthFirst) parts.push(encodeVarint(size, version));
	return concat(parts);
}

/**
 * Lite-07 GROUP_END, after a finished group's last frame: a zero `Payload Length`, where a frame
 * would start, then the group's exclusive `end` as a delta from `base`: the highest timestamp the
 * stream carried, or the group's start when it carried none. A finished group's end is at or
 * above both, so the delta is never negative.
 */
function groupEnd(version: StreamVersion, end: bigint, base: bigint): Uint8Array {
	return concat([encodeVarint(0, version), encodeVarint(end - base, version)]);
}

/** How a lite hop stamps a track's frames and datagrams. */
interface Stamp {
	/**
	 * The wire timescale TRACK_INFO declared (see `wireTimescale`), or undefined for a datagram on a
	 * scale-less lite-07 track, whose timestamp goes out as a bare number.
	 */
	timescale?: Timescale;
}

/**
 * How `version` stamps a track with `info`, or undefined when its frames carry no timestamps:
 * before lite-05, and on a scale-less lite-07 track, whose frame's time is its group's start (its
 * datagrams go out bare). Lite-05/06 declare milliseconds for a scale-less track, whose frames
 * carry their group's start.
 */
function stamping(version: Version, info: { timescale?: Timescale | null }): Stamp | undefined {
	const wire = wireTimescale(version, info.timescale);
	if (wire === undefined) return undefined;
	return { timescale: wire };
}

/**
 * A frame or datagram's `timestamp` as its raw wire value. A scale-less track's is its group's
 * start, or its datagram's sequence, which goes out verbatim on every version: nothing is stamped
 * with a clock, so a frame carries the same timestamp through every hop and fetch. Mirrors the
 * Rust `Stamp::wire`.
 */
function wireTime(timestamp: Timestamp, stamp: Stamp): bigint {
	const key = timestamp.key;
	if (key !== undefined) return key;
	// A timed timestamp on a track declared scale-less: the model never writes one.
	if (stamp.timescale === undefined) throw new TimestampMismatch();
	return BigInt(floorAt(timestamp, stamp.timescale));
}

/**
 * Settles once the peer acknowledged everything written before the FIN, or the stream failed.
 * Closing the session sooner would discard bytes still in flight.
 */
function acknowledged(writer: Writer): Promise<void> {
	return writer.closed.catch(() => {});
}

/**
 * Lite-07: whether a Group Stream this side just reset still accounts for its group, `held` once
 * a reset delivers its whole header. A reset of this side's delivers the held header, which does.
 * A STOP_SENDING owes a drop only when it evicted the stream before the peer acknowledged the
 * header, since the subscriber may never have read which group it carried; any other the
 * subscriber sent knowing the group, or wanting none of it. Mirrors the Rust `accounts`.
 */
async function accounts(stream: Writer, held: boolean): Promise<boolean> {
	const stop = stream.stopped;
	if (stop === undefined) return held;
	return stop !== StreamCode.Evicted || (await stream.heldAcknowledged());
}

// The TRACK_INFO a TRACK stream, or a listing's TRACKS_ADD, answers for a track.
/** A listing event as TRACKS carries it: an added track's info as its kind's request would answer. */
function tracksEvent(event: broadcast.TracksEvent, version: Version): TracksMessageEvent {
	if (event.kind !== "added") return event;
	if (event.datagrams)
		return { kind: "added", name: event.name, datagrams: true, info: new DatagramsInfo(event.info) };
	return { kind: "added", name: event.name, info: trackInfoMessage(event.info, version) };
}

function trackInfoMessage(info: track.Info, version: Version): TrackInfoMessage {
	return new TrackInfoMessage({
		priority: info.priority,
		refresh: info.refresh.ratio ?? undefined,
		refreshInterval: info.refresh.interval?.asMillis(),
		// Publisher Max Age: the publisher's staleness bound, advertised so relays re-serve
		// with the same bound.
		maxAge: info.maxAge?.asMillis(),
		// `#serveGroup` emits each frame converted to it.
		timescale: info.timescale ?? undefined,
		// An older peer is served the frames inflated.
		compress: hasCompression(version) && info.compression === "deflate",
	});
}

/** A track's cached TRACK_INFO, and the request behind it while requesters hold it. */
interface TrackInfoEntry {
	info: Promise<TrackInfoMessage>;
	/** The requesters holding the request: open TRACK streams, and FETCHes awaiting the answer. */
	holders: number;
	/** Whether the application answered, so a FETCH can reuse it after the request is let go. */
	answered: boolean;
	/** Lets the request go once its last holder leaves, abandoning it if still unanswered. */
	release: AbortController;
}

/** What {@link Publisher.openGroup} and {@link Publisher.serveGroup} need to serve one group. */
interface RunGroup {
	/** The subscription ID. */
	sub: bigint;

	/** The group to serve. */
	group: group.Consumer;
	/** Why to stop writing {@link group}, watched while it is served. */
	givenUp: GivenUp;

	/** How every frame is stamped, or undefined to send no timestamps. */
	stamp: Stamp | undefined;
	/** Whether to serve stored slices with their declared plain sizes. */
	compress: boolean;

	/** The subscription's ranking, which this stream joins for as long as it runs. */
	priority: Pick<Priority, "reserve" | "add" | "remove">;
	/** Lite-07: the group's own ranking within {@link priority}, by the ranges still wanting it. */
	ranking?: Ranking;

	/** Settles when the subscriber leaves, dropping a group still queued for a stream slot. */
	unsubscribed: Promise<void>;

	/** First frame to send; anything below it was excluded by the subscription. */
	start: number;

	/** Last frame to send (inclusive), or undefined for the rest of the group. */
	end?: number;

	/**
	 * Lite-07: where each group a range ends partway through stops, by group (see
	 * {@link FrameBounds.stops}). The stream parks there with the stream open, and sends the rest
	 * on it once an update moves the end. `ended` settles once the subscription's run ends, which
	 * resets a parked stream: no update will move its stop any more.
	 */
	stops?: { at: Getter<ReadonlyMap<bigint, number>>; ended: Promise<void> };

	/**
	 * Lite-07: called as the stream buffers GROUP_END with the group's end, so the starts inside
	 * the group's span are accounted for.
	 */
	ended?: (end: bigint) => void;

	/**
	 * Lite-07: called when a reset after GROUP_END was buffered accounts for the group, with the
	 * starts inside its span: the reset may cut the GROUP_END, so they are named again.
	 */
	cut?: (start: bigint, end: bigint) => void;
}

/**
 * The frame bounds a subscription placed on its start and end group, as they stand
 * after any SUBSCRIBE_UPDATE.
 *
 * Only the two named groups are qualified; the group range itself lives on the
 * subscriber's read cursor (see {@link frameRange}).
 */
type FrameBounds = {
	/** The group {@link startFrame} qualifies, if the subscription named one. */
	startGroup?: bigint;
	/** First frame to send within {@link startGroup}; every other group starts at 0. */
	startFrame: number;
	/** The group {@link endFrame} qualifies, if the subscription named one. */
	endGroup?: bigint;
	/** Last frame (inclusive) to send within {@link endGroup}; every other group runs to its end. */
	endFrame?: number;
	/** Lite-07: the first frame of each group a range starts partway through (see {@link positionFrames}). */
	starts: ReadonlyMap<bigint, number>;
	/** Lite-07: {@link starts} as each decoded update leaves them, until the run applies that update. */
	startsAfter: WeakMap<SubscribeUpdate, ReadonlyMap<bigint, number>>;
	/**
	 * Lite-07: the frame each group a range ends partway through stops before (see
	 * {@link positionStops}), which a group already streaming follows as an update moves it.
	 */
	stops: Signal<ReadonlyMap<bigint, number>>;
	/** Lite-07: {@link stops} as each decoded update leaves them, until the run applies that update. */
	stopsAfter: WeakMap<SubscribeUpdate, ReadonlyMap<bigint, number>>;
};

/** Whether two frame-end tables hold the same stops. */
function sameStops(a: ReadonlyMap<bigint, number>, b: ReadonlyMap<bigint, number>): boolean {
	if (a.size !== b.size) return false;
	for (const [group, frame] of a) if (b.get(group) !== frame) return false;
	return true;
}

/**
 * The frames of `key` a subscription asked for, as a start index and an inclusive end.
 *
 * The frame bounds qualify the start and end group only; every other group is served whole.
 * Which groups are served at all is the serving subscriber's ranges and floor,
 * applied when a group is popped rather than re-checked here.
 *
 * The serving loop calls this synchronously after the pop, before any SUBSCRIBE_UPDATE can
 * change `bounds`. Nothing downstream trims the frame range: it is a wire request,
 * deliberately decoupled from the receiver's local read cursor.
 */
function frameRange(bounds: FrameBounds, key: bigint): { start: number; end?: number } {
	return {
		start: bounds.starts.get(key) ?? (bounds.startGroup === key ? bounds.startFrame : 0),
		end: bounds.endGroup === key ? bounds.endFrame : undefined,
	};
}

/** What serving one group needs beyond the group itself. */
type ServeGroup = {
	/** The Subscribe ID the GROUP message references. */
	sub: bigint;
	/** How every frame is stamped, or undefined to send no timestamps. */
	stamp: Stamp | undefined;
	/** Whether to serve stored slices with their declared plain sizes. */
	compress: boolean;
	/** First frame to send; anything below it was excluded by the subscription. */
	start: number;
	/** Last frame to send (inclusive), or undefined for the rest of the group. */
	end?: number;
};

/** What serving fetched frames needs beyond the group and destination stream. */
type ServeFetch = Omit<ServeGroup, "sub"> & {
	/** Why to stop writing the group, which a write parked on flow control races. */
	givenUp: GivenUp;
};

/** What the serving loop takes from the subscribe stream instead of serving a group. */
type Control =
	/**
	 * The peer re-stated the subscription: priority, ordering, latency, and its ranges, with the
	 * Range IDs it retired and added on lite-07.
	 */
	| { kind: "update"; update: SubscribeUpdate; applied: Applied }
	/**
	 * Lite-07: the peer cleanly FIN'd its send half (no further updates; deliver every requested
	 * range and close cleanly once accounted for).
	 */
	| { kind: "fin" }
	/** The peer FIN'd (pre-lite-07), or our own half went away. Either way there is nobody left to serve. */
	| { kind: "done" }
	/** The stream failed; the subscription goes down with it. */
	| { kind: "error"; error: Error };

type SubscriptionControlOptions = {
	quic: WebTransport;
	reader: Reader;
	writer: Writer;
	version: Version;
	/** The SUBSCRIBE's ranges, which each lite-07 update edits. */
	live: LiveRanges;
	/** Apply an update, with the ranges it retired and added by Range ID on lite-07. */
	apply: (update: SubscribeUpdate, applied: Applied) => void;
	/** Pre-started `reader.done()` check from one-shot detection, if any. */
	firstDone?: Promise<boolean>;
};

/**
 * The subscribe stream's control half, decoded ahead of the serving loop.
 *
 * Decoding runs on its own and publishes each full subscription update immediately, so a
 * blocked response write cannot delay re-ranking streams already in flight. It separately
 * stores only the latest range state for the serving loop, which owns the local track cursor
 * and frame bounds. That keeps a group pop and its frame-range snapshot one indivisible step.
 *
 * Reading ahead makes control-first ordering hold for a burst. Coalescing bounds memory while
 * preserving the newest state decoded before the next group pop. The Rust publisher gets the
 * same ordering from `poll_decode_maybe`, which decodes straight out of the reader's buffer;
 * nothing here can decode synchronously, so it reads ahead instead.
 */
class SubscriptionControls {
	#writer: Writer;
	#live: LiveRanges;
	// The latest update the loop has yet to take, with every edit since the last one it took: a
	// range added then retired before then was never live, so it is in neither.
	#update?: {
		update: SubscribeUpdate;
		retired: bigint[];
		added: Map<bigint, WireRange>;
		retuned: Map<bigint, WireRange>;
	};
	// Sticky, first one wins: null once the stream is over, an Error once it failed.
	#end?: Error | null;
	// Lite-07: the subscriber cleanly FIN'd its send half (no further updates).
	#peerFin = false;
	#finTaken = false;
	#ended: Promise<Error | null>;
	#resolveEnd!: (end: Error | null) => void;
	#changed = new Signal(0);

	/** Settles once decoding stops, so teardown can wait for it rather than leaving it running. */
	readonly decoding: Promise<void>;

	constructor(options: SubscriptionControlOptions) {
		const { writer, live } = options;
		this.#writer = writer;
		this.#live = live;
		this.#ended = new Promise((resolve) => {
			this.#resolveEnd = resolve;
		});
		this.decoding = this.#decode(options);
		// Our own half going away ends the loop too, and has to reach it the same way: the
		// loop looks at nothing else.
		void writer.closed.then(
			() => this.#finish(null),
			(err: unknown) => this.#finish(error(err)),
		);
	}

	/** The next control to apply, or undefined while the peer is quiet. */
	take(): Control | undefined {
		const update = this.#update;
		this.#update = undefined;
		// An update decoded before the stream ended still applies before the sticky end.
		if (update)
			return {
				kind: "update",
				update: update.update,
				applied: { retired: update.retired, added: [...update.added], retuned: [...update.retuned] },
			};
		if (this.#peerFin && !this.#finTaken) {
			this.#finTaken = true;
			return { kind: "fin" };
		}
		if (this.#end === undefined) return undefined;
		return this.#end === null ? { kind: "done" } : { kind: "error", error: this.#end };
	}

	/**
	 * Where each live Range ID sits among the track model's ranges, which hold them in Range ID
	 * order. The decoder applies each update to the model as it decodes it, ahead of {@link take},
	 * so this rather than the last control taken is what indexes the model. Costs every range.
	 */
	positions(): Map<bigint, number> {
		return new Map(this.#live.ids().map((id, index) => [id, index]));
	}

	/** Whether the track model holds Range ID `id`, as {@link positions} would index it. */
	holds(id: bigint): boolean {
		return this.#live.get(id) !== undefined;
	}

	/** The live range with Range ID `id`, as the subscriber asked it. */
	range(id: bigint): WireRange | undefined {
		return this.#live.get(id);
	}

	/** The staleness budget in milliseconds of Range ID `id` when it is live and a live join. */
	joinBudget(id: bigint): number | undefined {
		const range = this.#live.get(id);
		return range && joinBudget(range);
	}

	/** Whether {@link take} has a control to answer with now. */
	pending(): boolean {
		return this.#update !== undefined || (this.#peerFin && !this.#finTaken) || this.#end !== undefined;
	}

	/** Whether the subscriber cleanly FIN'd its send half (lite-07). */
	get peerFin(): boolean {
		return this.#peerFin;
	}

	/** Calls `fn` once {@link take} may answer differently. */
	changed(fn: () => void): Dispose {
		return this.#changed.changed(fn);
	}

	/** Returns false when peer departure supersedes a blocked response write. */
	async response(pending: Promise<void>): Promise<boolean> {
		// `#ended` lives as long as the stream, so it is raced as-is rather than mapped per call.
		const result = await race([
			pending.then(
				() => ({ kind: "sent" }) as const,
				(err: unknown) => ({ kind: "error", error: error(err) }) as const,
			),
			this.#ended,
		]);

		if (result !== null && !(result instanceof Error)) {
			if (result.kind === "sent") return true;
			throw result.error;
		}

		// The race leaves the blocked encode running, so reset the writable half too.
		this.#writer.reset(result ?? new StreamError(StreamCode.Cancel, { message: "cancel" }));
		if (result) throw result;
		return false;
	}

	/** Settles once the stream is over: `null` when it ended cleanly, or the failure. */
	get ended(): Promise<Error | null> {
		return this.#ended;
	}

	#finish(end: Error | null) {
		if (this.#end !== undefined) return;
		this.#end = end;
		this.#resolveEnd(end);
		this.#changed.update((value) => value + 1);
	}

	async #decode({ reader, version, live, apply, quic, firstDone }: SubscriptionControlOptions) {
		try {
			// Runs until the peer's half ends, even past our own FIN: a lite-05+ publisher waits
			// for that end, and every other teardown stops the reader.
			for (let first = firstDone; ; first = undefined) {
				let update: SubscribeUpdate | undefined;
				if (first !== undefined) {
					if (await first) break;
					update = await SubscribeUpdate.decode(reader, version);
				} else {
					update = await SubscribeUpdate.decodeMaybe(reader, version);
					if (!update) break;
				}
				// Each lite-07 update edits the live ranges, which only the stream's history names.
				const edit = live.apply(update);
				// Lite-07 carries a priority and order per range, which rank each group by the ranges
				// wanting it (see `groupPolicy`). The subscription's as a whole still ranks a group no range
				// wants, and is what a copy asks upstream.
				if (hasRanges(version)) Object.assign(update, subscriptionPolicy(live.ranges()));
				apply(update, edit);
				// The latest one replaces any the loop has yet to take, its edit folded in.
				const pending = this.#update ?? {
					update,
					retired: [] as bigint[],
					added: new Map<bigint, WireRange>(),
					retuned: new Map<bigint, WireRange>(),
				};
				pending.update = update;
				for (const id of edit.retired) {
					pending.retuned.delete(id);
					if (!pending.added.delete(id)) pending.retired.push(id);
				}
				// A later retune of a range this added is that range added as it now reads, and one
				// of a range this retuned replaces that retune. Mirrors the Rust `Applied::then`.
				for (const [id, range] of edit.retuned) {
					if (pending.added.has(id)) pending.added.set(id, range);
					else pending.retuned.set(id, range);
				}
				for (const [id, range] of edit.added) pending.added.set(id, range);
				this.#update = pending;
				this.#changed.update((value) => value + 1);
			}
		} catch (err: unknown) {
			closeViolation(quic, err);
			this.#finish(error(err));
			return;
		}
		if (keepsSubscribeOpen(version)) {
			this.#peerFin = true;
			this.#changed.update((value) => value + 1);
			return;
		}
		this.#finish(null);
	}
}

/**
 * A subscription's requested ranges by Range ID, as the model holds them, with the bounds the read
 * cursor takes from them counted, so an edit moves them in what it costs. Mirrors the Rust
 * `Requested`.
 */
class Requested {
	#ranges = new Map<bigint, WireRange>();
	// The groups the ranges start at, and how many count back from the latest group instead.
	#starts = new Counts<bigint>();
	#unnamed = 0;
	// The exclusive groups the ranges end before, and how many have no such end.
	#ends = new Counts<bigint>();
	#unbounded = 0;

	/** The ranges a SUBSCRIBE carried, by position. */
	constructor(ranges: readonly WireRange[]) {
		for (const [id, range] of ranges.entries()) this.insert(BigInt(id), range);
	}

	get(id: bigint): WireRange | undefined {
		return this.#ranges.get(id);
	}

	/** Every live range. */
	values(): IterableIterator<WireRange> {
		return this.#ranges.values();
	}

	/**
	 * The narrowest max age in milliseconds a live join among the ranges asks, undefined when none
	 * limits it. Mirrors the Rust `Subscription::narrowest`.
	 */
	narrowest(): number | undefined {
		let narrowest: number | undefined;
		for (const range of this.#ranges.values()) {
			const max = range.start?.live?.max ?? range.start?.joined?.max;
			if (typeof max === "number") narrowest = Math.min(narrowest ?? max, max);
		}
		return narrowest;
	}

	/** Hold `range` as Range ID `id`, replacing any it held. */
	insert(id: bigint, range: WireRange): void {
		this.remove(id);
		this.#count(range, true);
		this.#ranges.set(id, range);
	}

	remove(id: bigint): void {
		const range = this.#ranges.get(id);
		if (range === undefined) return;
		this.#count(range, false);
		this.#ranges.delete(id);
	}

	#count(range: WireRange, add: boolean): void {
		const [from, to] = rangeBounds(range);
		if (from === undefined) this.#unnamed += add ? 1 : -1;
		else if (add) this.#starts.add(from);
		else this.#starts.remove(from);
		if (to === undefined) this.#unbounded += add ? 1 : -1;
		else if (add) this.#ends.add(to);
		else this.#ends.remove(to);
	}

	/** The lowest group the ranges start at, undefined unless each names its start; see {@link spanOf}. */
	start(): bigint | undefined {
		return this.#unnamed > 0 ? undefined : this.#starts.first();
	}

	/** The exclusive end past every range, undefined when one has no such end; see {@link spanOf}. */
	end(): bigint | undefined {
		return this.#unbounded > 0 ? undefined : this.#ends.last();
	}
}

/**
 * Lite-07 ranges whose time starts, live joins, or time ends may still resolve, by Range ID, each
 * due once the latest group reaches the lowest start that could resolve it. Mirrors the Rust
 * `Unpinned`.
 */
class Unpinned {
	#due = new Map<bigint, bigint>();
	#byDue = new Map<bigint, Set<bigint>>();
	#dues = new Counts<bigint>();

	/** Hold range `id` until `due`, or let it go for undefined. */
	insert(id: bigint, due: bigint | undefined): void {
		this.remove(id);
		if (due === undefined) return;
		this.#due.set(id, due);
		let ids = this.#byDue.get(due);
		if (!ids) {
			ids = new Set();
			this.#byDue.set(due, ids);
			this.#dues.add(due);
		}
		ids.add(id);
	}

	remove(id: bigint): void {
		const due = this.#due.get(id);
		if (due === undefined) return;
		this.#due.delete(id);
		const ids = this.#byDue.get(due);
		ids?.delete(id);
		if (ids?.size === 0) {
			this.#byDue.delete(due);
			this.#dues.remove(due);
		}
	}

	/** The ranges due once the latest group starts at `latest`. */
	due(latest: bigint): bigint[] {
		return this.#dues.through(latest).flatMap((due) => [...(this.#byDue.get(due) ?? [])]);
	}
}

/**
 * Extracts `(group, startFrame, endFrame)` for a lite-07 single-group Position SUBSCRIBE:
 * `[Position(G, f1), Position(G + 1, 0))` for a whole group from `f1`, or
 * `[Position(G, f1), Position(G, f2))` with `f2 > f1` for a frame slice (`endFrame` inclusive).
 */
function singleGroupPosition(
	msg: Subscribe,
	version: Version,
): { group: bigint; startFrame: number; endFrame?: number } | undefined {
	if (!keepsSubscribeOpen(version) || !msg.ranges || msg.ranges.length !== 1) return undefined;
	const range = msg.ranges[0];
	const start = range.start?.absolute;
	const end = range.end?.absolute;
	if (start === undefined || end === undefined) return undefined;
	const startFrame = range.start?.frame ?? 0;
	const endFrame = range.end?.frame ?? 0;
	if (endFrame === 0 && end === start + 1n) {
		return { group: start, startFrame };
	}
	if (end === start && endFrame > startFrame) {
		return { group: start, startFrame, endFrame: endFrame - 1 };
	}
	return undefined;
}

/**
 * The lowest latest group start that could resolve `range`'s time start, unless `started`, then
 * its time end. Undefined once neither can move. A live join is never pinned: as a time start its
 * groups would turn from live ones that expire past its max into history, and the model already
 * keeps it placed by its Range ID. Mirrors the Rust `due`.
 */
function due(range: WireRange, started: boolean): bigint | undefined {
	if (range.start?.live !== undefined || range.start?.joined !== undefined || isEmpty(requestedRange(range)))
		return undefined;
	if (!started && range.start !== undefined && range.start.absolute === undefined) return 0n;
	if (range.end?.time !== undefined) return range.end.time + 1n;
	return undefined;
}

// Whether `range` starts partway through its group: a Position start, or a placed join's.
function framed(range: WireRange): boolean {
	const start = range.start?.joined ?? range.start;
	return start?.absolute !== undefined && !!start.frame;
}

// A live join's staleness budget in milliseconds, undefined for any other range, one already placed
// included: it answers no join. Mirrors the Rust `join_budget`.
function joinBudget(range: WireRange): number | undefined {
	const live = range.start?.live;
	return live && (live.max ?? NO_MAX_DELAY);
}

// `entry`, a range this subscription already held, resolved as it was: a range resolves once, so
// an update that leaves it live never looks a time start up again in newer holdings. A live join
// stays as asked (see `due`). A start reaching back to the oldest group held stays as it was, since
// an absolute start would turn it into a floor the subscriber never named.
function pin(asked: WireRange, resolved: Resolved | undefined): WireRange {
	if (!resolved) return asked;
	const range = { ...asked };
	if (resolved.floored && range.start?.absolute === undefined) range.start = { absolute: resolved.start };
	// An end resolves once it settles; until then it is reported open.
	if (range.end?.time !== undefined && resolved.end !== undefined) range.end = { absolute: resolved.end };
	return range;
}

/** A lite-07 range still waiting for its SUBSCRIBE_START, as it was asked. Mirrors the Rust `Unstarted`. */
interface Asked {
	/** Its staleness budget in milliseconds when it starts with a live join not yet placed, which starts past the groups that budget expired. */
	joins: number | undefined;
	/** It follows the live edge, starting with a live join or ending at none, so its answer waits until this hop knows the largest position. */
	live: boolean;
	/** It starts at the oldest group, which places it at group 0 on a track with none yet. */
	oldest: boolean;
}

function asked(range: WireRange): Asked {
	const live = range.start?.live !== undefined || range.end === undefined;
	return { joins: joinBudget(range), live, oldest: range.start === undefined };
}

/**
 * What the subscribe stream has said of where delivery starts. Mirrors the Rust `Answer`, with
 * lite-07's per-range answers kept beside it as the ranges still `unstarted`.
 *
 * - `legacy`: lite-03/04 send no SUBSCRIBE_START, and so no drops either.
 * - `waiting`: lite-05/06, whose one START is the first message and names where delivery starts for
 *   good, so the `first` servable group and the edge seen before it was popped wait for the source to
 *   say where its feed starts. The groups given up meanwhile are named once the start is.
 * - `started`: lite-05/06, delivery starts at `from`.
 * - `unresolved`: lite-07, where delivery starts is not known yet. Groups are served meanwhile, the
 *   lowest at `served`, which delivery starts at or below, so a range waiting on its start holds back
 *   no other's. The groups given up meanwhile are named once delivery resolves.
 * - `withheld`: lite-07, delivery starts at `from`, but no range is answered yet, as one following an
 *   edge this hop does not know leaves it: the drops given up since go out after the first START.
 * - `answered`: lite-07, a range is answered, so drops go out as they are given up.
 */
type StartAnswer =
	| { kind: "legacy" }
	| {
			kind: "waiting";
			first?: { group: group.Consumer; largest: track.Position | undefined };
			skipped: Runs<DropRun>;
	  }
	| { kind: "started"; from: bigint }
	| { kind: "unresolved"; served?: bigint; skipped: Runs<DropRun> }
	| { kind: "withheld"; from: bigint; held: SubscribeResponse[] }
	| { kind: "answered"; from: bigint };

/** What `version` has said of where delivery starts before anything resolved it. */
function startAnswer(version: Version): StartAnswer {
	if (!hasTrackStream(version)) return { kind: "legacy" };
	const skipped = new Runs<DropRun>(sameHole);
	return hasRanges(version) ? { kind: "unresolved", skipped } : { kind: "waiting", skipped };
}

/** Where delivery starts once `answer` resolved it. */
function answerFrom(answer: StartAnswer): bigint | undefined {
	return answer.kind === "started" || answer.kind === "withheld" || answer.kind === "answered"
		? answer.from
		: undefined;
}

// The later of two locations, either of which may be unknown.
function newer(a: track.Position | undefined, b: track.Position | undefined): track.Position | undefined {
	if (!a || !b) return a ?? b;
	return b.group > a.group || (b.group === a.group && b.frame > a.frame) ? b : a;
}

// Register both readiness sources in the same turn after the caller observed neither ready.
// The winner disposes both registrations, so an idle subscription accumulates nothing no
// matter how many times it wakes.
function waitForSubscription(
	controls: SubscriptionControls,
	subscriber: track.Subscriber,
	drops: Signal<number>,
): Promise<void> {
	return armSubscription(controls, subscriber, drops).woke;
}

// `waitForSubscription`, registered now and awaited later, so a change between the two still wakes
// it. `dispose` drops a registration left unawaited.
function armSubscription(
	controls: SubscriptionControls,
	subscriber: track.Subscriber,
	drops: Signal<number>,
): { woke: Promise<void>; dispose: Dispose } {
	const dispose: Dispose[] = [];
	let settled = false;
	const settle = () => {
		if (settled) return false;
		settled = true;
		for (const close of dispose) close();
		return true;
	};
	const woke = new Promise<void>((resolve) => {
		const wake = () => {
			if (settle()) resolve();
		};
		dispose.push(controls.changed(wake), hooks.groupChanged(subscriber, wake));
		dispose.push(drops.changed(wake));
	});
	return { woke, dispose: () => void settle() };
}

/** Resolves once the subscriber's next control (an update, its FIN, or a failure) arrives. */
function waitForControl(controls: SubscriptionControls): Promise<void> {
	return new Promise((resolve) => {
		const dispose = controls.changed(() => {
			dispose();
			resolve();
		});
	});
}

/**
 * The floor of a subscription's read cursor for the wire serving it.
 *
 * Lite-07 has none: its ranges name the group containing a time start, which a floor at the time
 * would cut. Before lite-07 a start names the first group starting at or after it, so the floor
 * is the start the subscription named (or 0 on lite-06), and its max delay decides what above the
 * floor is worth delivering. Mirrors the Rust `Frames::floor`.
 *
 * Pre-06 wires are the exception: their drafts define an absent `Group Start` as the
 * latest group, so say so explicitly rather than letting the budget reach back. Lite-03/04/05
 * carry a `Subscriber Max Age`, but there it is a staleness tolerance only; lite-01/02 additionally get
 * an unbounded budget so nothing is dropped under them (they carry no max age, which decodes as none), which
 * must not read as a request to replay the whole cache on join.
 */
function cursorFloor(version: Version, startGroup: bigint | undefined, latest: bigint | undefined): bigint {
	if (hasRanges(version)) return 0n;
	if (startGroup !== undefined) return startGroup;
	if (resolvesStart(version)) return 0n;
	return latest ?? 0n;
}

/**
 * Handles publishing broadcasts and managing their lifecycle.
 *
 * @internal
 */
export class Publisher {
	#withdrawal = new Withdrawal();

	// Served SUBSCRIBE, FETCH, and TRACK requests, from dispatch until they end, which a
	// draining close waits for.
	#owed = new Set<Promise<void>>();

	// The version of the connection.
	readonly version: Version;

	// Per-connection origin appended to outbound Announce hops, so the peer
	// can detect loops and prefer shorter paths. Created by Connection and
	// shared with Subscriber, which can optionally use it to filter out its
	// own announcements.
	readonly hop: Hop;

	#quic: WebTransport;

	// Select the transport's current or legacy datagram send API once at setup.
	#datagramWriter?: ReturnType<typeof DatagramStream.datagramWriter>;

	// Originated advertisements this session forwards.
	#advertised: Getter<Advertisements | undefined>;

	#publish?: OriginConsumer;

	// Our grant: only what it lets us publish is announced and served, and a shrink
	// withdraws what it no longer covers. Undefined until the peer answers, and forever on a
	// version without AUTH, which allows everything.
	#grant?: Getter<Grant | undefined>;
	// Holds every incoming request to that grant from dispatch to its end.
	#gate: Gate;

	// Resolves once the tokens this session presented at setup are answered, so nothing is
	// announced before the grant it would be checked against.
	#ready: Promise<void>;

	// Lite-07: answers the prefixes the peer asks for, and its loop once the peer asked.
	#answer?: Answer;
	#answering?: Promise<void>;

	// TRACK_INFO is immutable per track, so resolve it from the application once
	// (via a throwaway subscribe whose info() resolves when the app calls accept)
	// and reuse it for every later TRACK request of the same track. Keyed by the
	// routing front rather than the path: immutability holds for one broadcast, and a
	// republish puts a different one on the path, so its entries must not be reused.
	// A rejected lookup is evicted so a retry can re-probe.
	#trackInfo = new WeakMap<broadcast.Consumer, Map<string, TrackInfoEntry>>();

	// Lite-05/06: the datagram tracks this session's answered TRACKs hold, read from each
	// answer on, oldest first. The requester reads TRACK_INFO as the publisher accepting the
	// name, but the datagrams flow on the SUBSCRIBE it sends alongside, which can arrive later.
	// That SUBSCRIBE reads from a clone of the hold, keeping its position, rather than from its
	// own arrival, which would lose what was written in between. The requester keeps its TRACK
	// open until the SUBSCRIBE has a response, so the hold outlasts its arrival. A hold is
	// matched by the track it reads, not by name, so one left on a replaced track never stands
	// in for the current one. It is a passive observer: it keeps a position without adding
	// demand, which the SUBSCRIBE adds when it reads the track.
	#heldDatagrams = new Set<datagram.Consumer>();

	/**
	 * Creates a new Publisher instance.
	 * @param quic - The WebTransport session to use
	 * @param version - Negotiated protocol version
	 * @param origin - Hop id shared with the Subscriber
	 * @param publish - The origin whose broadcasts this session serves; omit to publish nothing
	 * @param auth - The union of our tokens' grants, which bounds what we publish, and when the
	 *   setup tokens are answered
	 *
	 * @internal
	 */
	constructor(
		quic: WebTransport,
		version: Version,
		hop: Hop,
		publish?: OriginConsumer,
		auth?: { grant: Getter<Grant | undefined>; ready: Promise<void> },
	) {
		this.#quic = quic;
		this.#grant = auth?.grant;
		this.#gate = new Gate(auth?.grant);
		this.#ready = auth?.ready ?? Promise.resolve();
		this.version = version;
		this.hop = hop;
		const origin = publish && wireOf(publish);
		this.#advertised = origin?.advertised ?? new Signal(new Map());
		this.#publish = publish;
		if (hasRoutes(version)) {
			this.#answer = new Answer({
				version,
				advertised: this.#advertised,
				origin,
				grant: this.#grant,
				ready: this.#ready,
				closing: this.#withdrawal.closing,
				stream: AnnounceStream.of(quic, version),
			});
		}

		// Grab the datagram writer up front when the transport carries datagrams (no group
		// fallback, so it stays undefined otherwise). One writer for all subscriptions.
		if (hasDatagrams(version)) {
			this.#datagramWriter = DatagramStream.datagramWriter(quic);
		}
	}

	/**
	 * Whether the peer's edit continues in its next message, which must be its next ANNOUNCE_REQUEST.
	 *
	 * @internal
	 */
	continuing(): boolean {
		return this.#answer?.continuing() ?? false;
	}

	/**
	 * Lite-07 ANNOUNCE_REQUEST: the peer edits the prefixes it asks for, answered on this
	 * session's Announce Stream. Throws a {@link ProtocolViolation} for an edit the peer may not send.
	 *
	 * @internal
	 */
	request(more: boolean, remove: bigint[], add: Add[]): void {
		if (!this.#answer) throw new ProtocolViolation("ANNOUNCE_REQUEST before lite-07");
		const started = this.#answering === undefined;
		const answering = this.#answer.request(more, remove, add);
		if (!started) return;
		this.#answering = this.#withdrawal.track(answering);
		// The session is ending under it, or its Announce Stream broke, which closes it: its close
		// says why.
		this.#answering.catch((err: unknown) => console.debug("announce stream failed:", err));
	}

	/**
	 * Handles a pre-lite-07 announce interest message.
	 * @param msg - The announce interest message
	 * @param stream - The stream to write announcements to
	 *
	 * @internal
	 */
	runAnnounce(msg: AnnounceRequest, stream: Stream): Promise<void> {
		return this.#withdrawal.track(this.#runAnnounce(msg, stream));
	}

	async #runAnnounce(msg: AnnounceRequest, stream: Stream) {
		if (this.#withdrawal.closing.peek()) return;
		console.debug(`announce: prefix=${msg.prefix}`);

		// Keyed by suffix, valued by ref plus route, so a republish diffs as a restart
		// and a re-price as an update.
		let active = new Map<Path.Valid, Advertised>();

		// Lite06+: announce ids. Every active we send implicitly assigns the next per-stream
		// ordinal; ended/update/restart reference the id instead of repeating the path.
		let nextAnnounceId = 0n;
		const announceIds = new Map<Path.Valid, bigint>();

		const wireHops = (route: WireRoute): Hop[] => {
			if (hasAnnounceOk(this.version)) return [...route.hops];
			return [...route.hops, this.hop];
		};

		// What the peer decodes for a route: pre-lite-06 wires carry no cost, so a re-price
		// there must not restart.
		// moq-lite carries no per-request auth, so a swap alone sends nothing.
		const onWire = (route: WireRoute): WireRoute => ({
			...route,
			cost: hasRouteCost(this.version) ? route.cost : Cost.zero,
			auth: undefined,
		});

		// Another identity is another stream, which these versions announce as an end and a start.
		const restarted = (prev: Advertised, next: Advertised): boolean => !sameIdentity(prev, next);

		const announce = async (suffix: Path.Valid, route: WireRoute) => {
			console.debug(`announce: broadcast=${suffix} active=true identity=${route.identity}`);
			if (hasAnnounceId(this.version)) announceIds.set(suffix, nextAnnounceId++);
			await encodeAnnounceBroadcast(
				stream.writer,
				{ status: "active", suffix, hops: wireHops(route), cost: route.cost },
				this.version,
			);
		};

		const update = async (suffix: Path.Valid, route: WireRoute) => {
			if (!hasAnnounceId(this.version)) {
				await retract(suffix);
				await announce(suffix, route);
				return;
			}
			const id = announceIds.get(suffix);
			if (id === undefined) {
				await announce(suffix, route);
				return;
			}
			console.debug(`announce: broadcast=${suffix} update=true`);
			await encodeAnnounceBroadcast(
				stream.writer,
				{ status: "update", id, hops: wireHops(route), cost: route.cost },
				this.version,
			);
		};

		// Another identity: an end and a start.
		const restart = async (suffix: Path.Valid, route: WireRoute) => {
			await retract(suffix);
			await announce(suffix, route);
		};

		// Lite06+ retracts by announce id; older versions repeat the path (ended announces
		// don't need hops).
		const retract = async (suffix: Path.Valid) => {
			console.debug(`announce: broadcast=${suffix} active=false`);
			if (!hasAnnounceId(this.version)) {
				await encodeAnnounceBroadcast(stream.writer, { status: "ended", suffix }, this.version);
				return;
			}

			const id = announceIds.get(suffix);
			announceIds.delete(suffix);
			if (id === undefined) return; // never announced
			await encodeAnnounceBroadcast(stream.writer, { status: "endedId", id }, this.version);
		};

		// A hidden route stays off the wire: only a lite-07 request can opt in.
		const carries = (covered: Path.Valid) => !hiddenBelow(msg.prefix, covered);

		// What the peer currently sees: the table under the prefix, less whatever our grant
		// does not let us publish.
		const visible = (table: Advertisements): Map<Path.Valid, Advertised> => {
			const out = presented(msg.prefix, table, carries, { root: this.#publish && wireOf(this.#publish).root });
			const grant = this.#grant?.peek();
			if (grant) {
				for (const suffix of [...out.keys()]) {
					if (!grant.publish.matches(Path.join(msg.prefix, suffix))) out.delete(suffix);
				}
			}
			return out;
		};

		// Subscribe BEFORE writing anything: every encode below awaits the wire, and a publish
		// landing in that window only notifies the listeners already registered. One created
		// afterwards would sleep through it, leaving the change unannounced until something
		// unrelated moved. A grant change re-diffs the same way, withdrawing what it no longer
		// covers and announcing what it now does.
		const arm = () => nextChange(this.#advertised, this.#grant);
		let { changed, dispose } = arm();

		try {
			// Nothing is announced before the grant it would be checked against. A local close
			// meanwhile finishes the stream with nothing announced.
			const ready = this.#ready.then(() => "ready" as const);
			if ((await race([ready, stream.reader.closed, this.#withdrawal.closing])) !== "ready") {
				if (this.#withdrawal.closing.peek()) {
					stream.close();
					await stream.writer.closed;
				}
				return;
			}

			const initial = this.#advertised.peek();
			if (!initial) return; // closed

			for (const [name, snap] of visible(initial)) {
				active.set(name, snap);
			}

			switch (this.version) {
				case Version.DRAFT_01:
				case Version.DRAFT_02: {
					for (const suffix of active.keys()) {
						console.debug(`announce: broadcast=${suffix} active=true`);
					}
					const init = new AnnounceInit([...active.keys()]);
					await init.encode(stream.writer, this.version);
					break;
				}
				default: {
					if (!hasAnnounceOk(this.version)) {
						for (const [suffix, snap] of active) {
							await announce(suffix, snap.route);
						}
						break;
					}

					const ok = new AnnounceOk(this.hop, active.size);
					await ok.encode(stream.writer, this.version);
					for (const [suffix, snap] of active) {
						await announce(suffix, snap.route);
					}
					break;
				}
			}

			for (;;) {
				const woke = await race([changed, stream.reader.closed, this.#withdrawal.closing]);
				dispose();
				if (woke !== "changed") break;

				// Re-arm before reading, so an advertise that lands while we write is not lost.
				({ changed, dispose } = arm());

				const latest = this.#advertised.peek();
				if (!latest) break;

				const updated = visible(latest);

				for (const suffix of active.keys()) {
					if (!updated.has(suffix)) await retract(suffix);
				}
				for (const [suffix, snap] of updated) {
					const prev = active.get(suffix);
					if (!prev) {
						await announce(suffix, snap.route);
					} else if (restarted(prev, snap)) {
						await restart(suffix, snap.route);
					} else if (!routesEqual(onWire(prev.route), onWire(snap.route))) {
						await update(suffix, snap.route);
					}
				}

				active = updated;
			}
			if (this.#withdrawal.closing.peek()) {
				for (const suffix of active.keys()) await retract(suffix);
				stream.close();
				await stream.writer.closed;
			}
		} finally {
			dispose();
		}
	}

	/** The broadcast a request names, from this session's origin. */
	async #resolve(
		msg: { broadcast: Path.Valid; identity?: Identity.Valid },
		signal: AbortSignal,
	): Promise<broadcast.Consumer | undefined> {
		const origin = this.#publish && wireOf(this.#publish);
		if (!origin) return undefined;
		return origin.local(msg.broadcast, msg.identity) ?? (await origin.demand(msg.broadcast, msg.identity, signal));
	}

	/**
	 * Handles a subscribe message.
	 * @param msg - The subscribe message
	 * @param stream - The stream to write track data to
	 *
	 * @internal
	 */
	async runSubscribe(msg: Subscribe, stream: Stream) {
		// Serve only what our grant lets us publish, and stop once it no longer does. The
		// watch is armed before the first check and held to the end, so a shrink while the
		// broadcast resolves is never missed.
		const revoked = unauthorized(msg.broadcast);
		let serving: track.Subscriber | undefined;
		const watch = this.#gate.watch(msg.broadcast, () => {
			console.debug(`publish revoked: broadcast=${msg.broadcast} track=${msg.track}`);
			serving?.close(revoked);
			stream.abort(revoked);
		});
		try {
			await this.#serveSubscribe(msg, stream, watch, (track) => {
				serving = track;
			});
		} finally {
			watch.dispose();
		}
	}

	async #serveSubscribe(
		msg: Subscribe,
		stream: Stream,
		watch: { revoked: () => boolean },
		serving: (track: track.Subscriber) => void,
	) {
		// Checked before resolving, so a denied request never reaches the origin.
		if (watch.revoked()) {
			stream.writer.reset(unauthorized(msg.broadcast));
			return;
		}

		// The requester leaving while the broadcast and track resolve ends the request, as
		// Rust polls the requester throughout, so a cancelled subscription gives back its
		// charge instead of holding it until the track is answered, which may be never. A
		// subscriber's close (STOP_SENDING) and its abort (RESET_STREAM) both land here.
		const left = new AbortController();
		const cancel = new StreamError(StreamCode.Cancel, { message: "cancel" });
		const leave = () => left.abort(cancel);
		// On lite-07, clean subscriber FIN means "no further updates; deliver what was requested",
		// so only STOP_SENDING (writer.closed) or RESET_STREAM (reader.closed rejection) cancels.
		if (keepsSubscribeOpen(this.version)) {
			void stream.writer.closed.then(leave, leave);
			void stream.reader.closed.then(undefined, leave);
		} else {
			void race([stream.reader.closed, stream.writer.closed]).then(leave, leave);
		}

		let front: broadcast.Consumer | undefined;
		try {
			front = await this.#resolve(msg, left.signal);
		} catch (err: unknown) {
			stream.writer.reset(error(err));
			return;
		}
		// Revoked while the broadcast resolved: the watch already reset the stream.
		if (watch.revoked()) return;
		if (!front) {
			console.debug(`publish unknown: broadcast=${msg.broadcast}`);
			stream.writer.reset(new NotFound(`broadcast ${msg.broadcast}`));
			return;
		}

		// Lite-07 one-shot single-group Position SUBSCRIBE + immediate subscriber FIN:
		// serve via `fetchGroup` on the internal model while speaking standard lite-07
		// SUBSCRIBE responses on the wire (1 RTT, no live subscription created).
		let firstDone: Promise<boolean> | undefined;
		const oneShot = singleGroupPosition(msg, this.version);
		if (oneShot) {
			firstDone = stream.reader.done();
			const fin = await race([
				firstDone.then(
					(done) => done,
					() => false,
				),
				yieldNow().then(() => undefined),
			]);
			if (fin === true) {
				await this.#serveOneShotFetch(front, msg, oneShot, stream, watch, left.signal);
				return;
			}
		}

		const consumer = front.track(msg.track);
		// The live joins already placed, which start where they were placed.
		const absent = legacyStart(this.version, msg.maxDelay);
		const placed = requestedPlaced(msg, absent);
		const subscription = (ranges: track.Range[]): track.Subscription => {
			const out: track.Subscription = { priority: msg.priority, order: msg.order, ranges };
			if (placed.size > 0) out.placed = placed;
			return out;
		};
		let ranges: track.Range[];
		try {
			ranges = requestedRanges(msg, absent);
		} catch (err: unknown) {
			stream.writer.reset(error(err));
			return;
		}
		// Parked past a bounded window's end: the peer reads this stream's end as the track's.
		const track = hooks.parked(consumer.subscribe(subscription(ranges)));
		serving(track);
		if (watch.revoked()) {
			track.close();
			return;
		}
		const latest = hooks.latestKey(track);
		let floor = cursorFloor(this.version, msg.startGroup, latest);
		// The ranges cap what is served; the floor only pins where an older wire starts.
		track.withRange({ start: { included: floor } });

		// An old subscription carries both kinds until its stream track ends.
		let datagrams = Promise.resolve();
		const bounds: FrameBounds = {
			startGroup: msg.startGroup,
			startFrame: msg.startFrame,
			endGroup: msg.endGroup,
			endFrame: msg.endFrame,
			starts: hasRanges(this.version) ? positionFrames(msg.ranges ?? []) : new Map(),
			startsAfter: new WeakMap(),
			stops: new Signal<ReadonlyMap<bigint, number>>(
				hasRanges(this.version) ? positionStops(msg.ranges ?? []) : new Map(),
			),
			stopsAfter: new WeakMap(),
		};
		// A group is served whole from the frame asked: the reader fetches what its copy lacks.
		hooks.wholeGroups(track, (key) => frameRange(bounds, key).start);
		let legacy: datagram.Consumer | undefined;
		const legacyPriority = new Signal(msg.priority);
		let controls: SubscriptionControls | undefined;
		// Lite-05 and later name every key below the end that this subscription never
		// got with SUBSCRIBE_DROP, so the subscriber settles without waiting out its grace
		// for a group that will not come. A gap that never arrived stays until the end sweep
		// names it: this side cannot tell when the subscriber stopped waiting for it.
		const named = hasTrackStream(this.version);

		try {
			// Every draft waits for the track's info, as Rust does: it surfaces a rejected track
			// (accept never called, track closed) as an error here, which resets the stream, and
			// its priority breaks ties between this session's subscriptions at one subscriber
			// priority.
			let info: track.Info;
			try {
				if (oneShot && firstDone !== undefined) {
					const earlyFin = new Promise<"one-shot">((resolve) => {
						void firstDone.then(
							(done) => {
								if (done) resolve("one-shot");
							},
							() => {},
						);
					});
					const resolved = await race([untilAborted(track.info(), left.signal), earlyFin]);
					if (resolved === "one-shot") {
						track.close();
						await this.#serveOneShotFetch(front, msg, oneShot, stream, watch, left.signal);
						return;
					}
					info = resolved;
				} else {
					info = await untilAborted(track.info(), left.signal);
				}
			} catch (err) {
				if (
					hasDatagrams(this.version) &&
					!hasDatagramTracks(this.version) &&
					toStreamCode(error(err)) === StreamCode.NotFound
				) {
					track.close();
					await this.#serveLegacyDatagrams(front, msg, stream, left.signal);
					return;
				}
				throw err;
			}
			const publisher = info.priority;
			// A SUBSCRIBE can arrive before its track resolves, such as one a relay serves from a
			// copy still fetching upstream, or on lite-05 one sent alongside its TRACK: the newest
			// group known once it resolves stands in, including a largest the upstream's answer
			// reported, whether or not the copy already held a group. A newer group the answer
			// brings later is not waited for. Lite-01/02 keep their arrival floor, as Rust does.
			if (this.version !== Version.DRAFT_01 && this.version !== Version.DRAFT_02) {
				let newest = latest;
				for (const known of [hooks.latestKey(track), hooks.peekLargest(track).largest?.group]) {
					if (known !== undefined) newest = newest === undefined ? known : max(newest, known);
				}
				const resolved = cursorFloor(this.version, msg.startGroup, newest);
				if (resolved !== floor) {
					floor = resolved;
					track.withRange({ start: { included: floor } });
				}
			}
			// Older drafts carry no timestamps.
			let stamp: Stamp | undefined;
			let compress = false;

			if (hasTrackStream(this.version)) {
				// Lite-05+ accepts implicitly: no SUBSCRIBE_OK (the immutable
				// properties live in TRACK_INFO), and the resolved range arrives as
				// SUBSCRIBE_START / SUBSCRIBE_END emitted from #runTrack.
				//
				// The timescale is an immutable property, so serving MUST use exactly
				// what TRACK_INFO advertised. It comes from the producer's accept(), so
				// they always agree.
				stamp = stamping(this.version, info);
				compress = hasCompression(this.version) && info.compression === "deflate";
			} else {
				// Older drafts acknowledge with SUBSCRIBE_OK and stream frames verbatim.
				const ok = new SubscribeOk({
					priority: msg.priority,
					maxDelay: msg.maxDelay ?? 0,
					startGroup: msg.startGroup,
					endGroup: msg.endGroup,
				});
				await encodeSubscribeResponse(stream.writer, { ok }, this.version);
			}

			console.debug(`publish ok: broadcast=${msg.broadcast} track=${track.name}`);

			// Serve datagrams concurrently with groups whenever the transport carries them
			// (the writer exists iff so). No group fallback: otherwise they simply aren't sent.
			if (this.#datagramWriter && !hasDatagramTracks(this.version)) {
				const live = wireOf(front).liveDatagrams(msg.track);
				legacy = live && this.#adopt(live);
				if (legacy) {
					if (wireOf(front).localDatagrams) legacy.setPriority(msg.priority);
					datagrams = this.#runDatagrams(legacy, { sub: msg.id, stamp, priority: legacyPriority })
						.then(() => {})
						.catch((err) => console.debug(`datagram send stopped: ${reason(err)}`));
				}
			}

			// The SUBSCRIBE's ranges, whose positions are their Range IDs, which key them in the model.
			const asked = rangesOf(msg);
			// The ranges as the model holds them, pinned, by Range ID, with the read cursor's bounds.
			const requested = new Requested(asked);
			// Lite-07: the Range IDs of the live ranges that start partway through a group, so an
			// edit recomputes the frame starts only while one is live.
			const positions = new Set<bigint>();
			for (const [id, range] of asked.entries()) if (framed(range)) positions.add(BigInt(id));
			// Lite-07: the Range IDs of the live ranges that end partway through a group, likewise.
			const stopping = new Set<bigint>();
			for (const [id, range] of asked.entries())
				if (range.end?.absolute !== undefined && range.end.frame) stopping.add(BigInt(id));
			// Lite-07: the ranges whose starts or time ends may still resolve.
			const unpinned = new Unpinned();
			if (hasRanges(this.version))
				for (const [id, range] of asked.entries()) unpinned.insert(BigInt(id), due(range, false));
			// Each live range due once the latest group reached where it may resolve, with its start and
			// end replaced by the groups they resolved to: a range resolves once, so an update that
			// leaves it live never places a live join again against a newer live edge, nor looks a time
			// start up again in newer holdings, here or on a relay's copies. Only the ranges still
			// waiting are asked, and each is let go once its anchors settle, so this costs those ranges
			// rather than every range. Mirrors the Rust `TrackRun::pin`.
			const pinDue = (): [bigint, WireRange][] => {
				const latest = hooks.latestKey(track);
				if (latest === undefined) return [];
				const pins: [bigint, WireRange][] = [];
				const asking: [bigint, WireRange][] = [];
				for (const id of unpinned.due(latest)) {
					const range = requested.get(id);
					if (range === undefined) unpinned.remove(id);
					else asking.push([id, range]);
				}
				const resolution = hooks.servingResolvedKeys(
					track,
					asking.map(([id]) => id),
				);
				for (const [index, [id, range]] of asking.entries()) {
					const resolved = resolution[index];
					if (!resolved) continue;
					const pinned = pin(range, resolved);
					// The start settled; the end is due again once it may settle.
					unpinned.insert(id, resolved.end === undefined ? due(pinned, true) : undefined);
					// One pinned to the groups it already names holds the same model range.
					if (rangeKey(requestedRange(pinned)) === rangeKey(requestedRange(range)))
						requested.insert(id, pinned);
					else pins.push([id, pinned]);
				}
				return pins;
			};
			controls = new SubscriptionControls({
				quic: this.#quic,
				reader: stream.reader,
				writer: stream.writer,
				version: this.version,
				live: new LiveRanges(this.version, asked),
				firstDone,
				apply: (update, { retired, added, retuned }) => {
					legacyPriority.set(update.priority);
					if (wireOf(front).localDatagrams) legacy?.setPriority(update.priority);
					// Every update restates the wanted group. Past our largest it is a refresh, which
					// repeats the pending ask; at or below it the group already exists.
					const largest = hooks.peekLargest(track).largest?.group;
					if (update.wantedGroup !== undefined && (largest === undefined || update.wantedGroup > largest))
						hooks.askSubscriber(track, "refresh");
					if (!hasRanges(this.version)) {
						// An older update restates its one span, replacing every range: an absent start is
						// placed afresh within the new max age rather than retuned where the old one joined.
						const absent = legacyStart(this.version, update.maxDelay);
						const restated: track.Subscription = {
							priority: update.priority,
							order: update.order,
							ranges: requestedRanges(update, absent),
						};
						const placed = requestedPlaced(update, absent);
						if (placed.size > 0) restated.placed = placed;
						hooks.restateSubscriber(track, restated);
						return;
					}
					// Lite-07 hands the model the edit by Range ID, so it costs the ranges it names
					// whatever else is live. A pinned or retuned range is replaced in place, keeping its
					// Range ID: a live join is never pinned (see `due`), so a retune replaces it as it
					// stands, and the model keeps it placed where it was.
					for (const id of retired) unpinned.remove(id);
					const pins = pinDue();
					const replaced = [...pins, ...retuned];
					for (const id of retired) requested.remove(id);
					for (const [id, range] of [...replaced, ...added]) requested.insert(id, range);
					for (const [id, range] of added) unpinned.insert(id, due(range, false));
					for (const id of retired) positions.delete(id);
					for (const [id, range] of added) if (framed(range)) positions.add(id);
					bounds.startsAfter.set(
						update,
						positions.size > 0 ? positionFrames([...requested.values()]) : new Map(),
					);
					for (const id of retired) stopping.delete(id);
					for (const [id, range] of added)
						if (range.end?.absolute !== undefined && range.end.frame) stopping.add(id);
					bounds.stopsAfter.set(
						update,
						stopping.size > 0 ? positionStops([...requested.values()]) : new Map(),
					);
					// Refused before the model moves, as one SUBSCRIBE asking for nothing is.
					Object.assign(update, spanBetween(this.version, requested.start(), requested.end()));
					const add = [...replaced, ...added].map(([id, range]) => [id, requestedRange(range)] as const);
					// A placed join or exact-group Position range stays placed where it was.
					const placed = new Map<bigint, bigint>();
					for (const [id, range] of [...replaced, ...added]) {
						const at = range.start?.joined?.absolute ?? range.start?.absolute;
						if (at !== undefined) placed.set(id, at);
					}
					hooks.editSubscriber(track, {
						retire: [...retired, ...replaced.map(([id]) => id)],
						add,
						placed,
						priority: update.priority,
						order: update.order,
					});
				},
			});
			const result = await this.#runTrack(track, stream.writer, controls, {
				sub: msg.id,
				broadcast: msg.broadcast,
				stamp,
				publisher,
				compress,
				named,
				floor,
				bounds,
				initial: BigInt(asked.length),
				requested,
				oneShot: oneShot !== undefined,
			});
			if (result === "one-shot" && oneShot) {
				track.close();
				legacy?.close();
				await Promise.all([datagrams, controls.decoding]);
				await this.#serveOneShotFetch(front, msg, oneShot, stream, watch, left.signal);
				return;
			}
			const finished = result === true;

			console.debug(`publish done: broadcast=${msg.broadcast} track=${track.name}`);
			// A lite-05+ subscriber FINs once it has read the tail, so leave its half open and
			// wait for that: a receiver may drop unread bytes on close. Older versions stop it
			// here, which ends the decoder.
			if (waitsForSubscriberFin(this.version)) stream.writer.close();
			else stream.close();
			// Ends both kinds carried by this old subscription.
			track.close();
			legacy?.close();
			// A subscriber that left is owed nothing more, and its stream may already be reset.
			await Promise.all([datagrams, controls.decoding, finished && acknowledged(stream.writer)]);
		} catch (err: unknown) {
			closeViolation(this.#quic, err);
			const e = error(err);
			if (e === cancel) console.debug(`publish cancelled: broadcast=${msg.broadcast} track=${track.name}`);
			else console.warn(`publish error: broadcast=${msg.broadcast} track=${track.name} error=${reason(e)}`);
			track.close(e);
			legacy?.close(e);
			stream.abort(e);
			await Promise.all([datagrams, controls?.decoding]);
		}
	}

	/**
	 * Serves a lite-07 one-shot single-group Position SUBSCRIBE + immediate subscriber FIN via
	 * `fetchGroup` on the internal model while speaking standard lite-07 SUBSCRIBE on the wire.
	 */
	async #serveOneShotFetch(
		front: broadcast.Consumer,
		msg: Subscribe,
		req: { group: bigint; startFrame: number; endFrame?: number },
		stream: Stream,
		watch: { revoked: () => boolean },
		signal: AbortSignal,
	) {
		const hold = new AbortController();
		const holding = AbortSignal.any([signal, hold.signal]);
		let fetched: group.Consumer | undefined;
		let released = false;
		let abandon: Error | undefined;
		const releaseFetched = (reason: Error) => {
			if (fetched && !released) {
				released = true;
				hooks.releaseGroupReader(fetched, reason);
			}
		};
		let givenUp: GivenUp | undefined;
		const onAbort = () => {
			abandon ??= error(signal.reason ?? new StreamError(StreamCode.Cancel));
			releaseFetched(abandon);
		};
		signal.addEventListener("abort", onAbort, { once: true });
		try {
			// Start info lookup and fetch concurrently so a pipelined one-shot SUBSCRIBE
			// reaches upstream before TRACK_INFO resolves, while a refused track still resets.
			const infoPromise = this.#resolveTrackInfo(front, msg.track, holding, true);
			const options: track.FetchGroupOptions & broadcast.FetchFrom = {
				priority: msg.priority,
				signal,
				frameStart: req.startFrame,
				held: true,
			};
			const groupPromise = front.track(msg.track).fetchGroup(req.group, options);
			// Track the resolved group immediately so a failure, revoke, or abort before
			// `await groupPromise` still releases it without unhandled rejections.
			groupPromise.then(
				(group) => {
					fetched = group;
					if (abandon) releaseFetched(abandon);
				},
				() => {},
			);

			let info: TrackInfoMessage;
			try {
				info = await infoPromise;
			} finally {
				hold.abort();
			}
			if (watch.revoked()) {
				abandon ??= unauthorized(msg.broadcast);
				releaseFetched(abandon);
				return;
			}

			try {
				fetched = await groupPromise;
			} catch (err: unknown) {
				const e = error(err);
				if (signal.aborted || watch.revoked()) throw e;
				await encodeSubscribeResponse(
					stream.writer,
					{ start: new SubscribeStart({ id: 0n, start: req.group, largest: "unknown" }) },
					this.version,
				);
				await encodeSubscribeResponse(
					stream.writer,
					{ drop: SubscribeDrop.given(req.group, req.group + 1n, e, this.version) },
					this.version,
				);
				stream.writer.close();
				await acknowledged(stream.writer);
				return;
			}

			if (watch.revoked() || signal.aborted) {
				abandon ??= error(signal.reason ?? unauthorized(msg.broadcast));
				releaseFetched(abandon);
				return;
			}

			await encodeSubscribeResponse(
				stream.writer,
				{ start: new SubscribeStart({ id: 0n, start: req.group, largest: "unknown" }) },
				this.version,
			);

			const order = sendOrder({ priority: msg.priority, publisher: info.priority });
			const priority: Pick<Priority, "reserve" | "add" | "remove"> = {
				reserve: () => ({ rank: () => order, release: () => {} }),
				add: (w) => w.setPriority(order),
				remove: () => {},
			};
			const unsubscribed = signal.aborted
				? Promise.resolve()
				: new Promise<void>((resolve) => {
						signal.addEventListener("abort", () => resolve(), { once: true });
					});
			const pendingCuts: Run[] = [];
			givenUp = new GivenUp(fetched);
			const runOptions: RunGroup = {
				sub: msg.id,
				group: fetched,
				givenUp,
				stamp: stamping(this.version, info),
				compress: info.compress,
				priority,
				unsubscribed,
				start: req.startFrame,
				end: req.endFrame,
				cut: (start, end) => pendingCuts.push({ start, end }),
			};
			const writer = await this.#openGroup(runOptions);
			let failed: Error | undefined;
			try {
				failed = writer instanceof Error ? writer : await this.#serveGroup(writer, runOptions);
			} finally {
				givenUp.close();
			}
			if (watch.revoked() || signal.aborted) return;

			for (const { start, end } of pendingCuts) {
				await encodeSubscribeResponse(
					stream.writer,
					{ drop: SubscribeDrop.given(start, end, new StreamError(StreamCode.Skip), this.version) },
					this.version,
				);
			}
			if (failed) {
				const end = max(fetched.end ?? fetched.start, fetched.start + 1n);
				await encodeSubscribeResponse(
					stream.writer,
					{ drop: SubscribeDrop.given(fetched.start, end, failed, this.version) },
					this.version,
				);
			}
			stream.writer.close();
			await acknowledged(stream.writer);
		} catch (err: unknown) {
			closeViolation(this.#quic, err);
			const e = error(err);
			abandon ??= e;
			releaseFetched(e);
			stream.abort(e);
		} finally {
			signal.removeEventListener("abort", onAbort);
			hold.abort();
		}
	}

	/**
	 * Handles a FETCH stream by serving one group as bare frame records (lite-05+).
	 *
	 * @internal
	 */
	async runFetch(msg: Fetch, stream: Stream) {
		if (!hasTrackStream(this.version)) {
			stream.writer.reset(new Error("fetch requires moq-lite-05 or newer"));
			return;
		}
		// Like a subscription, the fetch holds its watch until the last frame: a group can stay
		// open as long as its track, so a check at accept alone would keep serving after a shrink.
		const revoked = unauthorized(msg.broadcast);
		let fetched: group.Consumer | undefined;
		const watch = this.#gate.watch(msg.broadcast, () => {
			console.debug(`fetch revoked: broadcast=${msg.broadcast} track=${msg.track} group=${msg.group}`);
			if (fetched) hooks.releaseGroupReader(fetched, revoked);
			stream.abort(revoked);
		});
		try {
			await this.#serveFetch(msg, stream, watch, (group) => {
				fetched = group;
			});
		} finally {
			watch.dispose();
		}
	}

	async #serveFetch(
		msg: Fetch,
		stream: Stream,
		watch: { revoked: () => boolean },
		serving: (group: group.Consumer) => void,
	) {
		if (watch.revoked()) {
			stream.writer.reset(unauthorized(msg.broadcast));
			return;
		}

		// A normal request FIN is not a cancellation: FETCH has no more request bytes.
		const left = new AbortController();
		void stream.reader.closed.catch(() => left.abort());
		void stream.writer.closed.catch(() => left.abort());
		// The lookups hold the broadcast and track only until the track info answers.
		const hold = new AbortController();
		const holding = AbortSignal.any([left.signal, hold.signal]);
		try {
			let front: broadcast.Consumer | undefined;
			try {
				front = await this.#resolve(msg, holding);
			} catch (err: unknown) {
				stream.writer.reset(error(err));
				return;
			}
			if (!front) {
				console.debug(`fetch unknown: broadcast=${msg.broadcast}`);
				stream.writer.reset(new NotFound(`broadcast ${msg.broadcast}`));
				return;
			}
			// Revoked while the broadcast resolved: the watch already reset the stream.
			if (watch.revoked()) return;

			// The subscriber opened this stream, so its send order only ranked the request. Rank the
			// response here, on the same scale as the group streams it competes with.
			stream.writer.setPriority(sendOrder({ priority: msg.priority }));

			// A FETCH requester leaves early by resetting its stream or losing its session, which
			// lets go of a lookup still waiting on the answer.
			let group: group.Consumer | undefined;
			let givenUp: GivenUp | undefined;
			try {
				// The timescale is immutable, so serve exactly what TRACK_INFO advertised. Both
				// come off the same front, so the metadata and the frames are one generation.
				let info: TrackInfoMessage;
				try {
					info = await this.#resolveTrackInfo(front, msg.track, holding, true);
				} finally {
					hold.abort();
				}
				// A group the track does not cache goes to its request handler at once, written yet
				// or not, as in Rust; with none open it is NotFound. A pending answer holds this
				// FETCH's slot until the requester leaves.
				const options: track.FetchGroupOptions & broadcast.FetchFrom = {
					priority: msg.priority,
					signal: left.signal,
					// A copy holding the frames asked on serves them; one missing them is filled whole.
					frameStart: msg.startFrame,
					// The budget judges the group until the response is acknowledged.
					held: true,
				};
				group = await front.track(msg.track).fetchGroup(msg.group, options);
				serving(group);
				// Revoked while the group resolved: the watch already reset the stream.
				if (watch.revoked()) {
					hooks.releaseGroupReader(group, unauthorized(msg.broadcast));
					return;
				}
				// The FETCH's own budget gives the group up, while a write waits on flow control and
				// while its FIN waits for the acknowledgement, as the Rust `FetchServe` does: stale
				// bytes must not hold the send window ahead of the streams behind them.
				givenUp = new GivenUp(group);
				try {
					await this.#runFetchGroup(group, stream.writer, {
						stamp: stamping(this.version, info),
						compress: info.compress,
						start: msg.startFrame,
						end: msg.endFrame,
						givenUp,
					});
					console.debug(`fetch done: broadcast=${msg.broadcast} track=${msg.track} group=${msg.group}`);
					stream.close();
					// Held until the acknowledgement, so its budget still judges it meanwhile.
					const stop = await givenUp.write(() => acknowledged(stream.writer));
					if (stop) throw stop;
					group.close();
				} finally {
					givenUp.close();
				}
			} catch (err: unknown) {
				const e = error(err);
				// Giving up a group past its budget is routine, as in Rust's `RequestServe`.
				const log = e === givenUp?.peek() ? console.debug : console.warn;
				log(`fetch error: broadcast=${msg.broadcast} track=${msg.track} group=${msg.group} error=${reason(e)}`);
				if (group) hooks.releaseGroupReader(group, e);
				stream.abort(e);
			}
		} finally {
			hold.abort();
		}
	}

	/**
	 * Runs a track and sends its data to the stream.
	 * @param sub - The subscription ID
	 * @param broadcast - The broadcast name
	 * @param track - The track to run
	 * @param stream - The stream to write to
	 * @returns Whether the track finished with every group stream done, rather than the
	 *   subscriber leaving
	 *
	 * @internal
	 */
	async #runTrack(
		track: track.Subscriber,
		stream: Writer,
		controls: SubscriptionControls,
		serving: {
			sub: bigint;
			broadcast: Path.Valid;
			stamp: Stamp | undefined;
			/** The track's publisher priority, breaking ties between subscriptions. */
			publisher: number;
			compress: boolean;
			// Whether the wire names what it never served (lite-05 and later), recording what it
			// told in the reader's own record.
			named: boolean;
			// The read cursor's floor; see {@link cursorFloor}.
			floor: bigint;
			bounds: FrameBounds;
			// How many ranges the SUBSCRIBE carried, which take Range IDs from 0.
			initial: bigint;
			// Live ranges currently requested on this subscription.
			requested: Requested;
			// Lite-07 single-group Position request eligible for one-shot fetch handoff if
			// subscriber FIN arrives after SUBSCRIBE (e.g. in a separate QUIC STREAM frame).
			oneShot?: boolean;
		},
	): Promise<boolean | "one-shot"> {
		const { sub, broadcast, stamp, publisher, compress, named, bounds, initial, requested } = serving;
		// What the wire was told of each key: served, or given up, and how. Kept in the
		// reader's own record, which its drops consult too. Undefined before lite-05.
		const record = named ? hooks.wire(track) : undefined;
		// The read cursor's floor; the subscription's ranges cap it.
		let floor = serving.floor;
		const moveCursor = () => track.withRange({ start: { included: floor } });
		// Raise the floor to `start`, keeping any higher floor already set.
		const raiseFloor = (start: bigint) => {
			if (start <= floor) return;
			floor = start;
			moveCursor();
		};
		// Lite-05+ resolves the range on the subscribe stream: SUBSCRIBE_START once the
		// first group is known, SUBSCRIBE_END when the track finishes.
		const emitRange = hasTrackStream(this.version);
		// Dispositions before delivery resolves refine its start; later ones are named as drops. Each
		// replaces the runs it covers, as a skip may go out over a drop, so they stay sorted and
		// disjoint. Where delivery resolved is lowered by an update that asks for groups below it.
		let answer = startAnswer(this.version);
		// Lower where delivery starts to `start`, once resolved.
		const lowerFrom = (start: bigint) => {
			if (answer.kind === "started" || answer.kind === "withheld" || answer.kind === "answered")
				answer.from = min(answer.from, start);
		};
		// Whether no group was served or held yet, so a late FIN can still hand the request to a fetch.
		const untouched = () =>
			(answer.kind === "waiting" && !answer.first) ||
			(answer.kind === "unresolved" && answer.served === undefined);
		// What this hop knows of the largest position, `position` the newest it saw: nothing on a
		// relay's copy its upstream has yet to answer, whatever its cache holds. Mirrors the Rust
		// `TrackRun::largest`.
		const edge = (position: track.Position | undefined): Edge => {
			if (!hooks.knowsEdge(track)) return "unknown";
			return position ? { position, ended: hooks.largestFinished(track, position) } : "empty";
		};
		const newest = () => newer(hooks.bufferedLargest(track), hooks.peekLargest(track).largest);
		let endSent = false;
		// Lite-07: the ranges no SUBSCRIBE_START answered yet, by Range ID, each as it was asked.
		const unstarted = new Map<bigint, Asked>();
		if (hasRanges(this.version)) {
			for (let id = 0n; id < initial; id++) {
				const range = controls.range(id);
				if (range) unstarted.set(id, asked(range));
			}
		}
		// Send a range's SUBSCRIBE_START, then whatever was withheld for the first one.
		const sendStart = async (start: SubscribeStart): Promise<boolean> => {
			if (!(await controls.response(encodeSubscribeResponse(stream, { start }, this.version)))) return false;
			if (answer.kind !== "withheld") return true;
			const { from, held } = answer;
			answer = { kind: "answered", from };
			for (const msg of held) {
				if (!(await controls.response(encodeSubscribeResponse(stream, msg, this.version)))) return false;
			}
			return true;
		};
		// Send `[start, end)` given up as `hole`, or hold it until a lite-07 subscription's first
		// SUBSCRIBE_START: one given up before delivery resolved, such as a group served meanwhile whose
		// stream failed, is named once it does.
		const sendHole = (start: bigint, end: bigint, hole: Hole): Promise<boolean> => {
			if (answer.kind === "unresolved") answer.skipped.set(start, end, { ...hole, start, end });
			else if (answer.kind === "withheld") answer.held.push(holeMessage(start, end, hole));
			else return controls.response(encodeSubscribeResponse(stream, holeMessage(start, end, hole), this.version));
			return Promise.resolve(true);
		};
		// Lite-07: whether the track is a relay's copy that places each range only where its upstream
		// would, from that upstream's answer for it or groups proving it, so a range it placed waits
		// on no other. A copy that places ranges from whatever groups it holds, or a local track,
		// waits for where its whole feed starts. Mirrors the Rust `TrackRun::per_range`.
		const perRange = () => hasRanges(this.version) && hooks.placesAsUpstream(track);

		// Resolve delivery at `from`, `position` the newest this hop saw, then name the groups given up
		// above it while it was held back. Before lite-07 the one start answers the subscription. On
		// lite-07 each of the SUBSCRIBE's ranges resolved there is answered now; one still waiting, or
		// one following the live edge before this hop knows where it is, is answered later by
		// `reportStarted`, and the drops given up meanwhile wait for the first answer. The only way out
		// of an unresolved answer, so none leaves its held drops unsent. Mirrors the Rust
		// `TrackRun::resolve`.
		const resolveStart = async (from: bigint, position: track.Position | undefined): Promise<boolean> => {
			if (answer.kind === "waiting") {
				const { skipped } = answer;
				answer = { kind: "started", from };
				if (!(await sendStart(new SubscribeStart({ id: 0n, start: from, largest: "unknown" })))) return false;
				return nameSkipped(from, skipped);
			}
			if (answer.kind !== "unresolved") throw new Error(`delivery resolves once, not when ${answer.kind}`);
			const { skipped } = answer;
			answer = { kind: "withheld", from, held: [] };
			const largest = edge(position);
			const ids = [...unstarted.keys()].filter((id) => id < initial && controls.holds(id));
			const resolution = hooks.servingResolvedKeys(track, ids);
			// A range reaching back to the oldest group, on a copy placing each range as its upstream
			// answered it, starts where the copy's feed does, which it may not know yet.
			const reaches = perRange() && hooks.sourceStart(track) === "pending";
			for (const [index, id] of ids.entries()) {
				const range = resolution[index];
				// An update taken while an earlier start went out may have retired it.
				if (!range || !controls.holds(id) || (unstarted.get(id)?.live && largest === "unknown")) continue;
				// A live join starts where it was placed, past the groups its max expired, which may be
				// below where delivery starts: a relay serves first the groups it holds, before those the
				// join is owed arrive. It moves delivery down to it. Any other range starts no earlier
				// than delivery.
				const budget = unstarted.get(id)?.joins;
				if (budget === undefined && reaches && !range.floored) continue;
				const start =
					budget !== undefined ? hooks.joinStart(track, range.start, budget) : max(range.start, from);
				unstarted.delete(id);
				const at = range.end === undefined || range.end > start ? start : undefined;
				if (at !== undefined) lowerFrom(at);
				if (!(await sendStart(new SubscribeStart({ id, start: at, largest })))) return false;
			}
			// A range an update added before this start is answered with it, before the drops held
			// for it, which a subscriber reads only once a range has started.
			if (!(await reportStarted(false))) return false;
			const resolved = answerFrom(answer);
			return resolved === undefined || nameSkipped(resolved, skipped);
		};

		// Name the runs in `skipped`, given up before delivery resolved at `start`, as they would have
		// been after it. Nothing below the start is owed. The final runs reaching up to it unbroken are
		// named all the same, so a reader filling below the start (a relay whose reader reaches lower)
		// learns nothing there exists. The rest, past a stale run, a group still held, or a key nothing
		// gave up, stay untold: naming them would send every gap in the track's history, as a lite-06
		// track keyed by timestamp holds one per group. An update that asks below the start names them
		// then, and a wider budget may still serve a stale one. Mirrors the Rust `TrackRun::name_skipped`.
		const nameSkipped = async (start: bigint, skipped: Runs<DropRun>): Promise<boolean> => {
			if (skipped.size === 0) return true;
			let reach = start;
			for (let i = skipped.size - 1; i >= 0; i--) {
				const range = skipped.all[i];
				if (range.start >= reach) continue;
				if (range.end < reach || range.stale !== undefined) break;
				reach = range.start;
			}
			// On lite-07, a subscription can also name disjoint history ranges below `reach` whose groups
			// were already given up before delivery resolved: forward their drops too.
			const send: Run[] = [];
			const maxEnd = 1n << 64n;
			insertRun(send, reach, maxEnd);
			if (hasRanges(this.version)) {
				for (const askedRange of requested.values()) {
					const model = requestedRange(askedRange);
					if (typeof model.start !== "bigint") continue;
					const to = model.end !== undefined ? model.end + 1n : maxEnd;
					if (model.start < to) insertRun(send, model.start, to);
				}
			}
			for (const range of skipped.all) {
				for (const below of runGaps(send, range.start, range.end)) record?.untell(below.start, below.end);
				for (const part of send) {
					const from = max(part.start, range.start);
					const to = min(part.end, range.end);
					if (from < to && !(await forward(from, to, range))) return false;
				}
			}
			return true;
		};

		// While the source has yet to say where its whole feed starts, `position` the newest this hop
		// saw: whether a range still unanswered, one the SUBSCRIBE carried or an update added since,
		// names a group the copy placed as its upstream answered (see `perRange`), so `resolveStart`
		// answers it now rather than wait on a live join or older history upstream still places.
		// Mirrors the Rust `TrackRun::places_any`.
		const placesAny = (position: track.Position | undefined): boolean => {
			if (!perRange()) return false;
			const known = edge(position) !== "unknown";
			const ids = [...unstarted.keys()].filter((id) => controls.holds(id));
			const resolution = hooks.servingResolvedKeys(track, ids);
			return ids.some((id, index) => resolution[index]?.floored && (known || !unstarted.get(id)?.live));
		};

		// Lite-07: resolve delivery from the lowest group served while unresolved, `position` the newest
		// this hop saw, once the source says where its feed starts, or once one of the SUBSCRIBE's
		// ranges is placed, with delivery at the floor, at or below where any range still unplaced
		// starts, so its answer later is never raised past its own start. Undefined while neither
		// holds. Mirrors the Rust `TrackRun::start`.
		const resolveServed = async (position: track.Position | undefined): Promise<boolean | undefined> => {
			if (answer.kind !== "unresolved" || answer.served === undefined) return undefined;
			let source = hooks.sourceStart(track);
			if (source === "pending" && placesAny(position)) source = floor;
			if (source === "pending") return undefined;
			const from = hooks.deliveryStart(track, { first: answer.served, source, givenUp: answer.skipped });
			return resolveStart(from, position);
		};

		// Lite-07: whether a range still unanswered waits on this copy's upstream to place it, which
		// answers every range once the track ends: only while the copy's source has yet to say where
		// its feed starts. Mirrors the Rust `TrackRun::awaits_upstream`.
		const awaitsUpstream = (): boolean => {
			if (!perRange()) return false;
			const held = [...unstarted.keys()].filter((id) => controls.holds(id));
			if (hooks.servingResolvedKeys(track, held).every((range) => range !== undefined)) return false;
			return hooks.sourceStart(track) === "pending";
		};

		// `edge(newest())`, or once `finished` with the edge unknown, what this hop holds: nothing more
		// arrives to tell it, so a range following the edge is answered with what it has. Mirrors the
		// Rust `TrackRun::settled_largest`.
		const settledLargest = (finished: boolean): Edge => {
			const position = newest();
			const largest = edge(position);
			if (largest !== "unknown" || !finished || position === undefined) return largest;
			return { position, ended: hooks.largestFinished(track, position) };
		};

		// Answer each range still waiting for its SUBSCRIBE_START once it resolves, or with no start
		// once nothing can resolve it (`finished`). One following the live edge also waits until this
		// hop knows where it is, unless the track finished, after which nothing more tells it. Waits
		// for delivery to resolve, which `resolveStart` does. Mirrors the Rust
		// `TrackRun::report_started`.
		const reportStarted = async (finished: boolean): Promise<boolean> => {
			const resolved = answerFrom(answer);
			if (resolved === undefined || unstarted.size === 0) return true;
			const largest = settledLargest(finished);
			// An update not taken yet retired one, and taking that update forgets it. In Range ID
			// order, as the Rust `TrackRun::report_started` reports them.
			const held = [...unstarted.keys()]
				.filter((id) => controls.holds(id))
				.sort((a, b) => (a < b ? -1 : a > b ? 1 : 0));
			// Every range clamps against where delivery started before this batch, so one reported
			// later isn't clamped against a start an earlier join in the batch lowered.
			const from = resolved;
			const per = perRange();
			const resolution = hooks.servingResolvedKeys(track, held);
			for (const [index, id] of held.entries()) {
				// Or one taken while an earlier start went out.
				if (!controls.holds(id)) continue;
				const ask = unstarted.get(id);
				// Never answered without the largest, even once nothing else can resolve it: this hop's
				// feed answering or ending tells it.
				if (ask?.live && largest === "unknown") continue;
				const range = resolution[index];
				if (!range && !finished) continue;
				let start: bigint | undefined;
				if (range) {
					// A live join placed behind where delivery started, further from the live edge than
					// the first ranges, starts where it was placed, past the groups its max expired: the
					// subscriber reads its answer as the join's start, so its groups from there are owed,
					// and delivery now starts there too. So does a range naming a group on a copy that
					// places it as its upstream answered. One reaching back there starts where the copy's
					// feed does, once the copy knows. Any other range starts no earlier than delivery.
					if (ask?.joins !== undefined) start = hooks.joinStart(track, range.start, ask.joins);
					else if (per && range.floored) start = range.start;
					else if (per) {
						const source = hooks.sourceStart(track);
						if (source === "pending" && !finished) continue;
						start = max(range.start, source === "pending" ? from : (source ?? from));
					} else start = max(range.start, from);
					if (range.end !== undefined && range.end <= start) start = undefined;
					else lowerFrom(start);
				}
				unstarted.delete(id);
				if (!(await sendStart(new SubscribeStart({ id, start, largest })))) return false;
			}
			return true;
		};

		// Lite-07, on a track this hop knows has no group yet: answer each range starting at the
		// oldest group at once, since every group the track produces is owed, so the subscriber
		// learns the track is empty rather than wait on a group. A time start above the floor, or a
		// live join, waits for a group to place it; one at or below the floor starts where delivery
		// does. Mirrors the Rust `TrackRun::resolve_empty`.
		const resolveEmpty = async (): Promise<boolean | undefined> => {
			if (!hasRanges(this.version) || ![...unstarted.values()].some((ask) => ask.oldest)) return undefined;
			if (newest() !== undefined || !hooks.knowsEdge(track) || hooks.subscriberEnd(track) !== undefined)
				return undefined;
			if (!(await resolveStart(0n, undefined))) return false;
			for (const [id, ask] of [...unstarted]) {
				if (!ask.oldest || !controls.holds(id)) continue;
				unstarted.delete(id);
				if (!(await sendStart(new SubscribeStart({ id, start: 0n, largest: "empty" })))) return false;
			}
			return true;
		};

		// Lite-07, on a track that ended before its first group: its SUBSCRIBE_END at group 0 answers
		// every range, so none waits for a start, and nothing given up is owed. Mirrors the Rust
		// `TrackRun::answer_empty`.
		const answerEmpty = () => {
			unstarted.clear();
			answer = { kind: "answered", from: 0n };
		};

		// The message naming `[start, end)` given up as `hole` says: lite-07 skips what was never
		// produced with `SKIP`, and earlier versions drop it as not found.
		// The message naming `[start, end)` given up as `hole`; an OLD drop with no verdict of its own
		// names the narrowest max age the live ranges ask, so only a reader no wider settles on it.
		const holeMessage = (start: bigint, end: bigint, hole: Hole): SubscribeResponse => ({
			drop: SubscribeDrop.hole(start, end, hole, requested.narrowest(), this.version),
		});

		// What the wire is told when `hole` goes out. Every report is final. Mirrors the Rust
		// `TrackRun::report`.
		const reportOf = (hole: Hole): Omit<Reported, "start" | "end"> => {
			if (!hasSkip(this.version)) return { kind: "settled" };
			return { kind: hole.skip ? "skip" : "dropped" };
		};

		// A drop of a group held whole names its span, `[start, max(End, start + 1))`, so the
		// subscriber accounts for the starts inside it too: no other group starts there. The
		// group is the highest one below `end`, starting at `end - 1n` or holding it in its span,
		// as the Rust `TrackState::span_end` takes it.
		const reach = (end: bigint): bigint =>
			max(end, hooks.wholeEnd(track, end - 1n) ?? hooks.wholeAround(track, end - 1n)?.end ?? end);

		// The starts of the groups whose stream is still serving, which may yet send GROUP_END.
		const streaming = new Set<bigint>();

		// Give up, as `hole` says, whatever part of `[start, end)` this subscription has not been
		// served or given up yet.
		const drop = async (start: bigint, end: bigint, hole: Hole): Promise<boolean> => {
			if (!record) return true;
			const report = reportOf(hole);
			for (const gap of record.untold(start, end)) {
				// The starts inside a whole group whose stream is still serving are its GROUP_END's to
				// account for, which may still be on its way, or its drop's, which names its span. A
				// stream that already ended without one left them untold, so they are named here.
				const around = hooks.wholeAround(track, gap.start);
				const from = around && streaming.has(around.start) ? min(around.end, gap.end) : gap.start;
				if (from >= gap.end) continue;
				record.tell(from, gap.end, report);
				if (!(await sendHole(from, gap.end, hole))) return false;
			}
			return true;
		};

		// Forward a verdict the reader reached on `[start, end)`, which its record already took in, so
		// nothing went out over it. A drop of a group held whole names its span.
		const forward = async (start: bigint, end: bigint, hole: Hole): Promise<boolean> => {
			if (!record || start >= end) return true;
			if (!hole.skip) end = reach(end);
			record.tell(start, end, reportOf(hole));
			return sendHole(start, end, hole);
		};

		// The track's exclusive final boundary. A Rust subscriber feeds SUBSCRIBE_END
		// straight into finish_at, so it must name the track's boundary (which counts
		// datagram sequences too), not the delivered range: a subscription cap can hold
		// produced groups back. The latest() fallback covers a subscription torn down
		// before the producer declared it.
		// Lite cannot name a frame, so a mid-group end rounds up past its group, whose own
		// stream FIN says where it stops.
		const boundary = () => {
			const end = hooks.subscriberEnd(track);
			return end ? finalKey(end) : (hooks.latestKey(track) ?? -1n) + 1n;
		};

		// Declare the boundary immediately; groups below it can still be in flight. Lite-07 carries
		// an errored end's error with it, which older versions reset with once the end is reached.
		const sendEnd = async (): Promise<boolean> => {
			endSent = true;
			if (!emitRange) return true;
			const endError = hooks.subscriberEndError(track);
			const code = endError && hasEndError(this.version) ? toStreamCode(endError) : undefined;
			return controls.response(
				encodeSubscribeResponse(stream, { end: new SubscribeEnd(boundary(), code) }, this.version),
			);
		};

		// The FIN tells the subscriber every group is accounted for, so it waits until each group
		// stream finished or reset. False once the subscriber left instead, which cancels whatever
		// is still queued. `race`, since this repeats per batch of drops against the subscription's end.
		const drained = async (): Promise<boolean> => {
			const done = Symbol("drained");
			const end = await race([Promise.all(groups).then(() => done), controls.ended]);
			if (end instanceof Error) throw end;
			return end === done;
		};
		// Lite-07: true once every group settled, or `undefined` once a control is there to take,
		// its FIN or failure included. An update may move a stop a group is parked at, which only
		// the loop applies, so the drain waits for it no longer. Each wait is disposed once it
		// wakes, so updates that leave a group parked leave nothing behind.
		const drainedOrControl = async (): Promise<true | undefined> => {
			for (;;) {
				if (controls.pending()) return undefined;
				if (groups.size === 0) return true;
				const dispose: Dispose[] = [];
				await new Promise<void>((resolve) => {
					dispose.push(
						controls.changed(resolve),
						groupEnded.changed(() => resolve()),
					);
				});
				for (const close of dispose) close();
			}
		};
		// Before lite-07 an errored end is only the reset at that end: a clean SUBSCRIBE_END sent
		// early could let the subscriber settle before the reset arrives. A subscription capped
		// short of the end is owed no error, so its boundary goes out.
		const declareEarly = () =>
			hasEndError(this.version) ||
			!hooks.subscriberEndError(track) ||
			(bounds.endGroup !== undefined && bounds.endGroup + 1n < boundary());

		// One ranking for the whole subscription, shared by every group it serves.
		const priority = new Priority(track, publisher);
		// Lite-07: the policy of the live range with Range ID `id`, as the latest update left it.
		// Mirrors the Rust `Steer::policy`.
		const rangePolicy = (id: bigint) => {
			const range = controls.range(id);
			return range && policyOf(range);
		};
		// Lite-07: a group goes out at the top of the live ranges still wanting it, falling as it ages
		// past the ones above, so its stream ranks by its own policy. The ranges covering it are fixed
		// as it goes out, as the Rust `GroupServe::covered_by`.
		const rankingOf = (group: group.Consumer): Ranking | undefined => {
			if (!hasRanges(this.version)) return undefined;
			const covers = hooks.servingCovering(track, group.start);
			const expiry = hooks.groupExpiry(group);
			// The widest budget the group is known to have aged past, which only grows.
			let aged: number | undefined;
			return {
				policy: (whole) => {
					for (;;) {
						const { policy, lasts } = groupPolicy({ whole, policy: rangePolicy }, covers, aged);
						if (lasts === undefined || !expiry?.aged?.(lasts)) return policy;
						aged = lasts;
					}
				},
				// Aging moves a group only between ranges, so one covered by a single range never does.
				changed: covers.length > 1 ? (expiry?.changed ?? []) : [],
			};
		};

		// Every group this subscription started serving, until its stream finishes or resets.
		const groups = new Set<Promise<void>>();
		// Bumped each time one of `groups` ends.
		const groupEnded = new Signal(0);
		const drops = new Signal(0);
		// Groups whose stream failed, with why, for the control stream to name, as runs: a group held
		// whole names its span.
		const pendingDrops: { start: bigint; end: bigint; hole: Hole }[] = [];
		// Lite-07: the starts inside whole groups' spans whose GROUP_END a reset may have cut, to name
		// again as a skip. Mirrors the Rust `GroupOutcomes::cut`.
		const pendingCuts: Run[] = [];

		// Cancels groups still queued for a stream slot. Only the subscriber leaving counts:
		// a track that ran out of groups still has to flush the ones already queued, and the
		// caller FINs the subscribe stream to say so.
		let finished = false;
		// Lite-07: the subscriber cleanly FIN'd its send half (no further updates).
		let peerFin = false;
		let canHandOff = Boolean(serving.oneShot);
		let armed: { woke: Promise<void>; dispose: Dispose } | undefined;
		// Park until the subscription changes, from the wait armed this pass if any.
		const park = () => armed?.woke ?? waitForSubscription(controls, track, drops);
		let unsubscribe!: () => void;
		const unsubscribed = new Promise<void>((resolve) => {
			unsubscribe = resolve;
		});
		// Unlike `unsubscribed`, settles however the run ends: a group parked at a stop waits on an
		// update, which no longer comes.
		let end!: () => void;
		const ended = new Promise<void>((resolve) => {
			end = resolve;
		});
		try {
			for (;;) {
				for (const { start, end } of pendingCuts.splice(0)) {
					record?.tell(start, end, { kind: "skip" });
					if (!(await sendHole(start, end, { error: new StreamError(StreamCode.Skip), skip: true })))
						return false;
				}
				if (pendingDrops.length > 0) {
					for (const { start, end, hole } of pendingDrops.splice(0)) {
						// The rest of the span is told with it; the served start keeps its record.
						for (const gap of record?.untold(start, end) ?? [])
							record?.tell(gap.start, gap.end, reportOf(hole));
						if (!(await sendHole(start, end, hole))) return false;
					}
				}
				// While a range waits for its START, the wait below is armed before `reportStarted` reads
				// what this hop knows, so an answer landing while this pass awaits still wakes it.
				armed?.dispose();
				armed = unstarted.size > 0 ? armSubscription(controls, track, drops) : undefined;
				if (!(await reportStarted(false))) return false;
				// Control before data, matching the Rust publisher: every control decoded while
				// the loop was parked applies to the next pop, never to one already made. This
				// drain is synchronous, so nothing the decoder holds can land between the pop
				// below and its frame range.
				const control = controls.take();
				if (control) {
					switch (control.kind) {
						case "fin":
							peerFin = true;
							if (
								canHandOff &&
								untouched() &&
								!endSent &&
								groups.size === 0 &&
								pendingDrops.length === 0 &&
								pendingCuts.length === 0
							) {
								return "one-shot";
							}
							// Already finished and waiting for updates or FIN: close cleanly now.
							if (finished) return true;
							continue;
						case "done":
							// The subscriber left. Its queued groups are pointless now, which
							// the finally below acts on since `finished` stays false. On lite-07 it
							// also FINs once it read a finished track's tail, which ends the
							// subscription as finished.
							return finished;
						case "error":
							throw control.error;
						case "update": {
							canHandOff = false;
							// An update may reach the finished track's history, so it is served again.
							finished = false;
							const update = control.update;
							if (hasRanges(this.version)) {
								// A range retired before its start resolved is owed no SUBSCRIBE_START.
								for (const id of control.applied.retired) unstarted.delete(id);
								for (const [id, range] of control.applied.added) unstarted.set(id, asked(range));
							}
							console.debug(`subscribe update: broadcast=${broadcast} track=${track.name}`);
							// `cursorFloor` pins an unfloored pre-06 SUBSCRIBE to the latest
							// group, so an update that drops a named floor re-pins there too. On
							// an empty track the next group is the latest, whatever its key.
							// Lite-06 encodes an absent start as no floor, so it drops back to 0:
							// the cursor only rises on its own, and a finished group below the old
							// floor (a quiet catalog) is never served otherwise.
							const requested = bounds.startGroup;
							const start = hasRanges(this.version)
								? 0n
								: update.startGroup === undefined &&
										requested !== undefined &&
										!resolvesStart(this.version)
									? (hooks.latestKey(track) ?? 0n)
									: (update.startGroup ?? (resolvesStart(this.version) ? 0n : undefined));
							if (start !== undefined) {
								floor = start;
								// Before SUBSCRIBE_START nothing is owed yet; the start accounts for it.
								// A lowered floor owes the groups it newly asks for, so the end sweep
								// covers them. A raised one sends no early drop: the subscriber already
								// stops owing below its floor, and a later update may lower it again to
								// ask for the groups the wire was told nothing of.
								lowerFrom(start);
							}
							bounds.startGroup = update.startGroup;
							bounds.startFrame = update.startFrame;
							bounds.endGroup = update.endGroup;
							bounds.endFrame = update.endFrame;
							bounds.starts = bounds.startsAfter.get(update) ?? new Map();
							const stops = bounds.stopsAfter.get(update);
							if (stops && !sameStops(stops, bounds.stops.peek())) bounds.stops.set(stops);
							moveCursor();
							// A microtask is too short: decoding one framed update crosses several
							// awaits, each of which can requeue behind the serving continuation. A
							// task boundary usually lets the decoder finish whatever the transport
							// already delivered before the next group pop. It orders nothing: an update
							// decoded later applies from the pop after it. Updates are rare, so groups
							// do not pay this scheduling cost on the normal path.
							await yieldNow();
							continue;
						}
					}
				}

				// Exactly-once arrival-order serving. This synchronous package-internal pop
				// and frameRange call are the operation's linearization point.
				// Popping or filtering a group removes the subscriber's view of its edge.
				// A relay can hold just the first group of a backlog its source already
				// answered past, so the source's largest says where the live feed is.
				const held = answer.kind === "waiting" ? answer.first : undefined;
				const largest =
					(answer.kind === "waiting" || answer.kind === "unresolved") && hasLargest(this.version)
						? newer(held?.largest, newest())
						: undefined;
				// Lite-05/06 hold their first group until the source says where its feed starts (below);
				// lite-07 serves its groups meanwhile, so a range waiting on its start holds back no
				// other's, and resolves from the lowest it served once it can.
				if (answer.kind === "unresolved" && answer.served !== undefined) {
					const resolved = await resolveServed(largest);
					if (resolved === false) return false;
					if (resolved) continue;
				}
				if (answer.kind === "waiting") answer.first = undefined;
				const recv = held
					? ({ kind: "group", group: held.group } as const)
					: hooks.tryRecvGroup(track, emitRange);
				switch (recv.kind) {
					case "error":
						throw recv.error;
					case "drop":
						if (answer.kind === "waiting" || answer.kind === "unresolved")
							answer.skipped.set(recv.start, recv.end, recv);
						else if (answer.kind !== "legacy" && !(await forward(recv.start, recv.end, recv))) return false;
						continue;
					case "idle":
						// A lite-07 window whose bounded ranges all resolved empty owes nothing: say so at
						// once, and the subscription ends as covered below. No group would ever answer it.
						if (answer.kind === "unresolved") {
							const end = hooks.servingEnd(track);
							if (end !== undefined && hooks.servingRuns(track, 0n, end).length === 0) {
								if (!(await resolveStart(end, largest))) return false;
								continue;
							}
						}
						// A declared end goes out while lower groups are still being produced.
						if (!endSent && hooks.subscriberEnd(track) !== undefined && declareEarly()) {
							if (!(await sendEnd())) return false;
							continue;
						}
						// Lite-07 keeps a covered subscription open for an update. One none of whose groups
						// can be served has no group to resolve its start: answer it now, as the Rust
						// publisher does, and the loop then names what it held back.
						if (answer.kind === "unresolved" && answer.served === undefined) {
							const end = hooks.servingEnd(track);
							const runs = end === undefined ? [] : hooks.servingRuns(track, 0n, end);
							const given: Run[] = [];
							for (const range of answer.skipped.all) insertRun(given, range.start, range.end);
							if (
								runs.length > 0 &&
								runs.every((run) => runGaps(given, run.start, run.end).length === 0)
							) {
								if (!(await resolveStart(runs[0].start, largest))) return false;
								continue;
							}
						}
						if (answer.kind === "unresolved" && answer.served === undefined) {
							const empty = await resolveEmpty();
							if (empty === false) return false;
							if (empty) continue;
						}
						// Lite-07 one-shot SUBSCRIBE + FIN: once every bounded range is resolved,
						// accounted for, and every group stream has finished, close cleanly.
						if (
							keepsSubscribeOpen(this.version) &&
							peerFin &&
							requested.end() !== undefined &&
							answer.kind === "answered" &&
							unstarted.size === 0 &&
							groups.size === 0 &&
							pendingDrops.length === 0 &&
							pendingCuts.length === 0 &&
							record
						) {
							const end = hooks.servingEnd(track);
							if (
								end !== undefined &&
								hooks
									.servingRuns(track, 0n, end)
									.every((run) => record.untold(run.start, run.end).length === 0)
							) {
								finished = true;
								return true;
							}
						}
						await park();
						continue;
					case "boundary":
						// The producer finished but is still holding groups above the cap.
						// Declare the boundary, then wait for an update to release them.
						if (!endSent && declareEarly()) {
							if (!(await sendEnd())) return false;
							continue;
						}
						if (
							keepsSubscribeOpen(this.version) &&
							peerFin &&
							requested.end() !== undefined &&
							answer.kind === "answered" &&
							unstarted.size === 0 &&
							groups.size === 0 &&
							pendingDrops.length === 0 &&
							pendingCuts.length === 0 &&
							record
						) {
							const end = hooks.servingEnd(track);
							if (
								end !== undefined &&
								hooks
									.servingRuns(track, 0n, end)
									.every((run) => record.untold(run.start, run.end).length === 0)
							) {
								finished = true;
								return true;
							}
						}
						await park();
						continue;
					case "done": {
						const endError = hooks.subscriberEndError(track);
						if (
							endError &&
							!hasEndError(this.version) &&
							(bounds.endGroup === undefined || bounds.endGroup + 1n >= boundary())
						)
							throw endError;
						// A relay's copy places a range only as its upstream would, so one its upstream has yet
						// to answer may start below every group held: nothing it owes is final until then.
						if (awaitsUpstream()) {
							await park();
							continue;
						}
						// Lite-07 sends no FIN to say nothing was served, so a track that ended before
						// delivery resolved still says where it starts, at or below any group served; the
						// drops below then account for the rest. One that ended before its first group is
						// answered by its SUBSCRIBE_END at 0 alone.
						if (answer.kind === "unresolved") {
							const fin = boundary();
							if (fin === 0n) answerEmpty();
							else {
								const first = hooks.servingRuns(track, 0n, fin)[0]?.start ?? fin;
								const from = answer.served === undefined ? first : min(first, answer.served);
								if (!(await resolveStart(from, largest))) return false;
								continue;
							}
						}
						// Nothing arrives to resolve a range still waiting on the live feed.
						if (!(await reportStarted(true))) return false;
						// Name every key from the start to the end this subscription never
						// got. A cap below the end bounds what the subscriber is owed, as on its side.
						const from = answerFrom(answer);
						if (from !== undefined) {
							// A key the publisher declared skipped went out as a skip already; one
							// it never declared might have existed, so it is not found.
							const unserved: Hole = {
								error: new StreamError(StreamCode.NotFound, { message: "group not served" }),
							};
							// Only the groups inside the ranges are owed.
							for (const run of hooks.servingRuns(track, from, boundary())) {
								if (!(await drop(run.start, run.end, unserved))) return false;
							}
						}
						if (!endSent) {
							if (!(await sendEnd())) return false;
							continue;
						}
						const drain = keepsSubscribeOpen(this.version) ? await drainedOrControl() : await drained();
						if (drain === undefined) continue;
						if (!drain) return false;
						if (pendingDrops.length > 0 || pendingCuts.length > 0) continue;
						// Lite-07 keeps the stream open past SUBSCRIBE_END until the subscriber FINs
						// (unless the subscriber already FIN'd for a one-shot SUBSCRIBE).
						if (keepsSubscribeOpen(this.version) && !peerFin) {
							finished = true;
							if (!controls.pending()) await waitForControl(controls);
							continue;
						}
						finished = true;
						return true;
					}
				}

				const group = recv.group;
				// Never a second stream for a key this subscription already accounted for (served,
				// sent as a datagram, or dropped, stale or not): a subscriber settles every drop for good,
				// and a reader wider than a stale one asks for the group with a request of its own.
				if (record?.told(group.start)) {
					group.close();
					continue;
				}
				const range = frameRange(bounds, group.start);

				if (answer.kind === "waiting") {
					// A group stream can beat the upstream START on its own stream, and resolving
					// from the group alone would give up the backlog that START still promises.
					const source = hooks.sourceStart(track);
					if (source === "pending") {
						answer.first = { group, largest };
						await park();
						continue;
					}
					// A group given up, or held and not servable, cannot be where the feed starts.
					const from = hooks.deliveryStart(track, { first: group.start, source, givenUp: answer.skipped });
					// A later group above an explicit floor is still delivered, so the cursor stays put.
					// A pre-06 subscribe that named no group is the exception: an absent
					// Group Start is the latest group, and this key becomes the floor.
					// Lite-06 encodes a floor of group 0 as 0, so an omitted start is not pinned.
					if (bounds.startGroup === undefined && !resolvesStart(this.version)) raiseFloor(from);
					if (!(await resolveStart(from, largest))) return false;
				} else if (answer.kind === "unresolved") {
					answer.served = answer.served === undefined ? group.start : min(answer.served, group.start);
					// Resolved before the group goes out when the source already answered, so its START
					// leads the group's stream.
					if ((await resolveServed(largest)) === false) return false;
				}
				record?.tell(group.start, group.start + 1n, { kind: "settled" });
				streaming.add(group.start);

				const options: RunGroup = {
					sub,
					group,
					givenUp: new GivenUp(group),
					stamp,
					compress,
					priority,
					ranking: rankingOf(group),
					unsubscribed,
					start: range.start,
					end: range.end,
					stops: hasRanges(this.version) ? { at: bounds.stops, ended } : undefined,
					// The starts inside a group sent whole are accounted for by its GROUP_END, so the
					// end-of-track scan never drops them.
					ended: (end) => {
						if (end > group.start + 1n) record?.tell(group.start + 1n, end, { kind: "settled" });
					},
					cut: (start, end) => {
						pendingCuts.push({ start, end });
						drops.update((revision) => revision + 1);
					},
				};
				// The FIN waits for every stream to finish or reset, including one that has not opened yet.
				// A stream reset to make room fails the group like any other reset, so it is dropped,
				// unless a lite-07 reset delivered its header, which accounts for the group.
				const task = (async () => {
					const writer = await this.#openGroup(options);
					let failed: Error | undefined;
					try {
						failed = writer instanceof Error ? writer : await this.#serveGroup(writer, options);
					} finally {
						options.givenUp.close();
						streaming.delete(group.start);
					}
					if (failed && emitRange) {
						const end = max(group.end ?? group.start, group.start + 1n);
						// A group that went stale names the budget it was judged under.
						const budget = group.staleUnder;
						const hole = budget === undefined ? { error: failed } : staleHole(budget);
						pendingDrops.push({ start: group.start, end, hole });
						drops.update((revision) => revision + 1);
					}
				})();
				groups.add(task);
				void task.finally(() => {
					groups.delete(task);
					groupEnded.update((ended) => ended + 1);
					if (groups.size === 0) drops.update((revision) => revision + 1);
				});
			}
		} finally {
			armed?.dispose();
			// A group held for the upstream START has no one left to read it.
			if (answer.kind === "waiting") answer.first?.group.close();
			if (!finished) unsubscribe();
			end();
			// A lite-07 subscriber FINs once it read the finished track's tail, or once it wants
			// nothing more, even while a reply is still being written. Either way the groups still
			// waiting for a slot are cancelled, but an opened stream may be a final group whose FIN
			// is still being acknowledged, so a draining close still waits for it; a reader that
			// wants none of it resets it.
			if (keepsSubscribeOpen(this.version) && groups.size > 0)
				void this.owe(Promise.allSettled(groups).then(() => undefined));
			priority.close();
		}
	}

	/**
	 * Answers a TRACK stream (0x6) with a single TRACK_INFO, then FINs.
	 *
	 * The open stream is interest in the track, held until the requester closes its side,
	 * so demand holds while the requester moves on to SUBSCRIBE. Only the reply is owed.
	 *
	 * @internal
	 */
	async runTrackInfo(msg: TrackMessage, stream: Stream) {
		const hold = new AbortController();
		// Armed before the first check and held until the answer is acknowledged, so a grant
		// that shrinks while the answer is blocked on flow control resets it, never sends it.
		const watch = this.#gate.watch(msg.broadcast, () => {
			console.debug(`track info revoked: broadcast=${msg.broadcast} track=${msg.track}`);
			hold.abort();
			stream.writer.reset(unauthorized(msg.broadcast));
		});
		if (watch.revoked()) {
			watch.dispose();
			hold.abort();
			stream.writer.reset(unauthorized(msg.broadcast));
			return;
		}
		// Watched from the start, so a requester leaving while the reply is still blocked on
		// flow control lets go of the track too.
		void stream.reader.done().then(
			(fin) => {
				hold.abort();
				if (fin) return;
				// TRACK is the requester's only message, so more breaks the session, as in Rust.
				const violation = new ProtocolViolation("data after TRACK");
				closeViolation(this.#quic, violation);
				stream.abort(violation);
			},
			() => hold.abort(),
		);
		try {
			const front = await this.#resolve(msg, hold.signal);
			if (!front) throw new NotFound(`broadcast ${msg.broadcast}`);

			let info: TrackInfoMessage;
			let datagrams: datagram.Consumer | undefined;
			try {
				info = await this.#resolveTrackInfo(front, msg.track, hold.signal);
			} catch (err) {
				if (
					!hasDatagrams(this.version) ||
					hasDatagramTracks(this.version) ||
					toStreamCode(error(err)) !== StreamCode.NotFound
				)
					throw err;
				const asked = front.datagrams(msg.track);
				try {
					info = trackInfoMessage(
						datagramTrackInfo(await untilAborted(asked.info(), hold.signal)),
						this.version,
					);
				} catch (err) {
					asked.close();
					throw err;
				}
				datagrams = asked;
			}
			// Lite-05/06: the name's datagram track, read from this answer on, held for the
			// SUBSCRIBE alongside, beside a stream track too.
			if (hasDatagrams(this.version) && !hasDatagramTracks(this.version)) {
				datagrams ??= wireOf(front).liveDatagrams(msg.track);
			}
			if (datagrams) this.#holdDatagrams(datagrams, hold.signal);
			if (watch.revoked()) throw unauthorized(msg.broadcast);
			await info.encode(stream.writer, this.version);
			console.debug(`track info: broadcast=${msg.broadcast} track=${msg.track}`);
			stream.writer.close();
			await acknowledged(stream.writer);
		} catch (err) {
			hold.abort();
			console.debug(`track unknown: broadcast=${msg.broadcast} track=${msg.track}`);
			stream.writer.reset(error(err));
		} finally {
			watch.dispose();
		}
	}

	/**
	 * Answers a TRACKS stream (0x8): the broadcast's listed tracks, SYNCED, then each change,
	 * until the broadcast ends (FIN) or a newer listing of it on this session replaces this one.
	 *
	 * Each listing queues the broadcast's whole listed set, so the session lists each broadcast
	 * once: a newer listing replaces the older rather than being refused, since a subscriber
	 * that stops listing and lists again can open the new stream before we see the old one
	 * close. The requester's FIN or reset cancels it. Not owed: it lasts as long as the broadcast.
	 *
	 * @internal
	 */
	async runTracks(msg: TracksMessage, stream: Stream) {
		const cancel = new AbortController();
		const watch = this.#gate.watch(msg.broadcast, () => {
			console.debug(`tracks revoked: broadcast=${msg.broadcast}`);
			cancel.abort(unauthorized(msg.broadcast));
		});
		// TRACKS is the requester's only message: its FIN or reset ends the listing.
		void stream.reader.done().then(
			(fin) => {
				if (fin) {
					cancel.abort(new StreamError(StreamCode.Cancel, { message: "listing cancelled" }));
					return;
				}
				const violation = new ProtocolViolation("data after TRACKS");
				closeViolation(this.#quic, violation);
				cancel.abort(violation);
			},
			(err: unknown) => cancel.abort(error(err)),
		);
		let tracks: broadcast.Tracks | undefined;
		try {
			if (watch.revoked()) throw unauthorized(msg.broadcast);
			const front = await this.#resolve(msg, cancel.signal);
			if (!front) throw new NotFound(`broadcast ${msg.broadcast}`);

			tracks = wireOf(front).tracks(this);
			console.debug(`tracks: broadcast=${msg.broadcast}`);
			for (;;) {
				// Pull an event only once the last is written, so a stalled requester's changes
				// collapse in the listing's queue rather than pile up here.
				const event = await untilAborted(tracks.next(), cancel.signal);
				if (!event) break;
				await untilAborted(
					encodeTracksEvent(stream.writer, tracksEvent(event, this.version), this.version),
					cancel.signal,
				);
			}
			stream.writer.close();
		} catch (err) {
			const e = cancel.signal.aborted ? error(cancel.signal.reason) : error(err);
			console.debug(`tracks end: broadcast=${msg.broadcast} error=${reason(e)}`);
			stream.writer.reset(e);
		} finally {
			tracks?.close();
			watch.dispose();
		}
	}

	// Hold `consumer`'s track from its position for the SUBSCRIBE alongside until `hold`
	// aborts, as a passive observer in place of `consumer`.
	#holdDatagrams(consumer: datagram.Consumer, hold: AbortSignal) {
		const held = hooks.observeDatagramConsumer(consumer);
		consumer.close();
		this.#heldDatagrams.add(held);
		const release = () => {
			this.#heldDatagrams.delete(held);
			held.close();
		};
		if (hold.aborted) release();
		else hold.addEventListener("abort", release, { once: true });
	}

	// `consumer`, or a clone of the oldest hold of the same track, read from that hold's answer on.
	// Several TRACKs of one track may be open, and the SUBSCRIBE could follow any of them, so it
	// reads from the earliest answer: a replay only duplicates, a later one would lose.
	#adopt(consumer: datagram.Consumer): datagram.Consumer {
		for (const held of this.#heldDatagrams) {
			if (!hooks.datagramSameTrack(held, consumer)) continue;
			consumer.close();
			return held.clone();
		}
		return consumer;
	}

	// Resolve (and cache) a track's immutable TRACK_INFO by asking the application.
	// `resolveTrackInfo` triggers a TrackRequest the app answers with accept(TrackInfo);
	// only the immutable properties are needed (not the groups). Cached because they're
	// fixed for the track's lifetime. Rejects if the track is unavailable, or once `hold`
	// aborts. Each requester holds the lookup until `hold` aborts, and the lookup ends with
	// its last holder, answered or not; an unanswered one is dropped, so the next requester
	// asks again. A TRACK stream is interest, so it always holds a live lookup, while a
	// FETCH (`reuse`) only needs the answer and takes one a released lookup left behind.
	#resolveTrackInfo(
		front: broadcast.Consumer,
		track: string,
		hold: AbortSignal,
		reuse = false,
	): Promise<TrackInfoMessage> {
		hold.throwIfAborted();
		let tracks = this.#trackInfo.get(front);
		if (!tracks) {
			tracks = new Map();
			this.#trackInfo.set(front, tracks);
		}

		let entry = tracks.get(track);
		if (reuse && entry?.answered) return entry.info;
		if (!entry || entry.release.signal.aborted) {
			const release = new AbortController();
			const info = (async () =>
				trackInfoMessage(await wireOf(front).resolveTrackInfo(track, release.signal), this.version))();

			const created: TrackInfoEntry = { info, holders: 0, answered: false, release };
			info.then(
				() => {
					created.answered = true;
				},
				// Don't poison the cache on failure: a later request may succeed.
				() => {
					if (tracks.get(track) === created) tracks.delete(track);
				},
			);
			tracks.set(track, created);
			entry = created;
		}

		const held = entry;
		held.holders++;
		hold.addEventListener(
			"abort",
			() => {
				if (--held.holders > 0) return;
				held.release.abort();
				if (!held.answered && tracks.get(track) === held) tracks.delete(track);
			},
			{ once: true },
		);
		return untilAborted(held.info, hold);
	}

	/** Serve a lite-07 datagram-track request until its publisher or subscriber ends it. */
	async runDatagrams(msg: Datagrams, stream: Stream): Promise<void> {
		const cancel = new AbortController();
		let track: datagram.Consumer | undefined;
		const watch = this.#gate.watch(msg.broadcast, () => cancel.abort(unauthorized(msg.broadcast)));
		let updates: Promise<void> | undefined;
		try {
			if (watch.revoked()) throw unauthorized(msg.broadcast);
			if (!this.#datagramWriter) throw new Error("transport does not support datagrams");
			// Read updates from dispatch so a requester leaving releases even an unanswered lookup.
			updates = (async () => {
				let next = msg.priority;
				for (;;) {
					const yielded = datagramBudget.poll();
					if (yielded !== undefined) await yielded;
					const update = await DatagramsUpdate.decodeMaybe(stream.reader, this.version);
					if (!update) return;
					next = update.priority;
					msg.priority = next;
					track?.setPriority(next);
				}
			})();
			void updates.then(
				() => cancel.abort(new StreamError(StreamCode.Cancel)),
				(err) => cancel.abort(error(err)),
			);
			const front = await this.#resolve(msg, cancel.signal);
			if (!front) throw new NotFound(`broadcast ${msg.broadcast}`);
			track = front.datagrams(msg.track);
			track.setPriority(msg.priority);
			const info = await untilAborted(track.info(), cancel.signal);
			await untilAborted(new DatagramsInfo(info).encode(stream.writer, this.version), cancel.signal);
			await untilAborted(
				this.#runDatagrams(track, { sub: msg.id, stamp: stamping(this.version, info) }),
				cancel.signal,
			);
			stream.writer.close();
			await updates;
		} catch (err) {
			closeViolation(this.#quic, err);
			stream.abort(error(err));
		} finally {
			track?.close();
			watch.dispose();
			stream.reader.stop(StreamCode.Cancel);
			await updates?.catch(() => {});
		}
	}

	// Lite-05/06: a name with no stream track serves its datagram track alone.
	async #serveLegacyDatagrams(
		front: broadcast.Consumer,
		msg: Subscribe,
		stream: Stream,
		signal: AbortSignal,
	): Promise<void> {
		const track = this.#adopt(front.datagrams(msg.track));
		track.setPriority(msg.priority);
		const updates = (async () => {
			for (;;) {
				const yielded = datagramBudget.poll();
				if (yielded !== undefined) await yielded;
				const update = await SubscribeUpdate.decodeMaybe(stream.reader, this.version);
				if (!update) return;
				track.setPriority(update.priority);
			}
		})();
		void updates.then(
			() => track.close(),
			(err) => track.close(error(err)),
		);
		try {
			const info = await untilAborted(track.info(), signal);
			await encodeSubscribeResponse(
				stream.writer,
				{ start: new SubscribeStart({ id: 0n, start: 0n, largest: "unknown" }) },
				this.version,
			);
			const end = await untilAborted(
				this.#runDatagrams(track, { sub: msg.id, stamp: stamping(this.version, info) }),
				signal,
			);
			await encodeSubscribeResponse(stream.writer, { end: new SubscribeEnd(end) }, this.version);
			stream.writer.close();
			await updates;
		} finally {
			track.close();
			stream.reader.stop(StreamCode.Cancel);
			await updates.catch(() => {});
		}
	}

	// Shared by dedicated lite-07 requests and the old SUBSCRIBE carry-both mapping.
	async #runDatagrams(
		track: datagram.Consumer,
		options: { sub: bigint; stamp?: Stamp; priority?: Getter<number> },
	): Promise<bigint> {
		if (!this.#datagramWriter) throw new Error("transport does not support datagrams");
		const info = await track.info();
		const priority = options.priority ?? hooks.datagramPriority(track);
		const rank = () => sendOrder({ priority: priority.peek(), publisher: info.priority });
		const sender = this.#datagramWriter.open(rank());
		const dispose = priority.subscribe(() => sender.setPriority(rank()));
		const left = hooks.datagramClosed(track);
		const { writer } = sender;
		const maxSize = Math.min(DatagramMessage.MAX_SIZE, DatagramStream.maxDatagramSize(this.#quic));
		let end = 0n;
		try {
			for (;;) {
				const datagram = await track.recv();
				if (!datagram) return end;
				end = max(end, datagram.sequence + 1n);
				// An old subscription uses its stream track's timescale for both kinds. Without one
				// the track is a scale-less lite-07 one, whose timestamp goes out bare.
				const timestamp = wireTime(datagram.timestamp, options.stamp ?? {});
				const body = new DatagramMessage(options.sub, datagram.sequence, timestamp, datagram.payload).encode(
					this.version,
				);
				if (body.byteLength > maxSize) {
					console.debug(
						`dropping oversize datagram: sub=${options.sub} size=${body.byteLength} max=${maxSize}`,
					);
					continue;
				}
				// The legacy writer is shared, so another request's write can hold back this one:
				// stop waiting once this request ends, and send nothing after it does.
				// Raced directly, so a ready writer drops its close listener rather than keep one per send.
				await race([writer.ready, left]);
				// As recv() does: a clean close ends the request, an error close fails it.
				const reason = left.peek();
				if (reason !== undefined) {
					if (reason) throw reason;
					return end;
				}
				await writer.write(body);
			}
		} finally {
			dispose();
			await sender.close();
		}
	}

	// Serialize a fetched group's frames onto the FETCH stream as bare records: each a
	// zigzag-delta timestamp (at the track's wire timescale, if any) followed by size + bytes.
	async #runFetchGroup(
		group: group.Consumer,
		stream: Writer,
		{ stamp, compress, start: startFrame, end: endFrame, givenUp }: ServeFetch,
	) {
		const write = async (op: () => Promise<unknown>) => {
			const stop = await givenUp.write(op);
			if (stop) throw stop;
		};
		// The response carries no header, so the receiver numbers the first frame it gets
		// as `startFrame`. Skipping the head here is the only thing keeping those numbers
		// honest; a group that ends before we reach it can't be served at all.
		if (hooks.groupIndex(group) > startFrame) throw new Error(`group lacks frame ${startFrame}`);
		while (hooks.groupIndex(group) < startFrame) {
			const skipped = await race([hooks.readGroupFrame(group, compress), stream.closed]);
			if (!skipped) {
				throw new Error(`fetch group ended before the requested start ${startFrame}`);
			}
		}

		// Lite-07 counts the first delta from the group's start, and GROUP_END from the highest.
		let prevTs = hasGroupEnd(this.version) ? group.start : 0n;
		let highest: bigint | undefined;
		for (let index = startFrame; endFrame === undefined || index <= endFrame; index++) {
			const read = await race([hooks.readGroupFrame(group, compress), stream.closed]);
			if (!read) break;
			const frame = read.frame;

			let delta: bigint | undefined;
			if (stamp !== undefined) {
				const ts = wireTime(frame.timestamp, stamp);
				delta = ts - prevTs;
				prevTs = ts;
				highest = max(highest ?? ts, ts);
			}

			const header = frameHeader(
				stream.version,
				delta,
				read.inflated,
				readSize(read),
				hasLengthFirst(this.version),
			);
			const chunks = read.chunks;
			// One budget race covers the frame's header and payload.
			await write(async () => {
				await stream.write(header);
				if (chunks) await sendPayload(chunks, (chunk) => stream.write(chunk), stream.closed);
				else if (frame.payload.length > 0) await stream.write(frame.payload);
			});
		}
		// Lite-07: a response that reached a finished group's end says so before the FIN.
		const end = group.end;
		if (hasGroupEnd(this.version) && end !== undefined && hooks.groupIndex(group) >= group.frameCount)
			await write(() => stream.write(groupEnd(stream.version, end, highest ?? group.start)));
	}

	/**
	 * Opens the unidirectional stream for one group, or closes the group and resolves
	 * an error when it cannot get one.
	 *
	 * @internal
	 */
	async #openGroup(options: RunGroup): Promise<Writer | Error> {
		const { group, givenUp, priority, ranking, unsubscribed, start } = options;
		try {
			// The transport drains streams by send order, and opens wait for stream credit in
			// it, so this is what makes a high-priority track (and a newer group within it) win
			// the link and the peer's credit when there isn't room for both. The place ranks the
			// open against the subscription's other groups while it waits.
			// Watch the group while the open waits, and release that watch once it settles.
			const place = priority.reserve(group.start, start, ranking);
			const opened = Promise.withResolvers<void>();
			let failure: Error | undefined;
			const abandoned = givenUp
				.write(() => opened.promise)
				.then((stop) => {
					failure = stop;
				});
			let stream: Writer | undefined;
			try {
				stream = await Writer.tryOpen(this.#quic, {
					version: this.version,
					sendOrder: place.rank,
					cancel: race([unsubscribed, abandoned]),
					group: true,
				});
			} finally {
				opened.resolve();
				place.release();
			}
			if (!stream) {
				const failed = failure ?? new Error("unsubscribed while waiting for a stream");
				hooks.releaseGroupReader(group, failed);
				return failed;
			}
			return stream;
		} catch (err: unknown) {
			const failed = error(err);
			hooks.releaseGroupReader(group, failed);
			return failed;
		}
	}

	/**
	 * Serves one group on the stream {@link #openGroup} opened for it, resolving the error a
	 * SUBSCRIBE_DROP names it with, or undefined when the stream accounts for the group.
	 *
	 * @internal
	 */
	async #serveGroup(stream: Writer, options: RunGroup) {
		const {
			sub,
			group,
			givenUp,
			stamp,
			compress,
			priority,
			ranking,
			start: startFrame,
			end: endFrame,
			stops,
		} = options;
		// The header names the requested position, so a copy missing it must be refused.
		const msg = new GroupMessage({ subscribe: sub, key: group.start, frameStart: startFrame });
		// The subscription's budget, or an eviction of what is left to read, may give up on the
		// group mid-write, which abandons the stream. A group already given up never starts the write.
		const write = async (op: () => Promise<unknown>) => {
			const stop = await givenUp.write(op);
			if (stop) throw stop;
		};
		// Lite-07: the transport took the whole GROUP header, which every later reset still delivers.
		let held = false;
		// Lite-07: the end of the span a buffered GROUP_END accounted for.
		let spanEnd: bigint | undefined;
		// Everything past this point runs inside the cleanup scope, so a failure never leaves
		// a finished group's stream being ranked.
		try {
			if (hooks.groupIndex(group) > startFrame) throw new Error(`group lacks frame ${startFrame}`);
			// A SUBSCRIBE_UPDATE re-ranks the subscription, so a group already on the wire
			// follows it too rather than keeping a stale rank until it finishes.
			priority.add(stream, group.start, startFrame, ranking);

			await write(async () => {
				await stream.u53(0); // stream type
				await msg.encode(stream, this.version);
			});
			if (hasReliableReset(this.version)) held = stream.hold();

			// Lite-07 counts the first delta from the group's start, whatever frame the stream starts
			// at, and GROUP_END from the highest timestamp sent, since B-frames go backwards.
			let prevTs = hasGroupEnd(this.version) ? group.start : 0n;
			let highest: bigint | undefined;
			// Whether the cursor ever reached the requested start, which decides how the
			// end of the group is read below. A received tail may already begin there.
			let reached = hooks.groupIndex(group) === startFrame;
			// Whether every frame went out, which a finished group's GROUP_END then claims.
			let whole = false;

			// Kept across loop iterations so waking on a Position End change never starts a
			// second concurrent readGroupFrame(), and stream.closed is derived lazily once
			// the first read starts (avoiding unhandled rejections if a cap breaks early).
			let pendingRead: ReturnType<typeof hooks.readGroupFrame> | undefined;
			let closed: Promise<undefined> | undefined;
			const stopWake = Symbol("stop-wake");
			// Track the next unsent frame index separately from `hooks.groupIndex(group)`,
			// which an in-flight `pendingRead` may advance before its frame is consumed.
			let nextUnsent = Math.max(hooks.groupIndex(group), startFrame);

			for (;;) {
				// Lite-07: reaching a Position end closes the stream cleanly without GROUP_END
				// (a slice, not the whole group) instead of parking open. If the group itself
				// already finished at or before `nextUnsent`, let the read below observe EOF so
				// GROUP_END is still sent.
				const stopBefore = stops?.at.peek().get(group.start);
				const settled = hooks.groupSettledFrames(group);
				if (
					((stopBefore !== undefined && nextUnsent >= stopBefore) ||
						(endFrame !== undefined && nextUnsent > endFrame)) &&
					(settled === undefined || settled > nextUnsent)
				) {
					pendingRead?.catch(() => {});
					closed?.catch(() => {});
					break;
				}
				pendingRead ??= hooks.readGroupFrame(group, compress);
				closed ??= stream.closed.then(() => undefined);
				let disposeStop: Dispose | undefined;
				let outcome: Awaited<ReturnType<typeof hooks.readGroupFrame>> | undefined | typeof stopWake;
				try {
					if (stops) {
						const moved = new Promise<typeof stopWake>((resolve) => {
							disposeStop = stops.at.changed(() => resolve(stopWake));
						});
						outcome = await race([pendingRead, closed, moved]);
					} else {
						outcome = await race([pendingRead, closed]);
					}
				} finally {
					disposeStop?.();
				}
				if (outcome === stopWake) continue;
				pendingRead = undefined;
				const read = outcome;
				if (!read) {
					// The group ended before the frame the subscriber asked to start
					// at, so this publisher can't serve the range at all. FINning here
					// would claim an empty group under that index; reset so it reads
					// as the gap it is.
					if (!reached) throw new Error(`group ended before frame ${startFrame}`);
					whole = true;
					break;
				}

				// A group that ends exactly at the start is a valid, empty range.
				if (read.index + 1 >= startFrame) reached = true;
				// Frames below the requested start were excluded, and the receiver
				// numbers what it gets from `startFrame`.
				if (read.index < startFrame) {
					nextUnsent = Math.max(nextUnsent, read.index + 1);
					continue;
				}
				const stop = stops?.at.peek().get(group.start);
				if ((endFrame !== undefined && read.index > endFrame) || (stop !== undefined && read.index >= stop)) {
					break;
				}
				nextUnsent = read.index + 1;

				let delta: bigint | undefined;
				if (stamp !== undefined) {
					// Convert each frame to the track's wire timescale.
					const ts = wireTime(read.frame.timestamp, stamp);
					delta = ts - prevTs;
					prevTs = ts;
					highest = max(highest ?? ts, ts);
				}

				const header = frameHeader(
					stream.version,
					delta,
					read.inflated,
					readSize(read),
					hasLengthFirst(this.version),
				);
				const chunks = read.chunks;
				const payload = chunks ? undefined : read.frame.payload;
				// One budget race covers the frame's header and payload.
				await write(async () => {
					await stream.write(header);
					if (chunks) await sendPayload(chunks, (chunk) => stream.write(chunk), stream.closed);
					else if (payload && payload.length > 0) await stream.write(payload);
				});
			}

			// Lite-07 ends a finished group with GROUP_END.
			const end = hasGroupEnd(this.version) && whole ? group.end : undefined;
			if (end !== undefined) {
				options.ended?.(end);
				spanEnd = end;
				await write(() => stream.write(groupEnd(stream.version, end, highest ?? group.start)));
			}
			stream.close();
			group.close();
			// A stream that fails after its FIN may have lost its header, so report it for a DROP,
			// unless a lite-07 reset still delivers it. Its bytes still hold the connection until
			// acknowledged, so a group gone stale meanwhile is reset like one still serving, as the
			// Rust `GroupServe` does: stale bytes must not fill the send window ahead of the streams
			// behind them.
			await write(() => stream.closed);
			return undefined;
		} catch (err: unknown) {
			const e = error(err);
			stream.reset(e);
			hooks.releaseGroupReader(group, e);
			if (hasReliableReset(this.version) && (await accounts(stream, held))) {
				// The reset may cut the GROUP_END, so the starts it accounted for are named again.
				if (spanEnd !== undefined && spanEnd > group.start + 1n) options.cut?.(group.start + 1n, spanEnd);
				return undefined;
			}
			return e;
		} finally {
			priority.remove(stream);
		}
	}

	/**
	 * Handles a probe stream by periodically reporting estimated bitrate.
	 * @param stream - The probe bidi stream
	 *
	 * @internal
	 */
	async runProbe(stream: Stream) {
		const getStats = statsOf(this.#quic);
		if (!getStats) {
			// Best-effort: we can't supply bandwidth estimates, so close the
			// whole bidi (FIN + STOP_SENDING) to let the peer release its end.
			stream.close();
			return;
		}

		let lastSent: Probe | undefined;
		let lastSentTime: Instant | undefined;

		// Whether a metric moved enough to be worth another report. Gaining or
		// losing a value always counts; both unknown never does.
		const moved = (prev?: number, next?: number, threshold = 0): boolean => {
			if (prev === undefined && next === undefined) return false;
			if (prev === undefined || next === undefined) return true;
			if (prev === 0) return next !== 0;
			return Math.abs(next - prev) / prev >= threshold;
		};

		const ended = new Once<Error | null>();
		const reading = (async () => {
			// Report ignores target values, but must consume them to observe the FIN.
			while (await Probe.decodeMaybe(stream.reader, this.version)) {}
		})().then(
			() => ended.set(null),
			(err: unknown) => {
				closeViolation(this.#quic, err);
				ended.set(error(err));
			},
		);

		try {
			for (;;) {
				const timer = new AbortController();
				const timeout = sleep(Duration.millis(PROBE_INTERVAL), { signal: timer.signal }).then(
					() => "timeout" as const,
					() => "timeout" as const,
				);
				let result: "timeout" | Error | null;
				try {
					result = await race([timeout, ended]);
				} finally {
					timer.abort();
				}
				if (result instanceof Error) throw result;
				if (result !== "timeout") break;

				// The two fields are independent on the wire, each using 0 for
				// unknown, so a transport exposing only one still has something to
				// report. Anything this version can't carry is dropped here rather
				// than by the encoder, so it reads as unknown to every check below.
				const stats = await getStats();
				// `smoothedRtt` is a DOMHighResTimeStamp, i.e. a double, but the wire
				// carries whole milliseconds and the varint encoder throws on a
				// fractional value. Round before it ever reaches `Probe`.
				const rtt = stats.smoothedRtt != null ? Math.round(stats.smoothedRtt) : undefined;
				const report = new Probe({
					bitrate: stats.estimatedSendRate ?? undefined,
					rtt: hasProbeRtt(this.version) ? rtt : undefined,
				});

				// Nothing left to report. Say so once if it retracts a value the peer
				// is still holding, then stay quiet rather than repeating "unknown"
				// every time the max age comes around.
				if (report.bitrate === undefined && report.rtt === undefined) {
					const retracts =
						lastSent !== undefined && (lastSent.bitrate !== undefined || lastSent.rtt !== undefined);
					if (!retracts) continue;
				}

				let shouldSend: boolean;
				if (lastSent === undefined || lastSentTime === undefined) {
					shouldSend = true;
				} else {
					const elapsed = now().since(lastSentTime).asMillis();
					// The bitrate threshold decays to zero as the last report ages: a
					// stale estimate is worth refreshing for a smaller move.
					const t = Math.max(PROBE_INTERVAL, Math.min(PROBE_MAX_AGE, elapsed));
					const range = PROBE_MAX_AGE - PROBE_INTERVAL;
					const threshold = (PROBE_MAX_DELTA * (PROBE_MAX_AGE - t)) / range;
					shouldSend =
						elapsed >= PROBE_MAX_AGE ||
						moved(lastSent.bitrate, report.bitrate, threshold) ||
						moved(lastSent.rtt, report.rtt, PROBE_RTT_DELTA);
				}

				if (shouldSend) {
					await report.encode(stream.writer, this.version);
					lastSent = report;
					lastSentTime = now();
				}
			}
		} catch (err: unknown) {
			closeViolation(this.#quic, err);
			console.warn("probe stream error", err);
		} finally {
			stream.close();
			await reading;
		}
	}

	/**
	 * Close the session when our origin publishes a broadcast our grant does not cover;
	 * see {@link enforceGrant}.
	 *
	 * @internal
	 */
	async runEnforce(setupAnswered: Promise<void>, auth?: AuthSession): Promise<void> {
		if (!this.#grant) return;
		await enforceGrant({ quic: this.#quic, advertised: this.#advertised, grant: this.#grant, setupAnswered, auth });
	}

	/**
	 * Owes the peer `task`, a served SUBSCRIBE, FETCH, or TRACK request, until it ends.
	 *
	 * @internal
	 */
	owe(task: Promise<void>): Promise<void> {
		const tracked = task.finally(() => this.#owed.delete(tracked));
		this.#owed.add(tracked);
		return tracked;
	}

	/**
	 * Withdraws announcements and waits for every owed request to end, including any that
	 * arrive meanwhile. Rejects if a withdrawal failed; an owed request ends either way.
	 *
	 * @internal
	 */
	async drain(): Promise<void> {
		const [withdrawn] = await Promise.allSettled([
			// Lite-07: the stream finishes once every route on it is ended, which says this side is leaving.
			this.#withdrawal.close().then(async () => {
				if (hasRoutes(this.version)) await AnnounceStream.of(this.#quic, this.version).finish();
			}),
		]);
		// Only after the withdrawals, so a request that arrived while they were in flight is
		// waited for too. Owed requests run on their own meanwhile, so this adds no delay.
		while (this.#owed.size > 0) await Promise.allSettled(this.#owed);
		if (withdrawn.status === "rejected") throw withdrawn.reason;
	}

	close() {
		// The broadcasts belong to the origin, which outlives this session; closing here
		// only drops the borrow. The peer sees the unannounce when the streams die.
		this.#answer?.close();

		// Release the datagram writer's lock so the stream can be torn down.
		this.#datagramWriter?.close();
		this.#datagramWriter = undefined;
	}
}

import { Duration, type Instant, now, sleep, yieldNow } from "@moq/clock";
import { type Dispose, type Getter, Once, race, Signal } from "@moq/signals";
import type { Grant } from "../auth.ts";
import { Gate } from "../auth_gate.ts";
import type { AuthSession } from "../auth_session.ts";
import { enforceGrant } from "../auth_session.ts";
import type * as broadcast from "../broadcast.ts";
import { statsOf } from "../connection/stats.ts";
import { Withdrawal } from "../connection/withdrawal.ts";
import type * as datagram from "../datagram.ts";
import * as DatagramStream from "../datagram_stream.ts";
import {
	error,
	NotFound,
	ProtocolViolation,
	reason,
	StreamCode,
	StreamError,
	toStreamCode,
	unauthorized,
} from "../error.ts";
import type * as group from "../group.ts";
import { Cost, type Hop, routesEqual, UNKNOWN_HOP, type WireRoute } from "../hop.ts";
import type * as Instance from "../instance.ts";
import {
	type DropRun,
	datagramTrackInfo,
	finalSequence,
	type Hole,
	hiddenBelow,
	hooks,
	insertRun,
	presented,
	type Run,
	runGaps,
} from "../internal.ts";
import type { Consumer as OriginConsumer } from "../origin.ts";
import * as Path from "../path.ts";
import { readSize, sendPayload, sendPayloadFrom } from "../payload.ts";
import { Priority, sendOrder } from "../priority.ts";
import { rescale } from "../rescale.ts";
import { concat, encodeVarint, type Reader, type Stream, type StreamVersion, Writer } from "../stream.ts";
import { floorAt, type Timescale, Timestamp } from "../time.ts";
import type * as track from "../track.ts";
import type { Reported } from "../track.ts";
import { untilAborted } from "../util/abort.ts";
import { nextChange } from "../util/change.ts";
import { GivenUp } from "../util/given_up.ts";
import { max, min } from "../util/sequence.ts";
import { Budget } from "../util/yield.ts";
import type { Resolved } from "../window.ts";
import { type Advertised, type Advertisements, sameInstance, wireOf } from "../wire.ts";
import {
	type Advert,
	type AnnounceBroadcast,
	AnnounceInit,
	AnnounceOk,
	type AnnounceReply,
	type AnnounceRequest,
	decodeAnnounceReplyMaybe,
	encodeAnnounceBroadcast,
} from "./announce.ts";
import { Datagram as DatagramMessage, type Datagrams, DatagramsInfo, DatagramsUpdate } from "./datagram.ts";
import { closeViolation } from "./error.ts";
import type { Fetch } from "./fetch.ts";
import { Group as GroupMessage } from "./group.ts";
import { Probe } from "./probe.ts";
import {
	encodeSubscribeResponse,
	hasWireTime,
	LiveRanges,
	rangesOf,
	requestedRanges,
	type Subscribe,
	SubscribeDrop,
	SubscribeFinish,
	SubscribeOk,
	type SubscribeResponse,
	SubscribeSkip,
	SubscribeStart,
	SubscribeStarted,
	SubscribeUpdate,
	spanOf,
	type WireRange,
} from "./subscribe.ts";
import { TrackInfo as TrackInfoMessage, type Track as TrackMessage } from "./track.ts";
import { encodeTracksEvent, type Tracks as TracksMessage } from "./tracks.ts";
import {
	hasAnnounceId,
	hasAnnounceOk,
	hasCompression,
	hasDatagrams,
	hasDatagramTracks,
	hasEndError,
	hasLargest,
	hasLengthFirst,
	hasProbeRtt,
	hasRanges,
	hasRouteCost,
	hasRoutes,
	hasSkip,
	hasTrackStream,
	hasYield,
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

/** How a lite hop stamps a track's frames and datagrams. */
interface Stamp {
	/** The wire timescale TRACK_INFO declared (see `wireTimescale`). */
	timescale: Timescale;
	/**
	 * The timed track whose frames' own timestamps go out, at its timescale, which the wire's
	 * equals. Lite-05/06 cannot encode an untimed track, so an untimed one carries its send time
	 * instead.
	 */
	timed?: { timescale?: Timescale | null };
}

/** How `version` stamps a track with `info`, or undefined when it carries no timestamps. */
function stamping(version: Version, info: { timescale?: Timescale | null }): Stamp | undefined {
	const timescale = info.timescale ?? undefined;
	const wire = wireTimescale(version, timescale);
	if (wire === undefined) return undefined;
	return timescale === undefined ? { timescale: wire } : { timescale: wire, timed: info };
}

/** A frame or datagram's `timestamp` as its raw wire value, or its send time when the track goes out untimed. */
function wireTime(timestamp: Timestamp | undefined, stamp: Stamp): number {
	const own = stamp.timed && rescale(stamp.timed, timestamp);
	return own ? own.value : floorAt(Timestamp.now(), stamp.timescale);
}

/**
 * Settles once the peer acknowledged everything written before the FIN, or the stream failed.
 * Closing the session sooner would discard bytes still in flight.
 */
function acknowledged(writer: Writer): Promise<void> {
	return writer.closed.catch(() => {});
}

// The TRACK_INFO a TRACK stream, or a listing's TRACKS_ADD, answers for a track.
function trackInfoMessage(info: track.Info, version: Version): TrackInfoMessage {
	return new TrackInfoMessage({
		priority: info.priority,
		refresh: info.refresh ?? undefined,
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
	priority: Priority;

	/** Settles when the subscriber leaves, dropping a group still queued for a stream slot. */
	unsubscribed: Promise<void>;

	/** First frame to send; anything below it was excluded by the subscription. */
	start: number;

	/** Last frame to send (inclusive), or undefined for the rest of the group. */
	end?: number;
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
};

/**
 * The frames of `sequence` a subscription asked for, as a start index and an inclusive end.
 *
 * The frame bounds qualify the start and end group only; every other group is served whole.
 * Which groups are served at all is the serving subscriber's ranges and floor,
 * applied when a group is popped rather than re-checked here.
 *
 * The serving loop calls this synchronously after the pop, before any SUBSCRIBE_UPDATE can
 * change `bounds`. Nothing downstream trims the frame range: it is a wire request,
 * deliberately decoupled from the receiver's local read cursor.
 */
function frameRange(bounds: FrameBounds, sequence: bigint): { start: number; end?: number } {
	return {
		start: bounds.startGroup === sequence ? bounds.startFrame : 0,
		end: bounds.endGroup === sequence ? bounds.endFrame : undefined,
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
	/** The byte frame `start` is served from: only its rest follows its full-size header. */
	byte: number;
};

/** What the serving loop takes from the subscribe stream instead of serving a group. */
type Control =
	/**
	 * The peer re-stated the subscription: priority, ordering, latency, and its ranges, with the
	 * Range ID of each on lite-07.
	 */
	| { kind: "update"; update: SubscribeUpdate; ids: bigint[] }
	/** The peer FIN'd, or our own half went away. Either way there is nobody left to serve. */
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
	/** Apply an update, with every live range by Range ID on lite-07. */
	apply: (update: SubscribeUpdate, live: { id: bigint; range: WireRange }[]) => void;
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
	#update?: { update: SubscribeUpdate; ids: bigint[] };
	// Sticky, first one wins: null once the stream is over, an Error once it failed.
	#end?: Error | null;
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
		if (update) return { kind: "update", ...update };
		if (this.#end === undefined) return undefined;
		return this.#end === null ? { kind: "done" } : { kind: "error", error: this.#end };
	}

	/**
	 * Where each live Range ID sits among the track model's ranges. The decoder applies each
	 * update to the model as it decodes it, ahead of {@link take}, so this rather than the
	 * last control taken is what indexes the model.
	 */
	positions(): Map<bigint, number> {
		return new Map(this.#live.ids().map((id, index) => [id, index]));
	}

	/** Whether {@link take} has a control to answer with now. */
	pending(): boolean {
		return this.#update !== undefined || this.#end !== undefined;
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

	async #decode({ reader, version, live, apply, quic }: SubscriptionControlOptions) {
		try {
			// Runs until the peer's half ends, even past our own FIN: a lite-05+ publisher waits
			// for that end, and every other teardown stops the reader.
			for (;;) {
				const update = await SubscribeUpdate.decodeMaybe(reader, version);
				if (!update) break;
				// Each lite-07 update edits the live ranges, which only the stream's history names.
				const ranges = live.apply(update);
				apply(update, ranges);
				// Resolved in full, so the latest one replaces any the loop has yet to take.
				this.#update = { update, ids: ranges.map(({ id }) => id) };
				this.#changed.update((value) => value + 1);
			}
		} catch (err: unknown) {
			closeViolation(quic, err);
			this.#finish(error(err));
			return;
		}
		this.#finish(null);
	}
}

// `entry`, a range this subscription already held, resolved as it was: a range resolves once, so
// an update that leaves it live never counts its relative anchors back from a newer latest group,
// nor looks a time start up again in newer holdings. A start reaching back to the oldest group
// held stays as it was, since an absolute start would turn it into a floor the subscriber never
// named.
function pin(asked: WireRange, resolved: Resolved | undefined): WireRange {
	if (!resolved) return asked;
	const range = { ...asked };
	if (resolved.floored && (range.start?.relative !== undefined || range.start?.time !== undefined))
		range.start = { absolute: resolved.start };
	// An end resolves once it settles; until then it is reported open.
	if ((range.end?.relative !== undefined || range.end?.time !== undefined) && resolved.end !== undefined)
		range.end = { absolute: resolved.end };
	return range;
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
	return new Promise((resolve) => {
		let settled = false;
		const dispose: Dispose[] = [];
		const wake = () => {
			if (settled) return;
			settled = true;
			for (const close of dispose) close();
			resolve();
		};
		dispose.push(controls.changed(wake), hooks.groupChanged(subscriber, wake));
		dispose.push(drops.changed(wake));
	});
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
 * The budget to serve a peer with, given what its wire could tell us.
 *
 * A version without the field decodes as `0`, which is indistinguishable from a peer
 * genuinely asking for the live edge. Serving that as real time would discard backlog
 * a legacy subscriber never declined, so fall back to the publisher's max age alone
 * and leave enforcement to the receiver, as the IETF path does for the same reason.
 */
function servingMaxDelay(version: Version, requested: number): Duration | null {
	return carriesMaxDelay(version) ? Duration.millis(requested) : null;
}

/** Whether this version's SUBSCRIBE carries Subscriber Max Age at all. */
function carriesMaxDelay(version: Version): boolean {
	return version !== Version.DRAFT_01 && version !== Version.DRAFT_02;
}

/**
 * The floor of a subscription's read cursor for the wire serving it.
 *
 * On lite-06 it is the group the subscription named (or 0), and its max delay decides what
 * above the floor is worth delivering.
 *
 * Pre-06 wires are the exception: their drafts define an absent `Group Start` as the
 * latest group, so say so explicitly rather than letting the budget reach back. Lite-03/04/05
 * carry a `Subscriber Max Age`, but there it is a staleness tolerance only; lite-01/02 additionally get
 * an unbounded budget so nothing is dropped under them (see {@link servingMaxDelay}), which
 * must not read as a request to replay the whole cache on join.
 */
function cursorFloor(version: Version, startGroup: bigint | undefined, latest: bigint | undefined): bigint {
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

		// Grab the datagram writer up front when the transport carries datagrams (no group
		// fallback, so it stays undefined otherwise). One writer for all subscriptions.
		if (hasDatagrams(version)) {
			this.#datagramWriter = DatagramStream.datagramWriter(quic);
		}
	}

	/**
	 * Handles an announce interest message.
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
		if (hasRoutes(this.version)) return this.#runRouted(msg, stream);

		// Keyed by suffix, valued by identity plus route, so a republish diffs as a restart
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

		// Another instance is another stream, which these versions announce as an end and a start.
		const restarted = (prev: Advertised, next: Advertised): boolean => !sameInstance(prev, next);

		const announce = async (suffix: Path.Valid, route: WireRoute) => {
			console.debug(`announce: broadcast=${suffix} active=true instance=${route.instance}`);
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

		// Another instance: an end and a start.
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

		// A hidden route stays off the wire unless the request opted in.
		const carries = (covered: Path.Valid) => msg.hidden || !hiddenBelow(msg.prefix, covered);

		// What the peer currently sees: the table under the prefix, less whatever our grant
		// does not let us publish.
		const visible = (table: Advertisements): Map<Path.Valid, Advertised> => {
			const out = presented(msg.prefix, table, carries);
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

	// Lite-07: answer the peer's request from this origin's own route. A browser forwards nothing,
	// so the stream carries at most one route, this origin's, and only while it shows the peer one
	// of its claims. Mirrors `publisher/routed.rs` in rs/moq-net for a node that does not proxy.
	async #runRouted(msg: AnnounceRequest, stream: Stream) {
		const origin = this.#publish && wireOf(this.#publish);

		// The route while the peer holds it: from the first claim shown until the last one goes.
		// `certified` is whether its last floor certified, so a grant change alone re-floors it.
		let route: { id: bigint; node: bigint; seqno: bigint; certified?: boolean } | undefined;
		let nextRoute = 0n;
		let nextAnnounce = 0n;
		// Route IDs ended and not yet echoed, and the origin node each reached. No route starts
		// until every end is echoed, so a peer that rotates our identity and withholds its echoes
		// holds at most one here.
		const ending = new Map<bigint, bigint>();
		// Whether our route went out on this stream, making it a path to this origin: its close
		// then advances the seqno, so a node remembering this shorter path accepts a longer one
		// without a ROUTE_REQUEST. A new stream lengthens no path. Mirrors `links` in
		// rs/moq-net's `route/node.rs` for a node that does not forward.
		let link = false;
		// The claims the peer holds, by suffix.
		const live = new Map<Path.Valid, { id: bigint; snap: Advertised }>();

		// A hidden claim stays off the wire unless the request opted in.
		const carries = (covered: Path.Valid) => msg.hidden || !hiddenBelow(msg.prefix, covered);
		const visible = (table: Advertisements): Map<Path.Valid, Advertised> => {
			const out = presented(msg.prefix, table, carries);
			const grant = this.#grant?.peek();
			if (grant) {
				for (const suffix of [...out.keys()]) {
					if (!grant.publish.matches(Path.join(msg.prefix, suffix))) out.delete(suffix);
				}
			}
			return out;
		};
		// Whether a floor may certify the whole requested prefix: nothing the handle's scope or our
		// grant leaves out lies beneath it, since the peer cannot tell what was left out from what
		// is gone. A hidden claim left out does not count: the peer did not ask to see it.
		const certified = () => {
			const grant = this.#grant?.peek();
			if (grant && !grant.publish.contains(Path.Pattern.subtree(msg.prefix))) return false;
			return origin?.covers(msg.prefix) ?? true;
		};

		const write = (out: AnnounceBroadcast) => encodeAnnounceBroadcast(stream.writer, out, this.version);

		// End the route on the wire: it retires every claim under it.
		const end = async () => {
			if (!route) return;
			const { id, node } = route;
			route = undefined;
			live.clear();
			ending.set(id, node);
			await write({ status: "routeEnd", id });
		};

		// Bring the peer's view in line with the origin's: one batch of claim changes, closed by its
		// floor, ending the route once it shows nothing.
		const sync = async () => {
			// Read together, so the floor certifies exactly the claims it closes.
			const table = this.#advertised.peek();
			const identity = origin?.own.identity.peek();
			const revision = origin?.own.revision();
			const latest = table && identity ? visible(table) : new Map<Path.Valid, Advertised>();
			const certifies = certified();

			// A rotated identity ends the old route with every claim under it.
			if (route && route.node !== identity?.node) await end();
			if (!identity) return;
			// Shown once the peer echoes the last route's end.
			if (ending.size > 0) return;

			let changed = false;
			for (const [suffix, { id }] of [...live]) {
				if (latest.has(suffix)) continue;
				live.delete(suffix);
				changed = true;
				console.debug(`announce: broadcast=${suffix} active=false`);
				await write({ status: "endedId", id });
			}
			if (route && route.seqno !== identity.seqno) {
				route.seqno = identity.seqno;
				await write({ status: "routeUpdate", id: route.id, advert: advert(identity.seqno) });
			}
			for (const [suffix, snap] of latest) {
				const prev = live.get(suffix);
				if (prev && !restarted(prev.snap, snap) && routesEqual(onWire(prev.snap.route), onWire(snap.route)))
					continue;
				changed = true;
				if (!route) {
					route = { id: nextRoute++, node: identity.node, seqno: identity.seqno };
					link = true;
					await write({ status: "routeStart", node: identity.node, advert: advert(identity.seqno) });
				}
				const { instance, epoch, cost } = snap.route;
				if (!prev) {
					const id = nextAnnounce++;
					live.set(suffix, { id, snap });
					console.debug(`announce: broadcast=${suffix} active=true instance=${instance}`);
					await write({ status: "active", suffix, route: route.id, instance, epoch, cost });
				} else if (restarted(prev.snap, snap)) {
					live.set(suffix, { id: prev.id, snap });
					console.debug(`announce: broadcast=${suffix} restart=true instance=${instance} epoch=${epoch}`);
					await write({ status: "restart", id: prev.id, instance, epoch, cost });
				} else {
					live.set(suffix, { id: prev.id, snap });
					await write({ status: "update", id: prev.id, cost });
				}
			}
			if (!route || (!changed && route.certified === certifies)) return;
			route.certified = certifies;
			await write({ status: "floor", id: route.id, revision: certifies ? revision : undefined });
			if (live.size === 0) await end();
		};

		// The origin's own route: it starts its Hops, alone, and forwards no other.
		const advert = (seqno: bigint): Advert => ({
			seqno,
			metric: 0n,
			downOnly: false,
			rtt: 0n,
			hops: origin ? [origin.own.hop] : [],
		});
		// What the peer decodes for a claim: lite-07 carries its instance, epoch, and cost.
		const onWire = (route: WireRoute): WireRoute => ({ ...route, auth: undefined });
		const restarted = (prev: Advertised, next: Advertised): boolean =>
			!sameInstance(prev, next) || (prev.route.epoch ?? 0n) !== (next.route.epoch ?? 0n);

		// Anything that may change what the peer is owed marks the stream dirty and wakes the loop,
		// which alone writes. A change landing mid-sync leaves it dirty for the next pass.
		let dirty = true;
		let notify: (() => void) | undefined;
		const poke = () => {
			dirty = true;
			notify?.();
		};

		// Replies arrive on their own task, each applied as it lands, so a peer that keeps sending
		// while our writes stall queues nothing: an echo frees a route to start again, and a request
		// raises the identity, whose change marks the stream dirty.
		let departed: Error | null | undefined;
		const reply = (msg: AnnounceReply) => {
			if (msg.kind === "request") {
				origin?.own.request(msg.node, msg.seqno);
				return;
			}
			if (!ending.delete(msg.id)) throw new ProtocolViolation(`echo of route ${msg.id}, which is not ending`);
			poke();
		};
		void (async () => {
			try {
				for (;;) {
					const msg = await decodeAnnounceReplyMaybe(stream.reader, this.version);
					if (!msg) break;
					reply(msg);
				}
				departed = null;
			} catch (err) {
				departed = error(err);
			}
			notify?.();
		})();

		// Woken by the origin's claims, our grant, or the identity they are originated under.
		const sources = [this.#advertised, this.#grant, origin?.own.identity];
		const stops = sources.map((source) => source?.subscribe(poke));
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

			await new AnnounceOk(UNKNOWN_HOP, 0).encode(stream.writer, this.version);
			dirty = false;
			await sync();
			// Every route the peer holds has its floor by now.
			await write({ status: "synced" });

			for (;;) {
				if (departed instanceof Error) throw departed;
				if (departed === null || this.#withdrawal.closing.peek()) break;
				if (dirty) {
					dirty = false;
					await sync();
					continue;
				}
				// The origin closed: the last sync ended its claims and route, and the stream finishes.
				if (this.#advertised.peek() === undefined) break;
				const woke = new Promise<void>((resolve) => {
					notify = resolve;
				});
				await race([woke, this.#withdrawal.closing]);
				notify = undefined;
			}
			if (this.#withdrawal.closing.peek()) {
				await end();
				stream.close();
				await stream.writer.closed;
			}
		} finally {
			for (const stop of stops) stop?.();
			if (link) origin?.own.bump();
		}
	}

	/**
	 * The broadcast a request names, from this session's origin. A request naming an origin node
	 * other than this one is not ours to serve: a browser forwards nothing.
	 */
	async #resolve(
		msg: { broadcast: Path.Valid; instance?: Instance.Valid; node: bigint },
		signal: AbortSignal,
	): Promise<broadcast.Consumer | undefined> {
		const origin = this.#publish && wireOf(this.#publish);
		if (!origin) return undefined;
		if (msg.node !== 0n && msg.node !== origin.own.node) {
			throw new StreamError(StreamCode.Unroutable, { message: `origin node ${msg.node} is not this one` });
		}
		return origin.local(msg.broadcast, msg.instance) ?? (await origin.demand(msg.broadcast, msg.instance, signal));
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
		void race([stream.reader.closed, stream.writer.closed]).then(leave, leave);

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

		const consumer = front.track(msg.track);
		const subscription = (ranges: track.Range[]): track.Subscription => ({
			priority: msg.priority,
			order: msg.order,
			delay: {
				max: servingMaxDelay(this.version, msg.maxDelay),
				min: msg.minDelay === undefined ? null : Duration.millis(msg.minDelay),
			},
			ranges,
		});
		// A time anchor counts in the timescale this peer was told, so it waits for the info.
		// Meanwhile the subscription asks for nothing but holds the track it resolves, so an
		// on-demand track's request answers it, rather than one released once its info lands.
		const timed = hasWireTime(msg.ranges);
		let ranges: track.Range[] = [];
		try {
			if (!timed) ranges = requestedRanges(msg, undefined);
		} catch (err: unknown) {
			stream.writer.reset(error(err));
			return;
		}
		// Parked past a bounded window's end: the peer reads this stream's end as the track's.
		const track = hooks.parked(consumer.subscribe(subscription(ranges)));
		serving(track);
		if (timed) {
			try {
				const info = await untilAborted(track.info(), left.signal);
				track.update(subscription(requestedRanges(msg, info.timescale ?? undefined)));
			} catch (err: unknown) {
				track.close();
				stream.writer.reset(error(err));
				return;
			}
		}
		if (watch.revoked()) {
			track.close();
			return;
		}
		const latest = hooks.latestSequence(track);
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
		};
		let legacy: datagram.Consumer | undefined;
		const legacyPriority = new Signal(msg.priority);
		let controls: SubscriptionControls | undefined;
		// Lite-05 and later name every sequence below the end that this subscription never
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
				info = await untilAborted(track.info(), left.signal);
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
				for (const known of [hooks.latestSequence(track), hooks.peekLargest(track).largest?.group]) {
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
				// SUBSCRIBE_START / SUBSCRIBE_FINISH emitted from #runTrack.
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
					maxDelay: msg.maxDelay,
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

			// The SUBSCRIBE's ranges, and the Range ID of each range the model holds, in its order.
			const asked = rangesOf(msg);
			// Where each Range ID sits among the model's ranges, as of the last update applied.
			let positions = new Map(asked.map((_, index) => [BigInt(index), index]));
			controls = new SubscriptionControls({
				quic: this.#quic,
				reader: stream.reader,
				writer: stream.writer,
				version: this.version,
				live: new LiveRanges(this.version, asked),
				apply: (update, live) => {
					legacyPriority.set(update.priority);
					if (wireOf(front).localDatagrams) legacy?.setPriority(update.priority);
					if (hasRanges(this.version)) {
						const resolved = hooks.servingResolved(track);
						const ranges = live.map(({ id, range }) => {
							const index = positions.get(id);
							return index === undefined ? range : pin(range, resolved[index]);
						});
						Object.assign(update, spanOf(this.version, ranges));
						positions = new Map(live.map(({ id }, index) => [id, index]));
					}
					// Every update restates the wanted group. Past our largest it is a join, which
					// repeats the pending ask; at or below it the group already exists.
					const largest = hooks.peekLargest(track).largest?.group;
					if (update.wantedGroup !== undefined && (largest === undefined || update.wantedGroup > largest))
						hooks.joinSubscriber(track);
					track.update({
						priority: update.priority,
						order: update.order,
						delay: {
							max: servingMaxDelay(this.version, update.maxDelay),
							min: update.minDelay === undefined ? null : Duration.millis(update.minDelay),
						},
						ranges: requestedRanges(update, info.timescale ?? undefined),
					});
				},
			});
			const finished = await this.#runTrack(track, stream.writer, controls, {
				sub: msg.id,
				broadcast: msg.broadcast,
				stamp,
				publisher,
				compress,
				named,
				floor,
				bounds,
				update: msg.update,
				initial: BigInt(asked.length),
			});

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
				group = await front.track(msg.track).fetchGroup(msg.group, {
					priority: msg.priority,
					maxDelay: msg.maxDelay === undefined ? undefined : Duration.millis(msg.maxDelay),
					signal: left.signal,
				});
				serving(group);
				// Revoked while the group resolved: the watch already reset the stream.
				if (watch.revoked()) {
					hooks.releaseGroupReader(group, unauthorized(msg.broadcast));
					return;
				}
				await this.#runFetchGroup(group, stream.writer, {
					stamp: stamping(this.version, info),
					compress: info.compress,
					start: msg.startFrame,
					byte: msg.startByte,
					end: msg.endFrame,
				});
				console.debug(`fetch done: broadcast=${msg.broadcast} track=${msg.track} group=${msg.group}`);
				stream.close();
				group.close();
				await acknowledged(stream.writer);
			} catch (err: unknown) {
				const e = error(err);
				console.warn(
					`fetch error: broadcast=${msg.broadcast} track=${msg.track} group=${msg.group} error=${reason(e)}`,
				);
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
			/** The SUBSCRIBE's update number (0), which a drop echoes until an update replaces it. */
			update: bigint;
			// How many ranges the SUBSCRIBE carried, whose starts SUBSCRIBE_START reports.
			initial: bigint;
		},
	): Promise<boolean> {
		const { sub, broadcast, stamp, publisher, compress, named, bounds, initial } = serving;
		// What the wire was told of each sequence: served, or given up, and how. Kept in the
		// reader's own record, which its drops consult too. Undefined before lite-05.
		const record = named ? hooks.wire(track) : undefined;
		let updateNumber = serving.update;
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
		// first group is known, SUBSCRIBE_FINISH when the track finishes.
		const emitRange = hasTrackStream(this.version);
		let startSent = false;
		// Dispositions before START refine its boundary; later ones must be named as drops.
		const skipped: DropRun[] = [];
		// Where SUBSCRIBE_START resolved, lowered by an update that asks for groups below it.
		let startSequence: bigint | undefined;
		// The first group and the edge seen before it was popped, held until the source
		// resolves where its feed starts.
		let first: { group: group.Consumer; largest: track.Position | undefined } | undefined;
		let endSent = false;
		// The Range IDs of the last update taken, and those an update added whose start no
		// SUBSCRIBE_STARTED reported yet. Lite-07 only.
		let ids = Array.from({ length: Number(initial) }, (_, index) => BigInt(index));
		let unstarted: bigint[] = [];

		// Where each of the SUBSCRIBE's ranges starts once delivery starts at `from`, by Range ID,
		// undefined for one with no group there or an update retired: SUBSCRIBE_STARTED answers
		// each one an update added. Before lite-07 there is one range, which starts there.
		const rangeStarts = (from: bigint): (bigint | undefined)[] => {
			if (!hasRanges(this.version)) return [from];
			const starts = hooks.servingStarts(track, from);
			const positions = controls.positions();
			return Array.from({ length: Number(initial) }, (_, id) => {
				const index = positions.get(BigInt(id));
				return index === undefined ? undefined : starts[index];
			});
		};

		// Report where each range an update added starts, once it resolves, or with no start once
		// nothing can resolve it (`finished`). SUBSCRIBE_START goes first.
		const reportStarted = async (finished: boolean): Promise<boolean> => {
			if (startSequence === undefined || unstarted.length === 0) return true;
			const resolved = hooks.servingResolved(track);
			const positions = controls.positions();
			const waiting: bigint[] = [];
			for (const id of unstarted) {
				const index = positions.get(id);
				// An update not taken yet retired it, and taking that update forgets it.
				if (index === undefined) {
					waiting.push(id);
					continue;
				}
				const range = resolved[index];
				if (!range && !finished) {
					waiting.push(id);
					continue;
				}
				let start: bigint | undefined;
				if (range) {
					start = max(range.start, startSequence);
					if (range.end !== undefined && range.end <= start) start = undefined;
				}
				const started = new SubscribeStarted(id, start);
				if (!(await controls.response(encodeSubscribeResponse(stream, { started }, this.version))))
					return false;
			}
			unstarted = waiting;
			return true;
		};

		// The message naming `[start, end)` given up as `hole` says: lite-07 skips what was never
		// produced, and earlier versions drop it as not found.
		const holeMessage = (start: bigint, end: bigint, hole: Hole): SubscribeResponse =>
			hole.skip && hasSkip(this.version)
				? { skip: new SubscribeSkip({ start, end }) }
				: { drop: SubscribeDrop.given(start, end, hole.error, this.version, updateNumber) };

		// Whether lite-07 names `hole` stale: an OLD drop, which the next update reopens.
		const sentStale = (hole: Hole) => !hole.skip && toStreamCode(hole.error) === StreamCode.Old;

		// What the wire is told when `hole` goes out. Mirrors the Rust `TrackRun::report`.
		const reportOf = (hole: Hole): Omit<Reported, "start" | "end"> => {
			// Every drop is final before lite-07.
			if (!hasSkip(this.version)) return { kind: "settled" };
			if (hole.skip) return { kind: "skip" };
			// The wire names every OLD drop stale, which the next update reopens. One given up as
			// final, such as one relayed from a version whose drops are all final, is dropped again
			// by each update, as an expired stream is.
			if (sentStale(hole)) return { kind: "stale", ...(hole.stale === undefined ? { expired: true } : {}) };
			return { kind: "dropped" };
		};

		// Give up, as `hole` says, whatever part of `[start, end)` this subscription has not been
		// served or given up yet.
		const drop = async (start: bigint, end: bigint, hole: Hole): Promise<boolean> => {
			if (!record) return true;
			const report = reportOf(hole);
			for (const gap of record.untold(start, end)) {
				record.tell(gap.start, gap.end, report);
				const msg = holeMessage(gap.start, gap.end, hole);
				if (!(await controls.response(encodeSubscribeResponse(stream, msg, this.version)))) return false;
			}
			return true;
		};

		// Forward a verdict the reader reached on `[start, end)`, which its record already took in,
		// replacing what it goes out over: on lite-07, a skip any drop already sent, and a final
		// drop a stale one, which the next update would otherwise reopen. A reader the earlier drop
		// ended stays ended.
		const forward = async (start: bigint, end: bigint, hole: Hole): Promise<boolean> => {
			if (!record || start >= end) return true;
			record.tell(start, end, reportOf(hole));
			const msg = holeMessage(start, end, hole);
			return controls.response(encodeSubscribeResponse(stream, msg, this.version));
		};

		// Record `[start, end)` as named stale (OLD) in a lite-07 drop that each update drops
		// again, which a later verdict or group replaces.
		const toldStale = (start: bigint, end: bigint) => record?.tell(start, end, { kind: "stale", expired: true });

		// The track's exclusive final boundary. A Rust subscriber feeds SUBSCRIBE_FINISH
		// straight into finish_at, so it must name the track's boundary (which counts
		// datagram sequences too), not the delivered range: a subscription cap can hold
		// produced groups back. The latest() fallback covers a subscription torn down
		// before the producer declared it.
		// Lite cannot name a frame, so a mid-group end rounds up past its group, whose own
		// stream FIN says where it stops.
		const boundary = () => {
			const end = hooks.subscriberEnd(track);
			return end ? finalSequence(end) : (hooks.latestSequence(track) ?? -1n) + 1n;
		};

		// Declare the boundary immediately; groups below it can still be in flight. Lite-07 carries
		// an errored end's error with it, which older versions reset with once the end is reached.
		const sendEnd = async (): Promise<boolean> => {
			endSent = true;
			if (!emitRange) return true;
			const endError = hooks.subscriberEndError(track);
			const code = endError && hasEndError(this.version) ? toStreamCode(endError) : undefined;
			return controls.response(
				encodeSubscribeResponse(stream, { finish: new SubscribeFinish(boundary(), code) }, this.version),
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
		// Before lite-07 an errored end is only the reset at that end: a clean SUBSCRIBE_FINISH sent
		// early could let the subscriber settle before the reset arrives. A subscription capped
		// short of the end is owed no error, so its boundary goes out.
		const declareEarly = () =>
			hasEndError(this.version) ||
			!hooks.subscriberEndError(track) ||
			(bounds.endGroup !== undefined && bounds.endGroup + 1n < boundary());

		// One ranking for the whole subscription, shared by every group it serves.
		const priority = new Priority(track, publisher);

		// Every group this subscription started serving, until its stream finishes or resets.
		const groups = new Set<Promise<void>>();
		const drops = new Signal(0);
		// Groups whose stream failed, with why, for the control stream to name, as runs.
		const pendingDrops: { start: bigint; end: bigint; error: Error }[] = [];
		// The expired groups an update re-offers, still cached and in range, served again in turn.
		const reoffered: group.Consumer[] = [];
		// Lite-07: a fresh read of the group `serving` served, whose stream the session reset to make
		// room, for a new stream to continue it at the same frames; undefined once the subscription's
		// budget or the cache gave it up. The Rust publisher keeps an unread clone of the reset
		// stream's cursor instead, which a read handle here cannot make.
		const continuation = (serving: RunGroup): RunGroup | undefined => {
			const sequence = serving.group.sequence;
			// From the reset stream's Frame Start, which a cache holding only the group's tail can serve.
			const [resumed, ...extra] = hooks.reofferGroups(track, sequence, sequence + 1n, serving.start).groups;
			for (const other of extra) other.close();
			if (!resumed) return undefined;
			return { ...serving, group: resumed, givenUp: new GivenUp(resumed) };
		};

		// Cancels groups still queued for a stream slot. Only the subscriber leaving counts:
		// a track that ran out of groups still has to flush the ones already queued, and the
		// caller FINs the subscribe stream to say so.
		let finished = false;
		let unsubscribe!: () => void;
		const unsubscribed = new Promise<void>((resolve) => {
			unsubscribe = resolve;
		});
		try {
			for (;;) {
				if (startSequence !== undefined && skipped.length > 0) {
					for (const range of skipped) {
						// A stale run below the start is never named, so a wider budget may serve it. A
						// final one is named all the same: no budget brings it back, and a subscriber
						// filling below the start (a relay whose reader reaches lower) must still learn
						// its fate. Mirrors the Rust `TrackRun::resolve`.
						const below = min(range.end, startSequence);
						if (range.stale !== undefined) {
							if (range.start < below) record?.untell(range.start, below);
						} else if (!(await forward(range.start, below, range))) return false;
						if (!(await forward(max(range.start, startSequence), range.end, range))) return false;
					}
					skipped.length = 0;
				}
				if (pendingDrops.length > 0) {
					for (const { start, end, error: failed } of pendingDrops.splice(0)) {
						let error = failed;
						const old = hasSkip(this.version) && toStreamCode(error) === StreamCode.Old;
						// The pool folded the run while its stream was open, which is final for this
						// reader: no update would re-offer a stale drop on it. Mirrors the Rust `TrackRun`.
						if (old && record?.folded(end - 1n)) error = new StreamError(StreamCode.Evicted);
						// A stream expired under this subscription's budget is a stale verdict, which
						// a later verdict or group replaces as any stale drop sent, and each update drops
						// again. A re-offer an update owed again is given up now, so the end sweep never
						// names it.
						else if (old) toldStale(start, end);
						else record?.tell(start, end, reportOf({ error }));
						const msg = SubscribeDrop.given(start, end, error, this.version, updateNumber);
						if (!(await controls.response(encodeSubscribeResponse(stream, { drop: msg }, this.version))))
							return false;
					}
				}
				if (!(await reportStarted(false))) return false;
				// Control before data, matching the Rust publisher: every control decoded while
				// the loop was parked applies to the next pop, never to one already made. This
				// drain is synchronous, so nothing the decoder holds can land between the pop
				// below and its frame range.
				const control = controls.take();
				if (control) {
					switch (control.kind) {
						case "done":
							// The subscriber left. Its queued groups are pointless now, which
							// the finally below acts on since `finished` stays false. On lite-07 it
							// also FINs once it read a finished track's tail, which ends the
							// subscription as finished.
							return finished;
						case "error":
							throw control.error;
						case "update": {
							// An update may reach the finished track's history, so it is served again.
							finished = false;
							const update = control.update;
							updateNumber = update.update;
							if (hasRanges(this.version)) {
								// A range retired before its start resolved is owed no SUBSCRIBE_STARTED.
								const live = new Set(control.ids);
								unstarted = unstarted.filter((id) => live.has(id));
								const held = new Set(ids);
								for (const id of control.ids) if (id >= initial && !held.has(id)) unstarted.push(id);
								ids = control.ids;
							}
							console.debug(`subscribe update: broadcast=${broadcast} track=${track.name}`);
							// `cursorFloor` pins an unfloored pre-06 SUBSCRIBE to the latest
							// group, so an update that drops a named floor re-pins there too. On
							// an empty track the next group is the latest, whatever its sequence.
							// Lite-06 encodes an absent start as no floor, so it drops back to 0:
							// the cursor only rises on its own, and a finished group below the old
							// floor (a quiet catalog) is never served otherwise.
							const requested = bounds.startGroup;
							const start =
								update.startGroup === undefined &&
								requested !== undefined &&
								!resolvesStart(this.version)
									? (hooks.latestSequence(track) ?? 0n)
									: (update.startGroup ?? (resolvesStart(this.version) ? 0n : undefined));
							if (start !== undefined) {
								floor = start;
								// Before SUBSCRIBE_START nothing is owed yet; the start accounts for it.
								// A lowered floor owes the groups it newly asks for, so the end sweep
								// covers them. A raised one sends no early drop: the subscriber already
								// stops owing below its floor, and a later update may lower it again to
								// reopen those groups.
								if (startSequence !== undefined) startSequence = min(startSequence, start);
							}
							bounds.startGroup = update.startGroup;
							bounds.startFrame = update.startFrame;
							bounds.endGroup = update.endGroup;
							bounds.endFrame = update.endFrame;
							moveCursor();
							// Lite-07 reopens every stale drop on any update, judged under its new terms
							// (applied as the update was decoded): what was reported stale is offered
							// again, and dropped again under this update's number if still stale.
							if (hasSkip(this.version)) {
								const owe = (start: bigint, end: bigint) => record?.untell(start, end);
								for (const run of hooks.reofferStale(track)) owe(run.start, run.end);
								// What each update drops again: the stale runs whose stream expired, or
								// that went out OLD for good.
								const expired: Run[] = record?.expired() ?? [];
								// A stale drop not sent yet, such as one an earlier update left to drop
								// again, is judged again under these terms too.
								for (const pending of pendingDrops.filter(
									(d) => toStreamCode(d.error) === StreamCode.Old,
								))
									insertRun(expired, pending.start, pending.end);
								pendingDrops.splice(
									0,
									pendingDrops.length,
									...pendingDrops.filter((d) => toStreamCode(d.error) !== StreamCode.Old),
								);
								// A group re-offered that has not opened yet is judged again too, since these
								// terms may no longer admit it.
								for (const group of reoffered.splice(0)) {
									insertRun(expired, group.sequence, group.sequence + 1n);
									group.close();
								}
								// A group whose stream already reset as stale: one still cached, in range, and
								// fresh under the new budget is served again; the rest is dropped again, by run.
								for (const run of expired.splice(0)) {
									const { groups, givenUp } = hooks.reofferGroups(track, run.start, run.end);
									for (const group of groups) {
										owe(group.sequence, group.sequence + 1n);
										reoffered.push(group);
									}
									for (const gone of givenUp)
										pendingDrops.push({ ...gone, error: new StreamError(StreamCode.Old) });
								}
							}
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
				const largest =
					!startSent && hasLargest(this.version)
						? newer(first?.largest, newer(hooks.bufferedLargest(track), hooks.peekLargest(track).largest))
						: undefined;
				// A group an update re-offers goes ahead of new ones.
				const again = first ? undefined : reoffered.shift();
				const recv = first
					? ({ kind: "group", group: first.group } as const)
					: again
						? ({ kind: "group", group: again } as const)
						: hooks.tryRecvGroup(track, emitRange);
				first = undefined;
				switch (recv.kind) {
					case "error":
						throw recv.error;
					case "drop":
						if (!startSent) skipped.push(recv);
						else if (!(await forward(recv.start, recv.end, recv))) return false;
						continue;
					case "idle":
						// A start past everything the track has (a subscriber resuming just after
						// what it holds) is answered at once with the largest position, on
						// versions that carry it: a quiet track may not reach that start for a
						// while, and the subscriber judges what it holds against the answer.
						if (emitRange && !startSent && hasLargest(this.version) && bounds.startGroup !== undefined) {
							const startFrame = bounds.startFrame;
							if (
								largest !== undefined &&
								(bounds.startGroup > largest.group ||
									(bounds.startGroup === largest.group && startFrame > largest.frame))
							) {
								startSent = true;
								startSequence = bounds.startGroup;
								raiseFloor(bounds.startGroup);
								const start = new SubscribeStart(rangeStarts(bounds.startGroup), largest);
								if (
									!(await controls.response(encodeSubscribeResponse(stream, { start }, this.version)))
								)
									return false;
								continue;
							}
						}
						// A lite-07 window whose bounded ranges all resolved empty owes nothing: say so at
						// once, and the subscription ends as covered below. No group would ever answer it.
						if (emitRange && !startSent && hasRanges(this.version)) {
							const end = hooks.servingEnd(track);
							if (end !== undefined && hooks.servingRuns(track, 0n, end).length === 0) {
								startSent = true;
								startSequence = end;
								const start = new SubscribeStart(rangeStarts(end), largest);
								if (
									!(await controls.response(encodeSubscribeResponse(stream, { start }, this.version)))
								)
									return false;
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
						if (keepsSubscribeOpen(this.version) && !startSent) {
							const end = hooks.servingEnd(track);
							const runs = end === undefined ? [] : hooks.servingRuns(track, 0n, end);
							const given: Run[] = [];
							for (const range of skipped) insertRun(given, range.start, range.end);
							if (
								runs.length > 0 &&
								runs.every((run) => runGaps(given, run.start, run.end).length === 0)
							) {
								startSent = true;
								startSequence = runs[0].start;
								const start = new SubscribeStart(rangeStarts(startSequence), largest);
								if (
									!(await controls.response(encodeSubscribeResponse(stream, { start }, this.version)))
								)
									return false;
								continue;
							}
						}
						await waitForSubscription(controls, track, drops);
						continue;
					case "boundary":
						// The producer finished but is still holding groups above the cap.
						// Declare the boundary, then wait for an update to release them.
						if (!endSent && declareEarly()) {
							if (!(await sendEnd())) return false;
							continue;
						}
						await waitForSubscription(controls, track, drops);
						continue;
					case "done": {
						const endError = hooks.subscriberEndError(track);
						if (
							endError &&
							!hasEndError(this.version) &&
							(bounds.endGroup === undefined || bounds.endGroup + 1n >= boundary())
						)
							throw endError;
						// Lite-07 sends no FIN to say nothing was served, so a track that ended before
						// any group resolved the start still says where delivery starts; the loop then
						// names what it held back, and the drops below account for the rest.
						if (keepsSubscribeOpen(this.version) && !startSent && boundary() > 0n) {
							startSent = true;
							startSequence = hooks.servingRuns(track, 0n, boundary())[0]?.start ?? boundary();
							const start = new SubscribeStart(rangeStarts(startSequence), largest);
							if (!(await controls.response(encodeSubscribeResponse(stream, { start }, this.version))))
								return false;
							continue;
						}
						// Nothing arrives to resolve a range still waiting on the live feed.
						if (!(await reportStarted(true))) return false;
						// Name every sequence from the start to the end this subscription never
						// got. A cap below the end bounds what the subscriber is owed, as on its side.
						if (startSequence !== undefined) {
							// A sequence the publisher declared skipped went out as a skip already; one
							// it never declared might have existed, so it is not found.
							const unserved: Hole = {
								error: new StreamError(StreamCode.NotFound, { message: "group not served" }),
							};
							// Only the groups inside the ranges are owed.
							for (const run of hooks.servingRuns(track, startSequence, boundary())) {
								if (!(await drop(run.start, run.end, unserved))) return false;
							}
						}
						if (!endSent) {
							if (!(await sendEnd())) return false;
							continue;
						}
						if (!(await drained())) return false;
						if (pendingDrops.length > 0) continue;
						// Lite-07 keeps the stream open past SUBSCRIBE_FINISH: an update may still ask
						// for the ended track's history, and the subscriber FINs once it wants nothing
						// more.
						if (keepsSubscribeOpen(this.version)) {
							finished = true;
							if (!controls.pending()) await waitForControl(controls);
							continue;
						}
						finished = true;
						return true;
					}
				}

				const group = recv.group;
				// Never a second stream for a sequence this subscription already accounted for
				// (served, sent as a datagram, or dropped): before lite-07 a subscriber settles a
				// drop for good. A stale drop a lite-07 update reopened is owed again (see the update above).
				// A lite-07 stale drop is no final verdict, and a Group Stream wins over it, so a group
				// handed out after one is served, as one a route the front switched to delivers: the
				// update that reopens the drop would never re-offer it.
				// The pool folded a re-offered group while it waited, which is final for this reader:
				// tell the wire it is gone, since an update re-offered it. Mirrors the Rust `TrackRun`.
				if (again && record?.folded(group.sequence)) {
					group.close();
					const evicted = new StreamError(StreamCode.Evicted);
					const msg = SubscribeDrop.given(
						group.sequence,
						group.sequence + 1n,
						evicted,
						this.version,
						updateNumber,
					);
					if (!(await controls.response(encodeSubscribeResponse(stream, { drop: msg }, this.version))))
						return false;
					continue;
				}
				if (record?.toldStale(group.sequence)) {
					record.replaceDrops(group.sequence, group.sequence + 1n);
				} else if (record?.told(group.sequence)) {
					group.close();
					continue;
				}
				const range = frameRange(bounds, group.sequence);

				if (emitRange && !startSent) {
					const source = hooks.sourceStart(track);
					// A group stream can beat the upstream START on its own stream, and resolving
					// from the group alone would give up the backlog that START still promises.
					if (source === "pending") {
						first = { group, largest };
						await waitForSubscription(controls, track, drops);
						continue;
					}
					startSent = true;
					// Newest-first delivery can put a group ahead of where the source's feed
					// starts. Preserve that start so a resumed subscriber still waits for the backlog.
					startSequence = min(group.sequence, source ?? group.sequence);
					// A group given up cannot be where the feed starts. Adjacent runs given up with
					// different errors stay separate, so walk them in order.
					skipped.sort((a, b) => (a.start < b.start ? -1 : a.start > b.start ? 1 : 0));
					for (const range of skipped) {
						if (range.start <= startSequence && range.end > startSequence)
							startSequence = min(range.end, group.sequence);
					}
					// A later group above an explicit floor is still delivered, so the cursor stays put.
					// A pre-06 subscribe that named no group is the exception: an absent
					// Group Start is the latest group, and this sequence becomes the floor.
					// Lite-06 encodes a floor of group 0 as 0, so an omitted start is not pinned.
					if (bounds.startGroup === undefined && !resolvesStart(this.version)) raiseFloor(startSequence);
					if (
						!(await controls.response(
							encodeSubscribeResponse(
								stream,
								{ start: new SubscribeStart(rangeStarts(startSequence), largest) },
								this.version,
							),
						))
					)
						return false;
				}
				record?.tell(group.sequence, group.sequence + 1n, { kind: "settled" });

				const options: RunGroup = {
					sub,
					group,
					givenUp: new GivenUp(group),
					stamp,
					compress,
					priority,
					unsubscribed,
					start: range.start,
					end: range.end,
				};
				// The FIN waits for every stream to finish or reset, including one that has not opened yet,
				// and a paused group's continuation.
				const task = (async () => {
					let serving = options;
					for (;;) {
						const writer = await this.#openGroup(serving);
						let failed: Error | undefined;
						try {
							failed = writer instanceof Error ? writer : await this.#serveGroup(writer, serving);
						} finally {
							serving.givenUp.close();
						}
						if (!failed) return;
						// Lite-07: the session reset the stream to make room, which only pauses the group.
						// The subscriber keeps what arrived, and a new stream continues the group from
						// the reset one's Frame Start, owing no drop. Mirrors the Rust `GroupServe::settle_failed`.
						if (!(writer instanceof Error) && writer.preempted && hasYield(this.version)) {
							// The group's own failure, such as the publisher's abort or an eviction, ends
							// it with that error, as the Rust continuation reads it off the group. Read
							// before the release replaces it with the reset's.
							const fate = serving.group.closed.peek();
							hooks.releaseGroupReader(serving.group, failed);
							const resumed = fate instanceof Error ? undefined : continuation(serving);
							if (resumed) {
								console.debug(
									`continuing paused group: track=${track.name} sequence=${group.sequence} frame_start=${serving.start}`,
								);
								serving = resumed;
								continue;
							}
							// Given up meanwhile, such as by the subscription's budget: the usual stale drop.
							failed = fate instanceof Error ? fate : new StreamError(StreamCode.Old);
						}
						if (emitRange) {
							pendingDrops.push({ start: group.sequence, end: group.sequence + 1n, error: failed });
							drops.update((revision) => revision + 1);
						}
						return;
					}
				})();
				groups.add(task);
				void task.finally(() => {
					groups.delete(task);
				});
			}
		} finally {
			if (!finished) unsubscribe();
			// An update's re-offers are reads of their own, which closing the track leaves open.
			for (const group of reoffered.splice(0)) group.close();
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
					encodeTracksEvent(
						stream.writer,
						event.kind === "added" ? { ...event, info: trackInfoMessage(event.info, this.version) } : event,
						this.version,
					),
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
			await encodeSubscribeResponse(stream.writer, { start: new SubscribeStart([0n]) }, this.version);
			const end = await untilAborted(
				this.#runDatagrams(track, { sub: msg.id, stamp: stamping(this.version, info) }),
				signal,
			);
			await encodeSubscribeResponse(stream.writer, { finish: new SubscribeFinish(end) }, this.version);
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
				const stamp = options.stamp;
				// An old subscription uses its stream track's timescale for both kinds.
				const timestamp =
					stamp === undefined
						? undefined
						: datagram.timestamp === undefined
							? floorAt(Timestamp.now(), stamp.timescale)
							: floorAt(datagram.timestamp, stamp.timescale);
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
		{ stamp, compress, start: startFrame, byte, end: endFrame }: ServeFetch,
	) {
		// A compressed frame inflates only from its first byte; no subscriber asks for less.
		if (byte > 0 && compress) throw new Error("a compressed frame is served whole");
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

		let prevTs = 0n;
		for (let index = startFrame; endFrame === undefined || index <= endFrame; index++) {
			const read = await race([hooks.readGroupFrame(group, compress), stream.closed]);
			if (!read) break;
			const frame = read.frame;

			let delta: bigint | undefined;
			if (stamp !== undefined) {
				const ts = BigInt(wireTime(frame.timestamp, stamp));
				delta = ts - prevTs;
				prevTs = ts;
			}

			// The first frame states its full size, then carries only the bytes from `byte`.
			const skip = index === startFrame ? byte : 0;
			const size = readSize(read);
			if (skip > size) throw new Error(`fetch start byte ${skip} past the frame's ${size}`);
			await stream.write(frameHeader(stream.version, delta, read.inflated, size, hasLengthFirst(this.version)));
			if (read.chunks) await sendPayloadFrom(read.chunks, skip, (chunk) => stream.write(chunk), stream.closed);
			else if (frame.payload.length > skip) await stream.write(frame.payload.subarray(skip));
		}
	}

	/**
	 * Opens the unidirectional stream for one group, or closes the group and resolves
	 * an error when it cannot get one.
	 *
	 * @internal
	 */
	async #openGroup(options: RunGroup): Promise<Writer | Error> {
		const { group, givenUp, priority, unsubscribed, start } = options;
		try {
			// The transport drains streams by send order, and opens wait for stream credit in
			// it, so this is what makes a high-priority track (and a newer group within it) win
			// the link and the peer's credit when there isn't room for both. The place ranks the
			// open against the subscription's other groups while it waits.
			// Watch the group while the open waits, and release that watch once it settles.
			const place = priority.reserve(group.sequence, start);
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
					// Lite-07: a reset to make room pauses the group, which a new stream continues.
					yields: hasYield(this.version),
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
	 * Serves one group on the stream {@link #openGroup} opened for it.
	 *
	 * @internal
	 */
	async #serveGroup(stream: Writer, options: RunGroup) {
		const { sub, group, givenUp, stamp, compress, priority, start: startFrame, end: endFrame } = options;
		// The header names the requested position, so a copy missing it must be refused.
		const msg = new GroupMessage({ subscribe: sub, sequence: group.sequence, frameStart: startFrame });
		// The subscription's budget, or an eviction of what is left to read, may give up on the
		// group mid-write, which abandons the stream. A group already given up never starts the write.
		const write = async (op: () => Promise<unknown>) => {
			const stop = await givenUp.write(op);
			if (stop) throw stop;
		};
		// Everything past this point runs inside the cleanup scope, so a failure never leaves
		// a finished group's stream being ranked.
		try {
			if (hooks.groupIndex(group) > startFrame) throw new Error(`group lacks frame ${startFrame}`);
			// A SUBSCRIBE_UPDATE re-ranks the subscription, so a group already on the wire
			// follows it too rather than keeping a stale rank until it finishes.
			priority.add(stream, group.sequence, startFrame);

			await write(async () => {
				await stream.u53(0); // stream type
				await msg.encode(stream, this.version);
			});

			let prevTs = 0n;
			// Whether the cursor ever reached the requested start, which decides how the
			// end of the group is read below. A received tail may already begin there.
			let reached = hooks.groupIndex(group) === startFrame;

			for (;;) {
				const read = await race([hooks.readGroupFrame(group, compress), stream.closed]);
				if (!read) {
					// The group ended before the frame the subscriber asked to start
					// at, so this publisher can't serve the range at all. FINning here
					// would claim an empty group under that index; reset so it reads
					// as the gap it is.
					if (!reached) throw new Error(`group ended before frame ${startFrame}`);
					break;
				}

				// A group that ends exactly at the start is a valid, empty range.
				if (read.sequence + 1 >= startFrame) reached = true;
				// Frames below the requested start were excluded, and the receiver
				// numbers what it gets from `startFrame`.
				if (read.sequence < startFrame) continue;
				if (endFrame !== undefined && read.sequence > endFrame) break;

				let delta: bigint | undefined;
				if (stamp !== undefined) {
					// Convert each frame to the track's wire timescale.
					const ts = BigInt(wireTime(read.frame.timestamp, stamp));
					delta = ts - prevTs;
					prevTs = ts;
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

			stream.close();
			group.close();
			// A stream that fails after its FIN may have lost its header, so report it for a DROP.
			// Its bytes still hold the connection until acknowledged, so a group gone stale
			// meanwhile is reset like one still serving, as the Rust `GroupServe` does.
			await write(() => stream.closed);
			return undefined;
		} catch (err: unknown) {
			const e = error(err);
			stream.reset(e);
			// A reset that pauses the group leaves the reader to the caller, which reads its fate
			// first. A reset stream is never preempted after, so the caller sees the same flag.
			if (!(stream.preempted && hasYield(this.version))) hooks.releaseGroupReader(group, e);
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
		const [withdrawn] = await Promise.allSettled([this.#withdrawal.close()]);
		// Only after the withdrawals, so a request that arrived while they were in flight is
		// waited for too. Owed requests run on their own meanwhile, so this adds no delay.
		while (this.#owed.size > 0) await Promise.allSettled(this.#owed);
		if (withdrawn.status === "rejected") throw withdrawn.reason;
	}

	close() {
		// The broadcasts belong to the origin, which outlives this session; closing here
		// only drops the borrow. The peer sees the unannounce when the streams die.

		// Release the datagram writer's lock so the stream can be torn down.
		this.#datagramWriter?.close();
		this.#datagramWriter = undefined;
	}
}

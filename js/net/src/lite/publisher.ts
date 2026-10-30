import { Instant, Milli, Nano, now, sleep, yieldNow } from "@moq/clock";
import { type Dispose, type Getter, Once, race, Signal } from "@moq/signals";
import type { Grant } from "../auth.ts";
import { Gate } from "../auth_gate.ts";
import type { AuthSession } from "../auth_session.ts";
import { enforceGrant } from "../auth_session.ts";
import type * as broadcast from "../broadcast.ts";
import { Withdrawal } from "../connection/withdrawal.ts";
import * as DatagramStream from "../datagram_stream.ts";
import { error, NotFound, ProtocolViolation, reason, StreamCode, StreamError, unauthorized } from "../error.ts";
import { stored } from "../flate/group.ts";
import type * as group from "../group.ts";
import { Cost, type Hop, type Route, routesEqual } from "../hop.ts";
import { finalSequence, hiddenBelow, hooks, insertRun, presented, type Run, runGaps } from "../internal.ts";
import type { Consumer as OriginConsumer } from "../origin.ts";
import * as Path from "../path.ts";
import { Priority, sendOrder } from "../priority.ts";
import { concat, encodeVarint, type Reader, type Stream, type StreamVersion, Writer } from "../stream.ts";
import { type Timescale, Timestamp } from "../time.ts";
import type * as track from "../track.ts";
import { untilAborted } from "../util/abort.ts";
import { nextChange } from "../util/change.ts";
import { untilGivenUp } from "../util/given_up.ts";
import { type Advertised, type Advertisements, sameInstance, wireOf } from "../wire.ts";
import { AnnounceInit, AnnounceOk, type AnnounceRequest, encodeAnnounceBroadcast } from "./announce.ts";
import { Datagram as DatagramMessage } from "./datagram.ts";
import { closeViolation } from "./error.ts";
import type { Fetch } from "./fetch.ts";
import { Group as GroupMessage } from "./group.ts";
import { Probe } from "./probe.ts";
import {
	encodeSubscribeResponse,
	exclusiveGroupEnd,
	type RangeGroups,
	type Subscribe,
	SubscribeDrop,
	SubscribeEnd,
	SubscribeOk,
	SubscribeStart,
	SubscribeUpdate,
} from "./subscribe.ts";
import { TrackInfo as TrackInfoMessage, type Track as TrackMessage } from "./track.ts";
import { encodeTracksEvent, type Tracks as TracksMessage } from "./tracks.ts";
import {
	hasAnnounceId,
	hasAnnounceOk,
	hasAnnounceRestart,
	hasCompression,
	hasDatagrams,
	hasLargest,
	hasProbeRtt,
	hasRouteCost,
	hasSynced,
	hasTrackStream,
	resolvesStart,
	Version,
	waitsForSubscriberFin,
	wireTimescale,
} from "./version.ts";

const PROBE_INTERVAL = 100; // ms
const PROBE_MAX_AGE = 10_000; // ms
const PROBE_MAX_DELTA = 0.25;
const PROBE_RTT_DELTA = 0.25;

/** Map a signed delta to an unsigned zigzag varint value (mirrors Rust `varint::zigzag`). */
function zigzag(delta: bigint): bigint {
	return delta >= 0n ? delta << 1n : (-delta << 1n) - 1n;
}

/**
 * A frame's fields ahead of its payload, built in memory for one write: a stream write is far
 * costlier than a varint, and a backlog pays it for every frame. The timestamp `delta` is present
 * when the track is stamped, and `inflated` when the frame was decompressed.
 */
function frameHeader(version: StreamVersion, delta: bigint | undefined, inflated: number | undefined, size: number) {
	const parts: Uint8Array[] = [];
	if (delta !== undefined) parts.push(encodeVarint(zigzag(delta), version));
	if (inflated !== undefined) parts.push(encodeVarint(inflated, version));
	parts.push(encodeVarint(size, version));
	return concat(parts);
}

/** How a lite hop stamps a track's frames and datagrams. */
interface Stamp {
	/** The wire timescale TRACK_INFO declared (see `wireTimescale`). */
	timescale: Timescale;
	/**
	 * Whether each frame's own timestamp goes out. Lite-05/06 cannot encode an untimed track, so
	 * one served untimed carries its send time instead, unchecked ones included.
	 */
	timed: boolean;
}

/** The timescale a moq-lite hop serves a track at: none for an {@link track.Info.ietf} one. */
function liteTimescale(info: track.Info): Timescale | undefined {
	return info.ietf === undefined ? (info.timescale ?? undefined) : undefined;
}

/** How `version` stamps a track served at `timescale` (see `liteTimescale`), or undefined when it carries none. */
function stamping(version: Version, timescale: Timescale | undefined): Stamp | undefined {
	const wire = wireTimescale(version, timescale);
	return wire === undefined ? undefined : { timescale: wire, timed: timescale !== undefined };
}

/** A frame or datagram's `timestamp` as its raw wire value, or its send time when the track goes out untimed. */
function wireTime(timestamp: Timestamp | undefined, stamp: Stamp): number {
	return Math.round(((stamp.timed ? timestamp : undefined) ?? Timestamp.now()).as(stamp.timescale));
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
		refresh: info.refresh,
		// Publisher Max Age: the publisher's retention bound, advertised so relays re-serve
		// with the same window.
		maxAge: info.maxAge,
		// `#serveGroup` emits each frame converted to it. Unchecked timestamps never enter
		// moq-lite, whose receivers check them.
		timescale: liteTimescale(info),
		// An older peer is served the frames inflated.
		compress: hasCompression(version) && !!info.compress,
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
	startGroup?: number;
	/** First frame to send within {@link startGroup}; every other group starts at 0. */
	startFrame: number;
	/** The group {@link endFrame} qualifies, if the subscription named one. */
	endGroup?: number;
	/** Last frame (inclusive) to send within {@link endGroup}; every other group runs to its end. */
	endFrame?: number;
};

/**
 * The frames of `sequence` a subscription asked for, as a start index and an inclusive end.
 *
 * The frame bounds qualify the start and end group only; every other group is served whole.
 * Which groups are served at all is the subscriber's read cursor (`withRange`),
 * applied when a group is popped rather than re-checked here.
 *
 * The serving loop calls this synchronously after the pop, before any SUBSCRIBE_UPDATE can
 * change `bounds`. Nothing downstream trims the frame range: it is a wire request,
 * deliberately decoupled from the receiver's local read cursor.
 */
function frameRange(bounds: FrameBounds, sequence: number): { start: number; end?: number } {
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
type ServeFetch = Omit<ServeGroup, "sub">;

/** What the serving loop takes from the subscribe stream instead of serving a group. */
type Control =
	/** The peer re-stated the subscription: priority, ordering, latency, and both ranges. */
	| { kind: "update"; update: SubscribeUpdate }
	/** The peer FIN'd, or our own half went away. Either way there is nobody left to serve. */
	| { kind: "done" }
	/** The stream failed; the subscription goes down with it. */
	| { kind: "error"; error: Error };

type SubscriptionControlOptions = {
	quic: WebTransport;
	reader: Reader;
	writer: Writer;
	version: Version;
	apply: (update: SubscribeUpdate) => void;
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
	#update?: SubscribeUpdate;
	// Sticky, first one wins: null once the stream is over, an Error once it failed.
	#end?: Error | null;
	#ended: Promise<Error | null>;
	#resolveEnd!: (end: Error | null) => void;
	#changed = new Signal(0);

	/** Settles once decoding stops, so teardown can wait for it rather than leaving it running. */
	readonly decoding: Promise<void>;

	constructor(options: SubscriptionControlOptions) {
		const { writer } = options;
		this.#writer = writer;
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
		if (update) return { kind: "update", update };
		if (this.#end === undefined) return undefined;
		return this.#end === null ? { kind: "done" } : { kind: "error", error: this.#end };
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

	async #decode({ reader, version, apply, quic }: SubscriptionControlOptions) {
		try {
			// Runs until the peer's half ends, even past our own FIN: a lite-07 publisher waits
			// for that end, and every other teardown stops the reader.
			for (;;) {
				const update = await SubscribeUpdate.decodeMaybe(reader, version);
				if (!update) break;
				apply(update);
				this.#update = update;
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

// Where each range starts once delivery starts at `from`, `undefined` for one with no group
// there. Before lite-07 there is one range, which starts there.
function rangeStarts(ranges: (RangeGroups | undefined)[] | undefined, from: number): (number | undefined)[] {
	if (!ranges) return [from];
	return ranges.map((range) => {
		if (!range) return undefined;
		const start = Math.max(range.start, from);
		return range.end === undefined || range.end > start ? start : undefined;
	});
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

/**
 * The budget to serve a peer with, given what its wire could tell us.
 *
 * A version without the field decodes as `0`, which is indistinguishable from a peer
 * genuinely asking for the live edge. Serving that as real time would discard backlog
 * a legacy subscriber never declined, so fall back to a window wide enough not to drop
 * and leave enforcement to the receiver, as the IETF path does for the same reason.
 */
function servingMaxDelay(version: Version, requested: number | undefined): number {
	return carriesMaxDelay(version) ? (requested ?? 0) : Number.MAX_SAFE_INTEGER;
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
function cursorFloor(version: Version, startGroup: number | undefined, latest: number | undefined): number {
	if (startGroup !== undefined) return startGroup;
	if (resolvesStart(version)) return 0;
	return latest ?? 0;
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

	// The one writer for the outbound datagram stream (getWriter locks it), acquired once at
	// construction when this version + transport carry datagrams, released in close(). Its
	// presence is the gate: undefined means datagrams aren't served on this connection. All
	// subscriptions share it, since a second getWriter on the same stream would throw.
	#datagramWriter?: WritableStreamDefaultWriter<Uint8Array>;

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

		// Keyed by suffix, valued by identity plus route, so a republish diffs as a restart
		// and a re-price as an update.
		let active = new Map<Path.Valid, Advertised>();

		// Lite06+: announce ids. Every active we send implicitly assigns the next per-stream
		// ordinal; ended/update/restart reference the id instead of repeating the path.
		let nextAnnounceId = 0n;
		const announceIds = new Map<Path.Valid, bigint>();

		const wireHops = (route: Route): Hop[] => {
			if (hasAnnounceOk(this.version)) return route.hops;
			return [...route.hops, this.hop];
		};

		// What the peer decodes for a route: pre-lite-06 wires carry no cost, so a re-price
		// there must not restart.
		// moq-lite carries no per-request auth, so a swap alone sends nothing.
		const onWire = (route: Route): Route => ({
			...route,
			cost: hasRouteCost(this.version) ? route.cost : Cost.zero,
			auth: undefined,
		});

		const announce = async (suffix: Path.Valid, route: Route) => {
			console.debug(`announce: broadcast=${suffix} active=true epoch=${route.epoch}`);
			if (hasAnnounceId(this.version)) announceIds.set(suffix, nextAnnounceId++);
			await encodeAnnounceBroadcast(
				stream.writer,
				{ status: "active", suffix, epoch: route.epoch, hops: wireHops(route), cost: route.cost },
				this.version,
			);
		};

		const update = async (suffix: Path.Valid, route: Route) => {
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

		// Another publisher instance: ANNOUNCE_RESTART on lite-07, an end and a start before it.
		const restart = async (suffix: Path.Valid, route: Route) => {
			const id = announceIds.get(suffix);
			if (id === undefined || !hasAnnounceRestart(this.version)) {
				await retract(suffix);
				await announce(suffix, route);
				return;
			}
			console.debug(`announce: broadcast=${suffix} restart=true epoch=${route.epoch}`);
			await encodeAnnounceBroadcast(
				stream.writer,
				{ status: "restart", id, epoch: route.epoch, hops: wireHops(route), cost: route.cost },
				this.version,
			);
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

			if (hasSynced(this.version)) {
				dispose();
				const pending = new Map<Path.Valid, { end: boolean; next?: Advertised }>();
				const closed = new Once<null | Error>();
				void stream.reader.closed.then(
					() => closed.set(null),
					(err: unknown) => closed.set(error(err)),
				);
				let wake: (() => void) | undefined;
				let ended = false;
				const queue = () => {
					const table = this.#advertised.peek();
					ended = table === undefined;
					const latest = table ? visible(table) : new Map<Path.Valid, Advertised>();
					for (const suffix of pending.keys()) {
						if (!active.has(suffix) && !latest.has(suffix)) pending.delete(suffix);
					}
					for (const suffix of active.keys()) {
						if (!latest.has(suffix)) pending.set(suffix, { end: true });
					}
					for (const [suffix, snap] of latest) {
						const prev = active.get(suffix);
						const queued = pending.get(suffix);
						if (
							queued?.end ||
							!prev ||
							!sameInstance(prev, snap) ||
							!routesEqual(onWire(prev.route), onWire(snap.route))
						) {
							pending.set(suffix, { end: queued?.end ?? false, next: snap });
						} else {
							pending.delete(suffix);
						}
					}
					wake?.();
				};
				const stopTable = this.#advertised.subscribe(queue);
				const stopGrant = this.#grant?.subscribe(queue);
				dispose = () => {
					stopTable();
					stopGrant?.();
				};
				queue();
				let remaining: number | undefined = pending.size;
				await new AnnounceOk(this.hop, 0).encode(stream.writer, this.version);
				for (;;) {
					if (remaining !== undefined && (remaining === 0 || pending.size === 0)) {
						remaining = undefined;
						await encodeAnnounceBroadcast(stream.writer, { status: "synced" }, this.version);
					}
					const departed = closed.peek();
					if (departed instanceof Error) throw departed;
					if (this.#withdrawal.closing.peek() || departed === null || (ended && pending.size === 0)) break;
					const first = pending.entries().next();
					if (first.done) {
						const changed = new Promise<"changed">((resolve) => {
							wake = () => resolve("changed");
						});
						if ((await race([changed, stream.reader.closed, this.#withdrawal.closing])) !== "changed")
							break;
						continue;
					}
					const [suffix, entry] = first.value;
					if (entry.end) {
						if (entry.next) pending.set(suffix, { end: false, next: entry.next });
						else pending.delete(suffix);
						active.delete(suffix);
						await retract(suffix);
					} else if (entry.next) {
						pending.delete(suffix);
						const prev = active.get(suffix);
						active.set(suffix, entry.next);
						if (!prev) await announce(suffix, entry.next.route);
						else if (!sameInstance(prev, entry.next)) await restart(suffix, entry.next.route);
						else await update(suffix, entry.next.route);
					}
					if (remaining !== undefined) remaining -= 1;
				}
				if (this.#withdrawal.closing.peek()) {
					if (remaining !== undefined)
						await encodeAnnounceBroadcast(stream.writer, { status: "synced" }, this.version);
					for (const suffix of active.keys()) await retract(suffix);
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
					} else if (!sameInstance(prev, snap)) {
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

		if (msg.ranges?.some((range) => range?.unresolved)) {
			stream.abort(new Error("relative and time subscription anchors are not supported"));
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
			front =
				this.#publish &&
				(wireOf(this.#publish).local(msg.broadcast, msg.epoch) ??
					(await wireOf(this.#publish).demand(msg.broadcast, msg.epoch, left.signal)));
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

		const endGroup = exclusiveGroupEnd(msg.endGroup);
		const track = front.track(msg.track).subscribe({
			priority: msg.priority,
			order: msg.order,
			maxDelay: Milli(servingMaxDelay(this.version, msg.maxDelay)),
			groups: {
				start: msg.startGroup === undefined ? undefined : { included: msg.startGroup },
				end: endGroup === undefined ? undefined : { excluded: endGroup },
			},
		});
		serving(track);
		const floor = cursorFloor(this.version, msg.startGroup, hooks.latestSequence(track));
		track.withRange({
			start: { included: floor },
			end: endGroup === undefined ? undefined : { excluded: endGroup },
		});

		// The best-effort datagram loop, started once serving begins. It parks when the
		// track finishes (recvDatagram returns undefined), so #runTrack alone ends the
		// subscription; awaited during teardown so it doesn't outlive the subscription.
		let datagrams = Promise.resolve();
		const bounds: FrameBounds = {
			startGroup: msg.startGroup,
			startFrame: msg.startFrame,
			endGroup: msg.endGroup,
			endFrame: msg.endFrame,
		};
		// Datagram reads continue while a reliable response is blocked, so they use
		// the latest decoded bounds rather than the reliable loop's next-pop snapshot.
		const datagramBounds = { ...bounds };
		let controls: SubscriptionControls | undefined;
		// Lite-05 and later name every sequence below the end that this subscription never
		// got with SUBSCRIBE_DROP, so the subscriber settles without waiting out its grace
		// for a group that will not come. The sequences served (as a group or a datagram) or
		// dropped. A gap that never arrived stays until the end sweep names it: this side
		// cannot tell when the subscriber stopped waiting for it. Undefined on other versions.
		const served: Run[] | undefined = hasTrackStream(this.version) ? [] : undefined;

		try {
			// Every draft waits for the track's info, as Rust does: it surfaces a rejected track
			// (accept never called, track closed) as an error here, which resets the stream, and
			// its priority breaks ties between this session's subscriptions at one subscriber
			// priority.
			const info = await untilAborted(track.info(), left.signal);
			const publisher = info.priority;
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
				stamp = stamping(this.version, liteTimescale(info));
				compress = hasCompression(this.version) && !!info.compress;
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
			if (this.#datagramWriter) {
				datagrams = this.#runDatagrams(track, { sub: msg.id, stamp, served, bounds: datagramBounds });
			}

			controls = new SubscriptionControls({
				quic: this.#quic,
				reader: stream.reader,
				writer: stream.writer,
				version: this.version,
				apply: (update) => {
					if (update.ranges?.some((range) => range?.unresolved))
						throw new Error("relative and time subscription anchors are not supported");
					Object.assign(datagramBounds, {
						startGroup: update.startGroup,
						startFrame: update.startFrame,
						endGroup: update.endGroup,
						endFrame: update.endFrame,
					});
					// Every update restates the wanted group. Past our largest it is a join, which
					// repeats the pending ask; at or below it the group already exists.
					const largest = hooks.peekLargest(track).largest?.group;
					if (update.wantedGroup !== undefined && (largest === undefined || update.wantedGroup > largest))
						hooks.joinSubscriber(track);
					const end = exclusiveGroupEnd(update.endGroup);
					track.update({
						priority: update.priority,
						order: update.order,
						maxDelay: Milli(servingMaxDelay(this.version, update.maxDelay)),
						groups: {
							start: update.startGroup === undefined ? undefined : { included: update.startGroup },
							end: end === undefined ? undefined : { excluded: end },
						},
					});
				},
			});
			const finished = await this.#runTrack(track, stream.writer, controls, {
				sub: msg.id,
				broadcast: msg.broadcast,
				stamp,
				publisher,
				compress,
				served,
				floor,
				bounds,
				ranges: msg.ranges,
			});

			console.debug(`publish done: broadcast=${msg.broadcast} track=${track.name}`);
			// A lite-07 subscriber FINs once it has read the tail, so leave its half open and
			// wait for that. Older versions stop it here, which ends the decoder.
			if (waitsForSubscriberFin(this.version)) stream.writer.close();
			else stream.close();
			// Ends the datagram loop.
			track.close();
			// A subscriber that left is owed nothing more, and its stream may already be reset.
			await Promise.all([datagrams, controls.decoding, finished && acknowledged(stream.writer)]);
		} catch (err: unknown) {
			closeViolation(this.#quic, err);
			const e = error(err);
			if (e === cancel) console.debug(`publish cancelled: broadcast=${msg.broadcast} track=${track.name}`);
			else console.warn(`publish error: broadcast=${msg.broadcast} track=${track.name} error=${reason(e)}`);
			track.close(e);
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
				front =
					this.#publish &&
					(wireOf(this.#publish).local(msg.broadcast, msg.epoch) ??
						(await wireOf(this.#publish).demand(msg.broadcast, msg.epoch, holding)));
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
				// A group that has not arrived yet holds this FETCH's slot until the requester leaves.
				group = await front.track(msg.track).fetchGroup(msg.group, {
					priority: msg.priority,
					maxDelay: msg.maxDelay,
					signal: left.signal,
				});
				serving(group);
				// Revoked while the group resolved: the watch already reset the stream.
				if (watch.revoked()) {
					hooks.releaseGroupReader(group, unauthorized(msg.broadcast));
					return;
				}
				await this.#runFetchGroup(group, stream.writer, {
					stamp: stamping(this.version, info.timescale),
					compress: info.compress,
					start: msg.startFrame,
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
			served: Run[] | undefined;
			// The read cursor's floor; see {@link cursorFloor}.
			floor: number;
			bounds: FrameBounds;
			// Each lite-07 range, which SUBSCRIBE_START answers for.
			ranges: (RangeGroups | undefined)[] | undefined;
		},
	): Promise<boolean> {
		const { sub, broadcast, stamp, publisher, compress, served, bounds } = serving;
		// The read cursor's floor, assigned with the cap in {@link bounds} on every move.
		let floor = serving.floor;
		let ranges = serving.ranges;
		const moveCursor = () =>
			track.withRange({
				start: { included: floor },
				end: bounds.endGroup === undefined ? undefined : { included: bounds.endGroup },
			});
		// Raise the floor to `start`, keeping any higher floor already set.
		const raiseFloor = (start: number) => {
			if (start <= floor) return;
			floor = start;
			moveCursor();
		};
		// Lite-05+ resolves the range on the subscribe stream: SUBSCRIBE_START once the
		// first group is known, SUBSCRIBE_END when the track finishes.
		const emitRange = hasTrackStream(this.version);
		let startSent = false;
		// Dispositions before START refine its boundary; later ones must be named as drops.
		const skipped: { start: number; end: number; error: Error }[] = [];
		// Where SUBSCRIBE_START resolved, lowered by an update that asks for groups below it.
		let startSequence: number | undefined;
		// The first group and the edge seen before it was popped, held until the source
		// resolves where its feed starts.
		let first: { group: group.Consumer; largest: track.Position | undefined } | undefined;
		let endSent = false;

		// Drop whatever part of `[start, end)` this subscription has not been served or dropped
		// yet, given up with `err`.
		const drop = async (start: number, end: number, err: Error): Promise<boolean> => {
			if (!served) return true;
			for (const gap of runGaps(served, start, end)) {
				insertRun(served, gap.start, gap.end);
				const msg = SubscribeDrop.given(gap.start, gap.end - 1, err, this.version);
				if (!(await controls.response(encodeSubscribeResponse(stream, { drop: msg }, this.version))))
					return false;
			}
			return true;
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
			return end ? finalSequence(end) : (hooks.latestSequence(track) ?? -1) + 1;
		};

		// Declare the boundary immediately; groups below it can still be in flight.
		const sendEnd = async (): Promise<boolean> => {
			endSent = true;
			if (!emitRange) return true;
			return controls.response(
				encodeSubscribeResponse(stream, { end: new SubscribeEnd(boundary()) }, this.version),
			);
		};

		// One ranking for the whole subscription, shared by every group it serves.
		const priority = new Priority(track, publisher);

		// Every group this subscription started serving, until its stream finishes or resets.
		const groups = new Set<Promise<void>>();
		const drops = new Signal(0);
		const pendingDrops: SubscribeDrop[] = [];

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
						// What was given up below the start is never named, so a wider budget may serve it.
						if (range.start < startSequence)
							hooks.unreport(track, range.start, Math.min(range.end, startSequence));
						if (!(await drop(Math.max(range.start, startSequence), range.end, range.error))) return false;
					}
					skipped.length = 0;
				}
				if (pendingDrops.length > 0) {
					for (const dropped of pendingDrops.splice(0)) {
						if (
							!(await controls.response(encodeSubscribeResponse(stream, { drop: dropped }, this.version)))
						)
							return false;
					}
				}
				// Control before data, matching the Rust publisher: every control decoded while
				// the loop was parked applies to the next pop, never to one already made. This
				// drain is synchronous, so nothing the decoder holds can land between the pop
				// below and its frame range.
				const control = controls.take();
				if (control) {
					switch (control.kind) {
						case "done":
							// The subscriber left. Its queued groups are pointless now, which
							// the finally below acts on since `finished` stays false.
							return false;
						case "error":
							throw control.error;
						case "update": {
							const update = control.update;
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
									? (hooks.latestSequence(track) ?? 0)
									: (update.startGroup ?? (resolvesStart(this.version) ? 0 : undefined));
							if (start !== undefined) {
								floor = start;
								// Before SUBSCRIBE_START nothing is owed yet; the start accounts for it.
								// A lowered floor owes the groups it newly asks for, so the end sweep
								// covers them. A raised one sends no early drop: the subscriber already
								// stops owing below its floor, and a later update may lower it again to
								// reopen those groups.
								if (startSequence !== undefined) startSequence = Math.min(startSequence, start);
							}
							bounds.startGroup = update.startGroup;
							bounds.startFrame = update.startFrame;
							bounds.endGroup = update.endGroup;
							bounds.endFrame = update.endFrame;
							ranges = update.ranges;
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
				const largest =
					!startSent && hasLargest(this.version)
						? newer(first?.largest, newer(hooks.bufferedLargest(track), hooks.peekLargest(track).largest))
						: undefined;
				const recv = first
					? ({ kind: "group", group: first.group } as const)
					: hooks.tryRecvGroup(track, emitRange);
				first = undefined;
				switch (recv.kind) {
					case "error":
						throw recv.error;
					case "drop":
						if (!startSent) skipped.push({ start: recv.start, end: recv.end, error: recv.error });
						else if (!(await drop(recv.start, recv.end, recv.error))) return false;
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
								const start = new SubscribeStart(rangeStarts(ranges, bounds.startGroup), largest);
								if (
									!(await controls.response(encodeSubscribeResponse(stream, { start }, this.version)))
								)
									return false;
								continue;
							}
						}
						// A declared end goes out while lower groups are still being produced.
						if (!endSent && hooks.subscriberEnd(track) !== undefined) {
							if (!(await sendEnd())) return false;
							continue;
						}
						await waitForSubscription(controls, track, drops);
						continue;
					case "boundary":
						// The producer finished but is still holding groups above the cap.
						// Declare the boundary, then wait for an update to release them.
						if (!endSent) {
							if (!(await sendEnd())) return false;
							continue;
						}
						await waitForSubscription(controls, track, drops);
						continue;
					case "done": {
						// Name every sequence from the start to the end this subscription never
						// got. A cap below the end bounds what the subscriber is owed, as on its side.
						if (startSequence !== undefined) {
							const end =
								bounds.endGroup === undefined ? boundary() : Math.min(boundary(), bounds.endGroup + 1);
							const unserved = new StreamError(StreamCode.NotFound, { message: "group not served" });
							if (!(await drop(startSequence, end, unserved))) return false;
						}
						if (!endSent) {
							if (!(await sendEnd())) return false;
							continue;
						}
						// The FIN tells the subscriber every group is accounted for, so it waits
						// until each group stream finished or reset. The subscriber leaving
						// instead cancels whatever is still queued.
						const drained = Symbol("drained");
						// `race`, since this repeats per batch of drops against the subscription's end.
						const end = await race([Promise.all(groups).then(() => drained), controls.ended]);
						if (end instanceof Error) throw end;
						if (end !== drained) return false;
						if (pendingDrops.length > 0) continue;
						finished = true;
						return true;
					}
				}

				const group = recv.group;
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
					startSequence = Math.min(group.sequence, source ?? group.sequence);
					// A group given up cannot be where the feed starts. Adjacent runs given up with
					// different errors stay separate, so walk them in order.
					skipped.sort((a, b) => a.start - b.start);
					for (const range of skipped) {
						if (range.start <= startSequence && range.end > startSequence)
							startSequence = Math.min(range.end, group.sequence);
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
								{ start: new SubscribeStart(rangeStarts(ranges, startSequence), largest) },
								this.version,
							),
						))
					)
						return false;
				}
				if (served) insertRun(served, group.sequence, group.sequence + 1);

				const options: RunGroup = {
					sub,
					group,
					stamp,
					compress,
					priority,
					unsubscribed,
					start: range.start,
					end: range.end,
				};
				// The FIN waits for every stream to finish or reset, including one that has not opened yet.
				const opened = this.#openGroup(options);
				const task = opened.then(async (writer) => {
					const failed = writer instanceof Error ? writer : await this.#serveGroup(writer, options);
					if (emitRange && failed instanceof Error) {
						pendingDrops.push(SubscribeDrop.given(group.sequence, group.sequence, failed, this.version));
						drops.update((revision) => revision + 1);
					}
				});
				groups.add(task);
				void task.finally(() => groups.delete(task));
			}
		} finally {
			if (!finished) unsubscribe();
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
				// TRACK is the requester's only message.
				if (!fin) stream.abort(new ProtocolViolation("data after TRACK"));
			},
			() => hold.abort(),
		);
		try {
			const front =
				this.#publish &&
				(wireOf(this.#publish).local(msg.broadcast, msg.epoch) ??
					(await wireOf(this.#publish).demand(msg.broadcast, msg.epoch, hold.signal)));
			if (!front) throw new NotFound(`broadcast ${msg.broadcast}`);

			const info = await this.#resolveTrackInfo(front, msg.track, hold.signal);
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
			const origin = this.#publish && wireOf(this.#publish);
			const front =
				origin &&
				(origin.local(msg.broadcast, msg.epoch) ??
					(await origin.demand(msg.broadcast, msg.epoch, cancel.signal)));
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

	/**
	 * Forwards a track's datagrams best-effort over QUIC datagrams (lite-05 §6.4), parallel to
	 * its groups. Each datagram is dropped (there is no group fallback) if the encoded body
	 * doesn't fit the wire and transport datagram limits or the send fails. Returns once the track
	 * finishes; a failure never tears down the subscription.
	 *
	 * @internal
	 */
	async #runDatagrams(
		track: track.Subscriber,
		options: {
			sub: bigint;
			stamp: Stamp | undefined;
			served: Run[] | undefined;
			bounds: FrameBounds;
		},
	) {
		const { sub, stamp, served, bounds } = options;
		const writer = this.#datagramWriter;
		if (!writer) return; // Only reached with a writer (see the #datagramWriter gate).
		const maxSize = Math.min(DatagramMessage.MAX_SIZE, DatagramStream.maxDatagramSize(this.#quic));

		try {
			for (;;) {
				const datagram = await track.recvDatagram();
				if (!datagram) return; // Track finished; #runTrack tears the subscription down.
				// A datagram is frame 0 of its group, subject to the reliable frame range.
				if (frameRange(bounds, datagram.sequence).start > 0) continue;

				// Convert the timestamp to the track's advertised timescale, matching #serveGroup.
				const ts = stamp === undefined ? undefined : wireTime(datagram.timestamp, stamp);
				const slice = hasCompression(this.version) ? stored(datagram) : undefined;
				const message = new DatagramMessage(sub, datagram.sequence, ts, slice?.payload ?? datagram.payload);
				message.inflated = slice?.inflated;
				const body = message.encode(this.version);

				// No group fallback: drop anything that doesn't fit a single datagram.
				if (body.byteLength > maxSize) {
					console.debug(`dropping oversize datagram: sub=${sub} size=${body.byteLength} max=${maxSize}`);
					continue;
				}

				// A sent datagram counts as delivered even if it is lost, so the end sweep skips it.
				if (served) insertRun(served, datagram.sequence, datagram.sequence + 1);
				await writer.ready;
				await writer.write(body);
			}
		} catch (err: unknown) {
			// Best-effort: a datagram send failure stops sending but never fails the subscription.
			console.debug(`datagram send stopped: sub=${sub} error=${reason(err)}`);
		}
	}

	// Serialize a fetched group's frames onto the FETCH stream as bare records: each a
	// zigzag-delta timestamp (at the track's wire timescale, if any) followed by size + bytes.
	async #runFetchGroup(
		group: group.Consumer,
		stream: Writer,
		{ stamp, compress, start: startFrame, end: endFrame }: ServeFetch,
	) {
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

			await stream.write(frameHeader(stream.version, delta, read.inflated, frame.payload.byteLength));
			if (frame.payload.length > 0) await stream.write(frame.payload);
		}
	}

	/**
	 * Opens the unidirectional stream for one group, or closes the group and resolves
	 * an error when it cannot get one.
	 *
	 * @internal
	 */
	async #openGroup(options: RunGroup): Promise<Writer | Error> {
		const { group, priority, unsubscribed, start } = options;
		try {
			// The transport drains streams by send order, so this is what makes a high-priority
			// track (and a newer group within it) win the link when there isn't room for both.
			//
			// One stream per group is faster than a peer at its limit can retire them, so this
			// is the one path that doesn't wait for a slot: the transport would serve the opens
			// in the order we asked, which is oldest-first, exactly backwards for live media.
			// Failing here drops the group and lets the next one compete for the next slot.
			// Watch the group while the transport parks the open, and release that watch
			// once the open settles. tryOpen owns resetting a slot that arrives too late.
			const opened = Promise.withResolvers<void>();
			let failure: Error | undefined;
			const abandoned = untilGivenUp(group, () => opened.promise).then((stop) => {
				failure = stop;
			});
			let stream: Writer | undefined;
			try {
				stream = await Writer.tryOpen(this.#quic, {
					version: this.version,
					sendOrder: priority.rank(group.sequence, start),
					cancel: race([unsubscribed, abandoned]),
					waitUntilAvailable: false,
				});
			} finally {
				opened.resolve();
			}
			if (!stream) {
				const failed = failure ?? new Error("no stream slot");
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
		const { sub, group, stamp, compress, priority, start: startFrame, end: endFrame } = options;
		// The header names the requested position, so a copy missing it must be refused.
		const msg = new GroupMessage({ subscribe: sub, sequence: group.sequence, frameStart: startFrame });
		// The subscription's budget, or an eviction of what is left to read, may give up on the
		// group mid-write, which abandons the stream. A group already given up never starts the write.
		const write = async (op: () => Promise<unknown>) => {
			const stop = await untilGivenUp(group, op);
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

				const header = frameHeader(stream.version, delta, read.inflated, read.frame.payload.byteLength);
				await write(() => stream.write(header));
				if (read.frame.payload.length > 0) await write(() => stream.write(read.frame.payload));
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
			hooks.releaseGroupReader(group, e);
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
		// getStats is not yet in the TypeScript WebTransport type definitions.
		const quic = this.#quic as unknown as {
			getStats?: () => Promise<{ estimatedSendRate: number | null; smoothedRtt?: number | null }>;
		};
		if (!quic.getStats) {
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
				const timeout = sleep(Nano.fromMilli(Milli(PROBE_INTERVAL)), { signal: timer.signal }).then(
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
				const stats = await quic.getStats();
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
					const elapsed = Nano.toMilli(Instant.durationSince(now(), lastSentTime));
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
		this.#datagramWriter?.releaseLock();
		this.#datagramWriter = undefined;
	}
}

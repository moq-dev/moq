import { type Dispose, type Getter, race, Signal } from "@moq/signals";
import type * as broadcast from "../broadcast.ts";
import { Withdrawal } from "../connection/withdrawal.ts";
import { controlTimeout, error, reason, StreamCode, StreamError } from "../error.ts";
import type * as group from "../group.ts";
import type { Hop, Route } from "../hop.ts";
import { hiddenBelow, hooks, presented } from "../internal.ts";
import type { Consumer as OriginConsumer } from "../origin.ts";
import * as Path from "../path.ts";
import { type Stream, Writer } from "../stream.ts";
import { Milli, Timescale } from "../time.ts";
import type { Subscriber as TrackSubscriber } from "../track.ts";
import { TimeoutError, withTimeout } from "../util/timeout.ts";
import * as Varint from "../varint.ts";
import { type Advertised, type Advertisements, wireOf } from "../wire.ts";
import type { Session } from "./adapter.ts";
import * as Cluster from "./cluster.ts";
import { type RequestKind, requestReason, toRequestCode } from "./error.ts";
import { type Fetch, FetchError, FetchHeader } from "./fetch.ts";
import * as Filter from "./filter.ts";
import * as Message from "./message.ts";
import { FetchFrame, Frame, Group as GroupMessage } from "./object.ts";
import { Parameters } from "./parameters.ts";
import { fromWire, toWire } from "./priority.ts";
import * as Properties from "./properties.ts";
import { PublishDone, PublishDoneStatus } from "./publish.ts";
import {
	PublishNamespace,
	PublishNamespaceDone,
	PublishNamespaceOk,
	PublishNamespaceUpdate,
} from "./publish_namespace.ts";
import { RequestError, RequestOk } from "./request.ts";
import { cancelled, finCancels } from "./request_stream.ts";
import { type Subscribe, SubscribeError, SubscribeOk } from "./subscribe.ts";
import {
	type SubscribeNamespace,
	SubscribeNamespaceEntry,
	SubscribeNamespaceEntryDone,
	SubscribeNamespaceOk,
} from "./subscribe_namespace.ts";
import type { TrackStatusRequest } from "./track.ts";
import { type IetfVersion, Version } from "./version.ts";

/** First wait before re-offering a namespace the peer refused or we couldn't open for. */
const RETRY_BASE = 100;

/** Ceiling on that wait. The loop retries for the life of the session, so it must not spin. */
const RETRY_MAX = 5000;

/** Cluster parameters for one snapshot: the advertised hops, then our own id last. */
function clusterFor(base: Cluster.Advert | undefined, route: Route): Cluster.Advert | undefined {
	if (!base) return undefined;
	const self = base.hops.at(-1);
	if (self === undefined) return undefined;
	const hops = route.hops.at(-1) === self ? [...route.hops] : [...route.hops, self];
	return { hops, cost: route.cost.warm };
}

/**
 * What it takes to move the peer from `held` to `next`.
 *
 * - `"same"`: the peer would decode the same advertisement. The cold cost, and the whole
 *   route without Cluster, never reach the wire, so a change there sends nothing.
 * - `"restart"`: nothing is held, nothing is wanted, or a different broadcast, which is
 *   withdrawn and advertised again.
 * - Otherwise the same broadcast at a new hop chain or warm cost: the cluster parameters
 *   the peer holds and the ones it should, for the target to update.
 */
function change(
	base: Cluster.Advert | undefined,
	held: Advertised | undefined,
	next: Advertised | undefined,
): "same" | "restart" | Reprice {
	if (held === undefined || next === undefined || held.identity !== next.identity) return "restart";
	// A new epoch is another broadcast, even from the same entry.
	if (held.route.epoch !== next.route.epoch) return "restart";
	const from = clusterFor(base, held.route);
	const to = clusterFor(base, next.route);
	if (from === undefined || to === undefined) return from === to ? "same" : "restart";
	if (from.cost === to.cost && sameHops(from.hops, to.hops)) return "same";
	return { from, to };
}

/** A held advertisement's cluster parameters, and the ones the peer should hold instead. */
interface Reprice {
	from: Cluster.Advert;
	to: Cluster.Advert;
}

/** Whether two hop paths list the same hops in the same order. */
function sameHops(a: Hop[], b: Hop[]): boolean {
	return a.length === b.length && a.every((hop, i) => hop === b[i]);
}

/**
 * How long one advertisement may take to be answered. Matches the Rust publisher, and the
 * peer accepting the stream is only half the exchange: one it never answers on holds the
 * loop just as effectively as one it never grants.
 */
const ADVERTISE_TIMEOUT_MS = 5000;

/** Sleep `delay`, jittered, so a relay's namespaces don't all retry on the same tick. */
function retryAfter(delay: number): Promise<void> {
	return new Promise((resolve) => setTimeout(resolve, delay * (0.5 + Math.random() / 2)));
}

/**
 * What a refusal said about re-offering the namespace.
 *
 * A peer answers a request it declines with a retry interval, and ignoring it is how a
 * permanent refusal (unauthorized, uninterested) turns into a request every few seconds
 * for the life of the session. `"never"` is an interval of 0; a number is the epoch
 * milliseconds before which we must not come back.
 */
type Refused = "never" | number;

/**
 * How the peer answered an advertisement or an update: it holds the namespace, it does
 * not and said nothing about coming back (`"dropped"`), or it refused.
 */
type Answer = "held" | "dropped" | Refused;

/** The open PUBLISH_NAMESPACE request per advertised path. */
type Requests = Map<Path.Valid, { path: Path.Valid; requestId: bigint; stream: Stream }>;

/** What one announce loop knows about the peer, keyed the way that loop names namespaces. */
interface Namespaces {
	/** What the peer holds, by identity plus route. */
	active: Map<Path.Valid, Advertised>;

	/** What the peer refused, and whether coming back is worth anything. */
	refused: Map<Path.Valid, Refused>;

	/** The advertisement each refusal was about, so a republish or new epoch at the same path clears it. */
	offered: Map<Path.Valid, Advertised>;
}

/** Where one announce loop sends its advertisements. */
interface Target {
	/** Advertise a namespace the peer does not hold. */
	advertise(key: Path.Valid, next: Advertised): Promise<Answer>;

	/** Reprice a namespace the peer holds: the same broadcast at a new hop chain or cost. */
	update(key: Path.Valid, next: Advertised, reprice: Reprice): Promise<Answer>;

	/** Withdraw a namespace the peer holds. */
	withdraw(key: Path.Valid): Promise<void>;
}

/** What {@link Publisher.runGroup} needs to serve one group. */
interface RunGroup {
	/** The subscription's request ID, doubling as the track alias. */
	requestId: bigint;

	/** The group to serve. */
	group: group.Consumer;

	/**
	 * The units each object's presentation timestamp is written in, or undefined when objects
	 * carry none.
	 *
	 * Undefined when SUBSCRIBE_OK carries no TIMESCALE: the track has no timeline, the
	 * subscriber sent INCLUDE_PROPERTIES=0, or the draft (14-16) cannot send the property. A
	 * timestamp whose units were never declared is worse than none. Our own reader discards
	 * it; another may read it as some default and time the media wrong.
	 */
	timescale?: Timescale;

	/** The publisher's tie-break priority, already converted to the IETF wire convention. */
	publisherPriority: number;

	/** The objects of this group the subscription's filter selects. */
	slice: GroupSlice;

	/** Settles when the subscriber leaves, dropping a group still queued for a stream slot. */
	unsubscribed: Promise<void>;

	/** The subscription's data stream count, which PUBLISH_DONE reports. */
	streams: StreamCount;
}

/** How many data streams a subscription opened, fill streams included. */
type StreamCount = { opened: number };

/** What {@link Publisher.runFill} needs to serve one subscription's backfill. */
interface RunFill {
	/** The subscription's request ID, which the fetch stream names. */
	requestId: bigint;

	/**
	 * The subscriber's delivery priority, applied to the cache subscription the fill reads.
	 *
	 * Deliberately not the fetch stream's send order: this publisher leaves its group
	 * streams at the transport's default, so ranking the fill alone would put backfill
	 * ahead of the live groups it is catching up to.
	 */
	priority: number;

	/** The range the fill resolved to. */
	fill: FillServe;

	/**
	 * An independent view of the same track, used to read the cached group without consuming
	 * anything the subscription will deliver.
	 */
	cache: TrackSubscriber;

	/** The units objects are stamped in, if any; see {@link RunGroup.timescale}. */
	timescale?: Timescale;

	/** Settles when the subscriber leaves, releasing a fill still waiting on its group. */
	unsubscribed: Promise<void>;

	/** The subscription's data stream count, which PUBLISH_DONE reports. */
	streams: StreamCount;
}

/**
 * Handles publishing broadcasts using moq-transport protocol.
 * Uses the stream-per-request pattern (real bidi streams for v17, virtual for v14-v16).
 *
 * @internal
 */
export class Publisher {
	#withdrawal = new Withdrawal();

	withdraw(): Promise<void> {
		return this.#withdrawal.close();
	}
	#quic: WebTransport;
	#session: Session;
	#requiresSolicitation: boolean;
	#hidden: boolean;

	// The origin this session serves, borrowed: it outlives the session, and closing the
	// session leaves its broadcasts alone. The namespaces are advertised with an unsolicited
	// PUBLISH_NAMESPACE (see {@link runPublishNamespaces}), or on request if the peer asked
	// for that (see {@link runSubscribeNamespace}).
	#advertised: Getter<Advertisements | undefined>;
	#publish?: OriginConsumer;

	// What every advertisement carries on a session that negotiated the MoQ Cluster
	// extension: a hop path holding our own id, so the peer can tell that what it hears
	// back came from us. `undefined` when nothing negotiated it.
	#advert?: Cluster.Advert;

	/**
	 * Creates a new Publisher instance.
	 *
	 * @internal
	 */
	constructor({
		quic,
		session,
		publish,
		requiresSolicitation,
		hidden = false,
		cluster,
	}: {
		/** The WebTransport session, for uni streams. */
		quic: WebTransport;
		/** The session abstraction for bidi streams and request IDs. */
		session: Session;
		/** The origin whose broadcasts this session serves; omit to publish nothing. */
		publish?: OriginConsumer;
		/** Whether the peer's SETUP asked to be told on request (MoQ Solicit). */
		requiresSolicitation: boolean;
		/** Whether the peer declared MoQ Hidden. */
		hidden?: boolean;
		/** The Hop IDs the SETUP exchange settled (MoQ Cluster). */
		cluster?: Cluster.Hops;
	}) {
		this.#quic = quic;
		this.#session = session;
		const origin = publish && wireOf(publish);
		this.#advertised = origin?.advertised ?? new Signal(new Map());
		this.#publish = publish;
		this.#requiresSolicitation = requiresSolicitation;
		this.#hidden = hidden;
		this.#advert = Cluster.advertise(cluster);
	}

	/**
	 * Handles an incoming SUBSCRIBE request on a bidi stream.
	 * Owns the full lifecycle: sends response, serves track data, waits for close.
	 *
	 * @internal
	 */
	async runSubscribe(msg: Subscribe, stream: Stream) {
		const version = this.#session.version;
		const name = msg.trackNamespace;
		let broadcast: broadcast.Consumer | undefined;
		let refusal: { errorCode: number; reasonPhrase: string } | undefined;

		// Legal requests we can't honor are refused one at a time. A subscription that
		// forwards nothing is only useful to a subscriber that later turns forwarding on,
		// and serving a Range Filter unfiltered would deliver objects it excluded.
		const unsupported = !msg.forward
			? "FORWARD=0 not supported"
			: msg.rangeFilters
				? "range filters not supported"
				: undefined;

		if (unsupported) {
			refusal = { errorCode: toRequestCode("not_supported", "subscribe", version), reasonPhrase: unsupported };
		} else {
			try {
				broadcast =
					this.#publish && (wireOf(this.#publish).local(name) ?? (await wireOf(this.#publish).demand(name)));
				if (!broadcast) {
					refusal = {
						errorCode: toRequestCode("does_not_exist", "subscribe", version),
						reasonPhrase: "broadcast not found",
					};
				}
			} catch (err: unknown) {
				const e = error(err);
				const condition =
					e instanceof StreamError && (e.code === StreamCode.NotFound || e.code === StreamCode.Unroutable)
						? "does_not_exist"
						: "internal";
				refusal = { errorCode: toRequestCode(condition, "subscribe", version), reasonPhrase: reason(e) };
			}
		}

		if (refusal) {
			if (version === Version.DRAFT_14) {
				await stream.writer.u53(SubscribeError.id);
				const err = new SubscribeError({
					requestId: msg.requestId,
					errorCode: refusal.errorCode,
					reasonPhrase: refusal.reasonPhrase,
				});
				await err.encode(stream.writer, version);
			} else {
				await stream.writer.u53(RequestError.id);
				const err = new RequestError({
					requestId: version === Version.DRAFT_15 || version === Version.DRAFT_16 ? msg.requestId : undefined,
					errorCode: refusal.errorCode,
					reasonPhrase: refusal.reasonPhrase,
				});
				await err.encode(stream.writer, version);
			}
			stream.close();
			return;
		}
		if (!broadcast) return;

		const priority = fromWire(msg.subscriberPriority);
		const track = wireOf(broadcast).subscribe(msg.trackName, {
			priority,
			// moq-transport has no subscriber latency parameter. Keep everything the
			// producer retained and let the receiving subscriber enforce its own budget.
			// Keep the sentinel encodable if this demand crosses a Lite hop before the
			// producer's retention bound is known.
			maxDelay: Milli(Varint.MAX_U53),
		});

		let cache: TrackSubscriber | undefined;

		try {
			const info = await track.info();
			// A Timestamp goes out only when this SUBSCRIBE_OK actually carries TIMESCALE.
			// Drafts 14-16 never write that property, so their objects stay unstamped.
			const timescale =
				msg.propertiesWanted && Properties.sendsTimescale(version) ? (info.timescale ?? undefined) : undefined;
			// The model ranks higher-first, the IETF wire lower-first. Every group this
			// subscription serves carries the same publisher priority, which is what lets a
			// relay prefer catalog and audio over video when it has no subscriber preference
			// to go on.
			const publisherPriority = toWire(info.priority);

			// The filter and any fill are relative to the live edge, so snapshot it once: the
			// fill ends exactly where a Next Object subscription begins, which is what lets
			// the draft's current-group join (Next Object plus a StartGroup=1 fill) cover the
			// group with no gap and no overlap.
			const edge = liveEdge(track);
			const range = subscribeRange(msg, edge, version);

			// The wire request tells an upstream what we need; the cursor is what actually
			// trims this subscriber, since the producer fans every cached group out to every
			// sink regardless. An absent start joins at the latest group, which is what
			// moq-lite means by joining a live track. The range's end is the inclusive last
			// group; the model's `endGroup` is exclusive.
			track.update({
				priority,
				maxDelay: Milli(Varint.MAX_U53),
				groups: {
					start: range.start ? { included: Number(range.start.group) } : undefined,
					end: range.end ? { included: Number(range.end.group) } : undefined,
				},
			});
			const startGroup = range.start ? Number(range.start.group) : track.latest();
			if (startGroup !== undefined) track.setGroups({ start: { included: startGroup } });

			// A fill reads the group cache through its own consumer, independent of the
			// subscription's cursor. Forked from this subscriber rather than resolved through
			// the broadcast again: a dynamic serve is one request per peer subscription, so
			// asking the broadcast would mint a second producer nobody has accepted.
			const fill =
				msg.fill && Filter.isDraft20(version) ? fillRange(msg.fill, msg.filter, edge.largest) : undefined;
			cache =
				fill && fill.kind !== "empty" ? track.fork({ priority, maxDelay: Milli(Varint.MAX_U53) }) : undefined;

			// Send SUBSCRIBE_OK
			await stream.writer.u53(SubscribeOk.id);
			const ok = new SubscribeOk({
				requestId:
					version === Version.DRAFT_14 || version === Version.DRAFT_15 || version === Version.DRAFT_16
						? msg.requestId
						: undefined,
				trackAlias: msg.requestId,
				// Required once the track has content; a fill-requesting subscriber sizes its
				// backfill against this. Below draft-20 round it down to the start of its
				// group, matching what the subscription serves there: a draft-20 subscriber
				// recovers a group's head with a FILL, which we serve, while older drafts
				// recover it with a joining FETCH, which we do not, so naming a mid-group
				// start would strand them.
				largest:
					edge.largest &&
					(Filter.isDraft20(version)
						? { groupId: edge.largest.group, objectId: edge.largest.object }
						: { groupId: edge.largest.group, objectId: 0n }),
				properties: msg.propertiesWanted
					? // TIMESCALE is what opts the track into timestamps, on drafts that can
						// send it. We serve the newest group first, matching moq-lite.
						{
							// An untimed track declares none, so it stays untimed downstream.
							timescale: info.timescale ?? undefined,
							priority: publisherPriority,
							groupOrder: Properties.DESCENDING,
							maxCacheDuration: info.maxAge === undefined ? undefined : BigInt(info.maxAge),
						}
					: // INCLUDE_PROPERTIES=0. The block stays present but empty, which also means
						// the track opts out of timestamps for this subscriber.
						{},
			});
			await ok.encode(stream.writer, version);
			console.debug(`publish ok: broadcast=${name} track=${track.name}`);

			// Cancels groups still queued for a stream slot. Only the subscriber leaving counts:
			// a track that ran out of groups still has to flush the ones already queued, and we
			// close the stream ourselves below to say so.
			let finished = false;
			let unsubscribe!: () => void;
			const unsubscribed = new Promise<void>((resolve) => {
				unsubscribe = resolve;
			});
			const updates = (async () => {
				if (version === Version.DRAFT_14 || version === Version.DRAFT_15 || version === Version.DRAFT_16)
					return stream.reader.closed;
				for (;;) {
					if (await stream.reader.done()) {
						if (finCancels(version)) return;
						return stream.writer.closed;
					}
					if ((await stream.reader.u53()) !== 0x02) throw new Error("unexpected message on subscribe stream");
					const params = await Message.decode(stream.reader, async (r) => {
						await r.u62();
						if (version === Version.DRAFT_17) await r.u62();
						return Parameters.decode(r, version);
					});
					const unsupported =
						params.forward === false ||
						params.bytes.size > 0 ||
						params.rangeFilters ||
						params.trackPropertyFilter ||
						[...params.vars.keys()].some((key) => key !== 0x10n && key !== 0x20n);
					if (unsupported) {
						await stream.writer.u53(RequestError.id);
						await new RequestError({
							requestId: undefined,
							errorCode: toRequestCode("not_supported", "subscribe", version),
							reasonPhrase: "REQUEST_UPDATE parameters not supported",
						}).encode(stream.writer, version);
						throw new UpdateFailed();
					}
					// update() replaces every option, so carry the rest over as Rust does.
					if (params.subscriberPriority !== undefined)
						track.update({ ...track.subscription.peek(), priority: fromWire(params.subscriberPriority) });
					await stream.writer.u53(RequestOk.id);
					await new RequestOk({ requestId: undefined }).encode(stream.writer, version);
				}
			})();
			const requestEnded =
				version === Version.DRAFT_14 || version === Version.DRAFT_15 || version === Version.DRAFT_16
					? updates
					: race([updates, stream.writer.closed]);
			void requestEnded.then(
				() => {
					if (!finished) unsubscribe();
				},
				// A reset is always the peer.
				() => unsubscribe(),
			);

			// Every group started, until its stream finishes or resets, and the data streams
			// opened for PUBLISH_DONE to report.
			const groups = new Set<Promise<void>>();
			const streams: StreamCount = { opened: 0 };

			// Serve groups until the track ends or the requester cancels.
			const serving = (async () => {
				for (;;) {
					const group = await track.recvGroup();
					if (!group) return;

					// Past the filter's end. Dropped here rather than through `setGroups`, which
					// parks a capped group instead: this range is fixed for the life of the
					// subscription, so a group above it is never coming back in, and holding
					// one keeps the loop from ever ending. A producer that publishes beyond
					// the end and then closes would otherwise strand PUBLISH_DONE.
					if (range.end !== undefined && BigInt(group.sequence) > range.end.group) {
						group.close();
						continue;
					}

					const task = this.#runGroup({
						requestId: msg.requestId,
						group,
						timescale,
						publisherPriority,
						slice: groupSlice(range, group.sequence),
						unsubscribed,
						streams,
					});
					groups.add(task);
					void task.finally(() => groups.delete(task));
				}
			})();

			// The fill (when one was requested) runs alongside on its own fetch stream; its
			// failures reset that stream and never touch the subscription.
			const filling =
				fill && cache
					? this.#runFill({
							requestId: msg.requestId,
							priority,
							fill,
							cache,
							timescale,
							unsubscribed,
							streams,
						})
					: Promise.resolve();

			let publishError: Error | undefined;
			let ended = false;
			try {
				const served = Symbol("served");
				ended = (await race([Promise.all([serving, filling]).then(() => served), requestEnded])) === served;
			} catch (err: unknown) {
				publishError = error(err);
			}

			// PUBLISH_DONE waits until every stream this subscription will open is closed, as
			// the draft requires, so its count is final. The subscriber leaving cancels the
			// ones still queued instead.
			await race([Promise.all(groups), unsubscribed]);

			// Draft 14 on has no end location in PUBLISH_DONE: an END_OF_TRACK object is what
			// tells the subscriber where the track ended.
			const final = track.final();
			if (ended && !publishError && final !== undefined) {
				await this.#runEndOfTrack({
					requestId: msg.requestId,
					final,
					publisherPriority,
					unsubscribed,
					streams,
				});
			}

			console.debug(`publish done: broadcast=${name} track=${track.name}`);
			if (publishError) {
				console.warn(`publish error: broadcast=${name} track=${track.name} error=${reason(publishError)}`);
			}

			// PUBLISH_DONE is required for every supported draft. The peer may have already
			// closed its side to unsubscribe, in which case there is nowhere to send it.
			try {
				await stream.writer.u53(PublishDone.id);
				const done = new PublishDone({
					requestId:
						version === Version.DRAFT_14 || version === Version.DRAFT_15 || version === Version.DRAFT_16
							? msg.requestId
							: undefined,
					statusCode:
						publishError instanceof UpdateFailed
							? PublishDoneStatus.UPDATE_FAILED
							: publishError
								? PublishDoneStatus.INTERNAL_ERROR
								: PublishDoneStatus.TRACK_ENDED,
					streamCount: BigInt(streams.opened),
					reasonPhrase:
						publishError instanceof UpdateFailed
							? "update failed"
							: publishError
								? "internal error"
								: "track ended",
				});
				await done.encode(stream.writer, version);
			} catch {
				// Stream might already be closed by peer.
			}

			// Our close follows PUBLISH_DONE; it must not cancel data still queued for delivery.
			finished = true;
			stream.close();
		} catch (err: unknown) {
			const e = error(err);
			console.warn(`publish error: broadcast=${name} track=${track.name} error=${reason(e)}`);
			stream.abort(e);
		} finally {
			track.close();
			// Idempotent, and the backstop for a throw between the fork and the fill starting:
			// #runFill closes it itself on every path it reaches.
			cache?.close();
		}
	}

	/**
	 * Runs a group and sends its frames using ObjectStream (Subgroup delivery mode).
	 */
	async #runGroup(options: RunGroup) {
		const { requestId, group, timescale, publisherPriority, slice, unsubscribed, streams } = options;
		try {
			// One stream per group is faster than a peer at its limit can retire them, so this
			// is the one path that doesn't wait for a slot: the transport would serve the opens
			// in the order we asked, which is oldest-first, exactly backwards for live media.
			// Failing here drops the group and lets the next one compete for the next slot.
			const stream = await Writer.tryOpen(this.#quic, {
				cancel: unsubscribed,
				version: this.#session.version,
				waitUntilAvailable: false,
			});
			if (!stream) {
				group.close(new Error("no stream slot"));
				return;
			}
			streams.opened += 1;

			const header = new GroupMessage({
				trackAlias: requestId,
				groupId: group.sequence,
				subGroupId: 0,
				publisherPriority,
				flags: {
					// The object properties carry the timestamp, so there is nothing to write
					// when the track declared no units to read one in.
					hasExtensions: timescale !== undefined,
					hasSubgroup: false,
					hasSubgroupObject: false,
					hasEnd: true,
					hasPriority: true,
					// Only honest when the stream really starts at the group's first object;
					// a trimmed head starts partway through.
					firstObject: slice.skip === 0,
				},
			});

			try {
				await hooks.guardGroup(group, () => header.encode(stream, this.#session.version));
				// The first written object goes on the wire as its absolute id, so a trimmed
				// head shows the true numbering rather than a silently renumbered group.
				let first = true;
				// The next object id that could be written, which starts at the filter rather
				// than at zero. A backwards range (`startObject` above `endObject` in the same
				// group) is empty, and starting here is what lets the check below see that
				// before the read parks on an object the range already excludes.
				let next = slice.skip;

				for (;;) {
					// The filter ends inside this group: everything at `until` and beyond is
					// outside the requested range, so stop without waiting for the group's end.
					if (slice.until !== undefined && next >= slice.until) break;

					// Reading from the filter's start drops the objects below it: they are outside
					// the requested range, so skipping them is not a gap.
					const read = await race([hooks.readGroupFrame(group, slice.skip), stream.closed]);
					if (!read) break;
					next = read.sequence + 1;
					if (slice.until !== undefined && read.sequence >= slice.until) {
						read.complete();
						break;
					}

					try {
						const obj = new Frame({ payload: read.frame.payload, timestamp: read.frame.timestamp });
						const delta = first ? read.sequence : 0;
						first = false;
						await hooks.guardGroup(group, () =>
							obj.encode(stream, header.flags, timescale, this.#session.version, delta),
						);
					} finally {
						read.complete();
					}
				}

				stream.close();
			} catch (err: unknown) {
				stream.reset(error(err));
			}
		} finally {
			group.close();
		}
	}

	/**
	 * Mark the track's end with an END_OF_TRACK object on its own stream, at object 0 of the
	 * group that will never exist.
	 *
	 * The last group's stream has usually finished before the track ends, so the marker cannot
	 * ride on it. A failure only costs the subscriber the early boundary.
	 */
	async #runEndOfTrack(options: {
		requestId: bigint;
		final: number;
		publisherPriority: number;
		unsubscribed: Promise<void>;
		streams: StreamCount;
	}) {
		const { requestId, final, publisherPriority, unsubscribed, streams } = options;
		const version = this.#session.version;
		const stream = await Writer.tryOpen(this.#quic, { cancel: unsubscribed, version }).catch(() => undefined);
		if (!stream) return;
		streams.opened += 1;

		try {
			const header = new GroupMessage({
				trackAlias: requestId,
				groupId: final,
				subGroupId: 0,
				publisherPriority,
				flags: {
					hasExtensions: false,
					hasSubgroup: false,
					hasSubgroupObject: false,
					hasEnd: false,
					hasPriority: true,
					firstObject: true,
				},
			});
			await header.encode(stream, version);
			await new Frame({ endOfTrack: true }).encode(stream, header.flags, Timescale.MILLI, version);
			stream.close();
		} catch (err: unknown) {
			stream.reset(error(err));
		}
	}

	/**
	 * Serve a draft-20 fill on its own fetch stream: the requested range, read from the
	 * group cache, capped at the Largest Object snapshot.
	 *
	 * A fill is a promise once requested. An empty range opens no stream, but a range we
	 * cannot serve still opens one and resets it right after the FETCH_HEADER, the draft's
	 * fill-failure signal. Nothing here touches the subscription either way.
	 */
	async #runFill(options: RunFill) {
		const { requestId, fill, cache, timescale, unsubscribed, streams } = options;
		const version = this.#session.version;

		// Everything is inside the try so the cache fork is released on every path out,
		// including a transport failure while opening the stream.
		let stream: Writer | undefined;
		try {
			if (fill.kind === "empty") return;

			stream = await Writer.tryOpen(this.#quic, { cancel: unsubscribed, version });
			if (!stream) {
				console.debug(`fill stream failed to open: fill=${requestId}`);
				return;
			}
			streams.opened += 1;

			await stream.u53(FetchHeader.type);
			await new FetchHeader({ requestId }).encode(stream, version);

			if (fill.kind !== "group") {
				throw new Error("a fill spanning several groups is not supported");
			}

			const group = takeGroup(cache, Number(fill.sequence));
			try {
				await this.#writeFillGroup(stream, group, fill, timescale, unsubscribed);
			} finally {
				group.close();
			}

			stream.close();
			console.debug(`fill complete: fill=${requestId}`);
		} catch (err: unknown) {
			// A fill never fails the subscription; its own stream carries the failure.
			const e = error(err);
			console.debug(`fill failed, resetting its stream: fill=${requestId} error=${reason(e)}`);
			stream?.reset(e);
		} finally {
			cache.close();
		}
	}

	/**
	 * Write one cached group's frames as draft-20 fetch objects.
	 *
	 * The cap is the Largest Object snapshot: the group may keep growing, but everything
	 * past the snapshot belongs to the subscription, not the fill.
	 */
	async #writeFillGroup(
		stream: Writer,
		group: group.Consumer,
		fill: FillGroup,
		timescale: Timescale | undefined,
		unsubscribed: Promise<void>,
	) {
		let first = true;
		let next = 0n;

		// The subscriber leaving has to end the fill too. An absolute filter ending below the
		// edge with no end object leaves `until` unset, so this reads until the group closes,
		// which a still-open past group may never do. Watching only the fetch stream would
		// then pin the group and the cache subscription behind it for the life of the track.
		let left = false;
		const cancelled = unsubscribed.then(() => {
			left = true;
			return undefined;
		});

		for (;;) {
			// The group may keep growing, but everything at `until` and beyond belongs to the
			// subscription, so stop without waiting for the group's end.
			if (fill.until !== undefined && next >= fill.until) break;

			// Reading from the fill's start drops everything below it; see the same read in #runGroup.
			const frame = await race([group.readFrameSequence({ from: Number(fill.skip) }), stream.closed, cancelled]);
			if (left) throw new Error("unsubscribed before the fill finished");
			if (!frame) break;
			next = BigInt(frame.sequence) + 1n;
			if (fill.until !== undefined && BigInt(frame.sequence) >= fill.until) break;

			const obj = new FetchFrame({ payload: frame.payload, timestamp: frame.timestamp });
			await obj.encode(
				stream,
				{ group: Number(fill.sequence), object: frame.sequence, first },
				timescale,
				this.#session.version,
			);
			first = false;
		}
	}

	/**
	 * Handles an incoming SUBSCRIBE_NAMESPACE on a bidi stream.
	 *
	 * This carries the advertisements when the peer asked to be told on request (MoQ
	 * Solicit); otherwise {@link runPublishNamespaces} has already announced everything
	 * visible and repeating it here would leave the peer holding two sources for one
	 * broadcast, so only the hidden namespaces it may see ride here (MoQ Hidden).
	 * Draft-16+ streams Namespace entries inline; draft-14/15 predate those messages, so
	 * each advertisement is a PUBLISH_NAMESPACE request of its own.
	 *
	 * @internal
	 */
	runSubscribeNamespace(msg: SubscribeNamespace, stream: Stream): Promise<void> {
		return this.#withdrawal.track(this.#runSubscribeNamespace(msg, stream));
	}

	async #runSubscribeNamespace(msg: SubscribeNamespace, stream: Stream) {
		if (this.#withdrawal.closing.peek()) return;
		const version = this.#session.version;
		const prefix = msg.namespace;
		const legacy = version === Version.DRAFT_14 || version === Version.DRAFT_15;

		// Draft-14/15: the open PUBLISH_NAMESPACE request per advertised suffix.
		const requests: Requests = new Map();
		// Drains the requests map, so calling it twice is safe and the second is a no-op.
		const withdrawAll = async () => {
			for (const path of [...requests.keys()]) {
				await this.#withdraw(path, requests);
			}
		};

		const ending = cancelled(stream, version);
		try {
			// Send OK response
			if (version === Version.DRAFT_14) {
				await stream.writer.u53(SubscribeNamespaceOk.id);
				const ok = new SubscribeNamespaceOk({ requestId: msg.requestId });
				await ok.encode(stream.writer, version);
			} else {
				await stream.writer.u53(RequestOk.id);
				const ok = new RequestOk({
					requestId: version === Version.DRAFT_15 || version === Version.DRAFT_16 ? msg.requestId : undefined,
				});
				await ok.encode(stream.writer, version);
			}

			// Hidden namespaces are left out unless the peer opted in (MoQ Hidden). Unless the
			// peer asked to be told only on request, it has already heard everything visible
			// from the empty prefix unasked, so this stream carries only what that hid.
			const carries = (covered: Path.Valid) =>
				(!this.#hidden || msg.hidden || !hiddenBelow(prefix, covered)) &&
				(this.#requiresSolicitation || (this.#hidden && hiddenBelow(Path.empty(), covered)));

			// Inline entries always land, and the receiver treats a repeated NAMESPACE as a
			// replacement, so repricing one is just sending it again, a new original
			// publisher included, as Rust does. Only a legacy PUBLISH_NAMESPACE request can
			// be declined.
			const namespace = async (suffix: Path.Valid, snap: Advertised): Promise<Answer> => {
				await stream.writer.u53(SubscribeNamespaceEntry.id);
				const cluster = clusterFor(this.#advert, snap.route);
				await new SubscribeNamespaceEntry({ suffix, cluster }).encode(stream.writer, version);
				return "held";
			};
			const target: Target = legacy
				? {
						advertise: (suffix, snap) =>
							this.#advertise(Path.join(prefix, suffix), requests, clusterFor(this.#advert, snap.route)),
						update: (suffix, _snap, reprice) => this.#update(Path.join(prefix, suffix), requests, reprice),
						withdraw: (suffix) => this.#withdraw(Path.join(prefix, suffix), requests),
					}
				: {
						advertise: namespace,
						update: namespace,
						withdraw: async (suffix) => {
							await stream.writer.u53(SubscribeNamespaceEntryDone.id);
							await new SubscribeNamespaceEntryDone({ suffix }).encode(stream.writer, version);
						},
					};

			// Keyed by suffix, as the entries name them.
			const ns: Namespaces = { active: new Map(), refused: new Map(), offered: new Map() };
			let retry = 0;

			for (;;) {
				// Subscribe BEFORE reconciling, for the same reason as
				// {@link runPublishNamespaces}: a publish landing while an advertisement
				// waits for its reply only notifies listeners already registered.
				// TODO Make a better helper within Signals.
				let dispose!: Dispose;
				const changed = new Promise<Advertisements | undefined>((resolve) => {
					dispose = this.#advertised.changed(resolve);
				});

				const advertised = this.#advertised.peek();
				if (!advertised) {
					dispose();
					break;
				}

				const outstanding = await this.#reconcile(ns, presented(prefix, advertised, carries), target);
				retry = outstanding ? Math.min(retry ? retry * 2 : RETRY_BASE, RETRY_MAX) : 0;

				// Wait for the next change, or for the peer to unsubscribe.
				const next = await (retry
					? race([changed, this.#withdrawal.closing, ending, retryAfter(retry).then(() => advertised)])
					: race([changed, this.#withdrawal.closing, ending]));
				dispose();
				if (!next || next === true) break;
			}

			// Withdraw before the ask stream FINs, as Rust does: a peer that reads that
			// FIN as the subscription ending can drop the DONEs still in flight.
			await withdrawAll();
			stream.close();
			await stream.writer.closed;
		} catch (err: unknown) {
			const e = error(err);
			console.debug(`subscribe_namespace stream error: ${reason(e)}`);
			stream.abort(e);
			if (this.#withdrawal.closing.peek()) throw e;
		} finally {
			// This subscription's advertisements die with it.
			await withdrawAll();
		}
	}

	/**
	 * Advertise every published broadcast with an unsolicited PUBLISH_NAMESPACE, until
	 * the publisher is closed.
	 *
	 * The peers that never send SUBSCRIBE_NAMESPACE are exactly the ones expecting a
	 * publisher to announce itself, so announcing is the default. A peer that would
	 * rather ask says so in its SETUP (MoQ Solicit) and this does nothing, leaving
	 * {@link runSubscribeNamespace} to carry the advertisements instead. Exactly one of
	 * the two is live, so the peer never hears a namespace twice.
	 *
	 * @internal
	 */
	runPublishNamespaces(): Promise<void> {
		return this.#withdrawal.track(this.#runPublishNamespaces());
	}

	async #runPublishNamespaces() {
		if (this.#withdrawal.closing.peek()) return;
		if (this.#requiresSolicitation) {
			// The peer asked to be told on request; runSubscribeNamespace answers it.
			return;
		}

		// The open PUBLISH_NAMESPACE request per advertised path.
		const requests: Requests = new Map();
		const target: Target = {
			advertise: (path, snap) => this.#advertise(path, requests, clusterFor(this.#advert, snap.route)),
			update: (path, _snap, reprice) => this.#update(path, requests, reprice),
			withdraw: (path) => this.#withdraw(path, requests),
		};

		// The origin outlives the session and its signal never ends, so this loop needs the
		// session's own ending to stop: without it a closed connection leaves the loop parked
		// on a shared origin forever, waking on someone else's publish to fail on a dead
		// transport. `runSubscribeNamespace` gets this from the stream the peer asked on; an
		// unsolicited loop has no stream of its own, so the session is what it watches.
		const closed = this.#quic.closed.then(
			() => undefined,
			() => undefined,
		);

		let dispose: Dispose | undefined;
		try {
			// Keyed by path.
			const ns: Namespaces = { active: new Map(), refused: new Map(), offered: new Map() };
			let retry = 0;

			for (;;) {
				// Subscribe BEFORE reconciling. Each advertisement below waits a round trip
				// for the peer's reply, and a publish that lands in that window notifies
				// only the listeners already registered; one created afterwards would sleep
				// through it and leave the namespace unadvertised until something unrelated
				// changed.
				// TODO Make a better helper within Signals.
				const changed = new Promise<Advertisements | undefined>((resolve) => {
					dispose = this.#advertised.changed(resolve);
				});

				const advertised = this.#advertised.peek();
				if (!advertised) {
					dispose?.();
					break;
				}

				const updated = new Map<Path.Valid, Advertised>();
				for (const [covered, candidates] of advertised) {
					// Only peers that declared MoQ Hidden filter unsolicited discovery.
					if ((this.#hidden && hiddenBelow(Path.empty(), covered)) || candidates.length === 0) continue;
					updated.set(covered, candidates[0]);
				}

				const outstanding = await this.#reconcile(ns, updated, target);
				retry = outstanding ? Math.min(retry ? retry * 2 : RETRY_BASE, RETRY_MAX) : 0;

				// Wait for the next change, which has already fired if one landed above.
				const next = await (retry
					? race([changed, this.#withdrawal.closing, closed, retryAfter(retry).then(() => advertised)])
					: race([changed, this.#withdrawal.closing, closed]));
				dispose?.();
				if (!next || next === true) break;
			}
		} catch (err: unknown) {
			// Nothing restarts this loop, so whatever got us here cost the session its
			// discovery. Not a debug-level event.
			console.warn(`publish_namespace loop failed: ${reason(error(err))}`);
			if (this.#withdrawal.closing.peek()) throw err;
		} finally {
			dispose?.();
			// Close out every open PUBLISH_NAMESPACE request.
			for (const path of [...requests.keys()]) {
				await this.#withdraw(path, requests);
			}
		}
	}

	/**
	 * Bring the peer's view in line with `updated`, one namespace at a time, and report
	 * whether anything it should hold is still missing and worth coming back for.
	 *
	 * Nothing the peer cannot see is sent, a reprice updates the namespace in place, and
	 * anything else is withdrawn first so a republish reads as withdraw-then-advertise.
	 * `ns` records what the peer holds rather than what we attempted: a declined
	 * advertisement or update stays out of it, so the next turn offers it fresh instead
	 * of believing it is already up.
	 */
	async #reconcile(ns: Namespaces, updated: Map<Path.Valid, Advertised>, target: Target): Promise<boolean> {
		// A namespace that is gone, or that a republish replaced, takes its refusal with
		// it: the peer refused a broadcast, not a path forever, so a different one at the
		// same path is offered again. The origin swaps the front in one mutation, so the
		// path never leaves `updated` and only the front says they differ. Rust gets this
		// by rebuilding the watched entry.
		for (const key of [...ns.refused.keys()]) {
			const snap = updated.get(key);
			const offered = ns.offered.get(key);
			// A new epoch is another broadcast too, even from the same front.
			if (snap === undefined || offered?.identity !== snap.identity || offered.route.epoch !== snap.route.epoch) {
				ns.refused.delete(key);
				ns.offered.delete(key);
			}
		}

		const held = new Map(ns.active);
		for (const [key, snap] of ns.active) {
			if (change(this.#advert, snap, updated.get(key)) !== "restart") continue;
			await target.withdraw(key);
			held.delete(key);
		}

		for (const [key, snap] of updated) {
			const prev = held.get(key);
			const diff = change(this.#advert, prev, snap);
			if (diff === "same") continue;

			let answer: Answer;
			if (prev !== undefined && diff !== "restart") {
				answer = await target.update(key, snap, diff);
			} else {
				// A fresh offer waits for what a refusal asked for, whatever brought us back.
				if (!this.#offerable(key, ns.refused)) continue;
				answer = await target.advertise(key, snap);
			}
			// `offered` pairs with `refused`, so a held or withdrawn path leaves nothing behind.
			if (answer === "held") {
				held.set(key, snap);
				ns.refused.delete(key);
				ns.offered.delete(key);
			} else {
				held.delete(key);
				if (answer !== "dropped") {
					ns.refused.set(key, answer);
					ns.offered.set(key, snap);
				}
			}
		}

		ns.active = held;

		// Whatever we wanted up and could not get up. Stream credit freeing, a transient
		// failure clearing, or the peer starting to answer raises no signal of its own, so
		// the only way back is to ask again on a timer.
		return [...updated].some(
			([key, snap]) => change(this.#advert, held.get(key), snap) !== "same" && this.#pending(key, ns.refused),
		);
	}

	/**
	 * Whether a namespace may be offered to the peer right now.
	 *
	 * A peer that asked never to be offered it again means it, whatever brought us back;
	 * one that named a minimum wait gets it, even when our own backoff comes round sooner.
	 */
	#offerable(key: Path.Valid, refused: Map<Path.Valid, Refused>): boolean {
		const entry = refused.get(key);
		if (entry === undefined) return true;
		return entry !== "never" && Date.now() >= entry;
	}

	/**
	 * Whether the loop should keep coming back to a namespace the peer does not hold.
	 *
	 * Distinct from {@link offerable}, and the difference is what arms the retry: a
	 * namespace waiting out a minimum is not offerable yet but is still pending, and
	 * gating the timer on offerable instead would disarm it for exactly the wait it is
	 * supposed to be counting. Only a refusal that forbids retrying ends it.
	 */
	#pending(key: Path.Valid, refused: Map<Path.Valid, Refused>): boolean {
		return refused.get(key) !== "never";
	}

	/**
	 * What a REQUEST_ERROR's retry interval, in milliseconds, says about re-offering the
	 * namespace.
	 *
	 * Draft-14/15 carry no such field, so a 0 there says nothing and our own backoff
	 * stands; everywhere else 0 means the peer does not want this again.
	 */
	#refusal(err: RequestError): Answer {
		const version = this.#session.version;
		if (version === Version.DRAFT_14 || version === Version.DRAFT_15) return "dropped";
		return err.retryInterval === 0n ? "never" : Date.now() + Number(err.retryInterval);
	}

	/**
	 * Advertise one namespace on its own PUBLISH_NAMESPACE request. A declined request
	 * is logged and skipped: a peer that wants none of this rejects each one and stays
	 * connected.
	 */
	async #advertise(
		path: Path.Valid,
		requests: Requests,
		cluster: Cluster.Advert | undefined = this.#advert,
	): Promise<Answer> {
		const requestId = await this.#session.nextRequestId();
		if (requestId === undefined) return "dropped";

		// Opening is inside the try because it fails like any other advertisement does:
		// a peer out of stream credit rejects here, and letting that escape would unwind
		// the announce loop and take discovery down for the whole session, rather than
		// costing this one namespace a turn.
		let request: Stream | undefined;
		try {
			request = await this.#session.openBi();
			const stream = request;

			// Bounded, like the subscriber's own request: the loop is single-threaded over
			// advertisements, so a peer that takes the stream and answers nothing strands
			// every publish and withdrawal behind it. Timing out costs this namespace a
			// turn and leaves it outstanding, which the retry above re-offers.
			const answer = await withTimeout(
				(async (): Promise<Answer> => {
					await stream.writer.u53(PublishNamespace.id);
					const msg = new PublishNamespace({ requestId, trackNamespace: path, cluster });
					await msg.encode(stream.writer, this.#session.version);

					// Read response (RequestOk and PublishNamespaceOk share 0x07)
					const respTypeId = await stream.reader.u53();
					if (respTypeId === RequestError.id) {
						const err = await RequestError.decode(stream.reader, this.#session.version);
						const why = requestReason(
							err.errorCode,
							err.reasonPhrase,
							"publish_namespace",
							this.#session.version,
						);
						console.warn(`announce refused: broadcast=${path} error=${why}`);
						stream.abort(new Error(`PublishNamespace rejected: ${why}`));
						return this.#refusal(err);
					}
					if (respTypeId !== RequestOk.id) {
						throw new Error(`PublishNamespace rejected: typeId=0x${respTypeId.toString(16)}`);
					}
					// Draft-14 sends PublishNamespaceOk (requestId only, no parameters)
					if (this.#session.version === Version.DRAFT_14) {
						await PublishNamespaceOk.decode(stream.reader, this.#session.version);
					} else {
						await RequestOk.decode(stream.reader, this.#session.version);
					}
					return "held";
				})(),
				ADVERTISE_TIMEOUT_MS,
				`advertisement timed out after ${ADVERTISE_TIMEOUT_MS}ms waiting for the peer's answer`,
			);

			if (answer === "held") requests.set(path, { path, requestId, stream });
			return answer;
		} catch (err: unknown) {
			// The peer never answered the advertisement: a control timeout, not late content.
			const e = err instanceof TimeoutError ? controlTimeout(err) : error(err);
			console.warn(`announce failed: broadcast=${path} error=${reason(e)}`);
			request?.abort(e);
			return "dropped";
		}
	}

	/**
	 * Update a namespace the peer holds: REQUEST_UPDATE on the request that carries it,
	 * with only the parameters that changed, then its answer.
	 *
	 * Waiting for the answer keeps one update outstanding per stream, which satisfies any
	 * MAX_REQUEST_UPDATES the peer set without reading it; a change landing meanwhile waits
	 * for the next turn. The answer decides whether the peer still holds the namespace: a
	 * REQUEST_ERROR closes the peer's side and withdraws the advertisement, so ours is
	 * finished too, and an unanswered update is dropped abruptly, since a peer that ignored
	 * it cannot be assumed to hold either price. Either way the retry re-offers it fresh.
	 *
	 * A different original publisher is an update like any other: the receiver drains what
	 * it already serves from the old one and never splices the two.
	 */
	async #update(path: Path.Valid, requests: Requests, { from, to }: Reprice): Promise<Answer> {
		const request = requests.get(path);
		if (!request) return "dropped";

		const update: Cluster.Update = {
			hops: sameHops(from.hops, to.hops) ? undefined : to.hops,
			cost: from.cost === to.cost ? undefined : to.cost,
		};
		const version = this.#session.version;
		let answer: Answer;
		try {
			const requestId = await this.#session.nextRequestId();
			if (requestId === undefined) throw new Error("no request ID for the update");

			answer = await withTimeout(
				(async (): Promise<Answer> => {
					const { writer, reader } = request.stream;
					await writer.u53(PublishNamespaceUpdate.id);
					await new PublishNamespaceUpdate({ requestId, update }).encode(writer, version);

					const typeId = await reader.u53();
					if (typeId === RequestError.id) {
						const err = await RequestError.decode(reader, version);
						const why = requestReason(err.errorCode, err.reasonPhrase, "publish_namespace", version);
						console.warn(`publish_namespace update refused: broadcast=${path} error=${why}`);
						return this.#refusal(err);
					}
					if (typeId !== RequestOk.id) {
						throw new Error(`unexpected answer to the update: typeId=0x${typeId.toString(16)}`);
					}
					await RequestOk.decode(reader, version);
					return "held";
				})(),
				ADVERTISE_TIMEOUT_MS,
				`update timed out after ${ADVERTISE_TIMEOUT_MS}ms waiting for the peer's answer`,
			);
		} catch (err: unknown) {
			const e = err instanceof TimeoutError ? controlTimeout(err) : error(err);
			console.warn(`publish_namespace update failed: broadcast=${path} error=${reason(e)}`);
			requests.delete(path);
			request.stream.abort(e);
			return "dropped";
		}

		// The peer already closed out a refused request, and its STOP_SENDING can reject the
		// wait on our FIN. That is the withdrawal finishing, not a reason to end the loop.
		if (answer !== "held") await this.#withdraw(path, requests).catch(() => undefined);
		return answer;
	}

	/**
	 * Close out a namespace's PUBLISH_NAMESPACE request with PUBLISH_NAMESPACE_DONE.
	 */
	async #withdraw(path: Path.Valid, requests: Requests) {
		const request = requests.get(path);
		if (!request) return;
		requests.delete(path);

		// Draft-17+ removed PUBLISH_NAMESPACE_DONE: the close below is the whole
		// withdrawal. Sending it anyway puts the type on the wire before the body throws,
		// and a receiver reading 0x09 there has no choice but to treat it as a protocol
		// violation.
		const version = this.#session.version;
		if (version === Version.DRAFT_14 || version === Version.DRAFT_15 || version === Version.DRAFT_16) {
			try {
				await request.stream.writer.u53(PublishNamespaceDone.id);
				const done = new PublishNamespaceDone({ trackNamespace: request.path, requestId: request.requestId });
				await done.encode(request.stream.writer, version);
			} catch {
				// Stream might already be closed
			}
			request.stream.close();
			await request.stream.writer.closed;
			return;
		}
		request.stream.close();
		// Wait for transport acknowledgment before opening a replacement request.
		// This assumes the peer processes the withdrawal by then; FIN acknowledgment
		// does not itself acknowledge application processing across streams.
		await request.stream.writer.closed;
	}

	/**
	 * Handles an incoming TRACK_STATUS_REQUEST on a bidi stream.
	 *
	 * @internal
	 */
	async runTrackStatusRequest(msg: TrackStatusRequest, stream: Stream) {
		// TRACK_STATUS_ERROR is 0x0f on draft-14.
		await this.#refuseUnsupported(stream, msg.requestId, "track_status", 0x0f, "TRACK_STATUS is not supported");
	}

	/**
	 * Refuses an incoming SUBSCRIBE_TRACKS (draft-18+): we never send the PUBLISH it asks for.
	 * The reply carries no Request ID, so the message body is left unread.
	 *
	 * @internal
	 */
	async runSubscribeTracks(stream: Stream) {
		const version = this.#session.version;
		await stream.writer.u53(RequestError.id);
		await new RequestError({
			errorCode: toRequestCode("not_supported", "subscribe_namespace", version),
			reasonPhrase: "SUBSCRIBE_TRACKS is not supported",
		}).encode(stream.writer, version);
		stream.close();
	}

	/**
	 * Handles an incoming FETCH on a bidi stream. We serve none.
	 *
	 * @internal
	 */
	async runFetch(msg: Fetch, stream: Stream) {
		await this.#refuseUnsupported(stream, msg.requestId, "fetch", FetchError.id, "FETCH is not supported");
	}

	/**
	 * Refuse a request NOT_SUPPORTED. Draft-14 gives each request its own error message,
	 * `errorId14`, with the SUBSCRIBE_ERROR body; later drafts use REQUEST_ERROR.
	 */
	async #refuseUnsupported(
		stream: Stream,
		requestId: bigint,
		kind: RequestKind,
		errorId14: number,
		reasonPhrase: string,
	) {
		const version = this.#session.version;
		const errorCode = toRequestCode("not_supported", kind, version);
		if (version === Version.DRAFT_14) {
			await stream.writer.u53(errorId14);
			await new SubscribeError({ requestId, errorCode, reasonPhrase }).encode(stream.writer, version);
		} else {
			await stream.writer.u53(RequestError.id);
			await new RequestError({
				requestId: version === Version.DRAFT_15 || version === Version.DRAFT_16 ? requestId : undefined,
				errorCode,
				reasonPhrase,
			}).encode(stream.writer, version);
		}
		stream.close();
	}
}

/** A `{group, object}` position, in the Location Filter's own units. */
interface Location {
	/** The group's sequence number. */
	group: bigint;
	/** The object's id within that group. */
	object: bigint;
}

/** Where a range ends, inclusive. An absent `object` includes the whole end group. */
interface EndLocation {
	/** The last group to serve. */
	group: bigint;
	/** The last object within it, or the whole group when absent. */
	object?: bigint;
}

/**
 * The live edge a SUBSCRIBE resolves against, snapshotted once so the subscription floor,
 * the fill cap, and the advertised LARGEST_OBJECT all agree on where it is.
 */
interface LiveEdge {
	/** The newest group sequence, absent before any group exists. */
	latest?: bigint;

	/**
	 * The precise Largest Object. Absent when the track has no readable frame, in which case
	 * nothing is advertised and no fill is servable.
	 */
	largest?: Location;

	/**
	 * One past the Largest Object, which is where a Next Object subscription begins. With no
	 * readable object anywhere this falls back to the newest group's start, which excludes
	 * nothing the cache can still name.
	 */
	next?: Location;
}

/** The Locations a SUBSCRIBE's Location Filter selects, resolved against the live edge. */
interface ServeRange {
	/** The first Location to serve, or the start of the latest group when absent. */
	start?: Location;

	/**
	 * Where the range ends, inclusive, or open ended when absent. The subscription stays open
	 * once the range is exhausted; draft-20 removed the notion of a filter ending one.
	 */
	end?: EndLocation;
}

/** The slice of one group a subscription's {@link ServeRange} selects. */
interface GroupSlice {
	/** Objects dropped from the front; also the first written object's absolute id. */
	skip: number;

	/** One past the last object to write, when the filter ends inside this group. */
	until?: number;
}

/** A fill resolved to a single group of the cache. */
interface FillGroup {
	kind: "group";
	/** The group to serve. */
	sequence: bigint;
	/** Objects dropped from the front. */
	skip: bigint;
	/** One past the last object to write, capping the fill at the Largest Object snapshot. */
	until?: bigint;
}

/** What a draft-20 fill request resolves to. */
type FillServe =
	/** The range is empty, so no fetch stream is opened at all. */
	| { kind: "empty" }
	| FillGroup
	/**
	 * A range spanning several groups, which we do not serve: multi-group fetch
	 * serialization depends on a negotiated group order we do not implement, so the stream is
	 * reset instead, the draft's fill-failure signal.
	 */
	| { kind: "unsupported" };

/**
 * Take one group out of a cache view, without waiting.
 *
 * A fill's group is at or below the Largest Object snapshot, so it is either still in the
 * retained window or gone for good: nothing republishes an old sequence, and waiting for one
 * would park for the life of the track. Scanning past it, or running dry, is a cache miss,
 * which the caller turns into the draft's fill-failure reset.
 */
function takeGroup(cache: TrackSubscriber, sequence: number): group.Consumer {
	for (;;) {
		const group = cache.tryRecvGroup();
		if (!group) throw new Error(`group not found: ${sequence}`);
		if (group.sequence === sequence) return group;

		group.close();
		if (group.sequence > sequence) throw new Error(`group not found: ${sequence}`);
	}
}

/** `a - b`, saturating at zero the way the draft's unsigned arithmetic does. */
function saturatingSub(a: bigint, b: bigint): bigint {
	return a > b ? a - b : 0n;
}

/** Snapshot the live edge of a track. */
function liveEdge(track: TrackSubscriber): LiveEdge {
	const latest = track.latest();
	if (latest === undefined) return {};

	const largest = track.largest();
	if (!largest) return { latest: BigInt(latest), next: { group: BigInt(latest), object: 0n } };

	const group = BigInt(largest.group);
	const object = BigInt(largest.frame);
	return { latest: BigInt(latest), largest: { group, object }, next: { group, object: object + 1n } };
}

/**
 * Resolve a SUBSCRIBE's Location Filter into the range to serve.
 *
 * Only draft-20 is honored. Earlier drafts have a Filter Type tag whose absolute forms we
 * never served, and starting to interpret them now would change what an existing peer
 * receives; draft-20 is also the first version whose relative forms can name a past group
 * without the subscriber knowing Largest Object.
 */
function subscribeRange(msg: Subscribe, edge: LiveEdge, version: IetfVersion): ServeRange {
	if (!Filter.isDraft20(version)) {
		if (msg.filter.kind !== "nextObject" && msg.filter.kind !== "unfiltered") {
			console.warn(`filter not supported before draft-20, ignoring: ${msg.filter.kind}`);
		}
		return {};
	}

	return filterRange(msg.filter, edge);
}

/** The Locations a single Location Filter selects, resolved against the live edge. */
function filterRange(filter: Filter.Filter, edge: LiveEdge): ServeRange {
	switch (filter.kind) {
		// No restriction. moq-lite starts at the beginning of the latest group, which is the
		// join point it is built around; a subscription passes objects as they are published,
		// so an absent filter is not a request to replay history.
		case "unfiltered":
			return {};
		// `{Largest.Group, Largest.Object + 1}`. Everything below it, including the already
		// published head of the current group, is outside the requested range, so the join is
		// mid-group by construction. The draft pairs this with a fill when the subscriber
		// wants the head; see {@link Publisher.runFill}.
		case "nextObject":
			return { start: edge.next };
		// `{Largest.Group + 1 - groups, 0}`: 0 is the next group and 1 is the current one.
		// Counted from `Largest.Group`, which sits below the newest group while that group has
		// no objects yet; only with no largest at all does the newest group stand in for it.
		case "relative": {
			const base = edge.largest?.group ?? edge.latest;
			if (base === undefined) return {};
			return { start: { group: saturatingSub(base + 1n, filter.groups), object: 0n } };
		}
		case "absolute":
			return {
				start: { group: filter.startGroup, object: filter.startObject },
				end: filter.endGroup === undefined ? undefined : { group: filter.endGroup, object: filter.endObject },
			};
	}
}

/**
 * Trim a group to the filter's object bounds, so nothing outside the requested range is
 * sent. Interior groups are served whole.
 */
function groupSlice(range: ServeRange, sequence: number): GroupSlice {
	const at = BigInt(sequence);
	return {
		skip: range.start?.group === at ? Number(range.start.object) : 0,
		until: range.end?.group === at && range.end.object !== undefined ? Number(range.end.object) + 1 : undefined,
	};
}

/**
 * Resolve a fill request using the Fetch rules: relative to Largest Object and never
 * extending beyond it. An omitted Location Filter inherits the subscription's.
 */
function fillRange(fill: Filter.Fill, subscription: Filter.Filter, largest?: Location): FillServe {
	// A Range Filter narrows which objects pass, which we do not implement; serving the
	// unfiltered range instead would deliver objects the peer excluded, so refuse it.
	if (fill.rangeFilters) return { kind: "unsupported" };
	const filter = fill.filter ?? subscription;

	// Nothing published (or no precise edge to cap at) means no fill is servable; an empty
	// range opens no stream.
	if (!largest) return { kind: "empty" };

	let start: Location;
	switch (filter.kind) {
		// A Fetch without a filter is the whole track up to Largest Object.
		case "unfiltered":
			start = { group: 0n, object: 0n };
			break;
		// One past the edge, which for a Fetch is always empty.
		case "nextObject":
			return { kind: "empty" };
		case "relative":
			start = { group: saturatingSub(largest.group + 1n, filter.groups), object: 0n };
			break;
		case "absolute":
			start = { group: filter.startGroup, object: filter.startObject };
			break;
	}

	// Cap the requested end at Largest Object.
	let end: EndLocation = { group: largest.group, object: largest.object };
	if (filter.kind === "absolute" && filter.endGroup !== undefined) {
		const below =
			filter.endGroup < largest.group ||
			(filter.endGroup === largest.group && filter.endObject !== undefined && filter.endObject < largest.object);
		if (below) end = { group: filter.endGroup, object: filter.endObject };
	}

	if (
		start.group > end.group ||
		(start.group === end.group && end.object !== undefined && end.object < start.object)
	) {
		return { kind: "empty" };
	}
	if (start.group !== end.group) return { kind: "unsupported" };

	return {
		kind: "group",
		sequence: start.group,
		skip: start.object,
		until: end.object === undefined ? undefined : end.object + 1n,
	};
}

class UpdateFailed extends Error {
	constructor() {
		super("subscription update failed");
	}
}

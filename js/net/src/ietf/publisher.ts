import { type Dispose, type Getter, Signal } from "@moq/signals";
import type * as broadcast from "../broadcast.ts";
import { error, reason } from "../error.ts";
import type * as group from "../group.ts";
import { hooks } from "../internal.ts";
import type { Consumer as OriginConsumer } from "../origin.ts";
import * as Path from "../path.ts";
import { type Stream, Writer } from "../stream.ts";
import type { Timescale } from "../time.ts";
import type { Subscriber as TrackSubscriber } from "../track.ts";
import { withTimeout } from "../util/timeout.ts";
import * as Varint from "../varint.ts";
import type { Session } from "./adapter.ts";
import * as Cluster from "./cluster.ts";
import { FetchHeader } from "./fetch.ts";
import * as Filter from "./filter.ts";
import { FetchFrame, Frame, Group as GroupMessage } from "./object.ts";
import { fromWire, toWire } from "./priority.ts";
import * as Properties from "./properties.ts";
import { PublishDone } from "./publish.ts";
import { PublishNamespace, PublishNamespaceDone, PublishNamespaceOk } from "./publish_namespace.ts";
import { RequestError, RequestOk } from "./request.ts";
import { type Subscribe, SubscribeError, SubscribeOk } from "./subscribe.ts";
import {
	type SubscribeNamespace,
	SubscribeNamespaceEntry,
	SubscribeNamespaceEntryDone,
	SubscribeNamespaceOk,
} from "./subscribe_namespace.ts";
import { TrackStatus, type TrackStatusRequest } from "./track.ts";
import { type IetfVersion, Version } from "./version.ts";

/** First wait before re-offering a namespace the peer refused or we couldn't open for. */
const RETRY_BASE = 100;

/** Ceiling on that wait. The loop retries for the life of the session, so it must not spin. */
const RETRY_MAX = 5000;

/** PUBLISH_DONE statuses this implementation emits. Stable across drafts 14 through 19. */
const PUBLISH_DONE_STATUS = {
	INTERNAL_ERROR: 0x0,
	TRACK_ENDED: 0x2,
} as const;

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

/** What {@link Publisher.runGroup} needs to serve one group. */
interface RunGroup {
	/** The subscription's request ID, doubling as the track alias. */
	requestId: bigint;

	/** The group to serve. */
	group: group.Consumer;

	/** The track's advertised timescale, applied to every frame timestamp. */
	timescale: Timescale;

	/** The publisher's tie-break priority, already converted to the IETF wire convention. */
	publisherPriority: number;

	/**
	 * Whether objects carry their presentation timestamp.
	 *
	 * False once the subscriber sends INCLUDE_PROPERTIES=0: that drops TIMESCALE from
	 * SUBSCRIBE_OK, and a timestamp whose units were never declared is worse than none. Our
	 * own reader discards it; another may read it as some default and time the media wrong.
	 */
	stamped: boolean;

	/** The objects of this group the subscription's filter selects. */
	slice: GroupSlice;

	/** Settles when the subscriber leaves, dropping a group still queued for a stream slot. */
	unsubscribed: Promise<void>;
}

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

	/** The track's advertised timescale, applied to every frame timestamp. */
	timescale: Timescale;

	/** Whether objects carry their presentation timestamp; see {@link RunGroup.stamped}. */
	stamped: boolean;

	/** Settles when the subscriber leaves, releasing a fill still waiting on its group. */
	unsubscribed: Promise<void>;
}

/**
 * Handles publishing broadcasts using moq-transport protocol.
 * Uses the stream-per-request pattern (real bidi streams for v17, virtual for v14-v16).
 *
 * @internal
 */
export class Publisher {
	#quic: WebTransport;
	#session: Session;
	#requiresSolicitation: boolean;

	// The published broadcasts, borrowed from the origin this session serves. The origin
	// outlives the session, so this is read-only here: subscribe_namespace streams watch it
	// for changes, and closing the session leaves the broadcasts alone. The namespaces are
	// advertised with an unsolicited PUBLISH_NAMESPACE (see {@link runPublishNamespaces}), or on
	// request if the peer asked for that (see {@link runSubscribeNamespace}).
	#broadcasts: Getter<ReadonlyMap<Path.Valid, broadcast.Consumer> | undefined>;

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
		/** The Hop IDs the SETUP exchange settled (MoQ Cluster). */
		cluster?: Cluster.Hops;
	}) {
		this.#quic = quic;
		this.#session = session;
		this.#broadcasts = publish?.broadcasts ?? new Signal(new Map());
		this.#requiresSolicitation = requiresSolicitation;
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
		const broadcast = this.#broadcasts.peek()?.get(name);

		if (!broadcast) {
			// Write error response
			if (version === Version.DRAFT_14) {
				await stream.writer.u53(SubscribeError.id);
				const err = new SubscribeError({
					requestId: msg.requestId,
					errorCode: 404,
					reasonPhrase: "Broadcast not found",
				});
				await err.encode(stream.writer, version);
			} else {
				await stream.writer.u53(RequestError.id);
				const err = new RequestError({
					requestId: version === Version.DRAFT_15 || version === Version.DRAFT_16 ? msg.requestId : undefined,
					errorCode: 404,
					reasonPhrase: "Broadcast not found",
				});
				await err.encode(stream.writer, version);
			}
			stream.close();
			return;
		}

		const priority = fromWire(msg.subscriberPriority);
		const track = broadcast.subscribe(msg.trackName, {
			priority,
			// moq-transport has no subscriber latency parameter. Keep everything the
			// producer retained and let the receiving subscriber enforce its own budget.
			// Keep the sentinel encodable if this demand crosses a Lite hop before the
			// producer's retention bound is known.
			maxAge: Varint.MAX_U53,
		});

		let cache: TrackSubscriber | undefined;

		try {
			// Declaring the timescale is what opts the track into timestamps; every object
			// Timestamp below is in these units.
			const info = await track.info();
			const timescale = info.timescale;
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
			// moq-lite means by joining a live track.
			track.update({
				priority,
				maxAge: Varint.MAX_U53,
				startGroup: range.start && Number(range.start.group),
				endGroup: range.end && Number(range.end.group),
			});
			const startGroup = range.start ? Number(range.start.group) : track.latest();
			if (startGroup !== undefined) track.startAt(startGroup);

			// A fill reads the group cache through its own consumer, independent of the
			// subscription's cursor. Forked from this subscriber rather than resolved through
			// the broadcast again: a dynamic serve is one request per peer subscription, so
			// asking the broadcast would mint a second producer nobody has accepted.
			const fill =
				msg.fill && Filter.isDraft20(version) ? fillRange(msg.fill, msg.filter, edge.largest) : undefined;
			cache = fill && fill.kind !== "empty" ? track.fork({ priority, maxAge: Varint.MAX_U53 }) : undefined;

			// Send SUBSCRIBE_OK
			await stream.writer.u53(SubscribeOk.id);
			const ok = new SubscribeOk({
				requestId:
					version === Version.DRAFT_14 || version === Version.DRAFT_15 || version === Version.DRAFT_16
						? msg.requestId
						: undefined,
				trackAlias: msg.requestId,
				// Required once the track has content; a fill-requesting subscriber sizes its
				// backfill against this.
				largest: edge.largest && { groupId: edge.largest.group, objectId: edge.largest.object },
				properties: msg.propertiesWanted
					? // Declaring the timescale is what opts the track into timestamps; every
						// object Timestamp below is in these units. We serve the newest group
						// first, matching moq-lite.
						{ timescale, groupOrder: Properties.DESCENDING }
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
			void stream.reader.closed.then(
				() => {
					if (!finished) unsubscribe();
				},
				// A reset is always the peer.
				() => unsubscribe(),
			);

			// Serve track groups, racing with stream close (= Unsubscribe)
			const serving = (async () => {
				for (;;) {
					const group = await track.recvGroup();
					if (!group) return;

					// Past the filter's end. Dropped here rather than through `endAt`, which
					// parks a capped group instead: this range is fixed for the life of the
					// subscription, so a group above it is never coming back in, and holding
					// one keeps the loop from ever ending. A producer that publishes beyond
					// the end and then closes would otherwise strand PUBLISH_DONE.
					if (range.end !== undefined && BigInt(group.sequence) > range.end.group) {
						group.close();
						continue;
					}

					void this.#runGroup({
						requestId: msg.requestId,
						group,
						timescale,
						publisherPriority,
						stamped: msg.propertiesWanted,
						slice: groupSlice(range, group.sequence),
						unsubscribed,
					});
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
							stamped: msg.propertiesWanted,
							unsubscribed,
						})
					: Promise.resolve();

			let publishError: Error | undefined;
			try {
				await Promise.race([Promise.all([serving, filling]), stream.reader.closed]);
			} catch (err: unknown) {
				publishError = error(err);
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
					statusCode: publishError ? PUBLISH_DONE_STATUS.INTERNAL_ERROR : PUBLISH_DONE_STATUS.TRACK_ENDED,
					reasonPhrase: publishError ? "internal error" : "track ended",
				});
				await done.encode(stream.writer, version);
			} catch {
				// Stream might already be closed by peer.
			}

			// Only now is the close below ours. Claiming it any earlier would read a peer FIN
			// that lands while PublishDone is still going out as our own completion, leaving
			// queued groups to open for a subscriber that has already left.
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
		const { requestId, group, timescale, publisherPriority, stamped, slice, unsubscribed } = options;
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

			const header = new GroupMessage({
				trackAlias: requestId,
				groupId: group.sequence,
				subGroupId: 0,
				publisherPriority,
				flags: {
					// The object properties carry the timestamp, so there is nothing to write
					// when the track declared no units to read one in.
					hasExtensions: stamped,
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
				await hooks.guardGroup(group, header.encode(stream, this.#session.version));
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

					// Reading from the filter's start drops the objects below it, including any the
					// group's cache evicted: they are outside the requested range, so losing them is
					// not the gap that would otherwise reset this stream and forfeit the rest of the
					// group. An eviction at or above the start is a real gap and still throws.
					const read = await Promise.race([hooks.readGroupFrame(group, slice.skip), stream.closed]);
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
						await hooks.guardGroup(
							group,
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
	 * Serve a draft-20 fill on its own fetch stream: the requested range, read from the
	 * group cache, capped at the Largest Object snapshot.
	 *
	 * A fill is a promise once requested. An empty range opens no stream, but a range we
	 * cannot serve still opens one and resets it right after the FETCH_HEADER, the draft's
	 * fill-failure signal. Nothing here touches the subscription either way.
	 */
	async #runFill(options: RunFill) {
		const { requestId, fill, cache, timescale, stamped, unsubscribed } = options;
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

			await stream.u53(FetchHeader.type);
			await new FetchHeader({ requestId }).encode(stream, version);

			if (fill.kind !== "group") {
				throw new Error("a fill spanning several groups is not supported");
			}

			const group = takeGroup(cache, Number(fill.sequence));
			try {
				await this.#writeFillGroup(stream, group, fill, timescale, stamped, unsubscribed);
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
		timescale: Timescale,
		stamped: boolean,
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

			// Reading from the fill's start drops everything below it, evicted objects included;
			// see the same read in #runGroup.
			const frame = await Promise.race([
				group.readFrameSequence({ from: Number(fill.skip) }),
				stream.closed,
				cancelled,
			]);
			if (left) throw new Error("unsubscribed before the fill finished");
			if (!frame) break;
			next = BigInt(frame.sequence) + 1n;
			if (fill.until !== undefined && BigInt(frame.sequence) >= fill.until) break;

			const obj = new FetchFrame({ payload: frame.payload, timestamp: stamped ? frame.timestamp : undefined });
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
	 * This carries the advertisements only when the peer asked to be told on request
	 * (MoQ Solicit); otherwise {@link runPublishNamespaces} has already announced
	 * everything and repeating it here would leave the peer holding two sources for one
	 * broadcast. Draft-16+ streams Namespace entries inline; draft-14/15 predate those
	 * messages, so each advertisement is a PUBLISH_NAMESPACE request of its own.
	 *
	 * @internal
	 */
	async runSubscribeNamespace(msg: SubscribeNamespace, stream: Stream) {
		const version = this.#session.version;
		const prefix = msg.namespace;
		const legacy = version === Version.DRAFT_14 || version === Version.DRAFT_15;

		// Draft-14/15: the open PUBLISH_NAMESPACE request per advertised suffix.
		const requests = new Map<Path.Valid, { path: Path.Valid; requestId: bigint; stream: Stream }>();

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

			if (!this.#requiresSolicitation) {
				// Already announced, unasked. Hold the stream open until the peer is done.
				await stream.reader.closed;
				stream.close();
				return;
			}

			// Reports whether the peer now holds the namespace: an inline entry always
			// lands, but a PUBLISH_NAMESPACE request can be declined.
			const advertise = async (suffix: Path.Valid): Promise<boolean> => {
				if (legacy) {
					return await this.#advertise(Path.join(prefix, suffix), requests, refused);
				}

				await stream.writer.u53(SubscribeNamespaceEntry.id);
				await new SubscribeNamespaceEntry({ suffix, cluster: this.#advert }).encode(stream.writer, version);
				return true;
			};
			const withdraw = async (suffix: Path.Valid) => {
				if (legacy) {
					await this.#withdraw(Path.join(prefix, suffix), requests);
				} else {
					await stream.writer.u53(SubscribeNamespaceEntryDone.id);
					await new SubscribeNamespaceEntryDone({ suffix }).encode(stream.writer, version);
				}
			};

			// What the peer holds: keyed by suffix, valued by the routing front, so a republish
			// diffs as withdraw-then-advertise rather than nothing.
			let active = new Map<Path.Valid, broadcast.Consumer>();
			let retry = 0;
			// What the peer refused, and whether coming back is worth anything.
			const refused = new Map<Path.Valid, Refused>();
			// The front each refusal was about, so a republish at the same path clears it.
			const offered = new Map<Path.Valid, broadcast.Consumer>();

			for (;;) {
				// Subscribe BEFORE reconciling, for the same reason as
				// {@link runPublishNamespaces}: a publish landing while an advertisement
				// waits for its reply only notifies listeners already registered.
				// TODO Make a better helper within Signals.
				let dispose!: Dispose;
				const changed = new Promise<ReadonlyMap<Path.Valid, broadcast.Consumer> | undefined>((resolve) => {
					dispose = this.#broadcasts.changed(resolve);
				});

				const broadcasts = this.#broadcasts.peek();
				if (!broadcasts) {
					dispose();
					break;
				}

				const updated = new Map<Path.Valid, broadcast.Consumer>();
				for (const [name, front] of broadcasts) {
					const suffix = Path.stripPrefix(prefix, name);
					if (suffix === null) continue;
					updated.set(suffix, front);
				}

				// A namespace that is gone, or that a republish replaced, takes its refusal with
				// it: the peer refused a broadcast, not a path forever, so a different one at
				// the same path is offered again. The origin swaps the front in one mutation,
				// so the path never leaves `updated` and only the front says they differ.
				for (const path of [...refused.keys()]) {
					const suffix = Path.stripPrefix(prefix, path);
					const front = suffix === null ? undefined : updated.get(suffix);
					if (front === undefined || offered.get(path) !== front) {
						refused.delete(path);
						offered.delete(path);
					}
				}

				// Track what the peer holds rather than what we attempted: a declined
				// advertisement stays out of `held`, so the next turn retries it instead of
				// believing the namespace is already up.
				const held = new Map<Path.Valid, broadcast.Consumer>(active);
				// Withdraw first so a republish reads as withdraw-then-advertise (a restart).
				for (const [removed, front] of active) {
					if (updated.get(removed) === front) continue;
					await withdraw(removed);
					held.delete(removed);
				}
				for (const [added, front] of updated) {
					if (held.get(added) === front) continue;
					if (!this.#offerable(Path.join(prefix, added), refused)) continue;
					offered.set(Path.join(prefix, added), front);
					if (await advertise(added)) held.set(added, front);
				}

				active = held;

				// Whatever we wanted up and could not get up, as {@link runPublishNamespaces}
				// does: only a legacy request can be declined, and nothing about the peer
				// starting to answer raises a signal this loop is watching.
				const outstanding = [...updated].some(
					([suffix, front]) =>
						active.get(suffix) !== front && this.#pending(Path.join(prefix, suffix), refused),
				);
				retry = outstanding ? Math.min(retry ? retry * 2 : RETRY_BASE, RETRY_MAX) : 0;

				// Wait for the next change, or for the peer to unsubscribe.
				const next = await (retry
					? Promise.race([changed, stream.reader.closed, retryAfter(retry).then(() => broadcasts)])
					: Promise.race([changed, stream.reader.closed]));
				dispose();
				if (!next) break;
			}

			stream.close();
		} catch (err: unknown) {
			const e = error(err);
			console.debug(`subscribe_namespace stream error: ${reason(e)}`);
			stream.abort(e);
		} finally {
			// This subscription's advertisements die with it.
			for (const path of [...requests.keys()]) {
				await this.#withdraw(path, requests);
			}
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
	async runPublishNamespaces() {
		if (this.#requiresSolicitation) {
			// The peer asked to be told on request; runSubscribeNamespace answers it.
			return;
		}

		// The open PUBLISH_NAMESPACE request per advertised path.
		const requests = new Map<Path.Valid, { path: Path.Valid; requestId: bigint; stream: Stream }>();

		// The origin outlives the session and its signal never ends, so this loop needs the
		// session's own ending to stop: without it a closed connection leaves the loop parked
		// on a shared origin forever, waking on someone else's publish to fail on a dead
		// transport. `runSubscribeNamespace` gets this from the stream the peer asked on; an
		// unsolicited loop has no stream of its own, so the session is what it watches.
		const closed = this.#quic.closed.then(
			() => undefined,
			() => undefined,
		);

		try {
			// What the peer holds: keyed by path, valued by the routing front, so a republish
			// diffs as withdraw-then-advertise rather than nothing.
			let active = new Map<Path.Valid, broadcast.Consumer>();
			let retry = 0;
			// What the peer refused, and whether coming back is worth anything.
			const refused = new Map<Path.Valid, Refused>();
			// The front each refusal was about, so a republish at the same path clears it.
			const offered = new Map<Path.Valid, broadcast.Consumer>();

			for (;;) {
				// Subscribe BEFORE reconciling. Each advertisement below waits a round trip
				// for the peer's reply, and a publish that lands in that window notifies
				// only the listeners already registered; one created afterwards would sleep
				// through it and leave the namespace unadvertised until something unrelated
				// changed.
				// TODO Make a better helper within Signals.
				let dispose!: Dispose;
				const changed = new Promise<ReadonlyMap<Path.Valid, broadcast.Consumer> | undefined>((resolve) => {
					dispose = this.#broadcasts.changed(resolve);
				});

				const broadcasts = this.#broadcasts.peek();
				if (!broadcasts) {
					dispose();
					break;
				}

				const updated = new Map<Path.Valid, broadcast.Consumer>(broadcasts);

				// A namespace that is gone, or that a republish replaced, takes its refusal with
				// it: the peer refused a broadcast, not a path forever, so a different one at
				// the same path is offered again. Rust gets this by rebuilding the watched entry.
				for (const path of [...refused.keys()]) {
					const front = updated.get(path);
					if (front === undefined || offered.get(path) !== front) {
						refused.delete(path);
						offered.delete(path);
					}
				}

				// Withdraw first so a republish reads as withdraw-then-advertise (a restart)
				// rather than nothing: the front changed, so the peer is holding a broadcast
				// that no longer exists.
				for (const [removed, front] of active) {
					if (updated.get(removed) === front) continue;
					await this.#withdraw(removed, requests);
				}
				for (const [added, front] of updated) {
					if (active.get(added) === front) continue;
					if (!this.#offerable(added, refused)) continue;
					offered.set(added, front);
					await this.#advertise(added, requests, refused);
				}

				// What the peer holds, not what we attempted: a declined PUBLISH_NAMESPACE
				// leaves no request behind, so it stays outstanding below.
				active = new Map<Path.Valid, broadcast.Consumer>(
					[...requests.keys()].flatMap((path) => {
						const front = updated.get(path);
						return front ? [[path, front] as const] : [];
					}),
				);

				// Whatever we wanted up and could not get up. Stream credit freeing, a
				// transient failure clearing, or the peer starting to answer raises no
				// signal of its own, so the only way back is to ask again on a timer.
				const outstanding = [...updated].some(
					([path, front]) => active.get(path) !== front && this.#pending(path, refused),
				);
				retry = outstanding ? Math.min(retry ? retry * 2 : RETRY_BASE, RETRY_MAX) : 0;

				// Wait for the next change, which has already fired if one landed above.
				const next = await (retry
					? Promise.race([changed, closed, retryAfter(retry).then(() => broadcasts)])
					: Promise.race([changed, closed]));
				dispose();
				if (!next) break;
			}
		} catch (err: unknown) {
			// Nothing restarts this loop, so whatever got us here cost the session its
			// discovery. Not a debug-level event.
			console.warn(`publish_namespace loop failed: ${reason(error(err))}`);
		} finally {
			// Close out every open PUBLISH_NAMESPACE request.
			for (const path of [...requests.keys()]) {
				await this.#withdraw(path, requests);
			}
		}
	}

	/**
	 * Whether a namespace may be offered to the peer right now.
	 *
	 * A peer that asked never to be offered it again means it, whatever brought us back;
	 * one that named a minimum wait gets it, even when our own backoff comes round sooner.
	 */
	#offerable(path: Path.Valid, refused: Map<Path.Valid, Refused>): boolean {
		const entry = refused.get(path);
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
	#pending(path: Path.Valid, refused: Map<Path.Valid, Refused>): boolean {
		return refused.get(path) !== "never";
	}

	/**
	 * Advertise one namespace on its own PUBLISH_NAMESPACE request. A declined request
	 * is logged and skipped: a peer that wants none of this rejects each one and stays
	 * connected.
	 *
	 * `refused` records what a refusal said about coming back, so a peer that asked not to
	 * be offered a namespace again is not re-offered it by the retry above.
	 */
	async #advertise(
		path: Path.Valid,
		requests: Map<Path.Valid, { path: Path.Valid; requestId: bigint; stream: Stream }>,
		refused: Map<Path.Valid, Refused>,
	): Promise<boolean> {
		const requestId = await this.#session.nextRequestId();
		if (requestId === undefined) return false;

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
			await withTimeout(
				(async () => {
					await stream.writer.u53(PublishNamespace.id);
					const msg = new PublishNamespace({ requestId, trackNamespace: path, cluster: this.#advert });
					await msg.encode(stream.writer, this.#session.version);

					// Read response (RequestOk and PublishNamespaceOk share 0x07)
					const respTypeId = await stream.reader.u53();
					if (respTypeId === RequestError.id) {
						// The peer named how long to stay away, in milliseconds. Draft-14/15
						// carry no such field, so a 0 there says nothing and our own backoff
						// stands; everywhere else 0 means it does not want this again.
						const err = await RequestError.decode(stream.reader, this.#session.version);
						const legacy =
							this.#session.version === Version.DRAFT_14 || this.#session.version === Version.DRAFT_15;
						if (!legacy) {
							refused.set(
								path,
								err.retryInterval === 0n ? "never" : Date.now() + Number(err.retryInterval),
							);
						}
						throw new Error(`PublishNamespace rejected: ${err.errorCode} ${err.reasonPhrase}`);
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
				})(),
				ADVERTISE_TIMEOUT_MS,
				`advertisement timed out after ${ADVERTISE_TIMEOUT_MS}ms waiting for the peer's answer`,
			);

			requests.set(path, { path, requestId, stream: request });
			return true;
		} catch (err: unknown) {
			const e = error(err);
			console.warn(`announce failed: broadcast=${path} error=${reason(e)}`);
			request?.abort(e);
			return false;
		}
	}

	/**
	 * Close out a namespace's PUBLISH_NAMESPACE request with PUBLISH_NAMESPACE_DONE.
	 */
	async #withdraw(
		path: Path.Valid,
		requests: Map<Path.Valid, { path: Path.Valid; requestId: bigint; stream: Stream }>,
	) {
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
		}
		request.stream.close();
	}

	/**
	 * Handles an incoming TRACK_STATUS_REQUEST on a bidi stream.
	 *
	 * @internal
	 */
	async runTrackStatusRequest(msg: TrackStatusRequest, stream: Stream) {
		const version = this.#session.version;

		if (version === Version.DRAFT_14) {
			// v14: respond with TrackStatus (0x0E = TRACK_STATUS_OK)
			await stream.writer.u53(TrackStatus.id);
			const status = new TrackStatus({
				trackNamespace: msg.trackNamespace,
				trackName: msg.trackName,
				statusCode: TrackStatus.STATUS_NOT_FOUND,
				lastGroupId: 0n,
				lastObjectId: 0n,
			});
			await status.encode(stream.writer, version);
		} else {
			// v15+: respond with RequestOk (0x07)
			await stream.writer.u53(RequestOk.id);
			const ok = new RequestOk({
				requestId: version === Version.DRAFT_15 || version === Version.DRAFT_16 ? msg.requestId : undefined,
			});
			await ok.encode(stream.writer, version);
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

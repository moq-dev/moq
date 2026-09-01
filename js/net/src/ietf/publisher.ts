import { type Dispose, Signal } from "@moq/signals";
import type * as broadcast from "../broadcast.ts";
import { error, reason } from "../error.ts";
import type * as group from "../group.ts";
import * as Path from "../path.ts";
import { type Stream, Writer } from "../stream.ts";
import type { Timescale } from "../time.ts";
import { withTimeout } from "../util/timeout.ts";
import type { Session } from "./adapter.ts";
import * as Cluster from "./cluster.ts";
import { Frame, Group as GroupMessage } from "./object.ts";
import { fromWire } from "./priority.ts";
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
import { Version } from "./version.ts";

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

	/** Settles when the subscriber leaves, dropping a group still queued for a stream slot. */
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

	// What every advertisement carries on a session that negotiated the MoQ Cluster
	// extension: a hop path holding our own id, so the peer can tell that what it hears
	// back came from us. `undefined` when nothing negotiated it.
	#advert?: Cluster.Advert;

	// Our published broadcasts.
	// It's a signal so we can live update any subscribe_namespace streams.
	#broadcasts = new Signal<Map<Path.Valid, broadcast.Producer> | undefined>(new Map());

	/**
	 * Creates a new Publisher instance.
	 *
	 * @internal
	 */
	constructor({
		quic,
		session,
		requiresSolicitation,
		cluster,
	}: {
		/** The WebTransport session, for uni streams. */
		quic: WebTransport;
		/** The session abstraction for bidi streams and request IDs. */
		session: Session;
		/** Whether the peer's SETUP asked to be told on request (MoQ Solicit). */
		requiresSolicitation: boolean;
		/** The Hop IDs the SETUP exchange settled (MoQ Cluster). */
		cluster?: Cluster.Hops;
	}) {
		this.#quic = quic;
		this.#session = session;
		this.#requiresSolicitation = requiresSolicitation;
		this.#advert = Cluster.advertise(cluster);
	}

	/**
	 * Publishes a broadcast with any associated tracks.
	 * The namespace is advertised with an unsolicited PUBLISH_NAMESPACE, or on request
	 * if the peer asked for that (see {@link runPublishNamespaces}).
	 */
	publish(path: Path.Valid, broadcast: broadcast.Producer) {
		this.#broadcasts.mutate((broadcasts) => {
			if (!broadcasts) throw new Error("closed");
			broadcasts.set(path, broadcast);
		});

		// Remove the broadcast from the lookup when it's closed, unless the path was republished.
		void broadcast.closed.then(() => {
			this.#broadcasts.mutate((broadcasts) => {
				if (broadcasts?.get(path) === broadcast) {
					broadcasts.delete(path);
				}
			});
		});
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

		const track = broadcast.subscribe(msg.trackName, { priority: fromWire(msg.subscriberPriority) });

		try {
			// Declaring the timescale is what opts the track into timestamps; every object
			// Timestamp below is in these units.
			const timescale = (await track.info()).timescale;

			// Send SUBSCRIBE_OK
			await stream.writer.u53(SubscribeOk.id);
			const ok = new SubscribeOk({
				requestId:
					version === Version.DRAFT_14 || version === Version.DRAFT_15 || version === Version.DRAFT_16
						? msg.requestId
						: undefined,
				trackAlias: msg.requestId,
				timescale,
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
					void this.#runGroup({ requestId: msg.requestId, group, timescale, unsubscribed });
				}
			})();

			let publishError: Error | undefined;
			try {
				await Promise.race([serving, stream.reader.closed]);
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
		}
	}

	/**
	 * Runs a group and sends its frames using ObjectStream (Subgroup delivery mode).
	 */
	async #runGroup(options: RunGroup) {
		const { requestId, group, timescale, unsubscribed } = options;
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
				publisherPriority: 0,
				flags: {
					hasExtensions: true,
					hasSubgroup: false,
					hasSubgroupObject: false,
					hasEnd: true,
					hasPriority: true,
					firstObject: true,
				},
			});

			await header.encode(stream, this.#session.version);

			try {
				for (;;) {
					const frame = await Promise.race([group.readFrame(), stream.closed]);
					if (!frame) break;

					const obj = new Frame({ payload: frame.payload, timestamp: frame.timestamp });
					await obj.encode(stream, header.flags, timescale, this.#session.version);
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

			let active = new Set<Path.Valid>();
			let retry = 0;
			// What the peer refused, and whether coming back is worth anything.
			const refused = new Map<Path.Valid, Refused>();

			for (;;) {
				// Subscribe BEFORE reconciling, for the same reason as
				// {@link runPublishNamespaces}: a publish landing while an advertisement
				// waits for its reply only notifies listeners already registered.
				// TODO Make a better helper within Signals.
				let dispose!: Dispose;
				const changed = new Promise<Map<Path.Valid, broadcast.Producer> | undefined>((resolve) => {
					dispose = this.#broadcasts.changed(resolve);
				});

				const broadcasts = this.#broadcasts.peek();
				if (!broadcasts) {
					dispose();
					break;
				}

				const updated = new Set<Path.Valid>();
				for (const name of broadcasts.keys()) {
					const suffix = Path.stripPrefix(prefix, name);
					if (suffix === null) continue;
					updated.add(suffix);
				}

				// A namespace that is gone takes its refusal with it, so re-announcing the
				// path offers it again.
				const live = new Set<Path.Valid>([...updated].map((suffix) => Path.join(prefix, suffix)));
				for (const path of [...refused.keys()]) {
					if (!live.has(path)) refused.delete(path);
				}

				// Track what the peer holds rather than what we attempted: a declined
				// advertisement stays out of `held`, so the next turn retries it instead of
				// believing the namespace is already up.
				const held = new Set<Path.Valid>(active);
				for (const added of updated.difference(active)) {
					if (this.#offerable(Path.join(prefix, added), refused)) {
						if (await advertise(added)) held.add(added);
					}
				}
				for (const removed of active.difference(updated)) {
					await withdraw(removed);
					held.delete(removed);
				}

				active = held;

				// Whatever we wanted up and could not get up, as {@link runPublishNamespaces}
				// does: only a legacy request can be declined, and nothing about the peer
				// starting to answer raises a signal this loop is watching.
				const outstanding = [...updated.difference(active)].some((suffix) =>
					this.#pending(Path.join(prefix, suffix), refused),
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

		try {
			let active = new Set<Path.Valid>();
			let retry = 0;
			// What the peer refused, and whether coming back is worth anything.
			const refused = new Map<Path.Valid, Refused>();

			for (;;) {
				// Subscribe BEFORE reconciling. Each advertisement below waits a round trip
				// for the peer's reply, and a publish that lands in that window notifies
				// only the listeners already registered; one created afterwards would sleep
				// through it and leave the namespace unadvertised until something unrelated
				// changed.
				// TODO Make a better helper within Signals.
				let dispose!: Dispose;
				const changed = new Promise<Map<Path.Valid, broadcast.Producer> | undefined>((resolve) => {
					dispose = this.#broadcasts.changed(resolve);
				});

				const broadcasts = this.#broadcasts.peek();
				if (!broadcasts) {
					dispose();
					break;
				}

				const updated = new Set<Path.Valid>(broadcasts.keys());

				// A namespace that is gone takes its refusal with it, so re-announcing the
				// path offers it again. Rust gets this by rebuilding the watched entry.
				for (const path of [...refused.keys()]) {
					if (!updated.has(path)) refused.delete(path);
				}

				for (const added of updated.difference(active)) {
					if (this.#offerable(added, refused)) {
						await this.#advertise(added, requests, refused);
					}
				}
				for (const removed of active.difference(updated)) {
					await this.#withdraw(removed, requests);
				}

				// What the peer holds, not what we attempted: a declined PUBLISH_NAMESPACE
				// leaves no request behind, so it stays outstanding below.
				active = new Set<Path.Valid>(requests.keys());

				// Whatever we wanted up and could not get up. Stream credit freeing, a
				// transient failure clearing, or the peer starting to answer raises no
				// signal of its own, so the only way back is to ask again on a timer.
				const outstanding = [...updated.difference(active)].some((path) => this.#pending(path, refused));
				retry = outstanding ? Math.min(retry ? retry * 2 : RETRY_BASE, RETRY_MAX) : 0;

				// Wait for the next change, which has already fired if one landed above.
				const next = await (retry
					? Promise.race([changed, retryAfter(retry).then(() => broadcasts)])
					: changed);
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

	/**
	 * Closes every published broadcast and stops accepting new ones.
	 *
	 * @internal
	 */
	close() {
		this.#broadcasts.update((broadcasts) => {
			for (const broadcast of broadcasts?.values() ?? []) {
				broadcast.close();
			}
			return undefined;
		});
	}
}

import { type Dispose, type Getter, Signal } from "@moq/signals";
import type * as broadcast from "../broadcast.ts";
import { error, reason } from "../error.ts";
import type * as group from "../group.ts";
import type { Consumer as OriginConsumer } from "../origin.ts";
import * as Path from "../path.ts";
import { type Stream, Writer } from "../stream.ts";
import type { Timescale } from "../time.ts";
import type { Session } from "./adapter.ts";
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

	// The published broadcasts, borrowed from the origin this session serves. The origin
	// outlives the session, so this is read-only here: subscribe_namespace streams watch it
	// for changes, and closing the session leaves the broadcasts alone. The namespaces are
	// only advertised in response to a SUBSCRIBE_NAMESPACE (see {@link runSubscribeNamespace}),
	// mirroring the moq-lite publisher.
	#broadcasts: Getter<ReadonlyMap<Path.Valid, broadcast.Consumer> | undefined>;

	/**
	 * Creates a new Publisher instance.
	 * @param quic - The WebTransport session (for uni streams)
	 * @param session - The session abstraction for bidi streams and request IDs
	 * @param publish - The origin whose broadcasts this session serves; omit to publish nothing
	 *
	 * @internal
	 */
	constructor(quic: WebTransport, session: Session, publish?: OriginConsumer) {
		this.#quic = quic;
		this.#session = session;
		this.#broadcasts = publish?.broadcasts ?? new Signal(new Map());
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

			await Promise.race([serving, stream.reader.closed]);

			console.debug(`publish done: broadcast=${name} track=${track.name}`);

			// v14-v16: send PublishDone before closing
			if (version === Version.DRAFT_14 || version === Version.DRAFT_15 || version === Version.DRAFT_16) {
				try {
					await stream.writer.u53(PublishDone.id);
					const done = new PublishDone({
						requestId: msg.requestId,
						statusCode: 200,
						reasonPhrase: "OK",
					});
					await done.encode(stream.writer, version);
				} catch {
					// Stream might already be closed by peer
				}
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
	 * Namespaces are only advertised in response to one of these, and the state
	 * is local to this stream's task, mirroring the moq-lite publisher. Draft-16+
	 * streams Namespace/NamespaceDone entries inline; draft-14/15 predate those
	 * messages, so each advertisement is a PUBLISH_NAMESPACE request of its own
	 * over the control stream, closed out with PUBLISH_NAMESPACE_DONE.
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

			const advertise = async (suffix: Path.Valid) => {
				if (legacy) {
					await this.#advertise(prefix, suffix, requests);
				} else {
					await stream.writer.u53(SubscribeNamespaceEntry.id);
					await new SubscribeNamespaceEntry({ suffix }).encode(stream.writer, version);
				}
			};
			const withdraw = async (suffix: Path.Valid) => {
				if (legacy) {
					await this.#withdraw(suffix, requests);
				} else {
					await stream.writer.u53(SubscribeNamespaceEntryDone.id);
					await new SubscribeNamespaceEntryDone({ suffix }).encode(stream.writer, version);
				}
			};

			// Advertise the currently published broadcasts under the prefix. Keyed by suffix,
			// valued by the routing front, so a republish diffs as withdraw-then-advertise
			// rather than nothing.
			let active = new Map<Path.Valid, broadcast.Consumer>();
			for (const [name, front] of this.#broadcasts.peek() ?? []) {
				const suffix = Path.stripPrefix(prefix, name);
				if (suffix === null) continue;
				active.set(suffix, front);
			}
			for (const suffix of active.keys()) {
				await advertise(suffix);
			}

			// Wait for updates to the broadcasts.
			for (;;) {
				// TODO Make a better helper within Signals.
				let dispose!: Dispose;
				const changed = new Promise<ReadonlyMap<Path.Valid, broadcast.Consumer> | undefined>((resolve) => {
					dispose = this.#broadcasts.changed(resolve);
				});

				// Wait until the map of broadcasts changes or the peer unsubscribes.
				const broadcasts = await Promise.race([changed, stream.reader.closed]);
				dispose();
				if (!broadcasts) break;

				const newActive = new Map<Path.Valid, broadcast.Consumer>();
				for (const [name, front] of broadcasts) {
					const suffix = Path.stripPrefix(prefix, name);
					if (suffix === null) continue;
					newActive.set(suffix, front);
				}

				// Withdraw first so a republish reads as withdraw-then-advertise (a restart).
				for (const [removed, front] of active) {
					if (newActive.get(removed) !== front) await withdraw(removed);
				}
				for (const [added, front] of newActive) {
					if (active.get(added) !== front) await advertise(added);
				}

				active = newActive;
			}

			stream.close();
		} catch (err: unknown) {
			const e = error(err);
			console.debug(`subscribe_namespace stream error: ${reason(e)}`);
			stream.abort(e);
		} finally {
			// This subscription's advertisements die with it: close out every open
			// draft-14/15 PUBLISH_NAMESPACE request.
			for (const suffix of [...requests.keys()]) {
				await this.#withdraw(suffix, requests);
			}
		}
	}

	/**
	 * Advertise one namespace on its own PUBLISH_NAMESPACE request (draft-14/15,
	 * which have no NAMESPACE entry message). A declined request is logged and
	 * skipped rather than failing the subscription.
	 */
	async #advertise(
		prefix: Path.Valid,
		suffix: Path.Valid,
		requests: Map<Path.Valid, { path: Path.Valid; requestId: bigint; stream: Stream }>,
	) {
		const path = Path.join(prefix, suffix);

		const requestId = await this.#session.nextRequestId();
		if (requestId === undefined) return;

		const request = await this.#session.openBi();
		try {
			await request.writer.u53(PublishNamespace.id);
			const msg = new PublishNamespace({ requestId, trackNamespace: path });
			await msg.encode(request.writer, this.#session.version);

			// Read response (RequestOk and PublishNamespaceOk share 0x07)
			const respTypeId = await request.reader.u53();
			if (respTypeId !== RequestOk.id) {
				throw new Error(`PublishNamespace rejected: typeId=0x${respTypeId.toString(16)}`);
			}
			// Draft-14 sends PublishNamespaceOk (requestId only, no parameters)
			if (this.#session.version === Version.DRAFT_14) {
				await PublishNamespaceOk.decode(request.reader, this.#session.version);
			} else {
				await RequestOk.decode(request.reader, this.#session.version);
			}

			requests.set(suffix, { path, requestId, stream: request });
		} catch (err: unknown) {
			const e = error(err);
			console.warn(`announce failed: broadcast=${path} error=${reason(e)}`);
			request.abort(e);
		}
	}

	/**
	 * Close out a namespace's PUBLISH_NAMESPACE request with PUBLISH_NAMESPACE_DONE
	 * (draft-14/15).
	 */
	async #withdraw(
		suffix: Path.Valid,
		requests: Map<Path.Valid, { path: Path.Valid; requestId: bigint; stream: Stream }>,
	) {
		const request = requests.get(suffix);
		if (!request) return;
		requests.delete(suffix);

		try {
			await request.stream.writer.u53(PublishNamespaceDone.id);
			const done = new PublishNamespaceDone({ trackNamespace: request.path, requestId: request.requestId });
			await done.encode(request.stream.writer, this.#session.version);
		} catch {
			// Stream might already be closed
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

import { type Dispose, Signal } from "@moq/signals";
import type * as broadcast from "../broadcast.ts";
import { error, reason } from "../error.ts";
import type * as group from "../group.ts";
import * as Path from "../path.ts";
import { type Stream, Writer } from "../stream.ts";
import type { Timescale } from "../time.ts";
import type { Session } from "./adapter.ts";
import { Frame, Group as GroupMessage } from "./object.ts";
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

/**
 * Handles publishing broadcasts using moq-transport protocol.
 * Uses the stream-per-request pattern (real bidi streams for v17, virtual for v14-v16).
 *
 * @internal
 */
export class Publisher {
	#quic: WebTransport;
	#session: Session;

	// Our published broadcasts.
	// It's a signal so we can live update any subscribe_namespace streams.
	#broadcasts = new Signal<Map<Path.Valid, broadcast.Producer> | undefined>(new Map());

	/**
	 * Creates a new Publisher instance.
	 * @param quic - The WebTransport session (for uni streams)
	 * @param session - The session abstraction for bidi streams and request IDs
	 *
	 * @internal
	 */
	constructor(quic: WebTransport, session: Session) {
		this.#quic = quic;
		this.#session = session;
	}

	/**
	 * Publishes a broadcast with any associated tracks.
	 * The namespace is only advertised in response to a SUBSCRIBE_NAMESPACE
	 * (see {@link runSubscribeNamespace}), mirroring the moq-lite publisher.
	 */
	publish(path: Path.Valid, broadcast: broadcast.Producer) {
		this.#broadcasts.mutate((broadcasts) => {
			if (!broadcasts) throw new Error("closed");
			broadcasts.set(path, broadcast);
		});

		// Remove the broadcast from the lookup when it's closed.
		void broadcast.closed.then(() => {
			this.#broadcasts.mutate((broadcasts) => {
				broadcasts?.delete(path);
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

		const track = broadcast.subscribe(msg.trackName, { priority: msg.subscriberPriority });

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

			// Serve track groups, racing with stream close (= Unsubscribe)
			const serving = (async () => {
				for (;;) {
					const group = await track.recvGroup();
					if (!group) return;
					void this.#runGroup(msg.requestId, group, timescale);
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
	async #runGroup(requestId: bigint, group: group.Consumer, timescale: Timescale) {
		try {
			const stream = await Writer.open(this.#quic, this.#session.version);

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

			// Advertise the currently published broadcasts under the prefix.
			let active = new Set<Path.Valid>();
			for (const name of this.#broadcasts.peek()?.keys() ?? []) {
				const suffix = Path.stripPrefix(prefix, name);
				if (suffix === null) continue;
				active.add(suffix);
			}
			for (const suffix of active) {
				await advertise(suffix);
			}

			// Wait for updates to the broadcasts.
			for (;;) {
				// TODO Make a better helper within Signals.
				let dispose!: Dispose;
				const changed = new Promise<Map<Path.Valid, broadcast.Producer> | undefined>((resolve) => {
					dispose = this.#broadcasts.changed(resolve);
				});

				// Wait until the map of broadcasts changes or the peer unsubscribes.
				const broadcasts = await Promise.race([changed, stream.reader.closed]);
				dispose();
				if (!broadcasts) break;

				const newActive = new Set<Path.Valid>();
				for (const name of broadcasts.keys()) {
					const suffix = Path.stripPrefix(prefix, name);
					if (suffix === null) continue;
					newActive.add(suffix);
				}

				for (const added of newActive.difference(active)) {
					await advertise(added);
				}
				for (const removed of active.difference(newActive)) {
					await withdraw(removed);
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

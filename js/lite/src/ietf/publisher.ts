import { Announced } from "../announced.ts";
import type { Broadcast } from "../broadcast.ts";
import type { Group } from "../group.ts";
import * as Path from "../path.ts";
import { type Stream, Writer } from "../stream.ts";
import { error } from "../util/error.ts";
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
	#broadcasts: Map<Path.Valid, Broadcast> = new Map();

	// Any consumers that want each new announcement.
	#announcedConsumers = new Set<Announced>();

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
	 * Opens a bidi stream to send PublishNamespace and waits for response.
	 */
	publish(path: Path.Valid, broadcast: Broadcast) {
		this.#broadcasts.set(path, broadcast);
		this.#notifyConsumers(path, true);
		void this.#runPublish(path, broadcast);
	}

	async #runPublish(path: Path.Valid, broadcast: Broadcast) {
		try {
			const requestId = await this.#session.nextRequestId();
			if (requestId === undefined) return;

			const stream = await this.#session.openBi();

			try {
				// Write PublishNamespace
				await stream.writer.u53(PublishNamespace.id);
				const msg = new PublishNamespace({ requestId, trackNamespace: path });
				await msg.encode(stream.writer, this.#session.version);

				// Read response (RequestOk and PublishNamespaceOk share 0x07)
				const respTypeId = await stream.reader.u53();
				if (respTypeId === RequestOk.id) {
					// Draft-14 sends PublishNamespaceOk (requestId only, no parameters)
					if (this.#session.version === Version.DRAFT_14) {
						await PublishNamespaceOk.decode(stream.reader, this.#session.version);
					} else {
						await RequestOk.decode(stream.reader, this.#session.version);
					}
				} else {
					throw new Error(`PublishNamespace rejected: typeId=0x${respTypeId.toString(16)}`);
				}

				// Wait for broadcast to close or stream to close (peer cancelled)
				await Promise.race([broadcast.closed, stream.reader.closed]);

				// For v14-v16: send explicit PublishNamespaceDone
				if (this.#session.version !== Version.DRAFT_17) {
					try {
						await stream.writer.u53(PublishNamespaceDone.id);
						const done = new PublishNamespaceDone({ trackNamespace: path, requestId });
						await done.encode(stream.writer, this.#session.version);
					} catch {
						// Stream might already be closed
					}
				}

				stream.close();
			} catch (err) {
				stream.abort(error(err));
				throw err;
			}
		} catch (err: unknown) {
			const e = error(err);
			console.warn(`announce failed: broadcast=${path} error=${e.message}`);
		} finally {
			broadcast.close();
			this.#broadcasts.delete(path);
			this.#notifyConsumers(path, false);
		}
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
		const broadcast = this.#broadcasts.get(name);

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
					requestId: version === Version.DRAFT_17 ? undefined : msg.requestId,
					errorCode: 404,
					reasonPhrase: "Broadcast not found",
				});
				await err.encode(stream.writer, version);
			}
			stream.close();
			return;
		}

		const track = broadcast.subscribe(msg.trackName, msg.subscriberPriority);

		try {
			// Send SUBSCRIBE_OK
			await stream.writer.u53(SubscribeOk.id);
			const ok = new SubscribeOk({
				requestId: version === Version.DRAFT_17 ? undefined : msg.requestId,
				trackAlias: msg.requestId,
			});
			await ok.encode(stream.writer, version);
			console.debug(`publish ok: broadcast=${name} track=${track.name}`);

			// Serve track groups, racing with stream close (= Unsubscribe)
			const serving = (async () => {
				for (;;) {
					const group = await track.nextGroup();
					if (!group) return;
					void this.#runGroup(msg.requestId, group);
				}
			})();

			await Promise.race([serving, stream.reader.closed]);

			console.debug(`publish done: broadcast=${name} track=${track.name}`);

			// v14-v16: send PublishDone before closing
			if (version !== Version.DRAFT_17) {
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
			console.warn(`publish error: broadcast=${name} track=${track.name} error=${e.message}`);
			stream.abort(e);
		} finally {
			track.close();
		}
	}

	/**
	 * Runs a group and sends its frames using ObjectStream (Subgroup delivery mode).
	 */
	async #runGroup(requestId: bigint, group: Group) {
		try {
			const stream = await Writer.open(this.#quic, this.#session.version);

			const header = new GroupMessage({
				trackAlias: requestId,
				groupId: group.sequence,
				subGroupId: 0,
				publisherPriority: 0,
				flags: {
					hasExtensions: false,
					hasSubgroup: false,
					hasSubgroupObject: false,
					hasEnd: true,
					hasPriority: true,
				},
			});

			await header.encode(stream);

			try {
				for (;;) {
					const frame = await Promise.race([group.readFrame(), stream.closed]);
					if (!frame) break;

					const obj = new Frame({ payload: frame });
					await obj.encode(stream, header.flags);
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
	 * Sends RequestOk, then streams Namespace/NamespaceDone entries.
	 *
	 * @internal
	 */
	async runSubscribeNamespace(msg: SubscribeNamespace, stream: Stream) {
		const version = this.#session.version;
		const prefix = msg.namespace;

		try {
			// Send OK response
			if (version === Version.DRAFT_14) {
				await stream.writer.u53(SubscribeNamespaceOk.id);
				const ok = new SubscribeNamespaceOk({ requestId: msg.requestId });
				await ok.encode(stream.writer, version);
			} else {
				await stream.writer.u53(RequestOk.id);
				const ok = new RequestOk({ requestId: version === Version.DRAFT_17 ? undefined : msg.requestId });
				await ok.encode(stream.writer, version);
			}

			// Create an Announced consumer and seed it with current broadcasts
			const announced = new Announced(prefix);
			for (const name of this.#broadcasts.keys()) {
				const suffix = Path.stripPrefix(prefix, name);
				if (suffix === null) continue;
				announced.append({ path: suffix, active: true });
			}
			this.#announcedConsumers.add(announced);

			// Close the consumer when the stream closes
			stream.reader.closed.then(
				() => announced.close(),
				() => announced.close(),
			);

			try {
				for (;;) {
					const entry = await announced.next();
					if (!entry) break;

					if (entry.active) {
						await stream.writer.u53(SubscribeNamespaceEntry.id);
						const e = new SubscribeNamespaceEntry({ suffix: entry.path });
						await e.encode(stream.writer, version);
					} else {
						await stream.writer.u53(SubscribeNamespaceEntryDone.id);
						const e = new SubscribeNamespaceEntryDone({ suffix: entry.path });
						await e.encode(stream.writer, version);
					}
				}
			} finally {
				announced.close();
				this.#announcedConsumers.delete(announced);
			}

			stream.close();
		} catch (err: unknown) {
			const e = error(err);
			console.debug(`subscribe_namespace stream error: ${e.message}`);
			stream.abort(e);
		}
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
			const ok = new RequestOk({ requestId: version === Version.DRAFT_17 ? undefined : msg.requestId });
			await ok.encode(stream.writer, version);
		}
		stream.close();
	}

	#notifyConsumers(path: Path.Valid, active: boolean) {
		for (const consumer of this.#announcedConsumers) {
			const suffix = Path.stripPrefix(consumer.prefix, path);
			if (suffix === null) continue;
			try {
				consumer.append({ path: suffix, active });
			} catch {
				// Consumer already closed, will be cleaned up
			}
		}
	}
}

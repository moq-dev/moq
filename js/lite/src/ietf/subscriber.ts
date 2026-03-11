import { Announced } from "../announced.ts";
import { Broadcast, type TrackRequest } from "../broadcast.ts";
import { Group } from "../group.ts";
import * as Path from "../path.ts";
import type { Reader, Stream } from "../stream.ts";
import type { Track } from "../track.ts";
import { error } from "../util/error.ts";
import type { Session } from "./adapter.ts";
import { Frame, type Group as GroupMessage } from "./object.ts";
import { type Publish, PublishError } from "./publish.ts";
import type { PublishNamespace } from "./publish_namespace.ts";
import { RequestError, RequestOk } from "./request.ts";
import { Subscribe, SubscribeOk, Unsubscribe } from "./subscribe.ts";
import {
	SubscribeNamespace,
	SubscribeNamespaceEntry,
	SubscribeNamespaceEntryDone,
	UnsubscribeNamespace,
} from "./subscribe_namespace.ts";
import { Version } from "./version.ts";

/**
 * Handles subscribing to broadcasts using moq-transport protocol.
 * Uses the stream-per-request pattern (real bidi streams for v17, virtual for v14-v16).
 *
 * @internal
 */
export class Subscriber {
	#session: Session;

	// Our subscribed tracks — keyed by trackAlias for group routing
	#subscribes = new Map<bigint, Track>();

	// Any currently active announcements.
	#announced = new Set<Path.Valid>();

	// Any consumers that want each new announcement.
	#announcedConsumers = new Set<Announced>();

	/**
	 * Creates a new Subscriber instance.
	 * @param session - The session abstraction for bidi streams and request IDs
	 *
	 * @internal
	 */
	constructor(session: Session) {
		this.#session = session;
	}

	/**
	 * Gets an announced reader for the specified prefix.
	 */
	announced(prefix = Path.empty()): Announced {
		const announced = new Announced(prefix);
		for (const active of this.#announced) {
			if (!active.startsWith(prefix)) continue;
			announced.append({ path: active, active: true });
		}
		this.#announcedConsumers.add(announced);

		void this.#runAnnounced(announced, prefix).finally(() => {
			this.#announcedConsumers.delete(announced);
		});

		return announced;
	}

	async #runAnnounced(announced: Announced, prefix: Path.Valid) {
		const version = this.#session.version;

		// v14/v15: SubscribeNamespace on control stream (via adapter virtual stream)
		// v16+: SubscribeNamespace on its own bidi stream
		// Both paths work the same way with the adapter/native session.

		const requestId = await this.#session.nextRequestId();
		if (requestId === undefined) return;

		try {
			const stream = await this.#session.openBi(requestId);

			try {
				// Write SubscribeNamespace
				await stream.writer.u53(SubscribeNamespace.id);
				const msg = new SubscribeNamespace({ namespace: prefix, requestId });
				await msg.encode(stream.writer, version);

				// Read response
				const respTypeId = await stream.reader.u53();
				if (respTypeId === RequestOk.id) {
					await RequestOk.decode(stream.reader, version);
				} else if (respTypeId === 0x12) {
					// v14: SubscribeNamespaceOk
					const size = await stream.reader.u16();
					await stream.reader.read(size);
				} else {
					throw new Error(`SubscribeNamespace rejected: typeId=0x${respTypeId.toString(16)}`);
				}

				// Loop reading Namespace/NamespaceDone entries
				const readLoop = (async () => {
					for (;;) {
						const done = await stream.reader.done();
						if (done) break;

						const msgType = await stream.reader.u53();
						if (msgType === SubscribeNamespaceEntry.id) {
							const entry = await SubscribeNamespaceEntry.decode(stream.reader, version);
							const path = Path.join(prefix, entry.suffix);
							console.debug(`announced: broadcast=${path} active=true`);

							this.#announced.add(path);
							for (const consumer of this.#announcedConsumers) {
								if (!path.startsWith(consumer.prefix)) continue;
								consumer.append({ path, active: true });
							}
						} else if (msgType === SubscribeNamespaceEntryDone.id) {
							const entry = await SubscribeNamespaceEntryDone.decode(stream.reader, version);
							const path = Path.join(prefix, entry.suffix);
							console.debug(`announced: broadcast=${path} active=false`);

							this.#announced.delete(path);
							for (const consumer of this.#announcedConsumers) {
								if (!path.startsWith(consumer.prefix)) continue;
								consumer.append({ path, active: false });
							}
						} else {
							throw new Error(
								`unexpected message on subscribe_namespace stream: 0x${msgType.toString(16)}`,
							);
						}
					}
				})();

				// Wait for either the read loop or the announced to close
				await Promise.race([readLoop, announced.closed]);

				// For v14/v15: send UnsubscribeNamespace before closing
				if (version === Version.DRAFT_14 || version === Version.DRAFT_15) {
					try {
						await stream.writer.u53(UnsubscribeNamespace.id);
						const unsub = new UnsubscribeNamespace({ requestId });
						await unsub.encode(stream.writer, version);
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
			console.warn(`subscribe_namespace error: ${e.message}`);
		}
	}

	/**
	 * Consumes a broadcast from the connection.
	 */
	consume(path: Path.Valid): Broadcast {
		const broadcast = new Broadcast();

		(async () => {
			for (;;) {
				const request = await broadcast.requested();
				if (!request) break;
				this.#runSubscribe(path, request);
			}
		})();

		return broadcast;
	}

	async #runSubscribe(broadcast: Path.Valid, request: TrackRequest) {
		const version = this.#session.version;
		const requestId = await this.#session.nextRequestId();
		if (requestId === undefined) return;

		console.debug(`subscribe start: id=${requestId} broadcast=${broadcast} track=${request.track.name}`);

		try {
			const stream = await this.#session.openBi(requestId);

			try {
				// Write Subscribe
				await stream.writer.u53(Subscribe.id);
				const msg = new Subscribe({
					requestId,
					trackNamespace: broadcast,
					trackName: request.track.name,
					subscriberPriority: request.priority,
				});
				await msg.encode(stream.writer, version);

				// Read response (SubscribeOk or error)
				const respTypeId = await stream.reader.u53();
				if (respTypeId === SubscribeOk.id) {
					const ok = await SubscribeOk.decode(stream.reader, version);
					// Register trackAlias for group routing
					this.#subscribes.set(ok.trackAlias, request.track);
					console.debug(`subscribe ok: id=${requestId} broadcast=${broadcast} track=${request.track.name}`);

					try {
						// Wait for stream close (= PublishDone) or track close (= local unsubscribe)
						await Promise.race([stream.reader.closed, request.track.closed]);

						// For v14-v16: send Unsubscribe before closing
						if (version !== Version.DRAFT_17) {
							try {
								await stream.writer.u53(Unsubscribe.id);
								const unsub = new Unsubscribe({ requestId });
								await unsub.encode(stream.writer, version);
							} catch {
								// Stream might already be closed
							}
						}

						request.track.close();
						stream.close();
						console.debug(
							`subscribe close: id=${requestId} broadcast=${broadcast} track=${request.track.name}`,
						);
					} finally {
						this.#subscribes.delete(ok.trackAlias);
					}
				} else {
					// Error response
					let reasonPhrase = "unknown error";
					try {
						if (respTypeId === 0x05) {
							// SubscribeError (v14) or RequestError (v15+)
							const err =
								version === Version.DRAFT_14
									? await (await import("./subscribe.ts")).SubscribeError.decode(
											stream.reader,
											version,
										)
									: await RequestError.decode(stream.reader, version);
							reasonPhrase = `code=${err.errorCode} reason=${err.reasonPhrase}`;
						}
					} catch {
						// Decoding error response failed, use default message
					}
					throw new Error(`SUBSCRIBE error: ${reasonPhrase}`);
				}
			} catch (err) {
				stream.abort(error(err));
				throw err;
			}
		} catch (err) {
			const e = error(err);
			request.track.close(e);
			console.warn(
				`subscribe error: id=${requestId} broadcast=${broadcast} track=${request.track.name} error=${e.message}`,
			);
		}
	}

	/**
	 * Handles an incoming PUBLISH_NAMESPACE on a bidi stream.
	 * Tracks announced broadcasts and notifies consumers.
	 *
	 * @internal
	 */
	async runPublishNamespace(msg: PublishNamespace, stream: Stream) {
		const version = this.#session.version;
		const path = msg.trackNamespace;

		if (this.#announced.has(path)) {
			console.warn("duplicate PublishNamespace");
			if (version === Version.DRAFT_14) {
				const { PublishNamespaceError } = await import("./publish_namespace.ts");
				await stream.writer.u53(PublishNamespaceError.id);
				const err = new PublishNamespaceError({
					requestId: msg.requestId,
					errorCode: 409,
					reasonPhrase: "duplicate namespace",
				});
				await err.encode(stream.writer, version);
			} else {
				await stream.writer.u53(RequestError.id);
				const err = new RequestError({
					requestId: version === Version.DRAFT_17 ? undefined : msg.requestId,
					errorCode: 409,
					reasonPhrase: "duplicate namespace",
				});
				await err.encode(stream.writer, version);
			}
			stream.close();
			return;
		}

		this.#announced.add(path);
		console.debug(`announced: broadcast=${path} active=true`);

		for (const consumer of this.#announcedConsumers) {
			const suffix = Path.stripPrefix(consumer.prefix, path);
			if (suffix === null) continue;
			consumer.append({ path, active: true });
		}

		try {
			// Send OK
			if (version === Version.DRAFT_14) {
				const { PublishNamespaceOk } = await import("./publish_namespace.ts");
				await stream.writer.u53(PublishNamespaceOk.id);
				const ok = new PublishNamespaceOk({ requestId: msg.requestId });
				await ok.encode(stream.writer, version);
			} else {
				await stream.writer.u53(RequestOk.id);
				const ok = new RequestOk({ requestId: version === Version.DRAFT_17 ? undefined : msg.requestId });
				await ok.encode(stream.writer, version);
			}

			// Wait for stream close (= PublishNamespaceDone)
			await stream.reader.closed;
		} finally {
			this.#announced.delete(path);
			console.debug(`announced: broadcast=${path} active=false`);

			for (const consumer of this.#announcedConsumers) {
				const suffix = Path.stripPrefix(consumer.prefix, path);
				if (suffix === null) continue;
				consumer.append({ path, active: false });
			}
		}
	}

	/**
	 * Handles an incoming PUBLISH on a bidi stream.
	 * We don't support reverse publish, so send error.
	 *
	 * @internal
	 */
	async runPublish(msg: Publish, stream: Stream) {
		const version = this.#session.version;

		if (version === Version.DRAFT_14) {
			await stream.writer.u53(PublishError.id);
			const err = new PublishError({
				requestId: msg.requestId,
				errorCode: 500,
				reasonPhrase: "publish not supported",
			});
			await err.encode(stream.writer, version);
		} else {
			await stream.writer.u53(RequestError.id);
			const err = new RequestError({
				requestId: version === Version.DRAFT_17 ? undefined : msg.requestId,
				errorCode: 500,
				reasonPhrase: "publish not supported",
			});
			await err.encode(stream.writer, version);
		}
		stream.close();
	}

	/**
	 * Handles an ObjectStream message (group + frames on uni stream).
	 *
	 * @internal
	 */
	async handleGroup(group: GroupMessage, stream: Reader) {
		const producer = new Group(group.groupId);

		if (group.subGroupId !== 0) {
			throw new Error("subgroups are not supported");
		}

		try {
			// Look up by trackAlias directly
			const track = this.#subscribes.get(group.trackAlias);
			if (!track) {
				// Fallback: try treating trackAlias as requestId (for compat)
				throw new Error(`unknown track: trackAlias=${group.trackAlias}`);
			}

			track.writeGroup(producer);

			for (;;) {
				const done = await Promise.race([stream.done(), producer.closed, track.closed]);
				if (done !== false) break;

				const frame = await Frame.decode(stream, group.flags);
				if (frame.payload === undefined) break;

				producer.writeFrame(frame.payload);
			}

			producer.close();
		} catch (err: unknown) {
			const e = error(err);
			producer.close(e);
			stream.stop(e);
		}
	}
}

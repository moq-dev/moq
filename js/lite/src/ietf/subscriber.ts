import { Announced } from "../announced.ts";
import { Broadcast, type TrackRequest } from "../broadcast.ts";
import { Group } from "../group.ts";
import * as Path from "../path.ts";
import type { Reader } from "../stream.ts";
import type { Track } from "../track.ts";
import { error } from "../util/error.ts";
import type * as Control from "./control.ts";
import { Frame, type Group as GroupMessage } from "./object.ts";
import { type Publish, type PublishDone, PublishError } from "./publish.ts";
import type { PublishNamespace, PublishNamespaceDone } from "./publish_namespace.ts";
import { RequestError, type RequestOk } from "./request.ts";
import { Subscribe, type SubscribeError, type SubscribeOk, Unsubscribe } from "./subscribe.ts";
import {
	SubscribeNamespace,
	type SubscribeNamespaceError,
	type SubscribeNamespaceOk,
	UnsubscribeNamespace,
} from "./subscribe_namespace.ts";
import type { TrackStatus } from "./track.ts";
import { Version } from "./version.ts";

/**
 * Handles subscribing to broadcasts using moq-transport protocol with lite-compatibility restrictions.
 *
 * @internal
 */
export class Subscriber {
	#control: Control.Stream;

	// Any currently active announcements.
	#announced = new Set<Path.Valid>();

	// Any consumers that want each new announcement.
	#announcedConsumers = new Set<Announced>();

	// Our subscribed tracks - keyed by request ID
	#subscribes = new Map<bigint, Track>();

	// A map of track aliases to request IDs
	#trackAliases = new Map<bigint, bigint>();

	// Track subscription responses - keyed by request ID
	#subscribeCallbacks = new Map<
		bigint,
		{
			resolve: (msg: SubscribeOk) => void;
			reject: (msg: Error) => void;
		}
	>();

	/**
	 * Creates a new Subscriber instance.
	 * @param quic - The WebTransport session to use
	 * @param control - The control stream writer for sending control messages
	 *
	 * @internal
	 */
	constructor(control: Control.Stream) {
		this.#control = control;
	}

	/**
	 * Gets an announced reader for the specified prefix.
	 * @param prefix - The prefix for announcements
	 * @returns An AnnounceConsumer instance
	 */
	announced(prefix = Path.empty()): Announced {
		const announced = new Announced(prefix);
		for (const active of this.#announced) {
			if (!active.startsWith(prefix)) continue;

			announced.append({
				path: active,
				active: true,
			});
		}

		this.#announcedConsumers.add(announced);
		this.#runAnnounced(announced, prefix).finally(() => {
			this.#announcedConsumers.delete(announced);
		});

		return announced;
	}

	async #runAnnounced(announced: Announced, prefix: Path.Valid) {
		const requestId = await this.#control.nextRequestId();
		if (requestId === undefined) return;

		try {
			this.#control.write(new SubscribeNamespace(prefix, requestId));
			await announced.closed;
		} finally {
			this.#control.write(new UnsubscribeNamespace(requestId));
		}
	}

	/**
	 * Consumes a broadcast from the connection.
	 *
	 * @param name - The name of the broadcast to consume
	 * @returns A Broadcast instance
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
		const requestId = await this.#control.nextRequestId();
		if (requestId === undefined) return;

		this.#subscribes.set(requestId, request.track);

		console.debug(`subscribe start: id=${requestId} broadcast=${broadcast} track=${request.track.name}`);

		const msg = new Subscribe(requestId, broadcast, request.track.name, request.priority);

		// Send SUBSCRIBE message on control stream and wait for response
		const responsePromise = new Promise<SubscribeOk>((resolve, reject) => {
			this.#subscribeCallbacks.set(requestId, { resolve, reject });
		});

		await this.#control.write(msg);

		try {
			const ok = await responsePromise;
			this.#trackAliases.set(ok.trackAlias, requestId);
			console.debug(`subscribe ok: id=${requestId} broadcast=${broadcast} track=${request.track.name}`);

			try {
				await request.track.closed;

				const msg = new Unsubscribe(requestId);
				await this.#control.write(msg);
				console.debug(`unsubscribe: id=${requestId} broadcast=${broadcast} track=${request.track.name}`);
			} finally {
				this.#trackAliases.delete(ok.trackAlias);
			}
		} catch (err) {
			const e = error(err);
			request.track.close(e);

			console.warn(
				`subscribe error: id=${requestId} broadcast=${broadcast} track=${request.track.name} error=${e.message}`,
			);
		} finally {
			this.#subscribes.delete(requestId);
			this.#subscribeCallbacks.delete(requestId);
		}
	}

	/**
	 * Handles a SUBSCRIBE_OK control message received on the control stream.
	 * @param msg - The SUBSCRIBE_OK message
	 *
	 * @internal
	 */
	async handleSubscribeOk(msg: SubscribeOk) {
		const callback = this.#subscribeCallbacks.get(msg.requestId);
		if (callback) {
			callback.resolve(msg);
		} else {
			console.warn("handleSubscribeOk unknown requestId", msg.requestId);
		}
	}

	/**
	 * Handles a SUBSCRIBE_ERROR control message received on the control stream.
	 * @param msg - The SUBSCRIBE_ERROR message
	 *
	 * @internal
	 */
	async handleSubscribeError(msg: SubscribeError) {
		const callback = this.#subscribeCallbacks.get(msg.requestId);
		if (callback) {
			callback.reject(new Error(`SUBSCRIBE_ERROR: code=${msg.errorCode} reason=${msg.reasonPhrase}`));
		} else {
			console.warn("handleSubscribeError unknown requestId", msg.requestId);
		}
	}

	/**
	 * Handles an ObjectStream message (moq-transport equivalent of moq-lite Group).
	 * @param msg - The ObjectStream message
	 * @param stream - The stream to read object data from
	 *
	 * @internal
	 */
	async handleGroup(group: GroupMessage, stream: Reader) {
		const producer = new Group(group.groupId);

		if (group.subGroupId !== 0) {
			console.warn("subgroup ID is not supported, ignoring");
		}

		try {
			let requestId = this.#trackAliases.get(group.trackAlias);
			if (requestId === undefined) {
				// Just hope the track alias is the request ID
				requestId = group.trackAlias;
				console.warn("unknown track alias, using request ID");
			}

			const track = this.#subscribes.get(requestId);
			if (!track) {
				throw new Error(
					`unknown track: trackAlias=${group.trackAlias} requestId=${this.#trackAliases.get(group.trackAlias)}`,
				);
			}

			// Convert to Group (moq-lite equivalent)
			track.writeGroup(producer);

			// Read objects from the stream until end of group
			for (;;) {
				const done = await Promise.race([stream.done(), producer.closed, track.closed]);
				if (done !== false) break;

				const frame = await Frame.decode(stream, group.flags);
				if (frame.payload === undefined) break;

				// Treat each object payload as a frame
				producer.writeFrame(frame.payload);
			}

			producer.close();
		} catch (err: unknown) {
			const e = error(err);
			producer.close(e);
			stream.stop(e);
		}
	}

	// we don't support publish, so send PUBLISH_ERROR
	async handlePublish(msg: Publish) {
		// TODO technically, we should send PUBLISH_OK if we had a SUBSCRIBE in flight for the same track.
		// Otherwise, the peer will SUBSCRIBE_ERROR because duplicate subscriptions are not allowed :(
		if (this.#control.version === Version.DRAFT_15) {
			const err = new RequestError(msg.requestId, 500, "publish not supported");
			await this.#control.write(err);
		} else if (this.#control.version === Version.DRAFT_14) {
			const err = new PublishError(msg.requestId, 500, "publish not supported");
			await this.#control.write(err);
		} else {
			const version: never = this.#control.version;
			throw new Error(`unsupported version: ${version}`);
		}
	}

	/**
	 * Handles a PUBLISH_DONE control message received on the control stream.
	 * @param msg - The PUBLISH_DONE message
	 */
	async handlePublishDone(msg: PublishDone) {
		// For lite compatibility, we treat this as subscription completion
		const callback = this.#subscribeCallbacks.get(msg.requestId);
		if (callback) {
			callback.reject(new Error(`PUBLISH_DONE: code=${msg.statusCode} reason=${msg.reasonPhrase}`));
		}
	}

	/**
	 * Handles a PUBLISH_NAMESPACE control message received on the control stream.
	 * @param msg - The PUBLISH_NAMESPACE message
	 */
	async handlePublishNamespace(msg: PublishNamespace) {
		if (this.#announced.has(msg.trackNamespace)) {
			console.warn("duplicate PUBLISH_NAMESPACE message");
			return;
		}

		this.#announced.add(msg.trackNamespace);
		console.debug(`announced: broadcast=${msg.trackNamespace} active=true`);

		for (const consumer of this.#announcedConsumers) {
			consumer.append({
				path: msg.trackNamespace,
				active: true,
			});
		}
	}

	/**
	 * Handles a PUBLISH_NAMESPACE_DONE control message received on the control stream.
	 * @param msg - The PUBLISH_NAMESPACE_DONE message
	 */
	async handlePublishNamespaceDone(msg: PublishNamespaceDone) {
		if (!this.#announced.has(msg.trackNamespace)) {
			console.warn("unknown PUBLISH_NAMESPACE_DONE message");
			return;
		}

		this.#announced.delete(msg.trackNamespace);
		console.debug(`announced: broadcast=${msg.trackNamespace} active=false`);

		for (const consumer of this.#announcedConsumers) {
			consumer.append({
				path: msg.trackNamespace,
				active: false,
			});
		}
	}

	async handleSubscribeNamespaceOk(_msg: SubscribeNamespaceOk) {
		// Don't care
	}

	async handleSubscribeNamespaceError(_msg: SubscribeNamespaceError) {
		throw new Error("SUBSCRIBE_NAMESPACE_ERROR messages are not supported");
	}

	/**
	 * Handles a TRACK_STATUS control message received on the control stream.
	 * @param msg - The TRACK_STATUS message
	 */
	async handleTrackStatus(_msg: TrackStatus) {
		throw new Error("TRACK_STATUS messages are not supported");
	}

	// v15: REQUEST_OK replaces SubscribeNamespaceOk, PublishNamespaceOk
	async handleRequestOk(msg: RequestOk) {
		// In v15, RequestOk is used for subscribe namespace acknowledgements
		// Route by request_id — treat similarly to SubscribeNamespaceOk for now
		console.debug("received REQUEST_OK", msg.requestId);
	}

	// v15: REQUEST_ERROR replaces SubscribeNamespaceError, etc.
	async handleRequestError(msg: RequestError) {
		// In v15, RequestError replaces SubscribeError for subscribe requests
		const callback = this.#subscribeCallbacks.get(msg.requestId);
		if (callback) {
			callback.reject(new Error(`REQUEST_ERROR: code=${msg.errorCode} reason=${msg.reasonPhrase}`));
		} else {
			console.warn("handleRequestError unknown requestId", msg.requestId);
		}
	}
}

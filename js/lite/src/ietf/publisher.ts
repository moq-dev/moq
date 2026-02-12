import type { Broadcast } from "../broadcast.ts";
import type { Group } from "../group.ts";
import * as Path from "../path.ts";
import { type Stream, Writer } from "../stream.ts";
import type { Track } from "../track.ts";
import { error } from "../util/error.ts";
import type * as Control from "./control.ts";
import { Frame, Group as GroupMessage } from "./object.ts";
import { PublishDone } from "./publish.ts";
import {
	PublishNamespace,
	type PublishNamespaceCancel,
	PublishNamespaceDone,
	type PublishNamespaceError,
	type PublishNamespaceOk,
} from "./publish_namespace.ts";
import { RequestError, RequestOk } from "./request.ts";
import { type Subscribe, SubscribeError, SubscribeOk, type Unsubscribe } from "./subscribe.ts";
import {
	SubscribeNamespace,
	SubscribeNamespaceEntry,
	SubscribeNamespaceEntryDone,
	type UnsubscribeNamespace,
} from "./subscribe_namespace.ts";
import { TrackStatus, type TrackStatusRequest } from "./track.ts";
import { type IetfVersion, Version } from "./version.ts";

/**
 * Handles publishing broadcasts using moq-transport protocol with lite-compatibility restrictions.
 *
 * @internal
 */
export class Publisher {
	#quic: WebTransport;
	#control: Control.Stream;

	// Our published broadcasts.
	#broadcasts: Map<Path.Valid, Broadcast> = new Map();

	/**
	 * Creates a new Publisher instance.
	 * @param quic - The WebTransport session to use
	 * @param control - The control stream writer for sending control messages
	 *
	 * @internal
	 */
	constructor({ quic, control }: { quic: WebTransport; control: Control.Stream }) {
		this.#quic = quic;
		this.#control = control;
	}

	/**
	 * Publishes a broadcast with any associated tracks.
	 * @param name - The broadcast to publish
	 */
	publish(path: Path.Valid, broadcast: Broadcast) {
		this.#broadcasts.set(path, broadcast);
		void this.#runPublish(path, broadcast);
	}

	async #runPublish(path: Path.Valid, broadcast: Broadcast) {
		try {
			const requestId = await this.#control.nextRequestId();
			if (requestId === undefined) return;

			const announce = new PublishNamespace({ requestId, trackNamespace: path });
			await this.#control.write(announce);

			// Wait until the broadcast is closed, then remove it from the lookup.
			await broadcast.closed;

			const unannounce = new PublishNamespaceDone({ trackNamespace: path });
			await this.#control.write(unannounce);
		} catch (err: unknown) {
			const e = error(err);
			console.warn(`announce failed: broadcast=${path} error=${e.message}`);
		} finally {
			broadcast.close();
			this.#broadcasts.delete(path);
		}
	}

	/**
	 * Handles a SUBSCRIBE control message received on the control stream.
	 * @param msg - The subscribe message
	 *
	 * @internal
	 */
	async handleSubscribe(msg: Subscribe) {
		// Convert track namespace/name to broadcast path (moq-lite compatibility)
		const name = msg.trackNamespace;
		const broadcast = this.#broadcasts.get(name);

		if (!broadcast) {
			if (this.#control.version === Version.DRAFT_15 || this.#control.version === Version.DRAFT_16) {
				const errorMsg = new RequestError({
					requestId: msg.requestId,
					errorCode: 404,
					reasonPhrase: "Broadcast not found",
				});
				await this.#control.write(errorMsg);
			} else if (this.#control.version === Version.DRAFT_14) {
				const errorMsg = new SubscribeError({
					requestId: msg.requestId,
					errorCode: 404,
					reasonPhrase: "Broadcast not found",
				});
				await this.#control.write(errorMsg);
			} else {
				const version: never = this.#control.version;
				throw new Error(`unsupported version: ${version}`);
			}

			return;
		}

		const track = broadcast.subscribe(msg.trackName, msg.subscriberPriority);

		// Send SUBSCRIBE_OK response on control stream
		const okMsg = new SubscribeOk({ requestId: msg.requestId, trackAlias: msg.requestId });
		await this.#control.write(okMsg);
		console.debug(`publish ok: broadcast=${name} track=${track.name}`);

		// Start sending track data using ObjectStream (Subgroup delivery mode only)
		void this.#runTrack(msg.requestId, name, track);
	}

	/**
	 * Runs a track and sends its data using ObjectStream messages.
	 * @param requestId - The subscription request ID (also used as track alias)
	 * @param broadcast - The broadcast path
	 * @param track - The track to run
	 *
	 * @internal
	 */
	async #runTrack(requestId: bigint, broadcast: Path.Valid, track: Track) {
		try {
			for (;;) {
				const group = await track.nextGroup();
				if (!group) break;
				void this.#runGroup(requestId, group);
			}

			console.debug(`publish done: broadcast=${broadcast} track=${track.name}`);
			const msg = new PublishDone({ requestId, statusCode: 200, reasonPhrase: "OK" });
			await this.#control.write(msg);
		} catch (err: unknown) {
			const e = error(err);
			console.warn(`publish error: broadcast=${broadcast} track=${track.name} error=${e.message}`);
			const msg = new PublishDone({ requestId, statusCode: 500, reasonPhrase: e.message });
			await this.#control.write(msg);
		} finally {
			track.close();
		}
	}

	/**
	 * Runs a group and sends its frames using ObjectStream (Subgroup delivery mode).
	 * @param requestId - The subscription request ID (also used as track alias)
	 * @param group - The group to run
	 *
	 * @internal
	 */
	async #runGroup(requestId: bigint, group: Group) {
		try {
			// Create a new unidirectional stream for this group
			const stream = await Writer.open(this.#quic);

			// Write STREAM_HEADER_SUBGROUP
			const header = new GroupMessage({
				trackAlias: requestId,
				groupId: group.sequence,
				subGroupId: 0,
				publisherPriority: 0,
				flags: {
					hasExtensions: false,
					hasSubgroup: false,
					hasSubgroupObject: false,
					// Automatically end the group on stream FIN
					hasEnd: true,
					hasPriority: true,
				},
			});

			console.debug("sending group header", header);
			await header.encode(stream);

			try {
				for (;;) {
					const frame = await Promise.race([group.readFrame(), stream.closed]);
					if (!frame) break;

					// Write each frame as an object
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
	 * Handles a TRACK_STATUS_REQUEST control message received on the control stream.
	 * @param msg - The track status request message
	 */
	async handleTrackStatusRequest(msg: TrackStatusRequest) {
		// moq-lite doesn't support track status requests
		const statusMsg = new TrackStatus({
			trackNamespace: msg.trackNamespace,
			trackName: msg.trackName,
			statusCode: TrackStatus.STATUS_NOT_FOUND,
			lastGroupId: 0n,
			lastObjectId: 0n,
		});
		await this.#control.write(statusMsg);
	}

	/**
	 * Handles an UNSUBSCRIBE control message received on the control stream.
	 * @param msg - The unsubscribe message
	 */
	async handleUnsubscribe(_msg: Unsubscribe) {
		// TODO
	}

	/**
	 * Handles a PUBLISH_NAMESPACE_OK control message received on the control stream.
	 * @param msg - The publish namespace ok message
	 */
	async handlePublishNamespaceOk(_msg: PublishNamespaceOk) {
		// TODO
	}

	/**
	 * Handles a PUBLISH_NAMESPACE_ERROR control message received on the control stream.
	 * @param msg - The publish namespace error message
	 */
	async handlePublishNamespaceError(_msg: PublishNamespaceError) {
		// TODO
	}

	/**
	 * Handles a PUBLISH_NAMESPACE_CANCEL control message received on the control stream.
	 * @param msg - The PUBLISH_NAMESPACE_CANCEL message
	 */
	async handlePublishNamespaceCancel(_msg: PublishNamespaceCancel) {
		// TODO
	}

	async handleSubscribeNamespace(_msg: SubscribeNamespace) {}

	async handleUnsubscribeNamespace(_msg: UnsubscribeNamespace) {}

	// v15: REQUEST_OK replaces PublishNamespaceOk, SubscribeNamespaceOk
	async handleRequestOk(_msg: RequestOk) {
		// TODO: route by request_id to determine what kind of request it belongs to
	}

	// v15: REQUEST_ERROR replaces SubscribeError, PublishError, etc.
	async handleRequestError(_msg: RequestError) {
		// TODO: route by request_id to determine what kind of request it belongs to
	}

	/**
	 * Handle a v16 SUBSCRIBE_NAMESPACE on a bidirectional stream.
	 * Reads the request, sends REQUEST_OK, then streams NAMESPACE/NAMESPACE_DONE.
	 */
	async handleSubscribeNamespaceStream(stream: Stream) {
		const version = this.#control.version;

		try {
			// Read the SubscribeNamespace message (type ID already consumed by connection)
			const msg = await SubscribeNamespace.decode(stream.reader, version);
			const prefix = msg.namespace;

			console.debug(`subscribe_namespace stream: prefix=${prefix}`);

			// Send REQUEST_OK
			await stream.writer.u53(RequestOk.id);
			const ok = new RequestOk({ requestId: msg.requestId });
			await ok.encode(stream.writer, version);

			// Send NAMESPACE for each currently published broadcast matching prefix
			const active = new Set<Path.Valid>();
			for (const name of this.#broadcasts.keys()) {
				const suffix = Path.stripPrefix(prefix, name);
				if (suffix === null) continue;
				console.debug(`namespace: broadcast=${name} suffix=${suffix}`);
				active.add(suffix);

				await stream.writer.u53(SubscribeNamespaceEntry.id);
				const entry = new SubscribeNamespaceEntry({ suffix });
				await entry.encode(stream.writer, version);
			}

			// Wait for broadcast changes and stream updates
			// We poll by watching for stream close or broadcasts changing.
			// This is a simplified version — we check periodically rather than using signals.
			await Promise.race([stream.reader.closed, this.#watchBroadcasts(stream, prefix, active, version)]);

			stream.close();
		} catch (err: unknown) {
			const e = error(err);
			console.debug(`subscribe_namespace stream error: ${e.message}`);
			stream.abort(e);
		}
	}

	async #watchBroadcasts(stream: Stream, prefix: Path.Valid, active: Set<Path.Valid>, version: IetfVersion) {
		// Simple polling approach - check for changes every 100ms
		// A proper implementation would use signals/events
		for (;;) {
			await new Promise((resolve) => setTimeout(resolve, 100));

			const newActive = new Set<Path.Valid>();
			for (const name of this.#broadcasts.keys()) {
				const suffix = Path.stripPrefix(prefix, name);
				if (suffix === null) continue;
				newActive.add(suffix);
			}

			// Send NAMESPACE for new broadcasts
			for (const added of newActive) {
				if (active.has(added)) continue;
				console.debug(`namespace: suffix=${added} active=true`);
				await stream.writer.u53(SubscribeNamespaceEntry.id);
				const entry = new SubscribeNamespaceEntry({ suffix: added });
				await entry.encode(stream.writer, version);
			}

			// Send NAMESPACE_DONE for removed broadcasts
			for (const removed of active) {
				if (newActive.has(removed)) continue;
				console.debug(`namespace: suffix=${removed} active=false`);
				await stream.writer.u53(SubscribeNamespaceEntryDone.id);
				const entry = new SubscribeNamespaceEntryDone({ suffix: removed });
				await entry.encode(stream.writer, version);
			}

			active.clear();
			for (const s of newActive) active.add(s);
		}
	}
}

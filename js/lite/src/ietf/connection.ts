import type { Announced } from "../announced.ts";
import type { Broadcast } from "../broadcast.ts";
import type { Established } from "../connection/established.ts";
import * as Path from "../path.ts";
import { type Reader, Readers, type Stream } from "../stream.ts";
import { ControlStreamAdapter, NativeSession, type Session } from "./adapter.ts";
import { GoAway } from "./goaway.ts";
import { Group } from "./object.ts";
import { Publish } from "./publish.ts";
import { PublishNamespace } from "./publish_namespace.ts";
import { Publisher } from "./publisher.ts";
import { Subscribe, SubscribeUpdate } from "./subscribe.ts";
import { SubscribeNamespace } from "./subscribe_namespace.ts";
import { Subscriber } from "./subscriber.ts";
import { TrackStatusRequest } from "./track.ts";
import { type IetfVersion, Version, versionName } from "./version.ts";

/**
 * Represents a connection to a MoQ server using moq-transport protocol.
 *
 * @public
 */
export class Connection implements Established {
	// The URL of the connection.
	readonly url: URL;

	// The negotiated protocol version.
	readonly version: string;

	// The established WebTransport session.
	#quic: WebTransport;

	// Session abstraction: adapter for v14-v16, native for v17.
	#session: Session;

	// Module for contributing tracks.
	#publisher: Publisher;

	// Module for distributing tracks.
	#subscriber: Subscriber;

	// Just to avoid logging when `close()` is called.
	#closed = false;

	/**
	 * Creates a new Connection instance.
	 * @param url - The URL of the connection
	 * @param quic - The WebTransport session
	 * @param control - The control/setup stream
	 * @param maxRequestId - The initial max request ID
	 * @param version - The negotiated protocol version
	 *
	 * @internal
	 */
	constructor({
		url,
		quic,
		control,
		maxRequestId,
		version,
	}: {
		url: URL;
		quic: WebTransport;
		control: Stream;
		maxRequestId: bigint;
		version: IetfVersion;
	}) {
		this.url = url;
		this.version = versionName(version);
		this.#quic = quic;

		// Two-path dispatch: v14-v16 uses adapter, v17 uses native bidi streams
		if (version === Version.DRAFT_17) {
			this.#session = new NativeSession(quic, version);
			// v17: control/setup stream only carries GoAway
			void this.#runGoAway(control);
		} else {
			const adapter = new ControlStreamAdapter(quic, control, version, maxRequestId);
			this.#session = adapter;
			// Start the adapter read loop (routes control messages to virtual streams)
			void adapter.run().catch((err: unknown) => {
				if (!this.#closed) console.error("adapter error", err);
				this.close();
			});
		}

		this.#publisher = new Publisher(this.#quic, this.#session);
		this.#subscriber = new Subscriber(this.#session);

		void this.#run();
	}

	/**
	 * Closes the connection.
	 */
	close() {
		if (this.#closed) return;

		this.#closed = true;

		this.#session.close?.();

		try {
			this.#quic.close();
		} catch {
			// ignore
		}
	}

	async #run(): Promise<void> {
		try {
			await Promise.all([this.#runBidis(), this.#runUnis()]);
		} catch (err) {
			if (!this.#closed) {
				console.error("fatal error running connection", err);
			}
		} finally {
			this.close();
		}
	}

	/**
	 * Publishes a broadcast to the connection.
	 * @param name - The broadcast path to publish
	 * @param broadcast - The broadcast to publish
	 */
	publish(path: Path.Valid, broadcast: Broadcast) {
		this.#publisher.publish(path, broadcast);
	}

	/**
	 * Gets an announced reader for the specified prefix.
	 * @param prefix - The prefix for announcements
	 * @returns An Announced instance
	 */
	announced(prefix = Path.empty()): Announced {
		return this.#subscriber.announced(prefix);
	}

	/**
	 * Consumes a broadcast from the connection.
	 *
	 * @remarks
	 * If the broadcast is not found, a "not found" error will be thrown when requesting any tracks.
	 *
	 * @param broadcast - The path of the broadcast to consume
	 * @returns A Broadcast instance
	 */
	consume(broadcast: Path.Valid): Broadcast {
		return this.#subscriber.consume(broadcast);
	}

	/**
	 * Accepts bidi streams (virtual for v14-v16, real for v17) and dispatches.
	 */
	async #runBidis() {
		for (;;) {
			const stream = await this.#session.acceptBi();
			if (!stream) break;

			void this.#runBidi(stream).catch((err: unknown) => {
				console.error("error processing bidi stream", err);
				stream.abort(new Error("bidi stream error"));
			});
		}
	}

	/**
	 * Unified bidi stream dispatch — reads typeId and routes to handler.
	 * Matches the lite module's runBidi pattern.
	 */
	async #runBidi(stream: Stream) {
		const typeId = await stream.reader.u53();

		switch (typeId) {
			case SubscribeUpdate.id: {
				// REQUEST_UPDATE (0x02) is a follow-up, not a valid initial message
				stream.abort(new Error("unexpected REQUEST_UPDATE as initial message"));
				break;
			}
			// Publisher handles incoming requests
			case Subscribe.id: {
				const msg = await Subscribe.decode(stream.reader, this.#session.version);
				await this.#publisher.runSubscribe(msg, stream);
				break;
			}
			case SubscribeNamespace.id: {
				const msg = await SubscribeNamespace.decode(stream.reader, this.#session.version);
				await this.#publisher.runSubscribeNamespace(msg, stream);
				break;
			}
			case TrackStatusRequest.id: {
				const msg = await TrackStatusRequest.decode(stream.reader, this.#session.version);
				await this.#publisher.runTrackStatusRequest(msg, stream);
				break;
			}

			// Subscriber handles incoming notifications
			case PublishNamespace.id: {
				const msg = await PublishNamespace.decode(stream.reader, this.#session.version);
				await this.#subscriber.runPublishNamespace(msg, stream);
				break;
			}
			case Publish.id: {
				const msg = await Publish.decode(stream.reader, this.#session.version);
				await this.#subscriber.runPublish(msg, stream);
				break;
			}

			default:
				console.warn(`unexpected bidi stream type: 0x${typeId.toString(16)}`);
				stream.abort(new Error("unexpected stream type"));
		}
	}

	/**
	 * Handles unidirectional streams for media delivery (groups).
	 */
	async #runUnis() {
		const readers = new Readers(this.#quic, this.#session.version);

		for (;;) {
			const stream = await readers.next();
			if (!stream) break;

			this.#runUni(stream)
				.then(() => {
					stream.stop(new Error("cancel"));
				})
				.catch((err: unknown) => {
					console.error("error processing object stream", err);
					stream.stop(err);
				});
		}
	}

	async #runUni(stream: Reader) {
		const header = await Group.decode(stream);
		await this.#subscriber.handleGroup(header, stream);
	}

	/**
	 * v17 only: reads GoAway from the setup/control stream.
	 */
	async #runGoAway(controlStream: Stream) {
		try {
			const done = await controlStream.reader.done();
			if (done) return;

			const typeId = await controlStream.reader.u53();
			if (typeId === GoAway.id) {
				const msg = await GoAway.decode(controlStream.reader, Version.DRAFT_17);
				console.warn(`received GOAWAY with redirect URI: ${msg.newSessionUri}`);
			} else {
				console.warn(`unexpected message on setup stream: 0x${typeId.toString(16)}`);
			}
		} catch (err) {
			if (!this.#closed) {
				console.error("error reading setup stream", err);
			}
		} finally {
			this.close();
		}
	}

	/**
	 * Returns a promise that resolves when the connection is closed.
	 * @returns A promise that resolves when closed
	 */
	get closed(): Promise<void> {
		return this.#quic.closed.then(() => undefined);
	}
}

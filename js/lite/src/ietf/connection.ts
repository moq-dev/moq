import type { Announced } from "../announced.ts";
import type { Broadcast } from "../broadcast.ts";
import type { Established } from "../connection/established.ts";
import * as Path from "../path.ts";
import { type Reader, Readers, Stream } from "../stream.ts";
import { unreachable } from "../util/index.ts";
import * as Control from "./control.ts";
import { Fetch, FetchCancel, FetchError, FetchOk } from "./fetch.ts";
import { GoAway } from "./goaway.ts";
import { Group } from "./object.ts";
import { Publish, PublishDone, PublishError, PublishOk } from "./publish.ts";
import {
	PublishNamespace,
	PublishNamespaceCancel,
	PublishNamespaceDone,
	PublishNamespaceError,
	PublishNamespaceOk,
} from "./publish_namespace.ts";
import { Publisher } from "./publisher.ts";
import { MaxRequestId, RequestError, RequestOk, RequestsBlocked } from "./request.ts";
import * as Setup from "./setup.ts";
import { Subscribe, SubscribeError, SubscribeOk, Unsubscribe } from "./subscribe.ts";
import {
	SubscribeNamespace,
	SubscribeNamespaceError,
	SubscribeNamespaceOk,
	UnsubscribeNamespace,
} from "./subscribe_namespace.ts";
import { Subscriber } from "./subscriber.ts";
import { TrackStatus, TrackStatusRequest } from "./track.ts";
import { type IetfVersion, Version } from "./version.ts";

/**
 * Represents a connection to a MoQ server using moq-transport protocol.
 *
 * @public
 */
export class Connection implements Established {
	// The URL of the connection.
	readonly url: URL;

	// The established WebTransport session.
	#quic: WebTransport;

	// The single bidirectional control stream for control messages
	#control: Control.Stream;

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
	 * @param controlStream - The control stream
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
		this.#quic = quic;
		this.#control = new Control.Stream({ stream: control, maxRequestId, version });

		this.#quic.closed.finally(() => {
			this.#control.close();
		});

		this.#publisher = new Publisher({ quic: this.#quic, control: this.#control });
		this.#subscriber = new Subscriber({ control: this.#control, quic: this.#quic });

		void this.#run();
	}

	/**
	 * Closes the connection.
	 */
	close() {
		if (this.#closed) return;

		this.#closed = true;

		try {
			this.#quic.close();
		} catch {
			// ignore
		}
	}

	async #run(): Promise<void> {
		const tasks: Promise<void>[] = [this.#runControlStream(), this.#runObjectStreams()];

		// v16: accept bidi streams for SUBSCRIBE_NAMESPACE
		if (this.#control.version === Version.DRAFT_16) {
			tasks.push(this.#runBidiStreams());
		}

		try {
			await Promise.all(tasks);
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
	 * @returns An AnnounceConsumer instance
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
	 * Handles control messages on the single bidirectional control stream.
	 */
	async #runControlStream() {
		for (;;) {
			try {
				const msg = await this.#control.read();

				// Route control messages to appropriate handlers based on type
				// Messages sent by Subscriber, received by Publisher:
				if (msg instanceof Subscribe) {
					await this.#publisher.handleSubscribe(msg);
				} else if (msg instanceof Unsubscribe) {
					await this.#publisher.handleUnsubscribe(msg);
				} else if (msg instanceof TrackStatusRequest) {
					await this.#publisher.handleTrackStatusRequest(msg);
				} else if (msg instanceof PublishNamespaceOk) {
					await this.#publisher.handlePublishNamespaceOk(msg);
				} else if (msg instanceof PublishNamespaceError) {
					await this.#publisher.handlePublishNamespaceError(msg);
				} else if (msg instanceof PublishNamespaceCancel) {
					await this.#publisher.handlePublishNamespaceCancel(msg);
				} else if (msg instanceof PublishNamespace) {
					await this.#subscriber.handlePublishNamespace(msg);
				} else if (msg instanceof PublishNamespaceDone) {
					await this.#subscriber.handlePublishNamespaceDone(msg);
				} else if (msg instanceof SubscribeOk) {
					await this.#subscriber.handleSubscribeOk(msg);
				} else if (msg instanceof SubscribeError) {
					await this.#subscriber.handleSubscribeError(msg);
				} else if (msg instanceof PublishDone) {
					await this.#subscriber.handlePublishDone(msg);
				} else if (msg instanceof TrackStatus) {
					await this.#subscriber.handleTrackStatus(msg);
				} else if (msg instanceof GoAway) {
					await this.#handleGoAway(msg);
				} else if (msg instanceof Setup.ClientSetup) {
					await this.#handleClientSetup(msg);
				} else if (msg instanceof Setup.ServerSetup) {
					await this.#handleServerSetup(msg);
				} else if (msg instanceof SubscribeNamespace) {
					await this.#publisher.handleSubscribeNamespace(msg);
				} else if (msg instanceof SubscribeNamespaceOk) {
					await this.#subscriber.handleSubscribeNamespaceOk(msg);
				} else if (msg instanceof SubscribeNamespaceError) {
					await this.#subscriber.handleSubscribeNamespaceError(msg);
				} else if (msg instanceof UnsubscribeNamespace) {
					await this.#publisher.handleUnsubscribeNamespace(msg);
				} else if (msg instanceof Publish) {
					await this.#subscriber.handlePublish(msg);
				} else if (msg instanceof PublishOk) {
					throw new Error("PUBLISH_OK messages are not supported");
				} else if (msg instanceof PublishError) {
					throw new Error("PUBLISH_ERROR messages are not supported");
				} else if (msg instanceof Fetch) {
					throw new Error("FETCH messages are not supported");
				} else if (msg instanceof FetchOk) {
					throw new Error("FETCH_OK messages are not supported");
				} else if (msg instanceof FetchError) {
					throw new Error("FETCH_ERROR messages are not supported");
				} else if (msg instanceof FetchCancel) {
					throw new Error("FETCH_CANCEL messages are not supported");
				} else if (msg instanceof MaxRequestId) {
					this.#control.maxRequestId(msg.requestId);
				} else if (msg instanceof RequestsBlocked) {
					console.warn("ignoring REQUESTS_BLOCKED message");
				} else if (msg instanceof RequestOk) {
					// v15: Route RequestOk to both publisher and subscriber
					await this.#publisher.handleRequestOk(msg);
					await this.#subscriber.handleRequestOk(msg);
				} else if (msg instanceof RequestError) {
					// v15: Route RequestError to both publisher and subscriber
					await this.#publisher.handleRequestError(msg);
					await this.#subscriber.handleRequestError(msg);
				} else {
					unreachable(msg);
				}
			} catch (err) {
				console.error("error processing control message", err);
				break;
			}
		}

		console.warn("control stream closed");
	}

	/**
	 * Handles a GoAway control message.
	 * @param msg - The GoAway message
	 */
	async #handleGoAway(msg: GoAway) {
		console.warn(`MOQLITE_INCOMPATIBLE: Received GOAWAY with redirect URI: ${msg.newSessionUri}`);
		// In moq-lite compatibility mode, we don't support session redirection
		// Just close the connection
		this.close();
	}

	/**
	 * Handles an unexpected CLIENT_SETUP control message.
	 * @param msg - The CLIENT_SETUP message
	 */
	async #handleClientSetup(_msg: Setup.ClientSetup) {
		console.error("Unexpected CLIENT_SETUP message received after connection established");
		this.close();
	}

	/**
	 * Handles an unexpected SERVER_SETUP control message.
	 * @param msg - The SERVER_SETUP message
	 */
	async #handleServerSetup(_msg: Setup.ServerSetup) {
		console.error("Unexpected SERVER_SETUP message received after connection established");
		this.close();
	}

	/**
	 * Handles object streams (unidirectional streams for media delivery).
	 */
	async #runObjectStreams() {
		const readers = new Readers(this.#quic);

		for (;;) {
			const stream = await readers.next();
			if (!stream) {
				break;
			}

			this.#runObjectStream(stream)
				.then(() => {
					stream.stop(new Error("cancel"));
				})
				.catch((err: unknown) => {
					stream.stop(err);
				});
		}
	}

	/**
	 * Handles a single object stream.
	 */
	async #runObjectStream(stream: Reader) {
		try {
			// we don't support other stream types yet
			const header = await Group.decode(stream);
			await this.#subscriber.handleGroup(header, stream);
		} catch (err) {
			console.error("error processing object stream", err);
		}
	}

	/**
	 * Accepts bidirectional streams for v16 SUBSCRIBE_NAMESPACE.
	 */
	async #runBidiStreams() {
		for (;;) {
			const stream = await Stream.accept(this.#quic);
			if (!stream) break;

			void this.#runBidiStream(stream).catch((err: unknown) => {
				console.error("error processing bidi stream", err);
				stream.abort(new Error("bidi stream error"));
			});
		}
	}

	/**
	 * Handles a single incoming bidi stream.
	 */
	async #runBidiStream(stream: Stream) {
		const messageType = await stream.reader.u53();

		if (messageType === SubscribeNamespace.id) {
			await this.#publisher.handleSubscribeNamespaceStream(stream);
		} else {
			console.warn(`unexpected bidi stream type: ${messageType}`);
			stream.abort(new Error("unexpected stream type"));
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

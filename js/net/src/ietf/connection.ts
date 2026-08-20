import { type Getter, Signal } from "@moq/signals";
import * as announce from "../announced.ts";
import type * as broadcast from "../broadcast.ts";
import type { Established } from "../connection/established.ts";
import { type Probe, type Stats, transportStats } from "../connection/stats.ts";
import { type Transport, transportOf } from "../connection/transport.ts";
import { error, fromClose, ProtocolViolation } from "../error.ts";
import type { Consumer as OriginConsumer } from "../origin.ts";
import * as Path from "../path.ts";
import { type Reader, Readers, type Stream } from "../stream.ts";
import { ControlStreamAdapter, NativeSession, type Session } from "./adapter.ts";
import * as Cluster from "./cluster.ts";
import { GoAway } from "./goaway.ts";
import { Group } from "./object.ts";
import { Publish } from "./publish.ts";
import { PublishNamespace } from "./publish_namespace.ts";
import { Publisher } from "./publisher.ts";
import { Subscribe, SubscribeUpdate } from "./subscribe.ts";
import { SubscribeNamespace, SubscribeNamespaceLegacy } from "./subscribe_namespace.ts";
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

	// The wire transport this session runs over.
	readonly transport: Transport;

	/** Whether the relay supports broadcast discovery; see {@link Established.discovery}. */
	readonly discovery: boolean;

	/** moq-transport has no PROBE, so this stays empty; see {@link Established.probe}. */
	readonly probe: Getter<Probe> = new Signal<Probe>({});

	// The established WebTransport session.
	#quic: WebTransport;

	// Session abstraction: adapter for v14-v16, native for v17.
	#session: Session;

	// Module for contributing tracks.
	#publisher: Publisher;

	// Module for distributing tracks.
	#subscriber: Subscriber;

	// What the peer declared about being solicited; see {@link Ietf.solicitFromSetup}.
	#solicit: boolean | undefined;

	// The Hop IDs this session declared; see {@link Cluster}.
	#cluster?: Cluster.Hops;

	// Just to avoid logging when `close()` is called.
	#closed = false;

	/**
	 * Creates a new Connection instance.
	 * @param url - The URL of the connection
	 * @param quic - The WebTransport session
	 * @param control - The control/setup stream
	 * @param maxRequestId - The initial max request ID
	 * @param version - The negotiated protocol version
	 * @param solicit - What the peer's SETUP declared (undefined when it declared nothing)
	 * @param cluster - The Hop IDs the SETUP exchange settled, on the versions that negotiate them
	 *
	 * @internal
	 */
	constructor({
		url,
		quic,
		control,
		maxRequestId,
		version,
		client,
		discovery = true,
		publish,
		solicit,
		cluster,
	}: {
		url: URL;
		quic: WebTransport;
		control: Stream;
		maxRequestId: bigint;
		version: IetfVersion;
		/** Whether this peer initiated the session, selecting the even request-ID space. */
		client: boolean;
		discovery?: boolean;
		/** The origin whose broadcasts are served to the peer. Omit to publish nothing. */
		publish?: OriginConsumer;
		/**
		 * What the peer declared about being solicited. `undefined` means it declared
		 * nothing, which is the one case where announcing at us unasked is not a bug.
		 */
		solicit?: boolean;
		/**
		 * The Hop IDs this session declared (MoQ Cluster). `undefined` on a version that
		 * cannot negotiate the extension, as is a `peer` the peer never declared.
		 */
		cluster?: Cluster.Hops;
	}) {
		this.url = url;
		this.discovery = discovery;
		this.version = versionName(version);
		this.transport = transportOf(quic);
		this.#quic = quic;

		// Two-path dispatch: v14-v16 uses adapter, v17+ uses native bidi streams
		if (version >= Version.DRAFT_17) {
			this.#session = new NativeSession(quic, version, client);
			// v17+: control/setup stream only carries GoAway
			void this.#runGoAway(control, version);
		} else {
			const adapter = new ControlStreamAdapter(quic, control, version, maxRequestId, client);
			this.#session = adapter;
			// Start the adapter read loop (routes control messages to virtual streams)
			void adapter.run().catch((err: unknown) => {
				if (!this.#closed) console.error("adapter error", err);
				this.close();
			});
		}

		this.#publisher = new Publisher({
			quic: this.#quic,
			session: this.#session,
			publish,
			requiresSolicitation: solicit ?? false,
			cluster,
		});
		this.#solicit = solicit;
		this.#cluster = cluster;
		this.#subscriber = new Subscriber({ session: this.#session, cluster });

		void this.#run();
	}

	/** Snapshot the transport's counters; see {@link Established.stats}. */
	async stats(): Promise<Stats> {
		return transportStats(this.#quic);
	}

	/**
	 * Closes the connection.
	 */
	close() {
		if (this.#closed) return;

		this.#closed = true;

		this.#session.close();

		try {
			this.#quic.close();
		} catch {
			// ignore
		}
	}

	async #run(): Promise<void> {
		try {
			await Promise.all([this.#runBidis(), this.#runUnis(), this.#publisher.runPublishNamespaces()]);
		} catch (err) {
			if (!this.#closed) {
				console.error("fatal error running connection", err);
			}
		} finally {
			this.close();
		}
	}

	/**
	 * Gets an announced reader for the specified prefix.
	 * @param prefix - The prefix for announcements
	 * @returns An Announced instance
	 */
	announced(prefix = Path.empty()): announce.Consumer {
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
	consume(path: Path.Valid): broadcast.Consumer {
		return this.#subscriber.consume(path);
	}

	/**
	 * Watches a broadcast, live only while it is announced.
	 *
	 * @param path - The path of the broadcast to watch
	 * @returns A reactive handle to the broadcast
	 */
	announcedBroadcast(path: Path.Valid): announce.Broadcast {
		return new announce.Broadcast({ connection: this, path });
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

				// The peer broke the protocol, so losing the stream is not enough: nothing
				// stops it repeating the violation on the next one.
				if (err instanceof ProtocolViolation) this.close();
			});
		}
	}

	/**
	 * Unified bidi stream dispatch. Reads typeId and routes to handler.
	 * Matches the lite module's runBidi pattern.
	 */
	async #runBidi(stream: Stream) {
		const typeId = await stream.reader.u53();

		switch (typeId) {
			// Draft-18 SUBSCRIBE_NAMESPACE (0x50) and the legacy 0x11 message decode
			// to the same request_id + namespace; the legacy options field is ignored.
			case SubscribeNamespace.id: {
				const msg = await SubscribeNamespace.decode(stream.reader, this.#session.version);
				await this.#publisher.runSubscribeNamespace(msg, stream);
				break;
			}
			case SubscribeNamespaceLegacy.id: {
				const legacy = await SubscribeNamespaceLegacy.decode(stream.reader, this.#session.version);
				const msg = new SubscribeNamespace({ requestId: legacy.requestId, namespace: legacy.namespace });
				await this.#publisher.runSubscribeNamespace(msg, stream);
				break;
			}
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
			case TrackStatusRequest.id: {
				const msg = await TrackStatusRequest.decode(stream.reader, this.#session.version);
				await this.#publisher.runTrackStatusRequest(msg, stream);
				break;
			}

			// Subscriber handles incoming notifications
			case PublishNamespace.id: {
				const msg = await PublishNamespace.decode(
					stream.reader,
					this.#session.version,
					Cluster.negotiated(this.#cluster),
				);

				// We always declare that advertisements to us must be solicited (MoQ
				// Solicit), and writing the option at all proves the peer implements the
				// extension, whichever value it chose. It also cannot have advertised
				// before reading our SETUP, since our SETUP is what says whether
				// advertising unasked is allowed. So this is a bug in the peer, and a
				// silent one on both sides if we tolerate it.
				//
				// Draft-14/15 are exempt: they have no inline NAMESPACE, so a
				// PUBLISH_NAMESPACE request is also how a peer answers our
				// SUBSCRIBE_NAMESPACE there, and the message alone does not say which.
				const legacy = this.#session.version === Version.DRAFT_14 || this.#session.version === Version.DRAFT_15;
				if (this.#solicit !== undefined && !legacy) {
					console.error(
						`unsolicited publish_namespace from a peer that implements MoQ Solicit: broadcast=${msg.trackNamespace}`,
					);
					this.close();
					break;
				}

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
		const header = await Group.decode(stream, this.#session.version);
		await this.#subscriber.handleGroup(header, stream);
	}

	/**
	 * v17+ only: reads GoAway from the setup/control stream.
	 */
	async #runGoAway(controlStream: Stream, version: IetfVersion) {
		try {
			const done = await controlStream.reader.done();
			if (done) return;

			const typeId = await controlStream.reader.u53();
			if (typeId === GoAway.id) {
				const msg = await GoAway.decode(controlStream.reader, version);
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

	/** Resolves when the session closes, decoding the peer's close code; see {@link Established.closed}. */
	get closed(): Promise<Error | null> {
		return this.#quic.closed.then(fromClose, (err: unknown) => error(err));
	}
}

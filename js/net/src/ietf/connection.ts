import { type Getter, Once, Signal } from "@moq/signals";
import type * as announce from "../announced.ts";
import type { Established } from "../connection/established.ts";
import type { Drain } from "../connection/goaway.ts";
import { type Probe, type Stats, transportStats } from "../connection/stats.ts";
import { type Transport, transportOf } from "../connection/transport.ts";
import { error, fromClose, ProtocolViolation, SessionCode, StreamCode, StreamError } from "../error.ts";
import type { Consumer as OriginConsumer } from "../origin.ts";
import type * as Path from "../path.ts";
import { type Reader, Readers, type Stream } from "../stream.ts";
import { withTimeout } from "../util/timeout.ts";
import { registerWire } from "../wire.ts";
import { ControlStreamAdapter, NativeSession, RequestWindowError, type Session } from "./adapter.ts";
import * as Cluster from "./cluster.ts";
import { Fetch, FetchHeader } from "./fetch.ts";
import { GoAway } from "./goaway.ts";
import { Group } from "./object.ts";
import { Publish } from "./publish.ts";
import { PublishNamespace } from "./publish_namespace.ts";
import { Publisher } from "./publisher.ts";
import { Subscribe, SubscribeUpdate } from "./subscribe.ts";
import {
	SUBSCRIBE_TRACKS_ID,
	SubscribeNamespace,
	SubscribeNamespaceLegacy,
	SubscribeOptions,
} from "./subscribe_namespace.ts";
import { Subscriber } from "./subscriber.ts";
import { TrackStatusRequest } from "./track.ts";
import { type IetfVersion, Version, versionName } from "./version.ts";

// The PADDING stream type (draft-18+): bytes a peer sends to probe for bandwidth.
const PADDING = 0x132b3e28n;

/**
 * Represents a connection to a MoQ server using moq-transport protocol.
 *
 * @public
 */
export class Connection implements Established {
	#closing?: Promise<void>;
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

	// Whether this side opened the session. Only a server may name a redirect.
	#client: boolean;

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

	// The peer's GOAWAY: read here on v17+, by the control stream adapter before that.
	#goaway: Once<Drain>;

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
	 * @param early - Uni streams that arrived before the peer's SETUP
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
		hidden = false,
		cluster,
		early = [],
		requestWindow,
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
		/** Whether the peer understands the HIDDEN parameter (MoQ Hidden). */
		hidden?: boolean;
		/**
		 * The Hop IDs this session declared (MoQ Cluster). `undefined` on a version that
		 * cannot negotiate the extension, as is a `peer` the peer never declared.
		 */
		cluster?: Cluster.Hops;
		/** Uni streams that arrived before the peer's SETUP, type unread (v17+). */
		early?: Reader[];
		/** Requests the peer may hold open on drafts 14 to 16, the window our SETUP advertised (default `REQUEST_WINDOW`). */
		requestWindow?: bigint;
	}) {
		this.url = url;
		this.discovery = discovery;
		this.version = versionName(version);
		this.transport = transportOf(quic);
		this.#quic = quic;
		this.#client = client;

		// Two-path dispatch: v14-v16 uses adapter, v17+ uses native bidi streams
		if (version >= Version.DRAFT_17) {
			this.#session = new NativeSession(quic, version, client);
			this.#goaway = new Once();
			// v17+: control/setup stream only carries GoAway
			void this.#runGoAway(control, version);
		} else {
			const adapter = new ControlStreamAdapter(quic, control, version, maxRequestId, client, requestWindow);
			this.#session = adapter;
			this.#goaway = adapter.goaway;
			// Start the adapter read loop (routes control messages to virtual streams)
			void adapter.run().catch((err: unknown) => {
				if (this.#closed) return;
				if (err instanceof RequestWindowError) {
					this.#close({ closeCode: err.code, reason: err.message });
					return;
				}
				console.error("adapter error", err);
				this.#close();
			});
		}

		this.#publisher = new Publisher({
			quic: this.#quic,
			session: this.#session,
			publish,
			requiresSolicitation: solicit ?? false,
			hidden,
			cluster,
		});
		this.#solicit = solicit;
		this.#cluster = cluster;
		this.#subscriber = new Subscriber({ session: this.#session, quic, cluster, hidden });
		registerWire(this, { consume: (path) => this.#subscriber.consume(path), goaway: this.#goaway });

		void this.#run(early);
	}

	/** Snapshot the transport's counters; see {@link Established.stats}. */
	async stats(): Promise<Stats> {
		return transportStats(this.#quic);
	}

	/** Withdraw announcements and wait up to one second for delivery before closing. */
	close(): Promise<void> {
		this.#closing ??= withTimeout(this.#publisher.withdraw(), 1000, "session close timed out").finally(() =>
			this.abort(),
		);
		return this.#closing;
	}

	/** End the session immediately without waiting for delivery. */
	abort(): void {
		if (this.#closed) return;
		this.#subscriber.close();
		this.#close();
	}

	// Close with the session code the peer should see, a clean close by default.
	#close(info?: WebTransportCloseInfo) {
		if (this.#closed) return;

		this.#closed = true;

		// Before the session, whose own close would send a clean code first.
		try {
			this.#quic.close(info);
		} catch {
			// ignore
		}

		this.#session.close();
	}

	// The peer broke the protocol, so losing the stream is not enough: nothing stops it
	// repeating the violation on the next one.
	#violated(err: ProtocolViolation) {
		this.#close({ closeCode: SessionCode.ProtocolViolation, reason: err.message });
	}

	async #run(early: Reader[]): Promise<void> {
		try {
			// Both run together. runPublishNamespaces is a no-op when the peer asked to be
			// solicited; otherwise it pushes PUBLISH_NAMESPACE. On draft-16 and later a
			// SUBSCRIBE_NAMESPACE stream is filled either way. A NAMESPACE is discovery,
			// not a second route.
			await Promise.all([this.#runBidis(), this.#runUnis(early), this.#publisher.runPublishNamespaces()]);
		} catch (err) {
			if (!this.#closed) {
				console.error("fatal error running connection", err);
			}
		} finally {
			// A graceful close owns the teardown while it drains. runPublishNamespaces is
			// a tracked withdrawal, so a failure in it ends this driver while close() is
			// still waiting on the sibling loops; closing here would drop their
			// withdrawals. close() aborts once its barrier settles or the deadline hits.
			if (!this.#closing) this.#close();
		}
	}

	/** Gets an announced reader for `scope`; see {@link Established.announced}. */
	announced(scope?: Path.Pattern, options?: announce.Options): announce.Consumer {
		return this.#subscriber.announced(scope, options);
	}

	/**
	 * Accepts bidi streams (virtual for v14-v16, real for v17) and dispatches.
	 */
	async #runBidis() {
		for (;;) {
			const stream = await this.#session.acceptBi();
			if (!stream) break;

			void this.#runBidi(stream).catch((err: unknown) => {
				if (err instanceof RequestWindowError) {
					this.#close({ closeCode: err.code, reason: err.message });
					return;
				}
				console.error("error processing bidi stream", err);
				stream.abort(new Error("bidi stream error"));
				if (err instanceof ProtocolViolation) this.#violated(err);
			});
		}
	}

	/**
	 * Unified bidi stream dispatch. Reads typeId and routes to handler.
	 * Matches the lite module's runBidi pattern.
	 */
	async #runBidi(stream: Stream) {
		// Full width, so unknown types above 2^53 still reach protocol classification.
		const typeId = await stream.reader.u62();

		switch (typeId) {
			// Draft-18 SUBSCRIBE_NAMESPACE (0x50) and the legacy 0x11 message decode
			// to the same request_id + namespace. We never send PUBLISH, so a legacy
			// request for PUBLISH alone is refused, and one for both gets only NAMESPACE.
			case BigInt(SubscribeNamespace.id): {
				const msg = await SubscribeNamespace.decode(stream.reader, this.#session.version);
				await this.#publisher.runSubscribeNamespace(msg, stream);
				break;
			}
			case BigInt(SubscribeNamespaceLegacy.id): {
				const legacy = await SubscribeNamespaceLegacy.decode(stream.reader, this.#session.version);
				if (legacy.subscribeOptions === SubscribeOptions.PUBLISH) {
					await this.#publisher.refuseSubscribeNamespace(legacy.requestId, stream);
					break;
				}
				const msg = new SubscribeNamespace({
					requestId: legacy.requestId,
					namespace: legacy.namespace,
					hidden: legacy.hidden,
				});
				// Draft-16 carries this on its own stream, so the control adapter never sees the ID.
				// Draft-14 and 15 already admitted it off the control stream.
				const adapter =
					this.#session instanceof ControlStreamAdapter && this.#session.version === Version.DRAFT_16
						? this.#session
						: undefined;
				if (adapter) adapter.admitRequest(legacy.requestId);
				try {
					await this.#publisher.runSubscribeNamespace(msg, stream);
				} finally {
					adapter?.releaseRequest(legacy.requestId);
				}
				break;
			}
			case BigInt(SubscribeUpdate.id): {
				// REQUEST_UPDATE (0x02) is a follow-up, not a valid initial message
				stream.abort(new Error("unexpected REQUEST_UPDATE as initial message"));
				break;
			}
			// Publisher handles incoming requests
			case BigInt(Subscribe.id): {
				const msg = await Subscribe.decode(stream.reader, this.#session.version);
				await this.#publisher.runSubscribe(msg, stream);
				break;
			}
			case BigInt(SUBSCRIBE_TRACKS_ID): {
				// 0x51 is only a message from draft-18 on.
				if (this.#session.version < Version.DRAFT_18) {
					throw new ProtocolViolation("SUBSCRIBE_TRACKS before draft-18");
				}
				await this.#publisher.runSubscribeTracks(stream);
				break;
			}
			case BigInt(TrackStatusRequest.id): {
				const msg = await TrackStatusRequest.decode(stream.reader, this.#session.version);
				await this.#publisher.runTrackStatusRequest(msg, stream);
				break;
			}
			case BigInt(Fetch.id): {
				const msg = await Fetch.decode(stream.reader, this.#session.version);
				await this.#publisher.runFetch(msg, stream);
				break;
			}

			// Subscriber handles incoming notifications
			case BigInt(PublishNamespace.id): {
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
					this.#close();
					break;
				}

				await this.#subscriber.runPublishNamespace(msg, stream);
				break;
			}
			case BigInt(Publish.id): {
				const msg = await Publish.decode(stream.reader, this.#session.version);
				await this.#subscriber.runPublish(msg, stream);
				break;
			}

			default:
				throw new ProtocolViolation(`unknown bidi stream type: 0x${typeId.toString(16)}`);
		}
	}

	/**
	 * Handles unidirectional streams for media delivery (groups).
	 */
	async #runUnis(early: Reader[]) {
		// Streams that beat the SETUP go first, in arrival order.
		for (const stream of early) this.#spawnUni(stream);

		const readers = new Readers(this.#quic, this.#session.version);
		for (;;) {
			const stream = await readers.next();
			if (!stream) break;
			this.#spawnUni(stream);
		}
	}

	#spawnUni(stream: Reader) {
		this.#runUni(stream)
			.then(() => {
				stream.stop(new StreamError(StreamCode.Cancel, { message: "cancel" }));
			})
			.catch((err: unknown) => {
				console.error("error processing object stream", err);
				stream.stop(err);

				// An unknown or invalid stream type MUST close the session, not just the stream.
				if (err instanceof ProtocolViolation) this.#violated(err);
			});
	}

	async #runUni(stream: Reader) {
		const version = this.#session.version;
		// Full width, so an unknown type past 2^53 is still classified rather than thrown.
		const type = await stream.u62();

		// SUBGROUP_HEADER types match 0b0XX1XXXX; Group.decode validates the bits per draft.
		if (type <= 0xffn && (type & 0x90n) === 0x10n) {
			const header = await Group.decode(stream, version, Number(type));
			await this.#subscriber.handleGroup(header, stream);
			return;
		}

		// The receiver MUST discard padding. We read it to the end rather than cancel,
		// so a peer probing for bandwidth gets the throughput it is measuring.
		if (type === PADDING && version >= Version.DRAFT_18) {
			await stream.discard();
			return;
		}

		// We never FETCH, so a fetch response answers nothing of ours.
		if (type === BigInt(FetchHeader.type)) throw new Error("unexpected fetch stream");

		// Anything else is unknown, and a second SETUP is a violation too.
		throw new ProtocolViolation(`unknown uni stream type: 0x${type.toString(16)}`);
	}

	/**
	 * v17+ only: reads GoAway from the setup/control stream.
	 *
	 * The session keeps serving after a GOAWAY so its groups in flight can finish while the
	 * caller migrates; only the stream ending, or a second GOAWAY, closes it here.
	 */
	async #runGoAway(controlStream: Stream, version: IetfVersion) {
		try {
			for (;;) {
				const done = await controlStream.reader.done();
				if (done) return;

				const typeId = await controlStream.reader.u53();
				if (typeId !== GoAway.id) {
					console.warn(`unexpected message on setup stream: 0x${typeId.toString(16)}`);
					return;
				}

				const msg = await GoAway.decode(controlStream.reader, version);
				if (this.#goaway.peek() !== undefined) throw new ProtocolViolation("duplicate GOAWAY");
				// A client may leave, but only the server may name where to go.
				if (!this.#client && msg.newSessionUri !== "") {
					throw new ProtocolViolation("client GOAWAY must not name a redirect");
				}
				this.#goaway.set(msg.drain());
			}
		} catch (err) {
			if (!this.#closed) {
				console.error("error reading setup stream", err);
			}
		} finally {
			this.#close();
		}
	}

	/** Resolves when the session closes, decoding the peer's close code; see {@link Established.closed}. */
	get closed(): Promise<Error | null> {
		return this.#quic.closed.then(fromClose, (err: unknown) => error(err));
	}
}

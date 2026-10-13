import { type Getter, Once, Signal } from "@moq/signals";
import type * as announce from "../announced.ts";
import type * as Auth from "../auth.ts";
import { AuthSession } from "../auth_session.ts";
import type { Established } from "../connection/established.ts";
import type { Drain } from "../connection/goaway.ts";
import { type Probe, type Stats, transportStats } from "../connection/stats.ts";
import { type Transport, transportOf } from "../connection/transport.ts";
import { error, fromClose, ProtocolViolation, StreamCode, StreamError } from "../error.ts";
import type { Consumer as OriginConsumer } from "../origin.ts";
import * as Path from "../path.ts";
import { type Reader, Readers, type Stream } from "../stream.ts";
import { withTimeout } from "../util/timeout.ts";
import { registerWire } from "../wire.ts";
import { ControlStreamAdapter, NativeSession, RequestWindowError, type Session } from "./adapter.ts";
import { AuthMessage, supported as authSupported, IetfAuthWire } from "./auth.ts";
import * as Cluster from "./cluster.ts";
import { type RequestKind, toRequestCode } from "./error.ts";
import { Fetch, FetchError, FetchHeader } from "./fetch.ts";
import { GoAway } from "./goaway.ts";
import * as Message from "./message.ts";
import { Group } from "./object.ts";
import { Publish, PublishError } from "./publish.ts";
import { PublishNamespace, PublishNamespaceError } from "./publish_namespace.ts";
import { Publisher } from "./publisher.ts";
import { RequestError } from "./request.ts";
import { Subscribe, SubscribeError, SubscribeUpdate } from "./subscribe.ts";
import {
	SUBSCRIBE_TRACKS_ID,
	SubscribeNamespace,
	SubscribeNamespaceError,
	SubscribeNamespaceLegacy,
	SubscribeOptions,
} from "./subscribe_namespace.ts";
import { Subscriber } from "./subscriber.ts";
import { MalformedAuthToken } from "./token.ts";
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

	/** Our tokens and grants, when the peer negotiated MoQ Auth; see {@link Established.auth}. */
	get auth(): Auth.Auth {
		return this.#auth;
	}

	// Our tokens and grants, and the answers to the peer's (MoQ Auth).
	#auth: AuthSession;

	// The established WebTransport session.
	#quic: WebTransport;

	// Whether this side opened the session. Only a server may name a redirect, and only
	// the dialing side fails loud on a publication its grant does not cover.
	#client: boolean;

	// Session abstraction: adapter for v14-v16, native for v17.
	#session: Session;

	// Module for contributing tracks.
	#publisher: Publisher;

	// Module for distributing tracks.
	#subscriber: Subscriber;

	// What the peer declared about being solicited; see {@link Ietf.solicitFromSetup}.
	#solicit: boolean | undefined;
	// Whether our SETUP declared MoQ Solicit, which is what makes an unasked announce a fault.
	#declaredSolicit: boolean;

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
		auth = false,
		early = [],
		requestWindow,
		extensions,
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
		/** Whether MoQ Auth is negotiated (draft-17+): offered by both SETUPs. */
		auth?: boolean;
		/** Uni streams that arrived before the peer's SETUP, type unread (v17+). */
		early?: Reader[];
		/** Requests the peer may hold open on drafts 14 to 16, the window our SETUP advertised (default `REQUEST_WINDOW`). */
		requestWindow?: bigint;
		/** What our own SETUP offered; every extension when omitted. */
		extensions?: { solicit: boolean };
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

		// What the peer's connection credential earns by default: publishing anything to us,
		// since we consume on demand, and subscribing to whatever we publish.
		const session = this.#session;
		this.#auth = new AuthSession({
			wire: auth && authSupported(version) ? new IetfAuthWire(session) : undefined,
			peerGrant: {
				publish: new Path.Patterns([Path.Pattern.all()]),
				subscribe: new Path.Patterns(publish ? [Path.Pattern.all()] : []),
			},
		});
		this.#client = client;

		this.#auth.violated.subscribe((err) => {
			if (err) this.#violated(err);
		});

		this.#publisher = new Publisher({
			quic: this.#quic,
			session: this.#session,
			publish,
			requiresSolicitation: solicit ?? false,
			hidden,
			cluster,
			grant: this.#auth.grant,
			ready: this.#auth.setupAnswered(),
		});
		this.#solicit = solicit;
		this.#declaredSolicit = extensions?.solicit ?? true;
		this.#cluster = cluster;
		this.#subscriber = new Subscriber({
			session: this.#session,
			quic,
			cluster,
			hidden,
			solicit,
			goaway: this.#goaway,
			grant: this.#auth.grant,
		});
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
		this.#publisher.close();

		this.#auth.close();

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
		this.#close({ closeCode: err.code, reason: err.message });
	}

	async #run(early: Reader[]): Promise<void> {
		try {
			// All run together. runPublishNamespaces is a no-op when the peer asked to be
			// solicited; otherwise it pushes PUBLISH_NAMESPACE. On draft-16 and later a
			// SUBSCRIBE_NAMESPACE stream is filled either way. A NAMESPACE is discovery,
			// not a second route.
			const tasks = [
				this.#runBidis(),
				this.#runUnis(early),
				this.#runDatagrams(),
				this.#publisher.runPublishNamespaces(),
			];
			// Fail loud on a publication our grant never covers, once the peer has answered the
			// credential we presented at setup.
			if (this.#client) tasks.push(this.#publisher.runEnforce(this.#auth.setupAnswered()));
			await Promise.all(tasks);
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
				const decoded = SubscribeNamespace.decode(stream.reader, this.#session.version);
				const msg = await this.#request(stream, "subscribe_namespace", decoded);
				if (msg) await this.#publisher.runSubscribeNamespace(msg, stream);
				break;
			}
			case BigInt(SubscribeNamespaceLegacy.id): {
				// Draft-16 carries this on its own stream, so the control adapter never sees the ID.
				// Draft-14 and 15 already admitted it off the control stream. A refused request
				// still spends its ID, so it is admitted and released like any other.
				const adapter =
					this.#session instanceof ControlStreamAdapter && this.#session.version === Version.DRAFT_16
						? this.#session
						: undefined;
				let legacy: SubscribeNamespaceLegacy;
				try {
					legacy = await SubscribeNamespaceLegacy.decode(stream.reader, this.#session.version);
				} catch (err) {
					if (!(err instanceof MalformedAuthToken) || err.requestId === undefined) throw err;
					adapter?.admitRequest(err.requestId);
					this.#spend(err.requestId);
					try {
						await this.#refuseToken(stream, "subscribe_namespace", err.requestId);
					} finally {
						adapter?.releaseRequest(err.requestId);
					}
					break;
				}
				if (adapter) adapter.admitRequest(legacy.requestId);
				this.#spend(legacy.requestId);
				try {
					if (legacy.subscribeOptions === SubscribeOptions.PUBLISH) {
						await this.#publisher.refuseSubscribeNamespace(legacy.requestId, stream);
					} else {
						const msg = new SubscribeNamespace({
							requestId: legacy.requestId,
							namespace: legacy.namespace,
							hidden: legacy.hidden,
						});
						await this.#publisher.runSubscribeNamespace(msg, stream);
					}
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
				const decoded = Subscribe.decode(stream.reader, this.#session.version);
				const msg = await this.#request(stream, "subscribe", decoded);
				if (msg) await this.#publisher.runSubscribe(msg, stream);
				break;
			}
			case BigInt(SUBSCRIBE_TRACKS_ID): {
				// 0x51 is only a message from draft-18 on.
				if (this.#session.version < Version.DRAFT_18) {
					throw new ProtocolViolation("SUBSCRIBE_TRACKS before draft-18");
				}
				const requestId = await Message.decode(stream.reader, async (reader) => {
					const id = await reader.u62();
					await reader.readAll();
					return id;
				});
				this.#session.spendRequest(requestId);
				await this.#publisher.runSubscribeTracks(stream);
				break;
			}
			case BigInt(TrackStatusRequest.id): {
				const decoded = TrackStatusRequest.decode(stream.reader, this.#session.version);
				const msg = await this.#request(stream, "track_status", decoded);
				if (msg) await this.#publisher.runTrackStatusRequest(msg, stream);
				break;
			}
			case BigInt(Fetch.id): {
				const decoded = Fetch.decode(stream.reader, this.#session.version);
				const msg = await this.#request(stream, "fetch", decoded);
				if (msg) await this.#publisher.runFetch(msg, stream);
				break;
			}

			// Subscriber handles incoming notifications
			case BigInt(PublishNamespace.id): {
				const decoded = PublishNamespace.decode(
					stream.reader,
					this.#session.version,
					Cluster.negotiated(this.#cluster),
				);
				const msg = await this.#request(stream, "publish_namespace", decoded);
				if (!msg) break;

				// Unless we declined it, we declare that advertisements to us must be
				// solicited (MoQ Solicit), and writing the option at all proves the peer
				// implements the extension, whichever value it chose. It also cannot have
				// advertised before reading our SETUP, since our SETUP is what says whether
				// advertising unasked is allowed. So this is a bug in the peer, and a
				// silent one on both sides if we tolerate it. Without our declaration an
				// unasked announce is what we invited.
				//
				// Draft-14/15 are exempt: they have no inline NAMESPACE, so a
				// PUBLISH_NAMESPACE request is also how a peer answers our
				// SUBSCRIBE_NAMESPACE there, and the message alone does not say which.
				const legacy = this.#session.version === Version.DRAFT_14 || this.#session.version === Version.DRAFT_15;
				if (this.#declaredSolicit && this.#solicit !== undefined && !legacy) {
					console.error(
						`unsolicited publish_namespace from a peer that implements MoQ Solicit: broadcast=${msg.trackNamespace}`,
					);
					this.#close();
					break;
				}

				await this.#subscriber.runPublishNamespace(msg, stream);
				break;
			}
			case BigInt(AuthMessage.id): {
				// Only a peer that negotiated MoQ Auth may send one.
				if (!this.#auth.negotiated) throw new ProtocolViolation("AUTH without MoQ Auth");
				await this.#auth.serve(stream);
				break;
			}
			case BigInt(Publish.id): {
				const decoded = Publish.decode(stream.reader, this.#session.version);
				const msg = await this.#request(stream, "publish", decoded);
				if (msg) await this.#subscriber.runPublish(msg, stream);
				break;
			}

			default:
				throw new ProtocolViolation(`unknown bidi stream type: 0x${typeId.toString(16)}`);
		}
	}

	/**
	 * Finish decoding a request, refusing it instead when its AUTHORIZATION TOKENs repeat.
	 * Section 8.9 rejects that message rather than the session, unlike any other decode error.
	 *
	 * @returns undefined once refused.
	 */
	async #request<T extends { requestId: bigint }>(
		stream: Stream,
		kind: RequestKind,
		decoded: Promise<T>,
	): Promise<T | undefined> {
		let msg: T;
		try {
			msg = await decoded;
		} catch (err) {
			if (!(err instanceof MalformedAuthToken) || err.requestId === undefined) throw err;
			this.#spend(err.requestId);
			await this.#refuseToken(stream, kind, err.requestId);
			return undefined;
		}
		this.#spend(msg.requestId);
		return msg;
	}

	/** Spend a request's ID on draft 17+; the control stream adapter already spent it on earlier drafts. */
	#spend(requestId: bigint) {
		if (this.#session instanceof NativeSession) this.#session.spendRequest(requestId);
	}

	/** Refuse a request MALFORMED_AUTH_TOKEN, ending its stream. Mirrors Rust's `token::reject`. */
	async #refuseToken(stream: Stream, kind: RequestKind, requestId: bigint) {
		const version = this.#session.version;
		// Each draft-14 error message has the REQUEST_ERROR body that draft-15 folded them into.
		const id14 = {
			subscribe: SubscribeError.id,
			fetch: FetchError.id,
			publish: PublishError.id,
			publish_namespace: PublishNamespaceError.id,
			subscribe_namespace: SubscribeNamespaceError.id,
			// TRACK_STATUS_ERROR
			track_status: 0x0f,
		}[kind];
		const legacy = version === Version.DRAFT_14 || version === Version.DRAFT_15 || version === Version.DRAFT_16;
		await stream.writer.u53(version === Version.DRAFT_14 ? id14 : RequestError.id);
		await new RequestError({
			requestId: legacy ? requestId : undefined,
			errorCode: toRequestCode("malformed_auth_token", kind, version),
			reasonPhrase: "repeated AUTHORIZATION TOKEN",
		}).encode(stream.writer, version);
		stream.close();
	}

	// A malformed OBJECT_DATAGRAM is the peer breaking the protocol, like an invalid stream type.
	async #runDatagrams() {
		try {
			await this.#subscriber.runDatagrams();
		} catch (err: unknown) {
			if (!(err instanceof ProtocolViolation)) throw err;
			console.warn("malformed datagram", err);
			this.#violated(err);
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

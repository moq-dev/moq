import { Nano, Second } from "@moq/clock";
import { type Getter, Once, race, Signal } from "@moq/signals";
import type * as announce from "../announced.ts";
import type * as Auth from "../auth.ts";
import { AuthSession } from "../auth_session.ts";
import type { Established } from "../connection/established.ts";
import type { Drain } from "../connection/goaway.ts";
import { type Probe, type Stats, transportStats } from "../connection/stats.ts";
import { type Transport, transportOf } from "../connection/transport.ts";
import { error, fromClose, ProtocolViolation, SessionCode, StreamCode, StreamError, sessionClosed } from "../error.ts";
import type { Consumer as OriginConsumer } from "../origin.ts";
import * as Path from "../path.ts";
import { type Reader, Readers, type Stream } from "../stream.ts";
import { withTimeout } from "../util/timeout.ts";
import { registerWire } from "../wire.ts";
import { AuthMessage, IetfAuthWire } from "./auth.ts";
import * as Cluster from "./cluster.ts";
import type { RequestKind } from "./error.ts";
import { Fetch, FetchHeader } from "./fetch.ts";
import { GoAway } from "./goaway.ts";
import { Group } from "./object.ts";
import { Publish } from "./publish.ts";
import { PublishNamespace } from "./publish_namespace.ts";
import { Publisher } from "./publisher.ts";
import { malformed, refuse } from "./request_stream.ts";
import { NativeSession } from "./session.ts";
import { Subscribe, SubscribeUpdate } from "./subscribe.ts";
import { SUBSCRIBE_TRACKS_ID, SubscribeNamespace } from "./subscribe_namespace.ts";
import { SubscribeTracks } from "./subscribe_tracks.ts";
import { Subscriber } from "./subscriber.ts";
import { MalformedAuthToken } from "./token.ts";
import { TrackStatusRequest } from "./track.ts";
import { type IetfVersion, Version, versionName } from "./version.ts";

// The PADDING stream type (draft-18+): bytes a peer sends to probe for bandwidth.
const PADDING = 0x132b3e28n;

// A handler owns its request stream, and ending the request (closing the session over a
// violation) is its job: closing again here could only send a competing code.
function handed(task: Promise<void>): Promise<void> {
	return task.catch((err: unknown) => console.error("error processing bidi stream", err));
}

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

	/** moq-transport has no PROBE, so this stays empty; see {@link Established.probe}. */
	readonly probe: Getter<Probe> = new Signal<Probe>({});

	/** Our tokens and grants, when the peer negotiated MoQ Auth; see {@link Established.auth}. */
	get auth(): Auth.Handle {
		return this.#auth.handle;
	}

	// Our tokens and grants, and the answers to the peer's (MoQ Auth).
	#auth: AuthSession;

	// The established WebTransport session.
	#quic: WebTransport;

	// Whether this side opened the session. Only a server may name a redirect, and only
	// the dialing side fails loud on a publication its grant does not cover.
	#client: boolean;

	// Opens request streams and allocates request IDs.
	#session: NativeSession;

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

	// Whether both SETUPs offered MoQ Epoch, so advertisements and requests may carry EPOCH.
	#epoch: boolean;

	// The peer's GOAWAY, read off its SETUP stream.
	#goaway = new Once<Drain>();

	// Just to avoid logging when `close()` is called.
	#closed = false;

	/** The implementation the peer advertised; see {@link Established.peerImplementation}. */
	readonly peerImplementation: Promise<string | undefined>;

	/**
	 * Creates a new Connection instance.
	 * @param url - The URL of the connection
	 * @param quic - The WebTransport session
	 * @param control - The SETUP stream, which then carries GOAWAY
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
		version,
		client,
		publish,
		solicit,
		hidden = false,
		maxAge = false,
		activeCount = false,
		cluster,
		auth = false,
		handle,
		epoch = false,
		early = [],
		extensions,
		peerImplementation,
	}: {
		url: URL;
		quic: WebTransport;
		control: Stream;
		version: IetfVersion;
		/** Whether this peer initiated the session, selecting the even request-ID space. */
		client: boolean;
		/** The origin whose broadcasts are served to the peer. Omit to publish nothing. */
		publish?: OriginConsumer;
		/**
		 * What the peer declared about being solicited. `undefined` means it declared
		 * nothing, which is the one case where announcing at us unasked is not a bug.
		 */
		solicit?: boolean;
		/** Whether the peer understands the HIDDEN parameter (MoQ Hidden). */
		hidden?: boolean;
		/** Whether the peer accepts subscriber max age. */
		maxAge?: boolean;
		/** Whether both SETUPs declared MoQ Active Count. */
		activeCount?: boolean;
		/**
		 * The Hop IDs this session declared (MoQ Cluster). `undefined` on a version that
		 * cannot negotiate the extension, as is a `peer` the peer never declared.
		 */
		cluster?: Cluster.Hops;
		/** Whether MoQ Auth is negotiated: offered by both SETUPs. */
		auth?: boolean;
		/** The auth handle to build the session on, or undefined for one of its own. */
		handle?: Auth.Handle;
		/** Whether MoQ Epoch is negotiated: offered by both SETUPs. */
		epoch?: boolean;
		/** Uni streams that arrived before the peer's SETUP, type unread. */
		early?: Reader[];
		/** What our own SETUP offered; every extension when omitted. */
		extensions?: { solicit: boolean };
		/** The implementation the peer's SETUP advertised, read before the session starts. */
		peerImplementation?: string;
	}) {
		this.url = url;
		this.version = versionName(version);
		this.transport = transportOf(quic);
		this.#quic = quic;
		this.#client = client;
		this.peerImplementation = Promise.resolve(peerImplementation);

		this.#session = new NativeSession(quic, version, client);
		// The SETUP stream only carries GOAWAY from here on.
		void this.#runGoAway(control, version);

		// What the peer's connection credential earns by default: publishing anything to us,
		// since we consume on demand, and subscribing to whatever we publish.
		const session = this.#session;
		this.#auth = new AuthSession({
			wire: auth ? new IetfAuthWire(session) : undefined,
			requests: true,
			peerGrant: {
				publish: new Path.Patterns([Path.Pattern.all()]),
				subscribe: new Path.Patterns(publish ? [Path.Pattern.all()] : []),
			},
			handle,
			draining: () => this.#goaway.peek() !== undefined || this.#closing !== undefined,
		});
		// Left before anything observes the close, so a successor on the same handle never
		// finds this session still active.
		const leave = () => this.#auth.close();
		void quic.closed.then(leave, leave);
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
			activeCount,
			cluster,
			epoch,
			grant: this.#auth.scope,
			auth: this.#auth,
			ready: this.#auth.setupAnswered(),
		});
		this.#solicit = solicit;
		this.#declaredSolicit = extensions?.solicit ?? true;
		this.#cluster = cluster;
		this.#epoch = epoch;
		this.#subscriber = new Subscriber({
			maxAge,
			session: this.#session,
			quic,
			cluster,
			epoch,
			hidden,
			activeCount,
			goaway: this.#goaway,
			grant: this.#auth.scope,
			auth: this.#auth,
		});
		registerWire(this, {
			consume: (path) => this.#subscriber.consume(path),
			goaway: this.#goaway,
			// The drafts refuse overlapping SUBSCRIBE_NAMESPACEs (PREFIX_OVERLAP). A peer that
			// declared MoQ Solicit is ours and holds both; any other may enforce it.
			exclusive: solicit === undefined,
			auth: this.#auth,
		});

		void this.#run(early);
	}

	/** Snapshot the transport's counters; see {@link Established.stats}. */
	async stats(): Promise<Stats> {
		return transportStats(this.#quic);
	}

	/** Withdraw announcements and wait up to one second for delivery before closing. */
	close(): Promise<void> {
		this.#closing ??= withTimeout(
			this.#publisher.withdraw(),
			Nano.fromSecond(Second(1)),
			"session close timed out",
		).finally(() => this.abort());
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
			// solicited; otherwise it pushes PUBLISH_NAMESPACE. A SUBSCRIBE_NAMESPACE stream
			// is filled either way. A NAMESPACE is discovery,
			// not a second route.
			const tasks = [
				this.#runBidis(),
				this.#runUnis(early),
				this.#runDatagrams(),
				this.#publisher.runPublishNamespaces(),
			];
			// Fail loud on a publication our grant never covers, once the peer has answered the
			// credential we presented at setup.
			if (this.#client) tasks.push(this.#publisher.runEnforce(this.#auth.setupAnswered(), this.#auth));
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

	/** Accepts request streams and dispatches them. */
	async #runBidis() {
		for (;;) {
			const stream = await this.#session.acceptBi();
			if (!stream) break;

			void this.#runBidi(stream).catch((err: unknown) => {
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
			case BigInt(SubscribeNamespace.id): {
				const decoded = SubscribeNamespace.decode(stream.reader, this.#session.version);
				const msg = await this.#request(stream, "subscribe_namespace", decoded);
				if (msg) await handed(this.#publisher.runSubscribeNamespace(msg, stream));
				break;
			}
			case BigInt(SubscribeUpdate.id): {
				// REQUEST_UPDATE (0x02) is a follow-up, not a valid initial message
				throw new ProtocolViolation("unexpected REQUEST_UPDATE as initial message");
			}
			// Publisher handles incoming requests
			case BigInt(Subscribe.id): {
				const decoded = Subscribe.decode(stream.reader, this.#session.version);
				const msg = await this.#request(stream, "subscribe", decoded);
				if (msg) await handed(this.#publisher.runSubscribe(msg, stream));
				break;
			}
			case BigInt(SUBSCRIBE_TRACKS_ID): {
				const decoded = SubscribeTracks.decode(stream.reader, this.#session.version);
				const msg = await this.#request(stream, "subscribe_namespace", decoded);
				if (msg) await handed(this.#publisher.runSubscribeTracks(msg, stream));
				break;
			}
			case BigInt(TrackStatusRequest.id): {
				const decoded = TrackStatusRequest.decode(stream.reader, this.#session.version);
				const msg = await this.#request(stream, "track_status", decoded);
				if (msg) await handed(this.#publisher.runTrackStatusRequest(msg, stream));
				break;
			}
			case BigInt(Fetch.id): {
				const decoded = Fetch.decode(stream.reader, this.#session.version);
				const msg = await this.#request(stream, "fetch", decoded);
				if (msg) await handed(this.#publisher.runFetch(msg, stream));
				break;
			}

			// Subscriber handles incoming notifications
			case BigInt(PublishNamespace.id): {
				const decoded = PublishNamespace.decode(stream.reader, this.#session.version, {
					cluster: Cluster.negotiated(this.#cluster),
					epoch: this.#epoch,
				});
				const msg = await this.#request(stream, "publish_namespace", decoded);
				if (!msg) break;

				// Unless we declined it, we declare that advertisements to us must be
				// solicited (MoQ Solicit), and writing the option at all proves the peer
				// implements the extension, whichever value it chose. It also cannot have
				// advertised before reading our SETUP, since our SETUP is what says whether
				// advertising unasked is allowed. So this is a bug in the peer, and a
				// silent one on both sides if we tolerate it. Without our declaration an
				// unasked announce is what we invited.
				if (this.#declaredSolicit && this.#solicit !== undefined) {
					throw new ProtocolViolation(
						`unsolicited publish_namespace from a peer that implements MoQ Solicit: broadcast=${msg.trackNamespace}`,
					);
				}

				await handed(this.#subscriber.runPublishNamespace(msg, stream));
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
				if (msg) await handed(this.#subscriber.runPublish(msg, stream));
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
			if (!(err instanceof MalformedAuthToken) || err.requestId === undefined) throw malformed(err, kind);
			this.#spend(err.requestId);
			await this.#refuseToken(stream, kind);
			return undefined;
		}
		this.#spend(msg.requestId);
		return msg;
	}

	/** Spend a request's ID, closing the session if it was already used. */
	#spend(requestId: bigint) {
		this.#session.checkRequestId(requestId);
	}

	/** Refuse a request MALFORMED_AUTH_TOKEN, ending its stream. Mirrors Rust's `token::reject`. */
	async #refuseToken(stream: Stream, kind: RequestKind) {
		await refuse(stream, this.#session.version, kind, {
			condition: "malformed_auth_token",
			reasonPhrase: "repeated AUTHORIZATION TOKEN",
		});
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
				// A peer resetting a stream it no longer wants to send is routine, not an error;
				// only a violation is, and that one closes the session with its code.
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
		if (type === PADDING) {
			await stream.discard();
			return;
		}

		// The head of a join, answering our draft-18 joining FETCH or draft-22 fill.
		if (type === BigInt(FetchHeader.type)) {
			await this.#subscriber.handleFetch(stream);
			return;
		}

		// Anything else is unknown, and a second SETUP is a violation too.
		throw new ProtocolViolation(`unknown uni stream type: 0x${type.toString(16)}`);
	}

	/**
	 * Reads GOAWAY from the SETUP stream.
	 *
	 * The session keeps serving after a GOAWAY so its groups in flight can finish while the
	 * caller migrates; only the stream ending, or a second GOAWAY, closes it here.
	 */
	async #runGoAway(controlStream: Stream, version: IetfVersion) {
		try {
			const read = async () => {
				for (;;) {
					const done = await controlStream.reader.done();
					if (done) throw new ProtocolViolation("control stream closed");

					const msg = await this.#readGoAway(controlStream, version).catch((err: unknown) => {
						throw malformed(err, "setup stream message");
					});
					if (this.#goaway.peek() !== undefined) throw new ProtocolViolation("duplicate GOAWAY");
					// A client may leave, but only the server may name where to go.
					if (!this.#client && msg.newSessionUri !== "") {
						throw new ProtocolViolation("client GOAWAY must not name a redirect");
					}
					// Draft-18's Request ID names our IDs: even when we are the client, odd for a server.
					// It is required here, on the control stream.
					if (version === Version.DRAFT_18 && msg.requestId === undefined) {
						throw new ProtocolViolation("draft-18 GOAWAY on the control stream lacks a Request ID");
					}
					if (msg.requestId !== undefined && msg.requestId % 2n !== (this.#client ? 0n : 1n)) {
						throw new ProtocolViolation("GOAWAY Request ID has the wrong parity", {
							code: SessionCode.InvalidRequestId,
						});
					}
					// Reject bytes already behind GOAWAY before a caller can close on the drain.
					if (controlStream.reader.tryDecode((c) => c.u8()) !== undefined)
						throw new ProtocolViolation("message after control stream GOAWAY");
					this.#goaway.set(msg.drain());
				}
			};
			const stopped = controlStream.writer.closed.then(() => {
				throw new ProtocolViolation("control stream stopped");
			});
			await race([read(), stopped]);
		} catch (err) {
			if (!this.#closed && !sessionClosed(err))
				this.#violated(
					err instanceof ProtocolViolation
						? err
						: new ProtocolViolation("control stream failed", { cause: err }),
				);
		} finally {
			this.#close();
		}
	}

	// The setup stream carries nothing but GOAWAY after SETUP.
	async #readGoAway(controlStream: Stream, version: IetfVersion): Promise<GoAway> {
		const typeId = await controlStream.reader.u53();
		if (typeId !== GoAway.id) {
			throw new ProtocolViolation(`unexpected message on setup stream: 0x${typeId.toString(16)}`);
		}
		return GoAway.decode(controlStream.reader, version);
	}

	/** Resolves when the session closes, decoding the peer's close code; see {@link Established.closed}. */
	get closed(): Promise<Error | null> {
		return this.#quic.closed.then(fromClose, (err: unknown) => error(err));
	}
}

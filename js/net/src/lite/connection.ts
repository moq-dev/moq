import { Milli, Nano } from "@moq/clock";
import { Effect, type Getter, Once, Signal } from "@moq/signals";
import type * as announce from "../announced.ts";
import type * as Auth from "../auth.ts";
import { AuthSession } from "../auth_session.ts";
import type { Established } from "../connection/established.ts";
import type { Drain } from "../connection/goaway.ts";
import { type Probe, type Stats, transportStats } from "../connection/stats.ts";
import { type Transport, transportOf } from "../connection/transport.ts";
import { closeError, error, fromClose, ProtocolViolation, StreamCode, sessionCause } from "../error.ts";
import { type Hop, randomHop } from "../hop.ts";
import { IMPLEMENTATION } from "../implementation.ts";
import type { Consumer as OriginConsumer } from "../origin.ts";
import * as Path from "../path.ts";
import { type Reader, Readers, Stream, Writer } from "../stream.ts";
import { Observed } from "../util/observed.ts";
import { withTimeout } from "../util/timeout.ts";
import { registerWire } from "../wire.ts";
import { AnnounceRequest } from "./announce.ts";
import { LiteAuthWire } from "./auth.ts";
import { closeViolation } from "./error.ts";
import { Fetch } from "./fetch.ts";
import { Goaway } from "./goaway.ts";
import { Group } from "./group.ts";
import { Publisher } from "./publisher.ts";
import { SessionInfo } from "./session.ts";
import { hasClientParameters, ProbeLevel, type Role, Setup } from "./setup.ts";
import { DataType, StreamId } from "./stream.ts";
import { Subscribe } from "./subscribe.ts";
import { Subscriber } from "./subscriber.ts";
import { Track as TrackMessage } from "./track.ts";
import { Tracks as TracksMessage } from "./tracks.ts";
import {
	hasAuth,
	hasDatagrams,
	hasGoaway,
	hasPadding,
	hasProbeRtt,
	hasSetupStream,
	hasTrackStream,
	hasTracks,
	Version,
	versionName,
} from "./version.ts";

/**
 * Constructor options for {@link Connection}.
 *
 * @internal
 */
export interface ConnectionProps {
	/** The URL of the connection. */
	url: URL;
	/** The established WebTransport session. */
	quic: WebTransport;
	/** The negotiated wire version. */
	version: Version;
	/** The session stream, absent on drafts that have none. */
	session?: Stream;
	/** The origin whose broadcasts are served to the peer. Omit to publish nothing. */
	publish?: OriginConsumer;
	/** The auth handle to build the session on, or undefined for one of its own. */
	handle?: Auth.Handle;
	/**
	 * Whether this side dialed. Only the dialing side aborts on a publication its grant does
	 * not cover: a server publishes whatever the peer may read, not what it intends to push.
	 * Defaults to true.
	 */
	client?: boolean;
	/** The connection credential the client's SETUP presents, on versions that carry one (lite-07+). */
	token?: Uint8Array;
	/** The implementation our SETUP advertises (lite-05+). Defaults to {@link IMPLEMENTATION}; empty sends nothing. */
	implementation?: string;
}

/**
 * Represents a connection to a MoQ server.
 *
 * @public
 */
export class Connection implements Established {
	#closing?: Promise<void>;
	// The URL of the connection.
	readonly url: URL;

	// The version of the connection as a human-readable string.
	readonly version: string;

	// The wire transport this session runs over.
	readonly transport: Transport;

	// The version used for encoding/decoding.
	#version: Version;

	// The established WebTransport session.
	#quic: WebTransport;

	// Use to receive/send session messages.
	#session?: Stream;

	// Module for contributing tracks.
	#publisher: Publisher;

	// Module for distributing tracks.
	#subscriber: Subscriber;

	// The received unidirectional streams still being read, which a closing session drains.
	#unis = new Set<Promise<void>>();

	/** The peer's PROBE estimates; see {@link Established.probe}. */
	readonly probe: Getter<Probe>;

	// Our tokens and grants, and the answers to the peer's; see {@link Established.auth}.
	#auth: AuthSession;

	/** Random per-connection Hop ID. Shared by Publisher (for outbound hop
	 * chains) and Subscriber (available for optional self-filtering on announces). */
	readonly hop: Hop;

	// The peer's SETUP, recorded once its Setup stream is read (lite-05+). Streams whose
	// encoding depends on a negotiated capability (e.g. PROBE) wait on this. undefined
	// until the peer's SETUP arrives; stays undefined forever on older drafts.
	#peerSetup = new Signal<Setup | undefined>(undefined);
	#setupSeen = false;

	// Mirrors the role out of #peerSetup, so the public surface exposes the peer's declared
	// direction without handing out the whole SETUP (whose probe level gates our own streams).
	#peerRole = new Signal<Role | undefined>(undefined);

	// Written by the Subscriber as PROBE messages arrive.
	#probe = new Observed<Probe>({});

	// Whether we dialed; see {@link ConnectionProps.client}.
	#client: boolean;
	// See {@link ConnectionProps.token}.
	#token?: Uint8Array;
	// What our SETUP advertises; see {@link ConnectionProps.implementation}.
	#implementation: string;
	#peerImplementation = Promise.withResolvers<string | undefined>();

	/** The implementation the peer advertised; see {@link Established.peerImplementation}. */
	readonly peerImplementation = this.#peerImplementation.promise;

	// The peer's GOAWAY. Lite carries no deadline, so only the URI is set.
	#goaway = new Once<Drain>();

	/**
	 * The {@link Role} the peer advertised in its SETUP, for a server deciding whether the
	 * peer's authorization grants the direction it intends to use.
	 *
	 * `undefined` until the peer's SETUP arrives, and forever on pre-lite-05 versions, which
	 * carry no in-band role. {@link Role.Both} is the absence of the parameter, so it is what
	 * a peer reports when it declines to declare a direction, sends a value we don't
	 * recognize, or is a server (which never sends one).
	 */
	get peerRole(): Getter<Role | undefined> {
		return this.#peerRole;
	}

	/**
	 * Creates a new Connection instance.
	 *
	 * @internal
	 */
	constructor({
		url,
		quic,
		version,
		session,
		publish,
		handle,
		client = true,
		token,
		implementation = IMPLEMENTATION,
	}: ConnectionProps) {
		this.url = url;
		// Older versions carry the credential in the URL instead.
		this.#token = client && hasAuth(version) ? token : undefined;
		this.#quic = quic;
		this.#session = session;
		// The session stream was opened at the SETUP exchange's version; the rest of it speaks this one.
		if (session) {
			session.reader.version = version;
			session.writer.version = version;
		}
		this.version = versionName(version);
		this.#version = version;
		this.transport = transportOf(quic);

		this.probe = this.#probe;

		this.hop = randomHop();
		// What the peer's connection credential earns by default: publishing anything to us,
		// since we consume on demand, and subscribing to whatever we publish.
		this.#auth = new AuthSession({
			wire: hasAuth(version) ? new LiteAuthWire(quic, version) : undefined,
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
		this.#auth.violated.subscribe((err) => {
			if (!err) return;
			closeViolation(this.#quic, err);
			this.#fail(err);
		});

		this.#publisher = new Publisher(this.#quic, this.#version, this.hop, publish, {
			grant: this.#auth.scope,
			ready: this.#auth.setupAnswered(),
		});
		this.#subscriber = new Subscriber(
			this.#quic,
			this.#version,
			this.hop,
			this.#probe,
			this.#peerSetup,
			this.#goaway,
			this.#auth.scope,
			undefined,
			this.#auth,
		);
		this.#client = client;
		this.#implementation = implementation;
		// Only the lite-05+ Setup Stream carries it.
		if (!hasSetupStream(version)) this.#peerImplementation.resolve(undefined);
		registerWire(this, {
			consume: (path) => this.#subscriber.consume(path),
			goaway: this.#goaway,
			exclusive: false,
			auth: this.#auth,
		});

		void this.#run();
	}

	/** Our tokens and grants; see {@link Established.auth}. */
	get auth(): Auth.Handle {
		return this.#auth.handle;
	}

	/**
	 * Withdraw announcements and finish the requests this session serves, waiting up to one
	 * second for delivery before closing.
	 *
	 * A served subscription first delivers its remaining group streams, and on lite-07 also
	 * waits for the subscriber's FIN, which says it read the tail. A live track never ends on
	 * its own, so it holds the close until the deadline.
	 */
	close(): Promise<void> {
		this.#closing ??= withTimeout(
			this.#publisher.drain(),
			Nano.fromMilli(Milli(1000)),
			"session close timed out",
		).finally(() => this.abort());
		return this.#closing;
	}

	/** End the session immediately without waiting for delivery. */
	abort(): void {
		this.#auth.close();
		this.#publisher.close();
		// Received tracks end in #run once what already arrived drains, as on the peer's close.
		this.#closeTransport();
	}

	#closeTransport(): void {
		try {
			// TODO: For whatever reason, this try/catch doesn't seem to work..?
			this.#quic.close();
		} catch {
			// ignore
		}
	}

	// End the received tracks with the session's end, then tear down the rest.
	#fail(err: Error): void {
		this.#subscriber.close(err);
		this.abort();
	}

	async #run(): Promise<void> {
		const tasks: Promise<void>[] = [this.#runSession(), this.#runBidis(), this.#runUnis()];

		if (hasSetupStream(this.#version)) {
			tasks.push(this.#sendSetup());
		}

		const probes = new Effect((effect) => {
			if (!effect.get(this.#probe.wanted)) return;
			effect.spawn(() => this.#subscriber.runProbe(effect.abort));
		});

		// Fail loud on a publication our grant never covers, once the peer has answered the
		// credential we presented at setup.
		if (this.#client && hasAuth(this.#version)) {
			tasks.push(this.#publisher.runEnforce(this.#auth.setupAnswered(), this.#auth));
		}

		// Route incoming QUIC datagrams into their subscriptions (lite-05+; runDatagrams
		// no-ops on a transport that doesn't carry them).
		if (hasDatagrams(this.#version)) {
			tasks.push(this.#subscriber.runDatagrams());
		}

		let fatal: Error | undefined;
		try {
			// An application close rejects QMux's pending accepts with a teardown error.
			// The transport's close result owns that termination, rather than the task failure.
			await Promise.race([Promise.all(tasks), this.#quic.closed]);
		} catch (err) {
			console.error("fatal error running connection", err);
			// A session-sourced failure is the peer's close, not the raw transport error.
			fatal = await sessionCause(this.#quic, err);
		} finally {
			probes.close();
			// A session that ends before the peer's SETUP never learns its implementation.
			this.#peerImplementation.resolve(undefined);
			const err = fatal ?? (await closeError(this.#quic));
			// A failed task can end the session over a live transport. Closing it first ends
			// every read the drain waits on.
			this.#closeTransport();
			// The peer's close can overtake the group streams and SUBSCRIBE FIN it sent first.
			// Read what already arrived, so a track its publisher ended finishes rather than aborts.
			await Promise.all([...this.#unis, this.#subscriber.drain()]);
			// The session ended under every track it was receiving, so they end with its
			// error: a clean close from either side reads as a cancelled session.
			this.#fail(err);
		}
	}

	announced(scope?: Path.Pattern, options?: announce.Options): announce.Consumer {
		return this.#subscriber.announced(scope, options);
	}

	async #runSession() {
		if (!this.#session) {
			return;
		}

		try {
			for (;;) {
				const msg = await SessionInfo.decodeMaybe(this.#session.reader, this.#version);
				if (!msg) break;
			}
		} finally {
			console.debug("session stream closed");
		}
	}

	// Open the unidirectional Setup Stream, send our single SETUP, and FIN (lite-05+).
	// The browser uses WebTransport, which carries the request URI, so we advertise no
	// path and leave routing to the URL. The probe level reflects what this transport
	// can actually measure; we never pad, so we never advertise Increase.
	// Role stays Both: the publish origin starts empty and fills later, and consume is
	// called after this point, so there is nothing to narrow yet. The origin declares
	// our session identity so the peer can filter
	// reflected announcements (lite-06 removed ANNOUNCE_REQUEST's exclude_hop for it).
	async #sendSetup(): Promise<void> {
		const writer = await Writer.open(this.#quic, { version: this.#version });
		try {
			await writer.u53(DataType.Setup);
			const probe = await probeLevel(this.#quic, this.#version);
			await new Setup({ probe, hop: this.hop, token: this.#token, implementation: this.#implementation }).encode(
				writer,
				this.#version,
			);
			writer.close();
		} catch (err: unknown) {
			writer.reset(err);
			throw err;
		}
	}

	async #runBidis() {
		for (;;) {
			const stream = await Stream.accept(this.#quic, this.#version);
			if (!stream) break;

			this.#runBidi(stream)
				.catch((err: unknown) => {
					stream.writer.reset(err);
					// A protocol violation on one stream is the peer breaking the session.
					// Resetting that stream would leave it free to repeat malformed input.
					const violation = closeViolation(this.#quic, err);
					if (violation) this.#fail(violation);
				})
				.finally(() => {
					stream.writer.close();
				});
		}
	}

	async #runBidi(stream: Stream) {
		const typ = await stream.reader.u53();

		// A type this version does not know, including one it predates, is an extension stream
		// opened without negotiating it: a plain error resets the stream and keeps the session.
		if (typ === StreamId.Session) {
			throw new ProtocolViolation("duplicate session stream");
		} else if (typ === StreamId.Announce) {
			const msg = await AnnounceRequest.decode(stream.reader, this.#version);
			await this.#publisher.runAnnounce(msg, stream);
		} else if (typ === StreamId.Subscribe) {
			// Owed from dispatch, so a close that lands mid-decode still waits for the request.
			await this.#publisher.owe(
				(async () => {
					const msg = await Subscribe.decode(stream.reader, this.#version);
					await this.#publisher.runSubscribe(msg, stream);
				})(),
			);
		} else if (typ === StreamId.Fetch) {
			if (!hasTrackStream(this.#version)) throw new Error("FETCH is not supported on this version");
			await this.#publisher.owe(
				(async () => {
					const msg = await Fetch.decode(stream.reader, this.#version);
					await this.#publisher.runFetch(msg, stream);
				})(),
			);
		} else if (typ === StreamId.Track) {
			if (!hasTrackStream(this.#version)) throw new Error("TRACK is not supported on this version");
			await this.#publisher.owe(
				(async () => {
					const msg = await TrackMessage.decode(stream.reader, this.#version);
					await this.#publisher.runTrackInfo(msg, stream);
				})(),
			);
		} else if (typ === StreamId.Tracks) {
			if (!hasTracks(this.#version)) throw new Error("TRACKS is not supported on this version");
			const msg = await TracksMessage.decode(stream.reader, this.#version);
			await this.#publisher.runTracks(msg, stream);
		} else if (typ === StreamId.Probe) {
			if (this.#version === Version.DRAFT_01 || this.#version === Version.DRAFT_02)
				throw new Error("PROBE is not supported on this version");
			await this.#publisher.runProbe(stream);
		} else if (typ === StreamId.Auth) {
			if (!hasAuth(this.#version)) throw new Error("AUTH is not supported on this version");
			await this.#auth.serve(stream);
		} else if (typ === StreamId.Goaway) {
			if (!hasGoaway(this.#version)) throw new Error("GOAWAY is not supported on this version");
			const msg = await Goaway.decode(stream.reader, this.#version);
			// A peer sends at most one; a second is a protocol violation.
			if (this.#goaway.peek() !== undefined) throw new ProtocolViolation("duplicate GOAWAY");
			if (!this.#client && msg.uri !== "") throw new ProtocolViolation("client GOAWAY URI must be empty");
			this.#goaway.set({ uri: msg.uri });
		} else {
			throw new Error(`unknown stream type: ${typ.toString()}`);
		}
	}

	async #runUnis() {
		const readers = new Readers(this.#quic, this.#version);

		for (;;) {
			const stream = await readers.next();
			if (!stream) break;

			const task = this.#runUni(stream)
				.then(() => {
					stream.stop(StreamCode.Cancel);
				})
				.catch((err: unknown) => {
					stream.stop(err);
					if (err instanceof ProtocolViolation) {
						closeViolation(this.#quic, err);
						this.#fail(err);
					}
				})
				.finally(() => this.#unis.delete(task));
			this.#unis.add(task);
		}
	}

	async #runUni(stream: Reader) {
		const typ = await stream.u53();
		if (typ === DataType.Group) {
			const msg = await Group.decode(stream, this.#version);
			await this.#subscriber.runGroup(msg, stream);
		} else if (typ === DataType.Padding && hasPadding(this.#version)) {
			await stream.discard();
		} else if (typ === DataType.Setup) {
			// Claim the stream before decoding, so two incomplete SETUPs are duplicates too.
			if (this.#setupSeen) throw new ProtocolViolation("duplicate SETUP");
			this.#setupSeen = true;
			// The peer sends exactly one SETUP, then FINs. Record it so capability-gated
			// streams (e.g. PROBE) can react.
			// The slot is claimed, so no other SETUP can arrive and the streams waiting on it
			// would hang. The session cannot continue.
			const setup = await Setup.decode(stream, this.#version).catch((err: unknown) => {
				throw new ProtocolViolation("invalid SETUP", { cause: err });
			});
			if (this.#client && hasClientParameters(setup))
				throw new ProtocolViolation("server SETUP contains Path, Role, or Token");
			if (!this.#client && setup.path !== undefined)
				throw new ProtocolViolation("Path on a URI-carrying binding");
			this.#peerSetup.set(setup);
			this.#peerRole.set(setup.role);
			this.#peerImplementation.resolve(setup.implementation);
		} else {
			throw new Error(`unknown stream type: ${typ.toString()}`);
		}
	}

	/** Snapshot the transport's counters; see {@link Established.stats}. */
	async stats(): Promise<Stats> {
		return transportStats(this.#quic);
	}

	/** Resolves when the session closes, decoding the peer's close code; see {@link Established.closed}. */
	get closed(): Promise<Error | null> {
		return this.#quic.closed.then(fromClose, (err: unknown) => error(err));
	}
}

/**
 * The probe level to advertise in SETUP, from what the transport can measure.
 *
 * `Report` claims we can measure and periodically report, so it is only truthful
 * when a metric this version can carry actually exists. The qmux/WebSocket fallback
 * implements no `getStats()` at all, and even a transport that has one may report
 * neither figure. Advertising `Report` and then holding the subscriber's PROBE
 * stream open with nothing to send is the state this avoids; the subscriber's own
 * gate then skips a stream it could not use.
 *
 * Mirrors `ProbeLevel::detect` in `moq-net`, including its limitation: a metric
 * that only appears after SETUP reads as unsupported here.
 *
 * @internal
 */
export async function probeLevel(quic: WebTransport, version: Version): Promise<ProbeLevel> {
	const getStats = (
		quic as unknown as {
			getStats?: () => Promise<{ estimatedSendRate: number | null; smoothedRtt?: number | null }>;
		}
	).getStats;
	if (typeof getStats !== "function") return ProbeLevel.None;

	// A transport that can't answer tells us nothing, which is itself an answer.
	let stats: { estimatedSendRate: number | null; smoothedRtt?: number | null };
	try {
		stats = await getStats.call(quic);
	} catch {
		return ProbeLevel.None;
	}

	const rtt = hasProbeRtt(version) ? stats.smoothedRtt : undefined;
	return stats.estimatedSendRate != null || rtt != null ? ProbeLevel.Report : ProbeLevel.None;
}

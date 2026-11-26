import { Duration } from "@moq/clock";
import { Effect, type GetPromise, type Getter, Once, Signal } from "@moq/signals";
import * as announce from "../announced.ts";
import type * as Auth from "../auth.ts";
import { AuthSession } from "../auth_session.ts";
import type { Goaway as Received } from "../connection/goaway.ts";
import type { Session } from "../connection/session.ts";
import { detect, type Probe, type Stats, statsOf, type TransportStats, transportStats } from "../connection/stats.ts";
import { type Transport, transportOf } from "../connection/transport.ts";
import { closeError, error, fromClose, ProtocolViolation, peerEnded, StreamCode, sessionCause } from "../error.ts";
import { type Cost, type Hop, randomHop } from "../hop.ts";
import { IMPLEMENTATION } from "../implementation.ts";
import { lines } from "../open.ts";
import type { Consumer as OriginConsumer } from "../origin.ts";
import * as Path from "../path.ts";
import { type Reader, Readers, Stream, Writer } from "../stream.ts";
import { Observed } from "../util/observed.ts";
import { withTimeout } from "../util/timeout.ts";
import { registerWire } from "../wire.ts";
import { type AnnounceBroadcast, AnnounceRequest, decodeAnnounceBroadcastMaybe } from "./announce.ts";
import { AnnounceStream } from "./announce_stream.ts";
import { LiteAuthWire } from "./auth.ts";
import { Datagrams } from "./datagram.ts";
import { closeViolation } from "./error.ts";
import { Fetch } from "./fetch.ts";
import { Goaway } from "./goaway.ts";
import { Group } from "./group.ts";
import { Publisher } from "./publisher.ts";
import { SessionInfo } from "./session.ts";
import { hasClientParameters, ProbeLevel, Setup } from "./setup.ts";
import { DataType, StreamId } from "./stream.ts";
import { Subscribe } from "./subscribe.ts";
import { Subscriber } from "./subscriber.ts";
import { Track as TrackMessage } from "./track.ts";
import { Tracks as TracksMessage } from "./tracks.ts";
import {
	hasAuth,
	hasDatagrams,
	hasDatagramTracks,
	hasFetch,
	hasGoaway,
	hasProbeRtt,
	hasRoutes,
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
	/** The implementation our SETUP advertises (lite-05+). Defaults to this library's; empty sends nothing. */
	implementation?: string;
	/** The PROBE target to send a peer that advertised `Increase`, in bits per second; zero sends none. */
	probeTarget?: Getter<number>;
	/** This link's price, declared in our SETUP and winning over the peer's (lite-06+). */
	cost?: Cost;
	/** This link's rank, which every route the peer sends carries at most. Defaults to 0. */
	priority?: number;
}

/** Where a session is: open, delivering what it owes before it closes, or closed. */
/** `leaving` is open but draining at this side's own request; see the wire view's `leave`. */
type Phase = { kind: "open" } | { kind: "leaving" } | { kind: "closing"; done: Promise<void> } | { kind: "closed" };

/**
 * Represents a connection to a MoQ server.
 *
 * @internal
 */
export class Connection implements Session {
	#phase: Phase = { kind: "open" };
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

	/** The peer's PROBE estimates; see {@link Session.probe}. */
	readonly probe: Getter<Probe>;

	// Our tokens and grants, and the answers to the peer's; see {@link Session.auth}.
	#auth: AuthSession;

	/** This side's Hop ID, shared by Publisher (for outbound hop chains) and Subscriber (to drop
	 * what crossed us). Random per connection, except a lite-07 publisher's, which is its origin's. */
	readonly hop: Hop;

	// The peer's SETUP, recorded once its Setup stream is read (lite-05+). Streams whose
	// encoding depends on a negotiated capability (e.g. PROBE) wait on this. undefined
	// until the peer's SETUP arrives; stays undefined forever on older drafts.
	#peerSetup = new Signal<Setup | undefined>(undefined);
	#setupSeen = false;
	// Lite-07: the peer opened its one Announce Stream.
	#announceSeen = false;
	// The transport closed, so a stream breaking says nothing about the peer.
	#transportClosed = false;

	// Written by the Subscriber as PROBE messages arrive.
	#probe = new Observed<Probe>({});

	// Whether we dialed; see {@link ConnectionProps.client}.
	#client: boolean;
	// See {@link ConnectionProps.token}.
	#token?: Uint8Array;
	// What our SETUP advertises; see {@link ConnectionProps.implementation}.
	#implementation: string;
	// See {@link ConnectionProps.probeTarget}.
	#probeTarget?: Getter<number>;
	// See {@link ConnectionProps.cost}.
	#cost?: Cost;
	#peerImplementation = Promise.withResolvers<string | undefined>();

	/** The implementation the peer advertised; see {@link Session.peerImplementation}. */
	readonly peerImplementation = this.#peerImplementation.promise;

	// The peer's GOAWAY. Lite carries no deadline, so only the URI is set.
	#goaway = new Once<Received>();

	// How the session ended: `fromClose` of the transport's close, settled after auth lets go of it.
	#closed = new Once<Error | null>();

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
		probeTarget,
		cost,
		priority = 0,
	}: ConnectionProps) {
		// A free link would leave a route as cheap as the one it came from, so lite-07 refuses one.
		if (cost === 0n && hasRoutes(version)) throw new RangeError("a moq-lite-07 link cannot cost 0");
		this.#cost = cost;
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
		detect(this, quic);

		this.probe = this.#probe;
		// Opens waiting for credit make room after about this RTT when the transport has none.
		lines(quic).probe = this.#probe;

		this.hop = randomHop();
		// What the peer's connection credential earns by default: publishing anything to us,
		// since we consume on demand, and subscribing to whatever we publish.
		this.#auth = new AuthSession({
			wire: hasAuth(version) ? new LiteAuthWire(quic, version) : undefined,
			peerGrant: {
				publish: Path.Prefixes.all(),
				subscribe: publish ? Path.Prefixes.all() : new Path.Prefixes(),
			},
			handle,
			draining: () => this.#goaway.peek() !== undefined || this.#phase.kind !== "open",
			// Without AUTH, the server checks a client's credential before its SETUP, which the client reads after going live.
			awaiting: client && hasSetupStream(version) && !hasAuth(version),
			// Lite-03/04 have neither, so the peer refuses the credential only by closing the
			// session. Lite-01/02 answered in the negotiation SETUP already read.
			silent: client && (version === Version.DRAFT_03 || version === Version.DRAFT_04),
		});
		// Left before anything observes the close, so a successor on the same handle never
		// finds this session still active.
		const leave = () => {
			this.#transportClosed = true;
			this.#auth.close();
		};
		void quic.closed.then(leave, leave);
		void quic.closed.then(
			(info) => this.#closed.set(fromClose(info)),
			(err: unknown) => this.#closed.set(error(err)),
		);
		// Lite-07: a send on this endpoint's Announce Stream failed while the session lives. A peer
		// that stopped it with no code of its own is leaving, as a server closing the session stops
		// it before its close arrives: the peer's routes end, and the session's close says why.
		// Otherwise what this endpoint said is lost and the session cannot go on, as rs/moq-net
		// closes it.
		if (hasRoutes(version)) {
			void AnnounceStream.of(quic, version).broken.then((err) => {
				if (this.#transportClosed || this.#phase.kind === "closed") return;
				if (peerEnded(err)) return this.#subscriber.announceFinished();
				this.#fail(error(err));
			});
		}
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
			{ cost, priority },
		);
		this.#client = client;
		this.#implementation = implementation;
		this.#probeTarget = probeTarget;
		// Only the lite-05+ Setup Stream carries it.
		if (!hasSetupStream(version)) this.#peerImplementation.resolve(undefined);
		registerWire(this, {
			consume: (path, instance) => this.#subscriber.consume(path, instance),
			exclusive: false,
			leave: () => this.#leave(),
			auth: this.#auth,
		});

		void this.#run();
	}

	#leave(): void {
		if (this.#phase.kind !== "open") return;
		this.#phase = { kind: "leaving" };
		if (!hasGoaway(this.#version)) return;
		const version = this.#version;
		const send = async () => {
			const stream = await Stream.open(this.#quic, { version });
			await stream.writer.u53(StreamId.Goaway);
			await new Goaway("").encode(stream.writer, version);
			stream.writer.close();
		};
		// A session closing first has nothing left to tell.
		send().catch((err: unknown) => console.debug("failed to send GOAWAY:", err));
	}

	/** Our tokens and grants; see {@link Session.auth}. */
	get auth(): Auth.Handle {
		return this.#auth.handle;
	}

	/**
	 * Withdraw announcements and finish the requests this session serves, waiting up to one
	 * second for delivery before closing.
	 *
	 * A served subscription first delivers its remaining group streams, and from lite-05 also
	 * waits for the subscriber's FIN, which says it read the tail. A live track never ends on
	 * its own, so it holds the close until the deadline.
	 */
	close(): Promise<void> {
		switch (this.#phase.kind) {
			case "open":
			case "leaving": {
				// Leaving finishes our Announce Stream, which asks the peer for nothing more.
				this.#subscriber.announceSilenced();
				const done = withTimeout(
					this.#publisher.drain(),
					Duration.millis(1000),
					"session close timed out",
				).finally(() => this.abort());
				this.#phase = { kind: "closing", done };
				return done;
			}
			case "closing":
				return this.#phase.done;
			case "closed":
				return Promise.resolve();
		}
	}

	/** End the session immediately without waiting for delivery. */
	abort(): void {
		if (this.#phase.kind === "closed") return;
		this.#phase = { kind: "closed" };
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
		const unis = this.#runUnis();
		const tasks: Promise<void>[] = [this.#runSession(), this.#runBidis(), unis];

		if (hasSetupStream(this.#version)) {
			tasks.push(this.#sendSetup());
		}

		const probes = new Effect((effect) => {
			if (!effect.get(this.#probe.wanted)) return;
			effect.spawn(() => this.#subscriber.runProbe(effect.abort, this.#probeTarget));
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
			// Accepting yields to the host, so take the streams still queued before draining.
			await unis.catch(() => {});
			await Promise.all([...this.#unis, this.#subscriber.drain()]);
			// The session ended under every track it was receiving, so they end with its
			// error: a clean close from either side reads as a cancelled session.
			this.#fail(err);
		}
	}

	/** The paths announced matching `filter`, each present or absent; see {@link Session.announced}. */
	announced(filter?: Path.Pattern, options?: announce.Options): announce.Consumer {
		return announce.list(this.#subscriber.routes(filter, options));
	}

	/** Every route change matching `filter`, for code that forwards routes. @internal */
	routes(filter?: Path.Pattern, options?: announce.RouteOptions): announce.Routes {
		return this.#subscriber.routes(filter, options);
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
	// called after this point, so there is nothing to narrow yet. Before lite-07 the hop
	// declares our session identity so the peer can filter reflected announcements
	// (lite-06 removed ANNOUNCE_REQUEST's exclude_hop for it). Lite-07 sends none: only a node
	// that forwards declares a Node ID.
	async #sendSetup(): Promise<void> {
		const writer = await Writer.open(this.#quic, { version: this.#version });
		try {
			await writer.u53(DataType.Setup);
			const probe = await probeLevel(this.#quic, this.#version);
			await new Setup({
				probe,
				cost: this.#cost,
				hop: this.hop,
				token: this.#token,
				implementation: this.#implementation,
			}).encode(writer, this.#version);
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
					stream.reader.stop(err);
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
		} else if (typ === StreamId.Announce && !hasRoutes(this.#version)) {
			// Lite-07 asks on its Announce Stream instead, and knows no such stream type.
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
		} else if (typ === StreamId.Datagrams) {
			if (!hasDatagramTracks(this.#version)) throw new Error("DATAGRAMS is not supported on this version");
			await this.#publisher.owe(
				(async () => {
					const msg = await Datagrams.decode(stream.reader, this.#version);
					await this.#publisher.runDatagrams(msg, stream);
				})(),
			);
		} else if (typ === StreamId.Fetch) {
			if (!hasFetch(this.#version)) throw new Error("FETCH is not supported on this version");
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
		} else if (typ === DataType.Announce && hasRoutes(this.#version)) {
			// Claim the stream before reading it, so two that say nothing yet are duplicates too.
			if (this.#announceSeen) throw new ProtocolViolation("duplicate Announce Stream");
			this.#announceSeen = true;
			await this.#runAnnounce(stream);
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
			this.#auth.accept();
			this.#peerImplementation.resolve(setup.implementation);
		} else {
			throw new Error(`unknown stream type: ${typ.toString()}`);
		}
	}

	// Lite-07: everything the peer says about discovery and routing, in order, until it leaves.
	// The peer leaves by finishing the stream, or resetting it with no code of its own.
	async #runAnnounce(stream: Reader) {
		for (;;) {
			let msg: AnnounceBroadcast | undefined;
			try {
				msg = await decodeAnnounceBroadcastMaybe(stream, this.#version);
			} catch (err: unknown) {
				// An ending session's own end says why. A peer that reset the stream with no code
				// of its own, even mid-message, is leaving: a server closing the session resets it
				// before its close arrives, with a code Chrome reports as an application's 0. Any
				// other reset, a message cut short, or one that does not decode lost what the peer
				// said.
				if (this.#transportClosed || this.#phase.kind === "closed") throw err;
				if (peerEnded(err)) break;
				throw new ProtocolViolation("Announce Stream broke", { cause: err });
			}
			if (!msg) break;
			// Nothing comes between the ANNOUNCE_REQUESTs of one edit.
			if (msg.status !== "request" && this.#publisher.continuing())
				throw new ProtocolViolation("a message inside an edit");
			await this.#subscriber.announce(msg);
			if (msg.status === "request") this.#publisher.request(msg.more, msg.remove, msg.add);
		}
		// The peer is leaving: its routes end, and what it asked for is still answered.
		this.#subscriber.announceFinished();
	}

	/** Snapshot the transport's counters; see {@link Session.stats}. */
	async stats(): Promise<Stats> {
		const stats = await transportStats(this.#quic);
		if (hasDatagrams(this.#version)) stats.datagramsUnrouted = this.#subscriber.datagramsUnrouted;
		return stats;
	}

	/** Settles with the peer's GOAWAY; see {@link Session.draining}. */
	get draining(): GetPromise<Received> {
		return this.#goaway;
	}

	/** Settles when the session closes, decoding the peer's close code; see {@link Session.closed}. */
	get closed(): GetPromise<Error | null> {
		return this.#closed;
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
	const getStats = statsOf(quic);
	if (!getStats) return ProbeLevel.None;

	// A transport that can't answer tells us nothing, which is itself an answer.
	let stats: TransportStats;
	try {
		stats = await getStats();
	} catch {
		return ProbeLevel.None;
	}

	const rtt = hasProbeRtt(version) ? stats.smoothedRtt : undefined;
	return stats.estimatedSendRate != null || rtt != null ? ProbeLevel.Report : ProbeLevel.None;
}

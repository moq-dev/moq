import { type Getter, Signal } from "@moq/signals";
import type * as announce from "../announced.ts";
import { type Bandwidth, createBandwidth } from "../bandwidth.ts";
import type * as broadcast from "../broadcast.ts";
import type { Established } from "../connection/established.ts";
import { type Transport, transportOf } from "../connection/transport.ts";
import * as Path from "../path.ts";
import { type Reader, Readers, Stream, Writer } from "../stream.ts";
import type * as Time from "../time.ts";
import { AnnounceRequest } from "./announce.ts";
import { Fetch } from "./fetch.ts";
import { Goaway } from "./goaway.ts";
import { Group } from "./group.ts";
import { type Origin, randomOrigin } from "./origin.ts";
import { Publisher } from "./publisher.ts";
import { SessionInfo } from "./session.ts";
import { ProbeLevel, type Role, Setup } from "./setup.ts";
import { DataType, StreamId } from "./stream.ts";
import { Subscribe } from "./subscribe.ts";
import { Subscriber } from "./subscriber.ts";
import { Track as TrackMessage } from "./track.ts";
import { hasDatagrams, hasSetupStream, Version, versionName } from "./version.ts";

const SEND_BW_POLL_INTERVAL = 100; // ms

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
	/** Whether the relay supports broadcast discovery. Defaults to true. */
	discovery?: boolean;
}

/**
 * Represents a connection to a MoQ server.
 *
 * @public
 */
export class Connection implements Established {
	// The URL of the connection.
	readonly url: URL;

	// The version of the connection as a human-readable string.
	readonly version: string;

	// The wire transport this session runs over.
	readonly transport: Transport;

	/** Whether the relay supports broadcast discovery; see {@link Established.discovery}. */
	readonly discovery: boolean;

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

	/** Estimated send bitrate from the congestion controller. */
	readonly sendBandwidth?: Bandwidth;

	/** Estimated receive bitrate from PROBE (moq-lite-03+ only). */
	readonly recvBandwidth?: Bandwidth;

	/** RTT in milliseconds from PROBE (moq-lite-04+ only). */
	readonly rtt?: Signal<Time.Milli | undefined>;

	/** Random per-connection origin id. Shared by Publisher (for outbound hop
	 * chains) and Subscriber (available for optional self-filtering on announces). */
	readonly origin: Origin;

	// The peer's SETUP, recorded once its Setup stream is read (lite-05+). Streams whose
	// encoding depends on a negotiated capability (e.g. PROBE) wait on this. undefined
	// until the peer's SETUP arrives; stays undefined forever on older drafts.
	#peerSetup = new Signal<Setup | undefined>(undefined);

	// Mirrors the role out of #peerSetup, so the public surface exposes the peer's declared
	// direction without handing out the whole SETUP (whose probe level gates our own streams).
	#peerRole = new Signal<Role | undefined>(undefined);

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
	constructor({ url, quic, version, session, discovery = true }: ConnectionProps) {
		this.url = url;
		this.#quic = quic;
		this.#session = session;
		this.version = versionName(version);
		this.#version = version;
		this.transport = transportOf(quic);
		this.discovery = discovery;

		// Send bandwidth is version-agnostic: depends on browser/QUIC support.
		const hasGetStats = typeof (quic as unknown as { getStats?: unknown }).getStats === "function";
		if (hasGetStats) {
			this.sendBandwidth = createBandwidth();
		}

		// Recv bandwidth requires PROBE support (Lite03+).
		if (version !== Version.DRAFT_01 && version !== Version.DRAFT_02) {
			this.recvBandwidth = createBandwidth();
		}

		// RTT can be populated by PROBE (Lite04+) or getStats() (when supported).
		// TODO prefer getStats() when both are available.
		this.rtt = new Signal<Time.Milli | undefined>(undefined);

		this.origin = randomOrigin();
		this.#publisher = new Publisher(this.#quic, this.#version, this.origin);
		this.#subscriber = new Subscriber(
			this.#quic,
			this.#version,
			this.origin,
			this.recvBandwidth,
			this.rtt,
			this.#peerSetup,
		);

		void this.#run();
	}

	/**
	 * Closes the connection.
	 */
	close() {
		this.#publisher.close();
		this.#subscriber.close();

		try {
			// TODO: For whatever reason, this try/catch doesn't seem to work..?
			this.#quic.close();
		} catch {
			// ignore
		}
	}

	async #run(): Promise<void> {
		const tasks: Promise<void>[] = [this.#runSession(), this.#runBidis(), this.#runUnis()];

		if (hasSetupStream(this.#version)) {
			tasks.push(this.#sendSetup());
		}

		if (this.sendBandwidth) {
			tasks.push(this.#runSendBandwidth(this.sendBandwidth));
		}

		if (this.recvBandwidth) {
			tasks.push(this.#subscriber.runProbe());
		}

		// Route incoming QUIC datagrams into their subscriptions (lite-05+; runDatagrams
		// no-ops on a transport that doesn't carry them).
		if (hasDatagrams(this.#version)) {
			tasks.push(this.#subscriber.runDatagrams());
		}

		try {
			await Promise.all(tasks);
		} catch (err) {
			console.error("fatal error running connection", err);
		} finally {
			this.close();
		}
	}

	publish(path: Path.Valid, producer: broadcast.Producer) {
		this.#publisher.publish(path, producer);
	}

	announced(prefix = Path.empty()): announce.Consumer {
		return this.#subscriber.announced(prefix);
	}

	consume(path: Path.Valid): broadcast.Consumer {
		return this.#subscriber.consume(path);
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
	// path and leave routing to the URL. We advertise probe = Report (we measure and
	// report bitrate over the PROBE stream, but don't actively pad the connection).
	// Role stays Both: publish/consume are called after this point, so there is nothing
	// to narrow yet.
	async #sendSetup(): Promise<void> {
		const writer = await Writer.open(this.#quic);
		try {
			await writer.u53(DataType.Setup);
			await new Setup({ probe: ProbeLevel.Report }).encode(writer, this.#version);
			writer.close();
		} catch (err: unknown) {
			writer.reset(err);
			throw err;
		}
	}

	async #runBidis() {
		for (;;) {
			const stream = await Stream.accept(this.#quic);
			if (!stream) break;

			this.#runBidi(stream)
				.catch((err: unknown) => {
					stream.writer.reset(err);
				})
				.finally(() => {
					stream.writer.close();
				});
		}
	}

	async #runBidi(stream: Stream) {
		const typ = await stream.reader.u53();

		if (typ === StreamId.Session) {
			throw new Error("duplicate session stream");
		} else if (typ === StreamId.Announce) {
			const msg = await AnnounceRequest.decode(stream.reader, this.#version);
			await this.#publisher.runAnnounce(msg, stream);
		} else if (typ === StreamId.Subscribe) {
			const msg = await Subscribe.decode(stream.reader, this.#version);
			await this.#publisher.runSubscribe(msg, stream);
		} else if (typ === StreamId.Fetch) {
			const msg = await Fetch.decode(stream.reader, this.#version);
			await this.#publisher.runFetch(msg, stream);
		} else if (typ === StreamId.Track) {
			const msg = await TrackMessage.decode(stream.reader, this.#version);
			await this.#publisher.runTrackInfo(msg, stream);
		} else if (typ === StreamId.Probe) {
			await this.#publisher.runProbe(stream);
		} else if (typ === StreamId.Goaway) {
			const msg = await Goaway.decode(stream.reader, this.#version);
			console.info("received goaway:", msg.uri);
		} else {
			throw new Error(`unknown stream type: ${typ.toString()}`);
		}
	}

	async #runUnis() {
		const readers = new Readers(this.#quic);

		for (;;) {
			const stream = await readers.next();
			if (!stream) break;

			this.#runUni(stream)
				.then(() => {
					stream.stop(new Error("cancel"));
				})
				.catch((err: unknown) => {
					stream.stop(err);
				});
		}
	}

	async #runUni(stream: Reader) {
		const typ = await stream.u53();
		if (typ === DataType.Group) {
			const msg = await Group.decode(stream);
			await this.#subscriber.runGroup(msg, stream);
		} else if (typ === DataType.Setup) {
			// The peer sends exactly one SETUP, then FINs. Record it so capability-gated
			// streams (e.g. PROBE) can react, then drain to the FIN.
			const setup = await Setup.decode(stream, this.#version);
			this.#peerSetup.set(setup);
			this.#peerRole.set(setup.role);
		} else {
			throw new Error(`unknown stream type: ${typ.toString()}`);
		}
	}

	async #runSendBandwidth(bandwidth: Bandwidth): Promise<void> {
		const quic = this.#quic as unknown as {
			getStats: () => Promise<{ estimatedSendRate: number | null }>;
		};

		return new Promise<void>((resolve) => {
			const id = setInterval(async () => {
				try {
					const stats = await quic.getStats();
					bandwidth.set(stats.estimatedSendRate ?? undefined);
				} catch {
					clearInterval(id);
					resolve();
				}
			}, SEND_BW_POLL_INTERVAL);

			void this.closed.then(() => {
				clearInterval(id);
				resolve();
			});
		});
	}

	get closed(): Promise<void> {
		return this.#quic.closed.then(() => undefined);
	}
}

import type { Announced } from "../announced.ts";
import { type Bandwidth, createBandwidth } from "../bandwidth.ts";
import type { Broadcast } from "../broadcast.ts";
import type { Established } from "../connection/established.ts";
import * as Path from "../path.ts";
import { type Reader, Readers, Stream } from "../stream.ts";
import { AnnounceInterest } from "./announce.ts";
import { Group } from "./group.ts";
import { Publisher } from "./publisher.ts";
import { SessionInfo } from "./session.ts";
import { StreamId } from "./stream.ts";
import { Subscribe } from "./subscribe.ts";
import { Subscriber } from "./subscriber.ts";
import { Version, versionName } from "./version.ts";

const SEND_BW_POLL_INTERVAL = 100; // ms

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

	// Just to avoid logging when `close()` is called.
	#closed = false;

	/** Estimated send bitrate from the congestion controller. */
	readonly sendBandwidth?: Bandwidth;

	/** Estimated receive bitrate from PROBE (moq-lite-03+ only). */
	readonly recvBandwidth?: Bandwidth;

	/**
	 * Creates a new Connection instance.
	 * @param url - The URL of the connection
	 * @param quic - The WebTransport session
	 * @param session - The session stream
	 *
	 * @internal
	 */
	constructor(url: URL, quic: WebTransport, version: Version, session?: Stream) {
		this.url = url;
		this.#quic = quic;
		this.#session = session;
		this.version = versionName(version);
		this.#version = version;

		// Set up bandwidth estimation for Lite03+.
		if (version === Version.DRAFT_03) {
			this.sendBandwidth = createBandwidth();
			this.recvBandwidth = createBandwidth();
		}

		this.#publisher = new Publisher(this.#quic, this.#version);
		this.#subscriber = new Subscriber(this.#quic, this.#version, this.recvBandwidth);

		this.#run();
	}

	/**
	 * Closes the connection.
	 */
	close() {
		if (this.#closed) return;

		this.#closed = true;
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
		const session = this.#runSession();
		const bidis = this.#runBidis();
		const unis = this.#runUnis();

		// Start polling send bandwidth if supported.
		if (this.sendBandwidth) {
			this.#runSendBandwidth(this.sendBandwidth);
		}

		try {
			await Promise.all([session, bidis, unis]);
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
	 * Gets the next announced broadcast.
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

	async #runSession() {
		if (!this.#session) {
			// moq-lite draft-03 doesn't use a session stream.
			return;
		}

		try {
			// Receive messages until the connection is closed.
			for (;;) {
				const msg = await SessionInfo.decodeMaybe(this.#session.reader, this.#version);
				if (!msg) break;
				// TODO use the session info
			}
		} finally {
			console.debug("session stream closed");
		}
	}

	async #runBidis() {
		for (;;) {
			const stream = await Stream.accept(this.#quic);
			if (!stream) {
				break;
			}

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
			const msg = await AnnounceInterest.decode(stream.reader);
			await this.#publisher.runAnnounce(msg, stream);
			return;
		} else if (typ === StreamId.Subscribe) {
			const msg = await Subscribe.decode(stream.reader, this.#version);
			await this.#publisher.runSubscribe(msg, stream);
			return;
		} else if (typ === StreamId.Probe) {
			await this.#publisher.runProbe(stream);
			return;
		} else {
			throw new Error(`unknown stream type: ${typ.toString()}`);
		}
	}

	async #runUnis() {
		const readers = new Readers(this.#quic);

		for (;;) {
			const stream = await readers.next();
			if (!stream) {
				break;
			}

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
		const typ = await stream.u8();
		if (typ === 0) {
			const msg = await Group.decode(stream);
			await this.#subscriber.runGroup(msg, stream);
		} else {
			throw new Error(`unknown stream type: ${typ.toString()}`);
		}
	}

	/**
	 * Polls the QUIC congestion controller for estimated send rate.
	 */
	#runSendBandwidth(bandwidth: Bandwidth) {
		// getStats is not yet in the TypeScript WebTransport type definitions.
		const quic = this.#quic as unknown as {
			getStats?: () => Promise<{ estimatedSendRate: number | null }>;
		};

		const getStats = quic.getStats?.bind(quic);
		if (!getStats) return;

		const run = async () => {
			while (!this.#closed) {
				await new Promise<void>((resolve) => setTimeout(resolve, SEND_BW_POLL_INTERVAL));
				if (this.#closed) break;

				try {
					const stats = await getStats();
					bandwidth.set(stats.estimatedSendRate ?? undefined);
				} catch {
					if (this.#closed) break;
				}
			}
		};

		void run();
	}

	/**
	 * Returns a promise that resolves when the connection is closed.
	 * @returns A promise that resolves when closed
	 */
	get closed(): Promise<void> {
		return this.#quic.closed.then(() => undefined);
	}
}

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

		// Send bandwidth is version-agnostic: depends on browser/QUIC support.
		const hasGetStats = typeof (quic as unknown as { getStats?: unknown }).getStats === "function";
		if (hasGetStats) {
			this.sendBandwidth = createBandwidth();
		}

		// Recv bandwidth requires PROBE support (not available in older drafts).
		if (version !== Version.DRAFT_01 && version !== Version.DRAFT_02) {
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

		if (this.sendBandwidth) {
			tasks.push(this.#runSendBandwidth(this.sendBandwidth));
		}

		if (this.recvBandwidth) {
			tasks.push(this.#subscriber.runProbe());
		}

		try {
			await Promise.all(tasks);
		} catch (err) {
			console.error("fatal error running connection", err);
		} finally {
			this.close();
		}
	}

	publish(path: Path.Valid, broadcast: Broadcast) {
		this.#publisher.publish(path, broadcast);
	}

	announced(prefix = Path.empty()): Announced {
		return this.#subscriber.announced(prefix);
	}

	consume(broadcast: Path.Valid): Broadcast {
		return this.#subscriber.consume(broadcast);
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
			const msg = await AnnounceInterest.decode(stream.reader);
			await this.#publisher.runAnnounce(msg, stream);
		} else if (typ === StreamId.Subscribe) {
			const msg = await Subscribe.decode(stream.reader, this.#version);
			await this.#publisher.runSubscribe(msg, stream);
		} else if (typ === StreamId.Probe) {
			await this.#publisher.runProbe(stream);
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
		const typ = await stream.u8();
		if (typ === 0) {
			const msg = await Group.decode(stream);
			await this.#subscriber.runGroup(msg, stream);
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

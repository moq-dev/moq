import type { Signal } from "@moq/signals";
import { Announced } from "../announced.ts";
import type { Bandwidth } from "../bandwidth.ts";
import { Broadcast, type TrackRequest } from "../broadcast.ts";
import { Group } from "../group.ts";
import * as Path from "../path.ts";
import { type Reader, Stream } from "../stream.ts";
import type * as Time from "../time.ts";
import type { Track } from "../track.ts";
import { error } from "../util/error.ts";
import { Announce, AnnounceInit, AnnounceInterest } from "./announce.ts";
import type { Group as GroupMessage } from "./group.ts";
import type { Origin } from "./origin.ts";
import { Probe } from "./probe.ts";
import { StreamId } from "./stream.ts";
import { decodeSubscribeResponse, Subscribe } from "./subscribe.ts";
import { Version } from "./version.ts";

/**
 * Options accepted by {@link Subscriber.announced}.
 */
export interface AnnouncedOptions {
	/**
	 * If true, skip announcements whose hop chain contains this connection's
	 * own origin id — useful for meshes that reflect announces back. Defaults
	 * to false for backwards compatibility: existing code (notably hang.live)
	 * relies on seeing its own publishes as the signal that a namespace
	 * published successfully.
	 */
	ignoreSelf?: boolean;
}

/**
 * Handles subscribing to broadcasts and managing their lifecycle.
 *
 * @internal
 */
export class Subscriber {
	#quic: WebTransport;

	// The version of the connection.
	readonly version: Version;

	// Shared with the Publisher so callers can optionally filter out their
	// own announcements on a per-call basis (see {@link AnnouncedOptions}).
	readonly origin: Origin;

	// Our subscribed tracks.
	#subscribes = new Map<bigint, Track>();
	#subscribeNext = 0n;

	// Recv bandwidth producer (Lite03+ only).
	#recvBandwidth?: Bandwidth;

	// RTT producer (Lite04+ only).
	#rtt?: Signal<Time.Milli | undefined>;

	/**
	 * Creates a new Subscriber instance.
	 * @param quic - The WebTransport session to use
	 * @param version - The protocol version
	 * @param origin - Origin id shared with the Publisher
	 * @param recvBandwidth - Optional bandwidth producer for PROBE
	 * @param rtt - Optional RTT signal for PROBE
	 *
	 * @internal
	 */
	constructor(
		quic: WebTransport,
		version: Version,
		origin: Origin,
		recvBandwidth?: Bandwidth,
		rtt?: Signal<Time.Milli | undefined>,
	) {
		this.#quic = quic;
		this.version = version;
		this.origin = origin;
		this.#recvBandwidth = recvBandwidth;
		this.#rtt = rtt;
	}

	/**
	 * Subscribe to broadcast announcements under `prefix`.
	 *
	 * Pass `{ ignoreSelf: true }` to skip announces that have already traversed
	 * this connection's {@link origin}.
	 */
	announced(prefix = Path.empty(), options: AnnouncedOptions = {}): Announced {
		const announced = new Announced();
		void this.#runAnnounced(announced, prefix, options);
		return announced;
	}

	async #runAnnounced(announced: Announced, prefix: Path.Valid, options: AnnouncedOptions): Promise<void> {
		console.debug(`announced: prefix=${prefix}`);
		const msg = new AnnounceInterest(prefix);

		try {
			// Open a stream and send the announce interest.
			const stream = await Stream.open(this.#quic);
			await stream.writer.u53(StreamId.Announce);
			await msg.encode(stream.writer, this.version);

			switch (this.version) {
				case Version.DRAFT_01:
				case Version.DRAFT_02: {
					// Receive ANNOUNCE_INIT first
					const init = await AnnounceInit.decode(stream.reader, this.version);

					// Process initial announcements
					for (const suffix of init.suffixes) {
						const path = Path.join(prefix, suffix);
						console.debug(`announced: broadcast=${path} active=true`);
						announced.append({ path, active: true });
					}
					break;
				}
				default:
					// Draft03+: no AnnounceInit, initial state comes via Announce messages.
					break;
			}

			// Receive announce updates (for Draft03, this includes initial state)
			for (;;) {
				const announce = await Promise.race([
					Announce.decodeMaybe(stream.reader, this.version),
					announced.closed,
				]);
				if (!announce) break;
				if (announce instanceof Error) throw announce;

				// Optionally drop reflected announces so callers asking for
				// "someone else's broadcasts" don't re-see their own publishes.
				if (options.ignoreSelf && announce.hops.includes(this.origin)) {
					continue;
				}

				const path = Path.join(prefix, announce.suffix);

				console.debug(`announced: broadcast=${path} active=${announce.active}`);
				announced.append({ path, active: announce.active });
			}

			announced.close();
		} catch (err: unknown) {
			announced.close(error(err));
		}
	}

	/**
	 * Consumes a broadcast from the connection.
	 *
	 * @param name - The name of the broadcast to consume
	 * @returns A Broadcast instance
	 */
	consume(path: Path.Valid): Broadcast {
		const broadcast = new Broadcast();

		(async () => {
			for (;;) {
				const request = await broadcast.requested();
				if (!request) break;
				this.#runSubscribe(path, request);
			}
		})();

		return broadcast;
	}

	async #runSubscribe(broadcast: Path.Valid, request: TrackRequest) {
		const id = this.#subscribeNext++;

		// Save the writer so we can append groups to it.
		this.#subscribes.set(id, request.track);

		console.debug(`subscribe start: id=${id} broadcast=${broadcast} track=${request.track.name}`);

		const msg = new Subscribe({ id, broadcast, track: request.track.name, priority: request.priority });

		const stream = await Stream.open(this.#quic);
		await stream.writer.u53(StreamId.Subscribe);
		await msg.encode(stream.writer, this.version);

		try {
			// The first response MUST be a SUBSCRIBE_OK.
			const resp = await decodeSubscribeResponse(stream.reader, this.version);
			if (!("ok" in resp)) {
				throw new Error("first subscribe response must be SUBSCRIBE_OK");
			}
			console.debug(`subscribe ok: id=${id} broadcast=${broadcast} track=${request.track.name}`);

			await Promise.race([stream.reader.closed, request.track.closed]);

			request.track.close();
			stream.close();
			console.debug(`subscribe close: id=${id} broadcast=${broadcast} track=${request.track.name}`);
		} catch (err) {
			const e = error(err);
			request.track.close(e);
			console.warn(
				`subscribe error: id=${id} broadcast=${broadcast} track=${request.track.name} error=${e.message}`,
			);
			stream.abort(e);
		} finally {
			this.#subscribes.delete(id);
		}
	}

	/**
	 * Handles a group message.
	 * @param group - The group message
	 * @param stream - The stream to read frames from
	 *
	 * @internal
	 */
	async runGroup(group: GroupMessage, stream: Reader) {
		const subscribe = this.#subscribes.get(group.subscribe);
		if (!subscribe) {
			if (group.subscribe >= this.#subscribeNext) {
				throw new Error(`unknown subscription: id=${group.subscribe}`);
			}

			return;
		}

		const producer = new Group(group.sequence);
		subscribe.writeGroup(producer);

		try {
			for (;;) {
				const done = await Promise.race([stream.done(), subscribe.closed, producer.closed]);
				if (done !== false) break;

				const size = await stream.u53();
				const payload = await stream.read(size);
				if (!payload) break;

				producer.writeFrame(payload);
			}

			producer.close();
			stream.stop(new Error("cancel"));
		} catch (err: unknown) {
			const e = error(err);
			producer.close(e);
			stream.stop(e);
		}
	}

	/**
	 * Opens a PROBE bidi stream to receive bandwidth estimates from the publisher.
	 * Returns immediately if recv bandwidth is not supported.
	 * Errors are fatal and propagate to the connection.
	 *
	 * @internal
	 */
	async runProbe(): Promise<void> {
		if (!this.#recvBandwidth) return;
		if (this.version === Version.DRAFT_01 || this.version === Version.DRAFT_02) return;

		const stream = await Stream.open(this.#quic);
		await stream.writer.u53(StreamId.Probe);

		for (;;) {
			const probe = await Probe.decodeMaybe(stream.reader, this.version);
			if (!probe) break;
			this.#recvBandwidth.set(probe.bitrate || undefined);
			if (this.#rtt && probe.rtt !== undefined) {
				this.#rtt.set(probe.rtt as Time.Milli);
			}
		}
	}

	close() {
		for (const track of this.#subscribes.values()) {
			track.close();
		}

		this.#subscribes.clear();
	}
}

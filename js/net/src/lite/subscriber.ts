import { Signal } from "@moq/signals";
import { Announced } from "../announced.ts";
import type { Bandwidth } from "../bandwidth.ts";
import { Broadcast, type TrackRequest } from "../broadcast.ts";
import { Compression, decompress } from "../compression.ts";
import { Group } from "../group.ts";
import * as Path from "../path.ts";
import { type Reader, Stream } from "../stream.ts";
import type * as Time from "../time.ts";
import type { Track } from "../track.ts";
import { error } from "../util/error.ts";
import { withTimeout } from "../util/timeout.ts";
import { Announce, AnnounceInit, AnnounceInterest } from "./announce.ts";
import type { Group as GroupMessage } from "./group.ts";
import type { Origin } from "./origin.ts";
import { Probe } from "./probe.ts";
import { StreamId } from "./stream.ts";
import { decodeSubscribeResponse, Subscribe, SubscribeUpdate } from "./subscribe.ts";
import { Version } from "./version.ts";

// Bound on how long stream-open plus SUBSCRIBE_OK may take. Browsers cap
// concurrent QUIC streams (Chrome ~100); past the cap createBidirectionalStream
// silently blocks. The timeout turns that into a clear error.
const SUBSCRIBE_OK_TIMEOUT_MS = 10_000;

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

interface SubscribeEntry {
	track: Track;
	// undefined until SUBSCRIBE_OK arrives and tells us the negotiated codec.
	compression: Signal<Compression | undefined>;
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

	// Our subscribed tracks. `compression` resolves once SUBSCRIBE_OK arrives;
	// group streams block on it before decoding any frame, since a group's QUIC
	// stream can race ahead of SUBSCRIBE_OK on its own stream.
	#subscribes = new Map<bigint, SubscribeEntry>();
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
		// Send our own session-level origin id so the peer can skip announces
		// whose hop chain already passed through us. Matches the Rust subscriber's
		// `exclude_hop: self.self_origin.id` in `run_announce_prefix`.
		const msg = new AnnounceInterest(prefix, this.origin);

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

		// Save the writer so we can append groups to it. `compression` stays
		// undefined until SUBSCRIBE_OK resolves it; runGroup blocks on it.
		const compression = new Signal<Compression | undefined>(undefined);
		this.#subscribes.set(id, { track: request.track, compression });

		console.debug(`subscribe start: id=${id} broadcast=${broadcast} track=${request.track.name}`);

		const msg = new Subscribe({ id, broadcast, track: request.track.name, priority: request.priority });

		// Open the stream and wait for SUBSCRIBE_OK under a timeout. The stream
		// handle flows back via `state` so the timeout path can abort it if it
		// finishes opening after the deadline.
		const state: { stream?: Stream } = {};
		const setup = this.#openSubscribe(state, msg);

		let stream: Stream;
		try {
			const ok = await withTimeout(
				setup,
				SUBSCRIBE_OK_TIMEOUT_MS,
				`subscribe timed out after ${SUBSCRIBE_OK_TIMEOUT_MS}ms waiting for SUBSCRIBE_OK (browser stream limit reached?)`,
			);
			stream = ok.stream;
			// Unblock any group streams waiting to learn how to decode frames.
			compression.set(ok.compression);
			console.debug(`subscribe ok: id=${id} broadcast=${broadcast} track=${request.track.name}`);
		} catch (err) {
			const e = error(err);
			request.track.close(e);
			this.#subscribes.delete(id);
			console.warn(
				`subscribe error: id=${id} broadcast=${broadcast} track=${request.track.name} error=${e.message}`,
			);
			// If the stream eventually opens after the timeout, abort it so we
			// don't leak it. Cover both branches: setup may resolve late, or it
			// may reject (e.g. encode/decode failure) after the stream is open.
			setup.then(
				() => state.stream?.abort(e),
				() => state.stream?.abort(e),
			);
			return;
		}

		try {
			// Watch for priority changes and send SUBSCRIBE_UPDATE. Lite01/Lite02
			// don't carry SUBSCRIBE_UPDATE on the wire, so skip the watcher there
			// and just wait on the stream/track like before.
			const waits: Promise<unknown>[] = [stream.reader.closed, request.track.closed];
			switch (this.version) {
				case Version.DRAFT_01:
				case Version.DRAFT_02:
					break;
				default:
					waits.push(this.#runPriorityUpdates(id, broadcast, request.track, msg, stream));
					break;
			}

			await Promise.race(waits);

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

	// Opens the subscribe stream, sends SUBSCRIBE, and reads SUBSCRIBE_OK.
	// `state.stream` is populated as soon as the stream opens so the caller
	// can clean it up on timeout even before this promise settles.
	async #openSubscribe(
		state: { stream?: Stream },
		msg: Subscribe,
	): Promise<{ stream: Stream; compression: Compression }> {
		state.stream = await Stream.open(this.#quic);
		await state.stream.writer.u53(StreamId.Subscribe);
		await msg.encode(state.stream.writer, this.version);

		// The first response MUST be a SUBSCRIBE_OK.
		const resp = await decodeSubscribeResponse(state.stream.reader, this.version);
		if (!("ok" in resp)) {
			throw new Error("first subscribe response must be SUBSCRIBE_OK");
		}
		return { stream: state.stream, compression: resp.ok.compression };
	}

	/**
	 * Send SUBSCRIBE_UPDATE messages whenever the track's priority signal changes.
	 *
	 * Resolves cleanly when the stream or track closes, so the caller can include
	 * this in Promise.race without leaving a dangling pending write that would
	 * become an unhandled rejection if the user calls updatePriority after close.
	 *
	 * Peeks the signal at the top of every iteration so that updates which landed
	 * before SubscribeOk arrived (or between iterations, before .next() registered
	 * its listener) aren't lost.
	 */
	async #runPriorityUpdates(
		id: bigint,
		broadcast: Path.Valid,
		track: Track,
		msg: Subscribe,
		stream: Stream,
	): Promise<void> {
		const stopped: Promise<null> = Promise.race([track.closed, stream.reader.closed]).then(() => null);
		let lastSent: number | undefined;

		for (;;) {
			const current = track.state.priority.peek();
			if (current === undefined || current === lastSent) {
				// Nothing new to send; wait for a change or termination.
				const next = await Promise.race([track.state.priority.next(), stopped]);
				if (next === null) return;
				continue;
			}

			// Round-trip the other Subscribe parameters so the publisher doesn't
			// interpret SUBSCRIBE_UPDATE as a reset of ordered/maxLatency/etc.
			const update = new SubscribeUpdate({
				priority: current,
				ordered: msg.ordered,
				maxLatency: msg.maxLatency,
				startGroup: msg.startGroup,
				endGroup: msg.endGroup,
			});
			await update.encode(stream.writer, this.version);
			lastSent = current;
			console.debug(`subscribe update: id=${id} broadcast=${broadcast} track=${track.name} priority=${current}`);
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
		const entry = this.#subscribes.get(group.subscribe);
		if (!entry) {
			if (group.subscribe >= this.#subscribeNext) {
				throw new Error(`unknown subscription: id=${group.subscribe}`);
			}

			return;
		}

		const { track, compression } = entry;
		const producer = new Group(group.sequence);
		track.writeGroup(producer);

		try {
			// Block until SUBSCRIBE_OK tells us the codec; the group's stream can
			// arrive before SUBSCRIBE_OK lands on the subscribe stream.
			let codec = compression.peek();
			while (codec === undefined) {
				if (track.state.closed.peek()) {
					// Subscription ended before SUBSCRIBE_OK; nothing to decode.
					producer.close();
					stream.stop(new Error("cancel"));
					return;
				}
				await Signal.race(compression, track.state.closed);
				codec = compression.peek();
			}

			for (;;) {
				const done = await Promise.race([stream.done(), track.closed, producer.closed]);
				if (done !== false) break;

				const size = await stream.u53();
				const payload = await stream.read(size);
				if (!payload) break;

				// On a compressed track the wire size is the compressed length;
				// inflate it back to the original frame the consumer sees.
				producer.writeFrame(codec === Compression.None ? payload : await decompress(codec, payload));
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
	 *
	 * Probe is best-effort telemetry: a stream-level failure (peer reset, FIN,
	 * missing peer support, transport hiccup) is caught and logged, never
	 * propagated to the connection. On exit the bandwidth/RTT signals are
	 * cleared so consumers see them as stale.
	 *
	 * @internal
	 */
	async runProbe(): Promise<void> {
		if (!this.#recvBandwidth) return;
		if (this.version === Version.DRAFT_01 || this.version === Version.DRAFT_02) return;

		// Probe is best-effort: any failure (stream reset by peer, missing peer support,
		// transport hiccup) MUST NOT tear down the connection. On error, drop the
		// bandwidth/RTT estimates so consumers know they're stale.
		try {
			const stream = await Stream.open(this.#quic);
			await stream.writer.u53(StreamId.Probe);

			for (;;) {
				const probe = await Probe.decodeMaybe(stream.reader, this.version);
				if (!probe) break;
				this.#recvBandwidth.set(probe.bitrate ?? undefined);
				if (this.#rtt && probe.rtt !== undefined) {
					this.#rtt.set(probe.rtt as Time.Milli);
				}
			}
		} catch (err: unknown) {
			console.warn("probe stream error", err);
		} finally {
			this.#recvBandwidth.set(undefined);
			this.#rtt?.set(undefined);
		}
	}

	close() {
		for (const { track } of this.#subscribes.values()) {
			track.close();
		}

		this.#subscribes.clear();
	}
}

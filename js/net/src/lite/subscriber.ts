import { Signal } from "@moq/signals";
import { Announced } from "../announced.ts";
import type { Bandwidth } from "../bandwidth.ts";
import { Broadcast, type TrackRequest } from "../broadcast.ts";
import { Group } from "../group.ts";
import * as Path from "../path.ts";
import { type Reader, Stream } from "../stream.ts";
import * as Time from "../time.ts";
import type { TrackProducer } from "../track.ts";
import { error } from "../util/error.ts";
import { withTimeout } from "../util/timeout.ts";
import { AnnounceBroadcast, AnnounceInit, AnnounceOk, AnnounceRequest } from "./announce.ts";
import type { Group as GroupMessage } from "./group.ts";
import type { Origin } from "./origin.ts";
import { Probe } from "./probe.ts";
import { ProbeLevel, type Setup } from "./setup.ts";
import { StreamId } from "./stream.ts";
import { decodeSubscribeResponse, decodeSubscribeResponseMaybe, Subscribe, SubscribeUpdate } from "./subscribe.ts";
import { TrackInfo, Track as TrackMessage } from "./track.ts";
import { Version } from "./version.ts";

// Bound on how long stream-open plus the first response (SUBSCRIBE_OK on older
// drafts, or TRACK_INFO on lite-05+) may take. Browsers cap concurrent QUIC
// streams (Chrome ~100); past the cap createBidirectionalStream silently blocks.
// The timeout turns that into a clear error.
const SUBSCRIBE_SETUP_TIMEOUT_MS = 10_000;

/** Decode an unsigned zigzag varint back to a signed delta (mirrors Rust `VarInt::to_zigzag`). */
function unzigzag(v: bigint): bigint {
	return (v >> 1n) ^ -(v & 1n);
}

// The TRACK stream and implicit SUBSCRIBE acceptance are lite-05+.
function supportsTrackStream(version: Version): boolean {
	switch (version) {
		case Version.DRAFT_01:
		case Version.DRAFT_02:
		case Version.DRAFT_03:
		case Version.DRAFT_04:
			return false;
		default:
			return true;
	}
}

/**
 * Options accepted by {@link Subscriber.announced}.
 */
export interface AnnouncedOptions {
	/**
	 * If true, skip announcements whose hop chain contains this connection's
	 * own origin id. Useful for meshes that reflect announces back. Defaults
	 * to false for backwards compatibility: existing code (notably hang.live)
	 * relies on seeing its own publishes as the signal that a namespace
	 * published successfully.
	 */
	ignoreSelf?: boolean;
}

interface SubscribeEntry {
	// The write side: incoming GROUP streams are routed here. The application reads
	// the matching TrackSubscriber it got from Broadcast.subscribe.
	track: TrackProducer;
	// Per-frame timestamp scale (0 = none). undefined until it's known (from TRACK_INFO
	// on lite-05+, or implicit defaults on older drafts). A non-zero value means each
	// frame on the group stream is prefixed with a zigzag-delta timestamp varint that
	// runGroup must consume to stay in sync; group streams block on it before decoding,
	// since a group's QUIC stream can race ahead of the subscribe stream.
	timescale: Signal<number | undefined>;
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

	// Our subscribed tracks. `timescale` resolves once known (from TRACK_INFO on
	// lite-05+, or implicit defaults on older drafts); group streams block on it
	// before decoding any frame, since a group's QUIC stream can race ahead.
	#subscribes = new Map<bigint, SubscribeEntry>();
	#subscribeNext = 0n;

	// Recv bandwidth producer (Lite03+ only).
	#recvBandwidth?: Bandwidth;

	// RTT producer (Lite04+ only).
	#rtt?: Signal<Time.Milli | undefined>;

	// The peer's SETUP (lite-05+), undefined until it arrives. Gates opening the PROBE
	// stream on the peer having advertised Probe >= Report.
	#peerSetup?: Signal<Setup | undefined>;

	/**
	 * Creates a new Subscriber instance.
	 * @param quic - The WebTransport session to use
	 * @param version - The protocol version
	 * @param origin - Origin id shared with the Publisher
	 * @param recvBandwidth - Optional bandwidth producer for PROBE
	 * @param rtt - Optional RTT signal for PROBE
	 * @param peerSetup - Optional peer SETUP slot for capability gating (lite-05+)
	 *
	 * @internal
	 */
	constructor(
		quic: WebTransport,
		version: Version,
		origin: Origin,
		recvBandwidth?: Bandwidth,
		rtt?: Signal<Time.Milli | undefined>,
		peerSetup?: Signal<Setup | undefined>,
	) {
		this.#quic = quic;
		this.version = version;
		this.origin = origin;
		this.#recvBandwidth = recvBandwidth;
		this.#rtt = rtt;
		this.#peerSetup = peerSetup;
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
		const msg = new AnnounceRequest(prefix, this.origin);

		try {
			// Open a stream and send the announce interest.
			const stream = await Stream.open(this.#quic);
			await stream.writer.u53(StreamId.Announce);
			await msg.encode(stream.writer, this.version);

			// Lite05+: the publisher reports its own origin id before any announces.
			// It no longer stamps itself onto each hop chain, so we append it here to
			// keep the ignoreSelf loop check seeing the full chain.
			let responderOrigin: Origin | undefined;
			if (this.version === Version.DRAFT_05_WIP) {
				const ok = await AnnounceOk.decode(stream.reader, this.version);
				responderOrigin = ok.origin;
			}

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
					AnnounceBroadcast.decodeMaybe(stream.reader, this.version),
					announced.closed,
				]);
				if (!announce) break;
				if (announce instanceof Error) throw announce;

				// Optionally drop reflected announces so callers asking for
				// "someone else's broadcasts" don't re-see their own publishes. In
				// Lite05 the sender's origin arrives via AnnounceOk, not in each hop
				// list, so fold it back in before checking.
				const hops = responderOrigin !== undefined ? [...announce.hops, responderOrigin] : announce.hops;
				if (options.ignoreSelf && hops.includes(this.origin)) {
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

		// Resolve TrackConsumer.info() via a TRACK stream (lite-05+). On older drafts
		// there's no TRACK stream, so info() rejects rather than fabricating defaults.
		broadcast.onTrackInfo(async (name) => {
			if (!supportsTrackStream(this.version)) {
				throw new Error("track info requires moq-lite-05 or newer");
			}
			const info = await this.#trackInfo(path, name);
			return {
				timescale: Time.Timescale(info.timescale),
				// Publisher Max Latency rides on the wire, so the local retention window
				// matches what the upstream advertises (relays re-serve with the same bound).
				cache: info.cache,
				priority: info.priority,
				ordered: info.ordered,
			};
		});

		void (async () => {
			for (;;) {
				const request = await broadcast.requested();
				if (!request) break;
				void this.#runSubscribe(path, request);
			}
		})();

		return broadcast;
	}

	async #runSubscribe(broadcast: Path.Valid, request: TrackRequest) {
		const id = this.#subscribeNext++;

		// `timescale` stays undefined until TRACK_INFO (or, on older drafts,
		// implicit defaults) resolves it; runGroup blocks on it before decoding.
		const timescale = new Signal<number | undefined>(undefined);

		console.debug(`subscribe start: id=${id} broadcast=${broadcast} track=${request.name}`);

		const msg = new Subscribe({ id, broadcast, track: request.name, priority: request.priority });

		// Open the stream under a timeout. The stream handle flows back via `state`
		// so the timeout path can abort it if it finishes opening after the deadline.
		const state: { stream?: Stream } = {};
		const setup = this.#openSubscribe(state, msg, request, id, timescale);

		let opened: { stream: Stream; producer: TrackProducer };
		try {
			opened = await withTimeout(
				setup,
				SUBSCRIBE_SETUP_TIMEOUT_MS,
				`subscribe timed out after ${SUBSCRIBE_SETUP_TIMEOUT_MS}ms waiting for the first response (browser stream limit reached?)`,
			);
			console.debug(`subscribe ok: id=${id} broadcast=${broadcast} track=${request.name}`);
		} catch (err) {
			const e = error(err);
			request.reject(e);
			this.#subscribes.delete(id);
			console.warn(`subscribe error: id=${id} broadcast=${broadcast} track=${request.name} error=${e.message}`);
			// If the stream eventually opens after the timeout, abort it so we
			// don't leak it. Cover both branches: setup may resolve late, or it
			// may reject (e.g. encode/decode failure) after the stream is open.
			setup.then(
				() => state.stream?.abort(e),
				() => state.stream?.abort(e),
			);
			return;
		}

		const { stream, producer } = opened;
		try {
			// Watch for priority changes and send SUBSCRIBE_UPDATE. Lite01/Lite02
			// don't carry SUBSCRIBE_UPDATE on the wire, so skip the watcher there
			// and just wait on the stream/track like before.
			//
			// On lite-05+ the publisher sends SUBSCRIBE_START/END/DROP on this stream;
			// drain them (we don't drive delivery off the resolved range) so the FIN is
			// observed. Older drafts just wait for the stream to close.
			const closed = supportsTrackStream(this.version) ? this.#drainResponses(stream) : stream.reader.closed;
			const waits: Promise<unknown>[] = [closed, producer.closed];
			switch (this.version) {
				case Version.DRAFT_01:
				case Version.DRAFT_02:
					break;
				default:
					waits.push(this.#runPriorityUpdates(id, broadcast, producer, msg, stream));
					break;
			}

			await Promise.race(waits);

			producer.close();
			stream.close();
			console.debug(`subscribe close: id=${id} broadcast=${broadcast} track=${request.name}`);
		} catch (err) {
			const e = error(err);
			producer.close(e);
			console.warn(`subscribe error: id=${id} broadcast=${broadcast} track=${request.name} error=${e.message}`);
			stream.abort(e);
		} finally {
			this.#subscribes.delete(id);
		}
	}

	// Determine the track's immutable properties, accept the request (so the
	// application's TrackSubscriber resolves and incoming groups have a producer to
	// write into), register it, then open the subscribe stream. `state.stream` is
	// populated as soon as the subscribe stream opens so the caller can clean it up
	// on timeout even before this promise settles.
	//
	// On lite-05+ the properties come from a TRACK stream opened first, and the
	// SUBSCRIBE is accepted implicitly (no SUBSCRIBE_OK). Older drafts carry no
	// per-track properties, so they resolve to defaults and just drain SUBSCRIBE_OK.
	async #openSubscribe(
		state: { stream?: Stream },
		msg: Subscribe,
		request: TrackRequest,
		id: bigint,
		timescale: Signal<number | undefined>,
	): Promise<{ stream: Stream; producer: TrackProducer }> {
		let producer: TrackProducer;
		let drainOk = false;

		if (supportsTrackStream(this.version)) {
			// Fetch the immutable properties once via the TRACK stream.
			const info = await this.#trackInfo(msg.broadcast, msg.track);
			producer = request.accept({
				timescale: Time.Timescale(info.timescale),
				// Publisher Max Latency rides on the wire, so the local retention window
				// matches what the upstream advertises (relays re-serve with the same bound).
				cache: info.cache,
				priority: info.priority,
				ordered: info.ordered,
			});
			timescale.set(info.timescale);
		} else {
			// Older drafts negotiate nothing per-track: verbatim frames, no timescale.
			producer = request.accept();
			timescale.set(0);
			drainOk = true;
		}

		// Register before opening SUBSCRIBE so a racing GROUP stream finds the entry.
		this.#subscribes.set(id, { track: producer, timescale });

		state.stream = await Stream.open(this.#quic);
		await state.stream.writer.u53(StreamId.Subscribe);
		await msg.encode(state.stream.writer, this.version);

		if (drainOk) {
			// The first response MUST be a SUBSCRIBE_OK (older drafts only).
			const resp = await decodeSubscribeResponse(state.stream.reader, this.version);
			if (!("ok" in resp)) {
				throw new Error("first subscribe response must be SUBSCRIBE_OK");
			}
		}

		return { stream: state.stream, producer };
	}

	// Opens a TRACK stream, reads the single TRACK_INFO, and FINs. Lite-05+ only.
	async #trackInfo(broadcast: Path.Valid, track: string): Promise<TrackInfo> {
		const stream = await Stream.open(this.#quic);
		try {
			await stream.writer.u53(StreamId.Track);
			await new TrackMessage(broadcast, track).encode(stream.writer, this.version);
			const info = await TrackInfo.decode(stream.reader, this.version);
			// The publisher FINs after TRACK_INFO; FIN our side too.
			stream.close();
			return info;
		} catch (err) {
			stream.abort(error(err));
			throw err;
		}
	}

	// Drains SUBSCRIBE_START/END/DROP on the subscribe stream until FIN (lite-05+).
	// The resolved range is informational here; the producer already orders groups.
	// Resolves (never rejects) on FIN or on the stream being reset out from under it,
	// so it's safe to drop from a Promise.race without an unhandled rejection.
	async #drainResponses(stream: Stream): Promise<void> {
		try {
			for (;;) {
				const resp = await decodeSubscribeResponseMaybe(stream.reader, this.version);
				if (!resp) return;
			}
		} catch {
			// Stream closed or reset; nothing more to drain.
		}
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
		track: TrackProducer,
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

		const { track, timescale } = entry;
		const producer = new Group(group.sequence);
		track.writeGroup(producer);

		try {
			// Block until the timescale is known; the group's stream can arrive before
			// TRACK_INFO (or implicit defaults) resolves it on the subscribe stream.
			let scale = timescale.peek();
			while (scale === undefined) {
				if (track.state.closed.peek()) {
					// Subscription ended before the scale resolved; nothing to decode.
					producer.close();
					stream.stop(new Error("cancel"));
					return;
				}
				await Signal.race(timescale, track.state.closed);
				scale = timescale.peek();
			}

			// A non-zero scale means every frame is prefixed with a zigzag-delta timestamp
			// (the lite-05 FRAME format), which we decode into a Timestamp at that scale.
			// Scale 0 (pre-lite-05) carries no timestamp, so we wall-clock-stamp.
			let prevTs = 0n;

			for (;;) {
				const done = await Promise.race([stream.done(), track.closed, producer.closed]);
				if (done !== false) break;

				let timestamp: Time.Timestamp;
				if (scale !== 0) {
					prevTs += unzigzag(await stream.u62());
					timestamp = new Time.Timestamp(Number(prevTs), Time.Timescale(scale));
				} else {
					timestamp = Time.Timestamp.now();
				}

				const size = await stream.u53();
				const payload = await stream.read(size);
				if (!payload) break;

				producer.writeFrame({ data: payload, timestamp });
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
	// Await the peer's advertised probe level, blocking until its SETUP arrives. The peer
	// MUST send exactly one SETUP, so this resolves once that stream is read.
	async #peerProbeLevel(peerSetup: Signal<Setup | undefined>): Promise<ProbeLevel> {
		let setup = peerSetup.peek();
		while (setup === undefined) {
			setup = await peerSetup.next();
		}
		return setup.probe;
	}

	async runProbe(): Promise<void> {
		if (!this.#recvBandwidth) return;
		if (this.version === Version.DRAFT_01 || this.version === Version.DRAFT_02) return;

		// Lite-05+ gates the PROBE stream on the peer advertising Probe >= Report in its
		// SETUP. Wait for the SETUP, then bail if the peer can't report bitrate. Older
		// drafts have no SETUP, so they keep probing unconditionally.
		if (this.#peerSetup) {
			const probe = await this.#peerProbeLevel(this.#peerSetup);
			if (probe < ProbeLevel.Report) return;
		}

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
					this.#rtt.set(Time.Milli(probe.rtt));
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

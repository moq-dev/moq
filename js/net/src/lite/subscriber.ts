import { Signal } from "@moq/signals";
import * as announce from "../announced.ts";
import type { Bandwidth } from "../bandwidth.ts";
import * as broadcast from "../broadcast.ts";
import { BroadcastCache } from "../consume.ts";
import * as netGroup from "../group.ts";
import * as Path from "../path.ts";
import { type Reader, Stream } from "../stream.ts";
import * as Time from "../time.ts";
import type * as track from "../track.ts";
import { error } from "../util/error.ts";
import { withTimeout } from "../util/timeout.ts";
import { AnnounceInit, AnnounceOk, AnnounceRequest, decodeAnnounceBroadcastMaybe } from "./announce.ts";
import { Datagram as DatagramMessage } from "./datagram.ts";
import * as DatagramStream from "./datagram_stream.ts";
import { Fetch as FetchMessage } from "./fetch.ts";
import type { Group as GroupMessage } from "./group.ts";
import type { Origin } from "./origin.ts";
import { Probe } from "./probe.ts";
import { ProbeLevel, type Setup } from "./setup.ts";
import { StreamId } from "./stream.ts";
import { decodeSubscribeResponse, decodeSubscribeResponseMaybe, Subscribe, SubscribeUpdate } from "./subscribe.ts";
import { TrackInfo, Track as TrackMessage } from "./track.ts";
import { hasAnnounceId, hasAnnounceOk, hasDatagrams, Version } from "./version.ts";

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
	// the matching track.Subscriber it got from broadcast.Consumer.subscribe.
	track: track.Producer;
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

	// Dedup consumed broadcasts per path: repeat consume() calls share one subscription.
	#consumes = new BroadcastCache();

	// Dedup in-flight one-shot fetches, keyed by [broadcast, track, sequence]. Concurrent (or
	// repeat, while still open) fetchGroup() calls for the same group share one FETCH stream and
	// each get an independent mirror; the entry is evicted once the group closes.
	#fetches = new Map<string, netGroup.Producer>();

	// Recv bandwidth producer (Lite03+ only).
	#recvBandwidth?: Bandwidth;

	// RTT producer (Lite04+ only).
	#rtt?: Signal<Time.Milli | undefined>;

	// The peer's SETUP (lite-05+), undefined until it arrives. Gates opening the PROBE
	// stream on the peer having advertised Probe >= Report.
	#peerSetup?: Signal<Setup | undefined>;

	// Distinguishes failures from streams torn down by Subscriber.close().
	#closed = new AbortController();
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
	announced(prefix = Path.empty(), options: AnnouncedOptions = {}): announce.Consumer {
		const announced = new announce.Producer(prefix);
		void this.#runAnnounced(announced, prefix, options);
		return announced.consume();
	}

	async #runAnnounced(announced: announce.Producer, prefix: Path.Valid, options: AnnouncedOptions): Promise<void> {
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
			if (hasAnnounceOk(this.version)) {
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
						announced.append({ path: suffix, active: true });
					}
					break;
				}
				default:
					// Draft03+: no AnnounceInit, initial state comes via Announce messages.
					break;
			}

			// Lite06+: announce ids. Each received `active` implicitly assigns the next
			// per-stream ordinal; `endedId`/`restart` reference it. Tracked even for
			// announces we skip via ignoreSelf, since the sender doesn't know we skipped.
			let nextAnnounceId = 0n;
			const announcedById = new Map<bigint, Path.Valid>();

			// Receive announce updates (for Draft03, this includes initial state)
			for (;;) {
				const announce = await Promise.race([
					decodeAnnounceBroadcastMaybe(stream.reader, this.version),
					announced.closed,
				]);
				// undefined: the stream ended. null: the consumer closed cleanly.
				if (!announce) break;
				if (announce instanceof Error) throw announce;

				let suffix: Path.Valid;
				let active: boolean;
				// Present on active/restart; ended messages never carry hops worth checking.
				let hops: Origin[] | undefined;

				switch (announce.status) {
					case "active":
						suffix = announce.suffix;
						active = true;
						hops = announce.hops;
						if (hasAnnounceId(this.version)) {
							announcedById.set(nextAnnounceId++, announce.suffix);
						}
						break;
					case "ended":
						suffix = announce.suffix;
						active = false;
						break;
					case "endedId": {
						// Resolve and retire the id; an unknown or retired id is a protocol violation.
						const path = announcedById.get(announce.id);
						if (path === undefined) throw new Error(`unknown announce id: ${announce.id}`);
						announcedById.delete(announce.id);
						suffix = path;
						active = false;
						break;
					}
					case "restart": {
						// Resolve the id; it stays live (the replacement reuses it).
						const path = announcedById.get(announce.id);
						if (path === undefined) throw new Error(`unknown announce id: ${announce.id}`);
						suffix = path;
						active = true;
						hops = announce.hops;
						break;
					}
				}

				// Optionally drop reflected announces so callers asking for
				// "someone else's broadcasts" don't re-see their own publishes. In
				// Lite05+ the sender's origin arrives via AnnounceOk, not in each hop
				// list, so fold it back in before checking.
				if (hops !== undefined && options.ignoreSelf) {
					const full = responderOrigin !== undefined ? [...hops, responderOrigin] : hops;
					if (full.includes(this.origin)) {
						continue;
					}
				}

				const path = Path.join(prefix, suffix);

				console.debug(`announced: broadcast=${path} active=${active}`);
				announced.append({ path: suffix, active });
			}

			announced.close();
		} catch (err: unknown) {
			announced.close(error(err));
		}
	}

	/**
	 * Consumes a broadcast from the connection.
	 *
	 * Deduplicated per path: repeat calls for the same still-live path share one reference-counted
	 * broadcast (and one upstream subscription). The shared broadcast closes once every caller has
	 * closed its handle, so callers close normally.
	 *
	 * @param name - The name of the broadcast to consume
	 * @returns A Broadcast instance
	 */
	consume(path: Path.Valid): broadcast.Consumer {
		return this.#consumes.get(path) ?? this.#consumes.insert(path, this.#createConsume(path));
	}

	#createConsume(path: Path.Valid): broadcast.Consumer {
		// A consumed broadcast resolves info() and fetchGroup() over the wire by reaching
		// back into this Subscriber (see ConsumeBroadcast below), rather than the wire
		// installing callbacks on the broadcast.
		const consumer = new ConsumeBroadcast(this, path);

		void (async () => {
			for (;;) {
				const request = await consumer.requested();
				if (!request) break;
				void this.#runSubscribe(path, request);
			}
		})();

		return consumer;
	}

	async #runSubscribe(broadcast: Path.Valid, request: track.Request) {
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

		let opened: { stream: Stream; producer: track.Producer };
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
			const waits: PromiseLike<unknown>[] = [closed, producer.closed];
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
	// application's track.Subscriber resolves and incoming groups have a producer to
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
		request: track.Request,
		id: bigint,
		timescale: Signal<number | undefined>,
	): Promise<{ stream: Stream; producer: track.Producer }> {
		let producer: track.Producer;
		let drainOk = false;

		if (supportsTrackStream(this.version)) {
			// Fetch the immutable properties once via the TRACK stream.
			const info = await this.#trackInfo(msg.broadcast, msg.track);
			producer = request.accept(this.#toModelInfo(info));
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

	// Map the wire TRACK_INFO onto the model track.Info a producer/consumer holds.
	#toModelInfo(info: TrackInfo): track.Info {
		return {
			timescale: Time.Timescale(info.timescale),
			// Publisher Max Latency rides on the wire, so the local retention window
			// matches what the upstream advertises (relays re-serve with the same bound).
			latencyMax: info.latencyMax,
			priority: info.priority,
			ordered: info.ordered,
		};
	}

	// Resolve a track's immutable model info via a TRACK stream (lite-05+), for the
	// ConsumeBroadcast backing track.Consumer.info(). On older drafts there's no TRACK
	// stream, so this rejects rather than fabricating defaults.
	async resolveTrackInfo(broadcast: Path.Valid, track: string): Promise<track.Info> {
		if (!supportsTrackStream(this.version)) {
			throw new Error("track info requires moq-lite-05 or newer");
		}
		return this.#toModelInfo(await this.#trackInfo(broadcast, track));
	}

	// Open a FETCH stream for one group and stream its bare frames into a group, for the
	// ConsumeBroadcast backing track.Consumer.fetchGroup() (lite-05+).
	fetchGroup(
		broadcast: Path.Valid,
		track: string,
		sequence: number,
		options: track.FetchGroupOptions = {},
	): Promise<netGroup.Consumer> {
		// Coalesce onto a still-open fetch of the same group so we don't open a second FETCH
		// stream (and re-download it); each caller reads an independent mirror.
		const key = JSON.stringify([broadcast, track, sequence]);
		const existing = this.#fetches.get(key);
		if (existing && !existing.isClosed) return Promise.resolve(existing.mirror());

		// Create and cache the group synchronously (before any await) so a concurrent fetch for
		// the same group finds it and coalesces rather than racing to open its own stream.
		const group = new netGroup.Producer(sequence);
		this.#fetches.set(key, group);
		void group.closed.then(() => {
			if (this.#fetches.get(key) === group) this.#fetches.delete(key);
		});

		return this.#runFetch(broadcast, track, sequence, options, group);
	}

	// Open the FETCH stream and pump the response into the shared group. Setup errors close the
	// group (so coalesced mirrors observe them and the entry evicts) and reject this caller.
	async #runFetch(
		broadcast: Path.Valid,
		track: string,
		sequence: number,
		options: track.FetchGroupOptions,
		group: netGroup.Producer,
	): Promise<netGroup.Consumer> {
		try {
			if (!supportsTrackStream(this.version)) {
				throw new Error("fetch group requires moq-lite-05 or newer");
			}

			const info = await this.#trackInfo(broadcast, track);
			const priority = options.priority ?? 0;
			const stream = await Stream.open(this.#quic, undefined, priority);

			try {
				await stream.writer.u53(StreamId.Fetch);
				await new FetchMessage(broadcast, track, priority, sequence).encode(stream.writer, this.version);
			} catch (err: unknown) {
				stream.abort(error(err));
				throw err;
			}

			void this.#runFetchResponse(stream, group, Time.Timescale(info.timescale));
			return group.mirror();
		} catch (err: unknown) {
			group.close(error(err));
			throw err;
		}
	}

	// Read the FETCH response (bare zigzag-delta-timestamped frames) into the group, then
	// FIN. A stream-level failure aborts the group so its reader observes the gap.
	async #runFetchResponse(stream: Stream, group: netGroup.Producer, timescale: Time.Timescale): Promise<void> {
		try {
			let prevTs = 0n;

			for (;;) {
				const done = await Promise.race([stream.reader.done(), group.closed]);
				if (done !== false) break;

				prevTs += unzigzag(await stream.reader.u62());
				const timestamp = new Time.Timestamp(Number(prevTs), timescale);
				const size = await stream.reader.u53();
				const payload = await stream.reader.read(size);
				if (!payload) break;
				group.writeFrame({ payload, timestamp });
			}

			group.close();
			stream.close();
		} catch (err: unknown) {
			const e = error(err);
			group.close(e);
			stream.abort(e);
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
		track: track.Producer,
		msg: Subscribe,
		stream: Stream,
	): Promise<void> {
		const stopped: Promise<null> = Promise.race([track.closed, stream.reader.closed]).then(() => null);
		let lastSent: number | undefined;

		for (;;) {
			const current = track.subscription.peek()?.priority;
			if (current === undefined || current === lastSent) {
				// Nothing new to send; wait for a change or termination.
				const next = await Promise.race([track.subscription.changed(), stopped]);
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
		const producer = new netGroup.Producer(group.sequence);
		track.writeGroup(producer);

		try {
			// Block until the timescale is known; the group's stream can arrive before
			// TRACK_INFO (or implicit defaults) resolves it on the subscribe stream.
			let scale = timescale.peek();
			while (scale === undefined) {
				if (track.closed.peek() !== undefined) {
					// Subscription ended before the scale resolved; nothing to decode.
					producer.close();
					stream.stop(new Error("cancel"));
					return;
				}
				await Signal.race(timescale, track.closed);
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

				producer.writeFrame({ payload, timestamp });
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
	 * Receives QUIC datagrams and routes each to its subscription's track producer (lite-05 §6.4).
	 *
	 * Returns immediately on a non-datagram transport or pre-lite-05 version. A decode error or an
	 * unknown subscribe id drops that datagram without tearing down the session (best-effort); the
	 * loop ends only when the datagram stream closes.
	 *
	 * @internal
	 */
	async runDatagrams(): Promise<void> {
		if (!hasDatagrams(this.version) || DatagramStream.maxDatagramSize(this.#quic) === 0) {
			return;
		}

		// Never reject: this loop is awaited alongside the connection's other tasks, so a
		// datagram-stream failure must not tear the whole session down (it's best-effort).
		const reader = DatagramStream.datagramReader(this.#quic);
		if (!reader) return;

		try {
			try {
				for (;;) {
					const { value, done } = await reader.read();
					if (done) break;
					if (!value) continue;

					try {
						await this.#routeDatagram(value);
					} catch (err: unknown) {
						console.debug(`dropping datagram: ${error(err).message}`);
					}
				}
			} finally {
				reader.releaseLock();
			}
		} catch (err: unknown) {
			const e = error(err);
			if (e.message === "The session is closed.") {
				console.debug(`datagram receive stopped: ${e.message}`);
			} else {
				console.warn("datagram stream error", err);
			}
		}
	}

	// Decode one datagram body and hand it to the matching subscription's producer. Drops the
	// datagram (best-effort) if the subscription is unknown/closed or its timescale isn't resolved.
	async #routeDatagram(payload: Uint8Array): Promise<void> {
		const dg = await DatagramMessage.decode(payload);

		const entry = this.#subscribes.get(dg.subscribe);
		if (!entry) return; // Unknown or already-closed subscription.

		// Datagrams are lite-05+, which always negotiates a timescale; if it hasn't resolved
		// yet (the datagram raced ahead of TRACK_INFO), drop rather than guess.
		const scale = entry.timescale.peek();
		if (!scale) return;

		const timestamp = new Time.Timestamp(dg.timestamp, Time.Timescale(scale));
		entry.track.writeDatagram({ sequence: dg.sequence, timestamp, payload: dg.payload });
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
			setup = await peerSetup.changed();
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
			if (!this.#closed.signal.aborted) {
				console.warn("probe stream error", err);
			}
		} finally {
			this.#recvBandwidth.set(undefined);
			this.#rtt?.set(undefined);
		}
	}

	close() {
		this.#closed.abort();

		for (const { track } of this.#subscribes.values()) {
			track.close();
		}

		this.#subscribes.clear();
	}
}

/**
 * A broadcast consumed from a lite session. It resolves `track.Consumer.info()` and
 * `.fetchGroup()` over the wire (lite-05+ TRACK / FETCH streams) by reaching into the
 * {@link Subscriber} it was opened from, the way the Rust `BroadcastConsumer` holds its
 * session. Live subscribes still flow through the inherited requested() queue.
 */
class ConsumeBroadcast extends broadcast.Consumer {
	#subscriber: Subscriber;
	#path: Path.Valid;

	constructor(subscriber: Subscriber, path: Path.Valid, state?: never) {
		super(state);
		this.#subscriber = subscriber;
		this.#path = path;
	}

	// Preserve the subclass (and its wire-backed info/fetchGroup) when the consume cache shares
	// this broadcast across callers.
	override clone(): ConsumeBroadcast {
		return new ConsumeBroadcast(this.#subscriber, this.#path, this.shareState());
	}

	override resolveTrackInfo(name: string): Promise<track.Info> {
		return this.#subscriber.resolveTrackInfo(this.#path, name);
	}

	override fetchGroup(name: string, sequence: number, options?: track.FetchGroupOptions): Promise<netGroup.Consumer> {
		return this.#subscriber.fetchGroup(this.#path, name, sequence, options);
	}
}

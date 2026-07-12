import { type Dispose, Signal } from "@moq/signals";
import type * as broadcast from "../broadcast.ts";
import type * as group from "../group.ts";
import * as Path from "../path.ts";
import { type Stream, Writer } from "../stream.ts";
import { Timescale } from "../time.ts";
import type * as track from "../track.ts";
import { error } from "../util/error.ts";
import { AnnounceBroadcast, AnnounceInit, AnnounceOk, type AnnounceRequest } from "./announce.ts";
import { Datagram as DatagramMessage } from "./datagram.ts";
import type { Fetch } from "./fetch.ts";
import { Group as GroupMessage } from "./group.ts";
import type { Origin } from "./origin.ts";
import { Probe } from "./probe.ts";
import {
	encodeSubscribeResponse,
	type Subscribe,
	SubscribeEnd,
	SubscribeOk,
	SubscribeStart,
	SubscribeUpdate,
} from "./subscribe.ts";
import { TrackInfo as TrackInfoMessage, type Track as TrackMessage } from "./track.ts";
import { hasAnnounceOk, hasDatagrams, Version } from "./version.ts";

const PROBE_INTERVAL = 100; // ms
const PROBE_MAX_AGE = 10_000; // ms
const PROBE_MAX_DELTA = 0.25;

/** Map a signed delta to an unsigned zigzag varint value (mirrors Rust `VarInt::from_zigzag`). */
function zigzag(delta: bigint): bigint {
	return delta >= 0n ? delta << 1n : (-delta << 1n) - 1n;
}

// The TRACK stream, implicit SUBSCRIBE acceptance, and SUBSCRIBE_START/END are
// all lite-05+.
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
 * Handles publishing broadcasts and managing their lifecycle.
 *
 * @internal
 */
export class Publisher {
	// The version of the connection.
	readonly version: Version;

	// Per-connection origin appended to outbound Announce hops, so the peer
	// can detect loops and prefer shorter paths. Created by Connection and
	// shared with Subscriber, which can optionally use it to filter out its
	// own announcements.
	readonly origin: Origin;

	#quic: WebTransport;

	// The one writer for the outbound datagram stream (getWriter locks it), acquired once at
	// construction when this version + transport carry datagrams, released in close(). Its
	// presence is the gate: undefined means datagrams aren't served on this connection. All
	// subscriptions share it, since a second getWriter on the same stream would throw.
	#datagramWriter?: WritableStreamDefaultWriter<Uint8Array>;

	// Our published broadcasts.
	// It's a signal so we can live update any announce streams.
	#broadcasts = new Signal<Map<Path.Valid, broadcast.Producer> | undefined>(new Map());

	// TRACK_INFO is immutable per track, so resolve it from the application once
	// (via a throwaway subscribe whose info() resolves when the app calls accept)
	// and reuse it for every later TRACK request of the same track. Keyed by
	// `broadcast\0track`. A rejected lookup is evicted so a retry can re-probe.
	#trackInfo = new Map<string, Promise<TrackInfoMessage>>();

	/**
	 * Creates a new Publisher instance.
	 * @param quic - The WebTransport session to use
	 * @param version - Negotiated protocol version
	 * @param origin - Origin id shared with the Subscriber
	 *
	 * @internal
	 */
	constructor(quic: WebTransport, version: Version, origin: Origin) {
		this.#quic = quic;
		this.version = version;
		this.origin = origin;

		// Grab the datagram writer up front when the transport carries datagrams (no group
		// fallback, so it stays undefined otherwise). One writer for all subscriptions.
		if (hasDatagrams(version) && quic.datagrams.maxDatagramSize > 0) {
			this.#datagramWriter = quic.datagrams.writable.getWriter();
		}
	}

	/**
	 * Publishes a broadcast with any associated tracks.
	 * @param name - The broadcast to publish
	 */
	publish(path: Path.Valid, broadcast: broadcast.Producer) {
		this.#broadcasts.mutate((broadcasts) => {
			if (!broadcasts) throw new Error("closed");
			broadcasts.set(path, broadcast);
		});

		// Remove the broadcast from the lookup when it's closed.
		void broadcast.closed.finally(() => {
			this.#broadcasts.mutate((broadcasts) => {
				broadcasts?.delete(path);
			});
		});
	}

	/**
	 * Handles an announce interest message.
	 * @param msg - The announce interest message
	 * @param stream - The stream to write announcements to
	 *
	 * @internal
	 */
	async runAnnounce(msg: AnnounceRequest, stream: Stream) {
		console.debug(`announce: prefix=${msg.prefix}`);

		// Send initial announcements
		let active = new Set<Path.Valid>();

		const broadcasts = this.#broadcasts.peek();
		if (!broadcasts) return; // closed

		for (const name of broadcasts.keys()) {
			const suffix = Path.stripPrefix(msg.prefix, name);
			if (suffix === null) continue;
			console.debug(`announce: broadcast=${name} active=true`);
			active.add(suffix);
		}

		switch (this.version) {
			case Version.DRAFT_01:
			case Version.DRAFT_02: {
				const init = new AnnounceInit([...active]);
				await init.encode(stream.writer, this.version);
				break;
			}
			default: {
				if (!hasAnnounceOk(this.version)) {
					// Draft03/04: send individual Announce messages, stamping our origin as a hop.
					for (const suffix of active) {
						const wire = new AnnounceBroadcast({ suffix, active: true, hops: [this.origin] });
						await wire.encode(stream.writer, this.version);
					}
					break;
				}

				// Report our origin id once via AnnounceOk and the count of initial announces
				// that follow; the subscriber stamps our origin onto each hop chain, so we omit it.
				const ok = new AnnounceOk(this.origin, active.size);
				await ok.encode(stream.writer, this.version);
				for (const suffix of active) {
					const wire = new AnnounceBroadcast({ suffix, active: true });
					await wire.encode(stream.writer, this.version);
				}
				break;
			}
		}

		// Wait for updates to the broadcasts.
		for (;;) {
			// TODO Make a better helper within Signals.
			let dispose!: Dispose;
			const changed = new Promise<Map<Path.Valid, broadcast.Producer> | undefined>((resolve) => {
				dispose = this.#broadcasts.changed(resolve);
			});

			// Wait until the map of broadcasts changes.
			const broadcasts = await Promise.race([changed, stream.reader.closed]);
			dispose();
			if (!broadcasts) break;

			// Create a new set of active broadcasts.
			// This is SLOW, but it's not worth optimizing because we often have just 1 broadcast anyway.
			const newActive = new Set<Path.Valid>();
			for (const name of broadcasts.keys()) {
				const suffix = Path.stripPrefix(msg.prefix, name);
				if (suffix === null) continue; // Not our prefix.
				newActive.add(suffix);
			}

			// Announce any new broadcasts. Lite05+ reports our origin once via AnnounceOk, so
			// the subscriber stamps it onto each hop chain; older versions stamp it here.
			for (const added of newActive.difference(active)) {
				console.debug(`announce: broadcast=${added} active=true`);
				const hops = hasAnnounceOk(this.version) ? [] : [this.origin];
				const wire = new AnnounceBroadcast({ suffix: added, active: true, hops });
				await wire.encode(stream.writer, this.version);
			}

			// Announce any removed broadcasts.
			// Ended announces don't need hops. The peer matches on path only.
			for (const removed of active.difference(newActive)) {
				console.debug(`announce: broadcast=${removed} active=false`);
				const wire = new AnnounceBroadcast({ suffix: removed, active: false });
				await wire.encode(stream.writer, this.version);
			}

			// NOTE: This is kind of a hack that won't work with a rapid UNANNOUNCE/ANNOUNCE cycle.
			// However, our client doesn't do that anyway.

			active = newActive;
		}
	}

	/**
	 * Handles a subscribe message.
	 * @param msg - The subscribe message
	 * @param stream - The stream to write track data to
	 *
	 * @internal
	 */
	async runSubscribe(msg: Subscribe, stream: Stream) {
		const broadcast = this.#broadcasts.peek()?.get(msg.broadcast);
		if (!broadcast) {
			console.debug(`publish unknown: broadcast=${msg.broadcast}`);
			stream.writer.reset(new Error("not found"));
			return;
		}

		const track = broadcast.subscribe(msg.track, { priority: msg.priority });

		// The best-effort datagram loop, started once serving begins. It parks when the
		// track finishes (recvDatagram returns undefined), so #runTrack alone ends the
		// subscription; awaited during teardown so it doesn't outlive the subscription.
		let datagrams: Promise<void> | undefined;

		try {
			let timescale: Timescale = Timescale.MILLI;

			if (supportsTrackStream(this.version)) {
				// Lite-05+ accepts implicitly: no SUBSCRIBE_OK (the immutable
				// properties live in TRACK_INFO), and the resolved range arrives as
				// SUBSCRIBE_START / SUBSCRIBE_END emitted from #runTrack.
				//
				// The timescale is an immutable property, so serving MUST use exactly
				// what TRACK_INFO advertised. It comes from the producer's accept(), so
				// they always agree. Awaiting info() also surfaces a rejected track
				// (accept never called, track closed) as an error here, which resets the
				// stream.
				const info = await track.info();
				timescale = info.timescale;
			} else {
				// Older drafts acknowledge with SUBSCRIBE_OK and stream frames verbatim.
				const ok = new SubscribeOk({ priority: msg.priority });
				await encodeSubscribeResponse(stream.writer, { ok }, this.version);
			}

			console.debug(`publish ok: broadcast=${msg.broadcast} track=${track.name}`);

			const serving = this.#runTrack(msg.id, msg.broadcast, track, stream.writer, timescale);

			// Serve datagrams concurrently with groups whenever the transport carries them
			// (the writer exists iff so). No group fallback: otherwise they simply aren't sent.
			if (this.#datagramWriter) {
				datagrams = this.#runDatagrams(msg.id, track, timescale);
			}

			for (;;) {
				const decode = SubscribeUpdate.decodeMaybe(stream.reader, this.version);

				const result = await Promise.any([serving, decode]);
				if (!result) break;

				if (result instanceof SubscribeUpdate) {
					console.debug(
						`subscribe update: broadcast=${msg.broadcast} track=${track.name} priority=${result.priority}`,
					);
					track.updatePriority(result.priority);
				}
			}

			console.debug(`publish done: broadcast=${msg.broadcast} track=${track.name}`);
			stream.close();
			track.close();
			// track.close ends the datagram loop; wait so it doesn't leak past teardown.
			await datagrams;
		} catch (err: unknown) {
			const e = error(err);
			console.warn(`publish error: broadcast=${msg.broadcast} track=${track.name} error=${e.message}`);
			track.close(e);
			stream.abort(e);
			await datagrams;
		}
	}

	/**
	 * Handles a FETCH stream by serving one group as bare frame records (lite-05+).
	 *
	 * @internal
	 */
	async runFetch(msg: Fetch, stream: Stream) {
		if (!supportsTrackStream(this.version)) {
			stream.writer.reset(new Error("fetch requires moq-lite-05 or newer"));
			return;
		}

		const broadcast = this.#broadcasts.peek()?.get(msg.broadcast);
		if (!broadcast) {
			console.debug(`fetch unknown: broadcast=${msg.broadcast}`);
			stream.writer.reset(new Error("not found"));
			return;
		}

		let group: group.Consumer | undefined;
		try {
			// The timescale is immutable, so serve exactly what TRACK_INFO advertised.
			const info = await this.#resolveTrackInfo(msg.broadcast, msg.track);
			group = await broadcast.track(msg.track).fetchGroup(msg.group, { priority: msg.priority });
			await this.#runFetchGroup(group, stream.writer, Timescale(info.timescale));
			console.debug(`fetch done: broadcast=${msg.broadcast} track=${msg.track} group=${msg.group}`);
			stream.close();
			group.close();
		} catch (err: unknown) {
			const e = error(err);
			console.warn(
				`fetch error: broadcast=${msg.broadcast} track=${msg.track} group=${msg.group} error=${e.message}`,
			);
			group?.close(e);
			stream.abort(e);
		}
	}

	/**
	 * Runs a track and sends its data to the stream.
	 * @param sub - The subscription ID
	 * @param broadcast - The broadcast name
	 * @param track - The track to run
	 * @param stream - The stream to write to
	 *
	 * @internal
	 */
	async #runTrack(sub: bigint, broadcast: Path.Valid, track: track.Subscriber, stream: Writer, timescale: Timescale) {
		// Lite-05+ resolves the range on the subscribe stream: SUBSCRIBE_START once the
		// first group is known, SUBSCRIBE_END when the track finishes.
		const emitRange = supportsTrackStream(this.version);
		let startSent = false;
		let lastSequence = 0;

		try {
			for (;;) {
				const next = track.recvGroup();
				const group = await Promise.race([next, stream.closed]);
				if (!group) {
					next.then((group) => group?.close()).catch(() => {});
					break;
				}

				if (emitRange && !startSent) {
					startSent = true;
					await encodeSubscribeResponse(stream, { start: new SubscribeStart(group.sequence) }, this.version);
				}
				lastSequence = group.sequence;

				void this.#runGroup(sub, group, timescale);
			}

			if (emitRange) {
				await encodeSubscribeResponse(stream, { end: new SubscribeEnd(lastSequence) }, this.version);
			}

			console.debug(`publish close: broadcast=${broadcast} track=${track.name}`);
			track.close();
			stream.close();
		} catch (err: unknown) {
			const e = error(err);
			console.warn(`publish error: broadcast=${broadcast} track=${track.name} error=${e.message}`);
			track.close(e);
			stream.reset(e);
		}
	}

	/**
	 * Answers a TRACK stream (0x6) with a single TRACK_INFO, then FINs.
	 *
	 * @internal
	 */
	async runTrackInfo(msg: TrackMessage, stream: Stream) {
		try {
			const info = await this.#resolveTrackInfo(msg.broadcast, msg.track);
			await info.encode(stream.writer, this.version);
			console.debug(`track info: broadcast=${msg.broadcast} track=${msg.track}`);
			stream.close();
		} catch (err) {
			console.debug(`track unknown: broadcast=${msg.broadcast} track=${msg.track}`);
			stream.writer.reset(error(err));
		}
	}

	// Resolve (and cache) a track's immutable TRACK_INFO by asking the application.
	// `broadcast.track(name).info()` triggers a TrackRequest the app answers with
	// accept(TrackInfo); only the immutable properties are needed (not the groups).
	// Cached because they're fixed for the track's lifetime. Rejects if the broadcast
	// or track is unavailable.
	#resolveTrackInfo(broadcast: Path.Valid, track: string): Promise<TrackInfoMessage> {
		const key = `${broadcast}\0${track}`;
		const cached = this.#trackInfo.get(key);
		if (cached) return cached;

		const pending = (async () => {
			const published = this.#broadcasts.peek()?.get(broadcast);
			if (!published) throw new Error("not found");

			const info = await published.track(track).info();
			return new TrackInfoMessage({
				priority: info.priority,
				ordered: info.ordered,
				// Publisher Max Latency: the publisher's retention bound, advertised so
				// relays re-serve with the same window.
				cache: info.cache,
				// Lite05 mandates per-frame timestamps. Advertise the track's timescale;
				// `#runGroup` emits each frame converted to it.
				timescale: info.timescale,
			});
		})();

		// Don't poison the cache on failure: a later request may succeed.
		pending.catch(() => this.#trackInfo.delete(key));
		this.#trackInfo.set(key, pending);
		return pending;
	}

	/**
	 * Forwards a track's datagrams best-effort over QUIC datagrams (lite-05 §6.4), parallel to
	 * its groups. Each datagram is dropped (there is no group fallback) if the encoded body
	 * doesn't fit the transport's datagram limit or the send fails. Returns once the track
	 * finishes; a failure never tears down the subscription.
	 *
	 * @internal
	 */
	async #runDatagrams(sub: bigint, track: track.Subscriber, timescale: Timescale) {
		const writer = this.#datagramWriter;
		if (!writer) return; // Only reached with a writer (see the #datagramWriter gate).
		const maxSize = this.#quic.datagrams.maxDatagramSize;

		try {
			for (;;) {
				const datagram = await track.recvDatagram();
				if (!datagram) return; // Track finished; #runTrack tears the subscription down.

				// Convert the timestamp to the track's advertised timescale, matching #runGroup.
				const ts = Math.round(datagram.timestamp.as(timescale));
				const body = new DatagramMessage(sub, datagram.sequence, ts, datagram.payload).encode();

				// No group fallback: drop anything that doesn't fit a single datagram.
				if (body.byteLength > maxSize) continue;

				await writer.ready;
				await writer.write(body);
			}
		} catch (err: unknown) {
			// Best-effort: a datagram send failure stops sending but never fails the subscription.
			console.debug(`datagram send stopped: sub=${sub} error=${error(err).message}`);
		}
	}

	/**
	 * Runs a group and sends its frames to the stream.
	 * @param sub - The subscription ID
	 * @param group - The group to run
	 *
	 * @internal
	 */
	// Serialize a fetched group's frames onto the FETCH stream as bare records: each a
	// zigzag-delta timestamp (at the track's advertised timescale) followed by size + bytes.
	async #runFetchGroup(group: group.Consumer, stream: Writer, timescale: Timescale) {
		let prevTs = 0n;
		for (;;) {
			const frame = await Promise.race([group.readFrame(), stream.closed]);
			if (!frame) break;

			const ts = BigInt(Math.round(frame.timestamp.as(timescale)));
			await stream.u62(zigzag(ts - prevTs));
			prevTs = ts;

			await stream.u53(frame.data.byteLength);
			await stream.write(frame.data);
		}
	}

	async #runGroup(sub: bigint, group: group.Consumer, timescale: Timescale) {
		const msg = new GroupMessage(sub, group.sequence);
		try {
			const stream = await Writer.open(this.#quic);
			await stream.u53(0); // stream type
			await msg.encode(stream);

			// Lite05+ prefixes every frame with a zigzag-delta timestamp at the track's
			// advertised timescale; older drafts omit it.
			const timestamps = supportsTrackStream(this.version);
			let prevTs = 0n;

			try {
				for (;;) {
					const frame = await Promise.race([group.readFrame(), stream.closed]);
					if (!frame) break;

					if (timestamps) {
						// Convert each frame to the track's advertised timescale.
						const ts = BigInt(Math.round(frame.timestamp.as(timescale)));
						await stream.u62(zigzag(ts - prevTs));
						prevTs = ts;
					}

					await stream.u53(frame.data.byteLength);
					await stream.write(frame.data);
				}

				stream.close();
				group.close();
			} catch (err: unknown) {
				const e = error(err);
				stream.reset(e);
				group.close(e);
			}
		} catch (err: unknown) {
			const e = error(err);
			group.close(e);
		}
	}

	/**
	 * Handles a probe stream by periodically reporting estimated bitrate.
	 * @param stream - The probe bidi stream
	 *
	 * @internal
	 */
	async runProbe(stream: Stream) {
		// getStats is not yet in the TypeScript WebTransport type definitions.
		const quic = this.#quic as unknown as {
			getStats?: () => Promise<{ estimatedSendRate: number | null }>;
		};
		if (!quic.getStats) {
			// Best-effort: we can't supply bandwidth estimates, so close the
			// whole bidi (FIN + STOP_SENDING) to let the peer release its end.
			stream.close();
			return;
		}

		let lastSentBitrate: number | undefined;
		let lastSentTime: number | undefined;

		try {
			for (;;) {
				const timeout = new Promise<"timeout">((resolve) =>
					setTimeout(() => resolve("timeout"), PROBE_INTERVAL),
				);
				const result = await Promise.race([timeout, stream.reader.closed]);
				if (result !== "timeout") break;

				const stats = await quic.getStats();
				const bitrate = stats.estimatedSendRate;
				if (bitrate == null) continue;

				let shouldSend: boolean;
				if (lastSentBitrate === undefined || lastSentTime === undefined) {
					shouldSend = true;
				} else if (lastSentBitrate === 0) {
					shouldSend = bitrate > 0;
				} else {
					const elapsed = performance.now() - lastSentTime;
					const t = Math.max(PROBE_INTERVAL, Math.min(PROBE_MAX_AGE, elapsed));
					const range = PROBE_MAX_AGE - PROBE_INTERVAL;
					const threshold = (PROBE_MAX_DELTA * (PROBE_MAX_AGE - t)) / range;
					const change = Math.abs(bitrate - lastSentBitrate) / lastSentBitrate;
					shouldSend = change >= threshold;
				}

				if (shouldSend) {
					await new Probe(bitrate).encode(stream.writer, this.version);
					lastSentBitrate = bitrate;
					lastSentTime = performance.now();
				}
			}
		} catch (err: unknown) {
			console.warn("probe stream error", err);
			stream.close();
		}
	}

	close() {
		this.#broadcasts.update((broadcasts) => {
			for (const broadcast of broadcasts?.values() ?? []) {
				broadcast.close();
			}
			return undefined;
		});

		// Release the datagram writer's lock so the stream can be torn down.
		this.#datagramWriter?.releaseLock();
		this.#datagramWriter = undefined;
	}
}

import { type Dispose, Signal } from "@moq/signals";
import type { Broadcast } from "../broadcast.ts";
import { Compression, compress as compressPayload } from "../compression.ts";
import type { Group } from "../group.ts";
import * as Path from "../path.ts";
import { type Stream, Writer } from "../stream.ts";
import type { TrackSubscriber } from "../track.ts";
import { error } from "../util/error.ts";
import { AnnounceBroadcast, AnnounceInit, AnnounceOk, type AnnounceRequest, epochNow } from "./announce.ts";
import { Group as GroupMessage } from "./group.ts";
import type { Origin } from "./origin.ts";
import { Probe } from "./probe.ts";
import type { Setup } from "./setup.ts";
import {
	encodeSubscribeResponse,
	type Subscribe,
	SubscribeEnd,
	SubscribeOk,
	SubscribeStart,
	SubscribeUpdate,
} from "./subscribe.ts";
import { TrackInfo as TrackInfoMessage, type Track as TrackMessage } from "./track.ts";
import { Version } from "./version.ts";

const PROBE_INTERVAL = 100; // ms
const PROBE_MAX_AGE = 10_000; // ms
const PROBE_MAX_DELTA = 0.25;

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

	// Our published broadcasts.
	// It's a signal so we can live update any announce streams.
	#broadcasts = new Signal<Map<Path.Valid, Broadcast> | undefined>(new Map());

	// Per-broadcast epoch (ms since 2020-01-01 UTC), stamped when the instance is
	// published. Mirrors the Rust `BroadcastInfo.epoch` (SystemTime::now at creation):
	// a newer instance of the same path carries a later epoch, so a consumer can prefer
	// the newest route. Sent in every ANNOUNCE_BROADCAST (lite-05+), including the
	// unannounce, so the peer can match the instance that ended.
	#epochs = new Map<Path.Valid, number>();

	// TRACK_INFO is immutable per track, so resolve it from the application once
	// (via a throwaway subscribe whose info() resolves when the app calls accept)
	// and reuse it for every later TRACK request of the same track. Keyed by
	// `broadcast\0track`. A rejected lookup is evicted so a retry can re-probe.
	#trackInfo = new Map<string, Promise<TrackInfoMessage>>();

	// The peer's SETUP, recorded by the connection once its Setup stream is read.
	// Consulted before compressing a track's egress: we may only use an algorithm
	// the peer advertised it can decompress. `undefined` until it arrives.
	#peerSetup?: Signal<Setup | undefined>;

	/**
	 * Creates a new Publisher instance.
	 * @param quic - The WebTransport session to use
	 * @param version - Negotiated protocol version
	 * @param origin - Origin id shared with the Subscriber
	 * @param peerSetup - Slot for the peer's SETUP, for compression negotiation (lite-05+)
	 *
	 * @internal
	 */
	constructor(quic: WebTransport, version: Version, origin: Origin, peerSetup?: Signal<Setup | undefined>) {
		this.#quic = quic;
		this.version = version;
		this.origin = origin;
		this.#peerSetup = peerSetup;
	}

	// Await the algorithms the peer can decompress, blocking until its SETUP arrives.
	// We MUST NOT compress with an algorithm the peer didn't advertise. An empty list
	// (no slot, or no Compression parameter) means everything must be sent verbatim.
	async #peerCompression(): Promise<Compression[]> {
		if (!this.#peerSetup) return [];
		let setup = this.#peerSetup.peek();
		while (setup === undefined) {
			setup = await this.#peerSetup.next();
		}
		return setup.compression;
	}

	/**
	 * Publishes a broadcast with any associated tracks.
	 * @param name - The broadcast to publish
	 */
	publish(path: Path.Valid, broadcast: Broadcast) {
		// Stamp the instance epoch at publish time (mirrors Rust's SystemTime::now default).
		this.#epochs.set(path, epochNow());
		this.#broadcasts.mutate((broadcasts) => {
			if (!broadcasts) throw new Error("closed");
			broadcasts.set(path, broadcast);
		});

		// Remove the broadcast from the lookup when it's closed. Keep the epoch around:
		// the unannounce announce (sent from the per-stream diff loop after the map change)
		// still needs it to identify the instance that ended. It's overwritten on the next
		// publish to the same path, so it can't go stale.
		void broadcast.closed.finally(() => {
			this.#broadcasts.mutate((broadcasts) => {
				broadcasts?.delete(path);
			});
		});
	}

	// The epoch of the broadcast published at `path`, or 0 if it was never seen.
	#epoch(path: Path.Valid): number {
		return this.#epochs.get(path) ?? 0;
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
			case Version.DRAFT_05_WIP: {
				// Report our origin id once via AnnounceOk and the count of initial announces
				// that follow; the subscriber stamps our origin onto each hop chain, so we omit it.
				const ok = new AnnounceOk(this.origin, active.size);
				await ok.encode(stream.writer, this.version);
				for (const suffix of active) {
					const epoch = this.#epoch(Path.join(msg.prefix, suffix));
					const wire = new AnnounceBroadcast({ suffix, active: true, epoch });
					await wire.encode(stream.writer, this.version);
				}
				break;
			}
			default:
				// Draft03/04: send individual Announce messages, stamping our origin as a hop.
				for (const suffix of active) {
					const wire = new AnnounceBroadcast({ suffix, active: true, hops: [this.origin] });
					await wire.encode(stream.writer, this.version);
				}
				break;
		}

		// Wait for updates to the broadcasts.
		for (;;) {
			// TODO Make a better helper within Signals.
			let dispose!: Dispose;
			const changed = new Promise<Map<Path.Valid, Broadcast> | undefined>((resolve) => {
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
				const hops = this.version === Version.DRAFT_05_WIP ? [] : [this.origin];
				const epoch = this.#epoch(Path.join(msg.prefix, added));
				const wire = new AnnounceBroadcast({ suffix: added, active: true, epoch, hops });
				await wire.encode(stream.writer, this.version);
			}

			// Announce any removed broadcasts.
			// Ended announces don't need hops — the peer matches on path only.
			// They carry the epoch of the instance that ended so the peer can match it.
			for (const removed of active.difference(newActive)) {
				console.debug(`announce: broadcast=${removed} active=false`);
				const epoch = this.#epoch(Path.join(msg.prefix, removed));
				const wire = new AnnounceBroadcast({ suffix: removed, active: false, epoch });
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

		const track = broadcast.subscribe(msg.track, msg.priority);

		try {
			let compress = false;
			let peerDeflate = false;

			if (supportsTrackStream(this.version)) {
				// Lite-05+ accepts implicitly: no SUBSCRIBE_OK (the immutable
				// properties live in TRACK_INFO), and the resolved range arrives as
				// SUBSCRIBE_START / SUBSCRIBE_END emitted from #runTrack.
				//
				// The compress hint is one of those immutable properties; it gates the
				// per-frame Compression field. Awaiting info() also surfaces a rejected
				// track (accept never called, track closed) as an error here, which
				// resets the stream. Whether we may actually use DEFLATE is the per-hop
				// SETUP negotiation; only wait on the peer's SETUP for a hinted track.
				const info = await track.info();
				compress = info.compress;
				if (compress) {
					peerDeflate = (await this.#peerCompression()).includes(Compression.Deflate);
				}
			} else {
				// Older drafts acknowledge with SUBSCRIBE_OK and stream frames verbatim.
				const ok = new SubscribeOk({ priority: msg.priority });
				await encodeSubscribeResponse(stream.writer, { ok }, this.version);
			}

			console.debug(`publish ok: broadcast=${msg.broadcast} track=${track.name}`);

			const serving = this.#runTrack(msg.id, msg.broadcast, track, stream.writer, compress, peerDeflate);

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
		} catch (err: unknown) {
			const e = error(err);
			console.warn(`publish error: broadcast=${msg.broadcast} track=${track.name} error=${e.message}`);
			track.close(e);
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
	async #runTrack(
		sub: bigint,
		broadcast: Path.Valid,
		track: TrackSubscriber,
		stream: Writer,
		compress: boolean,
		peerDeflate: boolean,
	) {
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

				void this.#runGroup(sub, group, compress, peerDeflate);
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
			// The wire no longer carries a cache hint (retention is best-effort, not a
			// guarantee); the local `info.cache` stays a purely local retention window.
			return new TrackInfoMessage({
				priority: info.priority,
				ordered: info.ordered,
				// This implementation doesn't produce per-frame timestamps yet.
				timescale: 0,
				compress: info.compress,
			});
		})();

		// Don't poison the cache on failure: a later request may succeed.
		pending.catch(() => this.#trackInfo.delete(key));
		this.#trackInfo.set(key, pending);
		return pending;
	}

	/**
	 * Runs a group and sends its frames to the stream.
	 * @param sub - The subscription ID
	 * @param group - The group to run
	 *
	 * @internal
	 */
	async #runGroup(sub: bigint, group: Group, compress: boolean, peerDeflate: boolean) {
		const msg = new GroupMessage(sub, group.sequence);
		try {
			const stream = await Writer.open(this.#quic);
			await stream.u8(0); // stream type
			await msg.encode(stream);

			try {
				for (;;) {
					const frame = await Promise.race([group.readFrame(), stream.closed]);
					if (!frame) break;

					if (!compress) {
						// No per-frame Compression field on a non-hinted track.
						await stream.u53(frame.byteLength);
						await stream.write(frame);
						continue;
					}

					// Compress-hinted track: every frame carries a Compression field naming
					// the codec used. Use DEFLATE only if the peer can inflate it and it
					// actually shrinks the (non-empty) payload; otherwise send verbatim.
					let codec: Compression = Compression.None;
					let payload = frame;
					if (peerDeflate && frame.byteLength > 0) {
						const deflated = await compressPayload(Compression.Deflate, frame);
						if (deflated.byteLength < frame.byteLength) {
							codec = Compression.Deflate;
							payload = deflated;
						}
					}
					await stream.u53(codec);
					await stream.u53(payload.byteLength);
					await stream.write(payload);
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
	}
}

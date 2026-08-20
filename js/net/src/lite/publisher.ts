import { type Dispose, type Getter, Signal } from "@moq/signals";
import type * as broadcast from "../broadcast.ts";
import { error, reason, StreamCode, toTransport } from "../error.ts";
import type * as group from "../group.ts";
import type { Origin } from "../hop.ts";
import { hooks } from "../internal.ts";
import type { Consumer as OriginConsumer } from "../origin.ts";
import * as Path from "../path.ts";
import { type Reader, type Stream, Writer } from "../stream.ts";
import { Timescale } from "../time.ts";
import type * as track from "../track.ts";
import { AnnounceInit, AnnounceOk, type AnnounceRequest, encodeAnnounceBroadcast } from "./announce.ts";
import { Datagram as DatagramMessage } from "./datagram.ts";
import * as DatagramStream from "./datagram_stream.ts";
import type { Fetch } from "./fetch.ts";
import { Group as GroupMessage } from "./group.ts";
import { Priority, sendOrder } from "./priority.ts";
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
import { hasAnnounceId, hasAnnounceOk, hasDatagrams, Version } from "./version.ts";

const PROBE_INTERVAL = 100; // ms
const PROBE_MAX_AGE = 10_000; // ms
const PROBE_MAX_DELTA = 0.25;

/** Map a signed delta to an unsigned zigzag varint value (mirrors Rust `VarInt::from_zigzag`). */
function zigzag(delta: bigint): bigint {
	return delta >= 0n ? delta << 1n : (-delta << 1n) - 1n;
}

/** What {@link Publisher.runGroup} needs to serve one group. */
interface RunGroup {
	/** The subscription ID. */
	sub: bigint;

	/** The group to serve. */
	group: group.Consumer;

	/** The track's advertised timescale, applied to every frame timestamp. */
	timescale: Timescale;

	/** The subscription's ranking, which this stream joins for as long as it runs. */
	priority: Priority;

	/** Settles when the subscriber leaves, dropping a group still queued for a stream slot. */
	unsubscribed: Promise<void>;

	/** First frame to send; anything below it was excluded by the subscription. */
	start: number;

	/** Last frame to send (inclusive), or undefined for the rest of the group. */
	end?: number;
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
 * The frame bounds a subscription placed on its start and end group, as they stand
 * after any SUBSCRIBE_UPDATE.
 *
 * Only the two named groups are qualified; the group range itself lives on the
 * subscriber's read cursor (see {@link frameRange}).
 */
type FrameBounds = {
	/** The group {@link startFrame} qualifies, if the subscription named one. */
	startGroup?: number;
	/** First frame to send within {@link startGroup}; every other group starts at 0. */
	startFrame: number;
	/** The group {@link endFrame} qualifies, if the subscription named one. */
	endGroup?: number;
	/** Last frame (inclusive) to send within {@link endGroup}; every other group runs to its end. */
	endFrame?: number;
};

/**
 * The frames of `sequence` a subscription asked for, as a start index and an inclusive end.
 *
 * The frame bounds qualify the start and end group only; every other group is served whole.
 * Which groups are served at all is the subscriber's read cursor (`startAt` / `endAt`),
 * applied when a group is popped rather than re-checked here.
 *
 * The serving loop calls this synchronously after the pop, before any SUBSCRIBE_UPDATE can
 * change `bounds`. Nothing downstream trims the frame range: it is a wire request,
 * deliberately decoupled from the receiver's local read cursor.
 */
function frameRange(bounds: FrameBounds, sequence: number): { start: number; end?: number } {
	return {
		start: bounds.startGroup === sequence ? bounds.startFrame : 0,
		end: bounds.endGroup === sequence ? bounds.endFrame : undefined,
	};
}

/** What serving one group needs beyond the group itself. */
type ServeGroup = {
	/** The Subscribe ID the GROUP message references. */
	sub: bigint;
	/** The track's advertised timescale, which every frame timestamp is converted to. */
	timescale: Timescale;
	/** First frame to send; anything below it was excluded by the subscription. */
	start: number;
	/** Last frame to send (inclusive), or undefined for the rest of the group. */
	end?: number;
};

/** What serving fetched frames needs beyond the group and destination stream. */
type ServeFetch = Omit<ServeGroup, "sub">;

/** What the serving loop takes from the subscribe stream instead of serving a group. */
type Control =
	/** The peer re-stated the subscription: priority, ordering, latency, and both ranges. */
	| { kind: "update"; update: SubscribeUpdate }
	/** The peer FIN'd, or our own half went away. Either way there is nobody left to serve. */
	| { kind: "done" }
	/** The stream failed; the subscription goes down with it. */
	| { kind: "error"; error: Error };

type SubscriptionControlOptions = {
	reader: Reader;
	writer: Writer;
	version: Version;
	apply: (update: SubscribeUpdate) => void;
};

/**
 * The subscribe stream's control half, decoded ahead of the serving loop.
 *
 * Decoding runs on its own and publishes each full subscription update immediately, so a
 * blocked response write cannot delay re-ranking streams already in flight. It separately
 * stores only the latest range state for the serving loop, which owns the local track cursor
 * and frame bounds. That keeps a group pop and its frame-range snapshot one indivisible step.
 *
 * Reading ahead makes control-first ordering hold for a burst. Coalescing bounds memory while
 * preserving the newest state decoded before the next group pop. The Rust publisher gets the
 * same ordering from `poll_decode_maybe`, which decodes straight out of the reader's buffer;
 * nothing here can decode synchronously, so it reads ahead instead.
 */
class SubscriptionControls {
	#writer: Writer;
	#update?: SubscribeUpdate;
	// Sticky, first one wins: null once the stream is over, an Error once it failed.
	#end?: Error | null;
	#ended: Promise<Error | null>;
	#resolveEnd!: (end: Error | null) => void;
	#changed = new Signal(0);

	/** Settles once decoding stops, so teardown can wait for it rather than leaving it running. */
	readonly decoding: Promise<void>;

	constructor({ reader, writer, version, apply }: SubscriptionControlOptions) {
		this.#writer = writer;
		this.#ended = new Promise((resolve) => {
			this.#resolveEnd = resolve;
		});
		this.decoding = this.#decode(reader, version, apply);
		// Our own half going away ends the loop too, and has to reach it the same way: the
		// loop looks at nothing else.
		void writer.closed.then(
			() => this.#finish(null),
			(err: unknown) => this.#finish(error(err)),
		);
	}

	/** The next control to apply, or undefined while the peer is quiet. */
	take(): Control | undefined {
		const update = this.#update;
		this.#update = undefined;
		// An update decoded before the stream ended still applies before the sticky end.
		if (update) return { kind: "update", update };
		if (this.#end === undefined) return undefined;
		return this.#end === null ? { kind: "done" } : { kind: "error", error: this.#end };
	}

	/** Calls `fn` once {@link take} may answer differently. */
	changed(fn: () => void): Dispose {
		return this.#changed.changed(fn);
	}

	/** Returns false when peer departure supersedes a blocked response write. */
	async response(pending: Promise<void>): Promise<boolean> {
		const result = await Promise.race([
			pending.then(
				() => ({ kind: "sent" }) as const,
				(err: unknown) => ({ kind: "error", error: error(err) }) as const,
			),
			this.#ended.then((end) => ({ kind: "ended", end }) as const),
		]);

		if (result.kind === "sent") return true;
		if (result.kind === "error") throw result.error;
		// Promise.race leaves the blocked encode running, so reset the writable half too.
		this.#writer.reset(result.end ?? toTransport(StreamCode.Cancel, "cancel"));
		if (result.end) throw result.end;
		return false;
	}

	#finish(end: Error | null) {
		if (this.#end !== undefined) return;
		this.#end = end;
		this.#resolveEnd(end);
		this.#changed.update((value) => value + 1);
	}

	async #decode(reader: Reader, version: Version, apply: (update: SubscribeUpdate) => void) {
		try {
			while (this.#end === undefined) {
				const update = await SubscribeUpdate.decodeMaybe(reader, version);
				if (!update) break;
				apply(update);
				this.#update = update;
				this.#changed.update((value) => value + 1);
			}
		} catch (err: unknown) {
			this.#finish(error(err));
			return;
		}
		this.#finish(null);
	}
}

// A microtask is too short: decoding one framed update crosses several awaits, each of which
// can requeue behind the serving continuation. A task boundary lets the decoder finish whatever
// the transport already delivered before the next group pop. Updates are rare, so groups do not
// pay this scheduling cost on the normal path.
const yieldToControls = () => new Promise<void>((resolve) => setTimeout(resolve, 0));

// Register both readiness sources in the same turn after the caller observed neither ready.
// The winner disposes both registrations, so an idle subscription accumulates nothing no
// matter how many times it wakes.
function waitForSubscription(controls: SubscriptionControls, subscriber: track.Subscriber): Promise<void> {
	return new Promise((resolve) => {
		let settled = false;
		const dispose: Dispose[] = [];
		const wake = () => {
			if (settled) return;
			settled = true;
			for (const close of dispose) close();
			resolve();
		};
		dispose.push(controls.changed(wake), hooks.groupChanged(subscriber, wake));
	});
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

	// The published broadcasts, borrowed from the origin this session serves. The origin
	// outlives the session, so this is read-only here: announce streams watch it for
	// changes, and closing the session leaves the broadcasts alone.
	#broadcasts: Getter<ReadonlyMap<Path.Valid, broadcast.Consumer> | undefined>;

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
	 * @param publish - The origin whose broadcasts this session serves; omit to publish nothing
	 *
	 * @internal
	 */
	constructor(quic: WebTransport, version: Version, origin: Origin, publish?: OriginConsumer) {
		this.#quic = quic;
		this.version = version;
		this.origin = origin;
		this.#broadcasts = publish?.broadcasts ?? new Signal(new Map());

		// Grab the datagram writer up front when the transport carries datagrams (no group
		// fallback, so it stays undefined otherwise). One writer for all subscriptions.
		if (hasDatagrams(version)) {
			this.#datagramWriter = DatagramStream.datagramWriter(quic);
		}
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

		// Send initial announcements. Keyed by suffix, valued by the routing front, so a
		// republish (a new broadcast taking the path) diffs as ended-then-active rather
		// than nothing; the subscriber treats that as a restart and re-consumes.
		let active = new Map<Path.Valid, broadcast.Consumer>();

		const broadcasts = this.#broadcasts.peek();
		if (!broadcasts) return; // closed

		for (const [name, front] of broadcasts) {
			const suffix = Path.stripPrefix(msg.prefix, name);
			if (suffix === null) continue;
			console.debug(`announce: broadcast=${name} active=true`);
			active.set(suffix, front);
		}

		// Lite06+: announce ids. Every active we send implicitly assigns the next
		// per-stream ordinal; ended references the id instead of repeating the path.
		let nextAnnounceId = 0n;
		const announceIds = new Map<Path.Valid, bigint>();

		switch (this.version) {
			case Version.DRAFT_01:
			case Version.DRAFT_02: {
				const init = new AnnounceInit([...active.keys()]);
				await init.encode(stream.writer, this.version);
				break;
			}
			default: {
				if (!hasAnnounceOk(this.version)) {
					// Draft03/04: send individual Announce messages, stamping our origin as a hop.
					for (const suffix of active.keys()) {
						await encodeAnnounceBroadcast(
							stream.writer,
							{ status: "active", suffix, hops: [this.origin] },
							this.version,
						);
					}
					break;
				}

				// Report our origin id once via AnnounceOk and the count of initial announces
				// that follow; the subscriber stamps our origin onto each hop chain, so we omit it.
				const ok = new AnnounceOk(this.origin, active.size);
				await ok.encode(stream.writer, this.version);
				for (const suffix of active.keys()) {
					if (hasAnnounceId(this.version)) {
						announceIds.set(suffix, nextAnnounceId++);
					}
					await encodeAnnounceBroadcast(stream.writer, { status: "active", suffix, hops: [] }, this.version);
				}
				break;
			}
		}

		// Wait for updates to the broadcasts.
		for (;;) {
			// TODO Make a better helper within Signals.
			let dispose!: Dispose;
			const changed = new Promise<ReadonlyMap<Path.Valid, broadcast.Consumer> | undefined>((resolve) => {
				dispose = this.#broadcasts.changed(resolve);
			});

			// Wait until the map of broadcasts changes.
			const broadcasts = await Promise.race([changed, stream.reader.closed]);
			dispose();
			if (!broadcasts) break;

			// Create a new map of active broadcasts.
			// This is SLOW, but it's not worth optimizing because we often have just 1 broadcast anyway.
			const newActive = new Map<Path.Valid, broadcast.Consumer>();
			for (const [name, front] of broadcasts) {
				const suffix = Path.stripPrefix(msg.prefix, name);
				if (suffix === null) continue; // Not our prefix.
				newActive.set(suffix, front);
			}

			// Retract removed and superseded broadcasts first, so a republish reads as
			// ended-then-active (a restart) on the wire. Lite06+ retracts by announce id;
			// older versions repeat the path (ended announces don't need hops).
			for (const [removed, front] of active) {
				if (newActive.get(removed) === front) continue;
				console.debug(`announce: broadcast=${removed} active=false`);
				if (hasAnnounceId(this.version)) {
					const id = announceIds.get(removed);
					announceIds.delete(removed);
					if (id === undefined) continue; // never announced
					await encodeAnnounceBroadcast(stream.writer, { status: "endedId", id }, this.version);
				} else {
					await encodeAnnounceBroadcast(stream.writer, { status: "ended", suffix: removed }, this.version);
				}
			}

			// Announce new and superseding broadcasts. Lite05+ reports our origin once via
			// AnnounceOk, so the subscriber stamps it onto each hop chain; older versions
			// stamp it here.
			for (const [added, front] of newActive) {
				if (active.get(added) === front) continue;
				console.debug(`announce: broadcast=${added} active=true`);
				const hops = hasAnnounceOk(this.version) ? [] : [this.origin];
				if (hasAnnounceId(this.version)) {
					announceIds.set(added, nextAnnounceId++);
				}
				await encodeAnnounceBroadcast(stream.writer, { status: "active", suffix: added, hops }, this.version);
			}

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

		const track = broadcast.subscribe(msg.track, {
			priority: msg.priority,
			ordered: msg.ordered,
			maxAge: msg.maxAge,
			startGroup: msg.startGroup,
			endGroup: msg.endGroup,
		});
		const startGroup = msg.startGroup ?? track.latest();
		if (startGroup !== undefined) track.startAt(startGroup);
		track.endAt(msg.endGroup);

		// The best-effort datagram loop, started once serving begins. It parks when the
		// track finishes (recvDatagram returns undefined), so #runTrack alone ends the
		// subscription; awaited during teardown so it doesn't outlive the subscription.
		let datagrams = Promise.resolve();
		let controls: SubscriptionControls | undefined;

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
				const ok = new SubscribeOk({
					priority: msg.priority,
					ordered: msg.ordered,
					maxAge: msg.maxAge,
					startGroup: msg.startGroup,
					endGroup: msg.endGroup,
				});
				await encodeSubscribeResponse(stream.writer, { ok }, this.version);
			}

			console.debug(`publish ok: broadcast=${msg.broadcast} track=${track.name}`);

			// Serve datagrams concurrently with groups whenever the transport carries them
			// (the writer exists iff so). No group fallback: otherwise they simply aren't sent.
			if (this.#datagramWriter) {
				datagrams = this.#runDatagrams(msg.id, track, timescale);
			}

			controls = new SubscriptionControls({
				reader: stream.reader,
				writer: stream.writer,
				version: this.version,
				apply: (update) => {
					track.update({
						priority: update.priority,
						ordered: update.ordered,
						maxAge: update.maxAge,
						startGroup: update.startGroup,
						endGroup: update.endGroup,
					});
				},
			});
			await this.#runTrack(track, stream.writer, controls, {
				sub: msg.id,
				broadcast: msg.broadcast,
				timescale,
				bounds: {
					startGroup: msg.startGroup,
					startFrame: msg.startFrame,
					endGroup: msg.endGroup,
					endFrame: msg.endFrame,
				},
			});

			console.debug(`publish done: broadcast=${msg.broadcast} track=${track.name}`);
			stream.close();
			track.close();
			// Closing the stream ends the decoder and track.close ends the datagram loop.
			await Promise.all([datagrams, controls.decoding]);
		} catch (err: unknown) {
			const e = error(err);
			console.warn(`publish error: broadcast=${msg.broadcast} track=${track.name} error=${reason(e)}`);
			track.close(e);
			stream.abort(e);
			await Promise.all([datagrams, controls?.decoding]);
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

		// The subscriber opened this stream, so its send order only ranked the request. Rank the
		// response here, on the same scale as the group streams it competes with.
		stream.writer.setPriority(sendOrder({ priority: msg.priority }));

		let group: group.Consumer | undefined;
		try {
			// The timescale is immutable, so serve exactly what TRACK_INFO advertised.
			const info = await this.#resolveTrackInfo(msg.broadcast, msg.track);
			group = await broadcast.track(msg.track).fetchGroup(msg.group, { priority: msg.priority });
			await this.#runFetchGroup(group, stream.writer, {
				timescale: Timescale(info.timescale),
				start: msg.startFrame,
				end: msg.endFrame,
			});
			console.debug(`fetch done: broadcast=${msg.broadcast} track=${msg.track} group=${msg.group}`);
			stream.close();
			group.close();
		} catch (err: unknown) {
			const e = error(err);
			console.warn(
				`fetch error: broadcast=${msg.broadcast} track=${msg.track} group=${msg.group} error=${reason(e)}`,
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
	async #runTrack(
		track: track.Subscriber,
		stream: Writer,
		controls: SubscriptionControls,
		serving: { sub: bigint; broadcast: Path.Valid; timescale: Timescale; bounds: FrameBounds },
	) {
		const { sub, broadcast, timescale, bounds } = serving;
		// Lite-05+ resolves the range on the subscribe stream: SUBSCRIBE_START once the
		// first group is known, SUBSCRIBE_END when the track finishes.
		const emitRange = supportsTrackStream(this.version);
		let startSent = false;
		let endSent = false;

		// The track's exclusive final boundary. A Rust subscriber feeds SUBSCRIBE_END
		// straight into finish_at, so it must name the track's boundary (which counts
		// datagram sequences too), not the delivered range: a subscription cap can hold
		// produced groups back. The latest() fallback covers a subscription torn down
		// before the producer declared it.
		const boundary = () => track.final() ?? (track.latest() ?? -1) + 1;

		// SUBSCRIBE_END names that boundary, which a cap can hold groups back from, so it goes
		// out as soon as the producer finishes and the subscription keeps serving whatever a
		// later cap raise releases (see the Rust publisher's Recv::Boundary).
		const sendEnd = async (): Promise<boolean> => {
			endSent = true;
			if (emitRange) {
				return controls.response(
					encodeSubscribeResponse(stream, { end: new SubscribeEnd(boundary()) }, this.version),
				);
			}
			return true;
		};

		// One ranking for the whole subscription, shared by every group it serves.
		const priority = new Priority(track);

		// Cancels groups still queued for a stream slot. Only the subscriber leaving counts:
		// a track that ran out of groups still has to flush the ones already queued, and the
		// caller FINs the subscribe stream to say so.
		let finished = false;
		let unsubscribe!: () => void;
		const unsubscribed = new Promise<void>((resolve) => {
			unsubscribe = resolve;
		});
		try {
			for (;;) {
				// Control before data, matching the Rust publisher: every control decoded while
				// the loop was parked applies to the next pop, never to one already made. This
				// drain is synchronous, so nothing the decoder holds can land between the pop
				// below and its frame range.
				const control = controls.take();
				if (control) {
					switch (control.kind) {
						case "done":
							// The subscriber left. Its queued groups are pointless now, which
							// the finally below acts on since `finished` stays false.
							return;
						case "error":
							throw control.error;
						case "update": {
							const update = control.update;
							console.debug(`subscribe update: broadcast=${broadcast} track=${track.name}`);
							if (update.startGroup !== undefined) track.startAt(update.startGroup);
							track.endAt(update.endGroup);
							bounds.startGroup = update.startGroup;
							bounds.startFrame = update.startFrame;
							bounds.endGroup = update.endGroup;
							bounds.endFrame = update.endFrame;
							await yieldToControls();
							continue;
						}
					}
				}

				// Exactly-once arrival-order serving. This synchronous package-internal pop
				// and frameRange call are the operation's linearization point.
				const recv = hooks.tryRecvGroup(track);
				switch (recv.kind) {
					case "error":
						throw recv.error;
					case "idle":
						await waitForSubscription(controls, track);
						continue;
					case "boundary":
						// The producer finished but is still holding groups above the cap.
						// Declare the boundary, then wait for an update to release them.
						if (!endSent) {
							if (!(await sendEnd())) return;
							continue;
						}
						await waitForSubscription(controls, track);
						continue;
					case "done":
						if (!endSent) {
							if (!(await sendEnd())) return;
							continue;
						}
						finished = true;
						return;
				}

				const group = recv.group;
				const range = frameRange(bounds, group.sequence);

				if (emitRange && !startSent) {
					startSent = true;
					// SUBSCRIBE_START promises nothing below this sequence will be delivered.
					// Arrival-order serving could later surface a straggler below the first
					// group, so pin the floor to what was announced.
					track.startAt(group.sequence);
					if (
						!(await controls.response(
							encodeSubscribeResponse(
								stream,
								{ start: new SubscribeStart(group.sequence) },
								this.version,
							),
						))
					)
						return;
				}

				void this.#runGroup({
					sub,
					group,
					timescale,
					priority,
					unsubscribed,
					start: range.start,
					end: range.end,
				});
			}
		} finally {
			if (!finished) unsubscribe();
			priority.close();
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
				// Publisher Max Age: the publisher's retention bound, advertised so
				// relays re-serve with the same window.
				maxAge: info.maxAge,
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
		const maxSize = DatagramStream.maxDatagramSize(this.#quic);

		try {
			for (;;) {
				const datagram = await track.recvDatagram();
				if (!datagram) return; // Track finished; #runTrack tears the subscription down.

				// Convert the timestamp to the track's advertised timescale, matching #runGroup.
				const ts = Math.round(datagram.timestamp.as(timescale));
				const body = new DatagramMessage(sub, datagram.sequence, ts, datagram.payload).encode();

				// No group fallback: drop anything that doesn't fit a single datagram.
				if (body.byteLength > maxSize) {
					console.debug(`dropping oversize datagram: sub=${sub} size=${body.byteLength} max=${maxSize}`);
					continue;
				}

				await writer.ready;
				await writer.write(body);
			}
		} catch (err: unknown) {
			// Best-effort: a datagram send failure stops sending but never fails the subscription.
			console.debug(`datagram send stopped: sub=${sub} error=${reason(err)}`);
		}
	}

	// Serialize a fetched group's frames onto the FETCH stream as bare records: each a
	// zigzag-delta timestamp (at the track's advertised timescale) followed by size + bytes.
	async #runFetchGroup(
		group: group.Consumer,
		stream: Writer,
		{ timescale, start: startFrame, end: endFrame }: ServeFetch,
	) {
		// The response carries no header, so the receiver numbers the first frame it gets
		// as `startFrame`. Skipping the head here is the only thing keeping those numbers
		// honest; a group that ends before we reach it can't be served at all.
		for (let i = 0; i < startFrame; i++) {
			if (!(await Promise.race([group.readFrame(), stream.closed]))) {
				throw new Error(`fetch group ended at frame ${i}, before the requested start ${startFrame}`);
			}
		}

		let prevTs = 0n;
		for (let index = startFrame; endFrame === undefined || index <= endFrame; index++) {
			const frame = await Promise.race([group.readFrame(), stream.closed]);
			if (!frame) break;

			const ts = BigInt(Math.round(frame.timestamp.as(timescale)));
			await stream.u62(zigzag(ts - prevTs));
			prevTs = ts;

			await stream.u53(frame.payload.byteLength);
			await stream.write(frame.payload);
		}
	}

	/**
	 * Serves one group on its own unidirectional stream.
	 *
	 * @internal
	 */
	async #runGroup(options: RunGroup) {
		const { sub, group, timescale, priority, unsubscribed, start: startFrame, end: endFrame } = options;
		// This model holds whole groups, so frame `startFrame` is always reachable unless
		// the group ends first. Declaring it up front keeps the stream self-describing.
		const msg = new GroupMessage({ subscribe: sub, sequence: group.sequence, frameStart: startFrame });
		try {
			// The transport drains streams by send order, so this is what makes a high-priority
			// track (and a newer group within it) win the link when there isn't room for both.
			//
			// One stream per group is faster than a peer at its limit can retire them, so this
			// is the one path that doesn't wait for a slot: the transport would serve the opens
			// in the order we asked, which is oldest-first, exactly backwards for live media.
			// Failing here drops the group and lets the next one compete for the next slot.
			const stream = await Writer.tryOpen(this.#quic, {
				sendOrder: priority.rank(group.sequence),
				cancel: unsubscribed,
				waitUntilAvailable: false,
			});
			if (!stream) {
				group.close(new Error("no stream slot"));
				return;
			}

			// Everything past this point runs inside the cleanup scope, so a failure never leaves
			// a finished group's stream being ranked.
			try {
				// A SUBSCRIBE_UPDATE re-ranks the subscription, so a group already on the wire
				// follows it too rather than keeping a stale rank until it finishes.
				priority.add(stream, group.sequence);

				await stream.u53(0); // stream type
				await msg.encode(stream, this.version);

				// Lite05+ prefixes every frame with a zigzag-delta timestamp at the track's
				// advertised timescale; older drafts omit it.
				const timestamps = supportsTrackStream(this.version);
				let prevTs = 0n;
				// Whether the cursor ever reached the requested start, which decides how the
				// end of the group is read below.
				let reached = startFrame === 0;

				for (;;) {
					const frame = await Promise.race([group.readFrameSequence(), stream.closed]);
					if (!frame) {
						// The group ended before the frame the subscriber asked to start
						// at, so this publisher can't serve the range at all. FINning here
						// would claim an empty group under that index; reset so it reads
						// as the gap it is.
						if (!reached) throw new Error(`group ended before frame ${startFrame}`);
						break;
					}

					// Frames below the requested start were excluded, and the receiver
					// numbers what it gets from `startFrame`.
					if (frame.sequence < startFrame) continue;
					if (endFrame !== undefined && frame.sequence > endFrame) break;
					reached = true;

					if (timestamps) {
						// Convert each frame to the track's advertised timescale.
						const ts = BigInt(Math.round(frame.timestamp.as(timescale)));
						await stream.u62(zigzag(ts - prevTs));
						prevTs = ts;
					}

					await stream.u53(frame.payload.byteLength);
					await stream.write(frame.payload);
				}

				stream.close();
				group.close();
			} catch (err: unknown) {
				const e = error(err);
				stream.reset(e);
				group.close(e);
			} finally {
				priority.remove(stream);
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
		// The broadcasts belong to the origin, which outlives this session; closing here
		// only drops the borrow. The peer sees the unannounce when the streams die.

		// Release the datagram writer's lock so the stream can be torn down.
		this.#datagramWriter?.releaseLock();
		this.#datagramWriter = undefined;
	}
}

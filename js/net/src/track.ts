/**
 * Track role handles: a live stream of groups (and best-effort datagrams) within a broadcast.
 *
 * @module
 */
import { type GetPromise, type Getter, Once, Signal } from "@moq/signals";
import type { Datagram } from "./datagram.ts";
import { type Frame, type Consumer as GroupConsumer, Producer as GroupProducer, Lagged } from "./group.ts";
import { hooks } from "./internal.ts";
import { Timescale, type Timestamp } from "./time.ts";

export type { Datagram } from "./datagram.ts";

/** Default {@link Info.latencyMax} window (milliseconds) when the publisher does not set one. */
export const DEFAULT_LATENCY_MAX_MS = 5000;

/**
 * How long (milliseconds) a datagram stays in the per-subscriber buffer before it is dropped.
 *
 * Datagrams are a best-effort send buffer, not a replay cache (unlike groups): only the last few
 * tens of milliseconds are kept, so a consumer that stalls loses stale datagrams instead of
 * replaying them. Mirrors the Rust `MAX_DATAGRAM_AGE`.
 */
const MAX_DATAGRAM_AGE_MS = 50;

/** A datagram buffered with its arrival time, so the send buffer can evict by age. */
type BufferedDatagram = { datagram: Datagram; time: number };

/**
 * Sanity cap on a datagram payload: the QUIC DATAGRAM frame ceiling. The real limit is
 * per-hop (the negotiated transport datagram size minus a small header) and oversize
 * datagrams are dropped there; a payload above this cap could never fit anywhere.
 */
const MAX_DATAGRAM_BYTES = 65535;

/**
 * A track's immutable publisher properties, fixed for the lifetime of the track.
 *
 * A producer declares these once (via {@link Request.accept} or
 * {@link Producer.accept}); a consumer awaits them via {@link Subscriber.info}
 * (resolved from the wire TRACK_INFO on lite-05+). They map 1:1 onto TRACK_INFO.
 */
export interface Info {
	/**
	 * Units per second for this track's frame timestamps (reported in TRACK_INFO on
	 * Lite05+). Defaults to milliseconds; set it finer (e.g. {@link Timescale.MICRO})
	 * for media that needs sub-millisecond timing.
	 */
	timescale: Timescale;
	/**
	 * Publisher Max Latency: the maximum age (milliseconds) of a non-latest group before
	 * the publisher evicts it. Reported in TRACK_INFO (Lite05+) so relays re-serve with the
	 * same bound. The publisher-side half of the budget a subscriber sets for itself.
	 */
	latencyMax: number;
	/** Tie-break priority between subscriptions of equal subscriber priority. */
	priority: number;
	/**
	 * Whether groups are prioritized in sequence order. Groups may always arrive
	 * out-of-order (or not at all) over the network. Defaults to `false` (newest-first).
	 */
	ordered: boolean;
}

/** Fill in any unset {@link Info} fields with their defaults. */
export function infoDefaults(info: Partial<Info> = {}): Info {
	return {
		timescale: info.timescale ?? Timescale.MILLI,
		latencyMax: info.latencyMax ?? DEFAULT_LATENCY_MAX_MS,
		priority: info.priority ?? 0,
		ordered: info.ordered ?? false,
	};
}

/**
 * Per-subscription options, requested when a subscription opens and adjustable later via
 * {@link Subscriber.update}. Mirrors the Rust `Subscription`.
 */
export interface Subscription {
	/** Delivery priority relative to this session's other subscriptions. Defaults to `0`. */
	priority?: number;
}

/**
 * A request for a track the peer wants, yielded by `Broadcast.Producer.requested`.
 *
 * Created internally by the broadcast when a subscription (or info lookup) needs a track
 * served; answer it with {@link accept} or {@link reject}.
 *
 * @public
 */
export class Request {
	/** The requested track name. */
	readonly name: string;
	/** The subscriber's priority for this track. */
	readonly priority: number;

	#producer: Producer;

	private constructor(name: string, producer: Producer, priority: number) {
		this.name = name;
		this.#producer = producer;
		this.priority = priority;
	}

	static {
		hooks.makeRequest = (name, producer, priority) => new Request(name, producer, priority);
	}

	/** Accept the request, committing the track's immutable {@link Info}. */
	accept(info: Partial<Info> = {}): Producer {
		return this.#producer.accept(info);
	}

	/** Reject the request, closing the track optionally with an error. */
	reject(err?: Error): void {
		this.#producer.close(err);
	}
}

/** Options for {@link Consumer.fetchGroup}. */
export interface FetchGroupOptions {
	/** Delivery priority for the fetch stream. Defaults to `0`. */
	priority?: number;
}

/**
 * The per-track operations a lazy {@link Consumer} delegates to the broadcast it came from.
 *
 * Implemented by `broadcast.Producer` / `broadcast.Consumer` (and the wire-layer subclasses
 * that resolve them over the network), so a track handle holds a reference to its broadcast
 * and calls methods on it rather than capturing a bag of callbacks.
 */
export interface Broadcast {
	/** Open a live subscription to the named track. */
	subscribe(name: string, options?: Subscription): Subscriber;
	/** Resolve the named track's immutable info. */
	resolveTrackInfo(name: string): Promise<Info>;
	/** Fetch a single group of the named track by sequence. */
	fetchGroup(name: string, sequence: number, options?: FetchGroupOptions): Promise<GroupConsumer>;
}

/**
 * A lazy handle to a track on a consumed broadcast.
 *
 * @public
 */
export class Consumer {
	/** The track name. */
	readonly name: string;

	#broadcast: Broadcast;

	constructor(name: string, broadcast: Broadcast) {
		this.name = name;
		this.#broadcast = broadcast;
	}

	/** Open a live subscription to the track. */
	subscribe(options?: Subscription): Subscriber {
		return this.#broadcast.subscribe(this.name, options);
	}

	/** Fetch the track's immutable publisher properties without subscribing. */
	info(): Promise<Info> {
		return this.#broadcast.resolveTrackInfo(this.name);
	}

	/** Fetch a single group by sequence without holding a live subscription. */
	fetchGroup(sequence: number, options?: FetchGroupOptions): Promise<GroupConsumer> {
		return this.#broadcast.fetchGroup(this.name, sequence, options);
	}
}

// The shared state behind a Producer / Subscriber pair. Package-internal
// wiring, unexported so it never appears in the published type declarations.
class TrackState {
	groups = new Signal<GroupConsumer[]>([]);
	/** Best-effort datagram channel, parallel to {@link groups}; an age-evicted send buffer per subscriber. */
	datagrams = new Signal<BufferedDatagram[]>([]);
	closed = new Once<Error | null>();
	update = new Signal<Subscription | undefined>(undefined);
	/** Resolved once the producer commits the immutable properties. */
	info = new Signal<Info | undefined>(undefined);
}

// Settle a track state's closed Once. Idempotent: Once.set throws on a second settle, and a
// Producer closing its sinks races the Subscriber closing itself (the sink is only removed from
// #sinks a microtask later, via the closed subscription below).
function closeTrackState(state: TrackState, abort?: Error): boolean {
	if (state.closed.peek() !== undefined) return false;
	state.closed.set(abort ?? null);
	return true;
}

// Resolve the track's immutable publisher properties, or reject if it closes first.
// On a producer this resolves once info is committed (at accept time); on a consumer
// once the wire layer commits the TRACK_INFO it received (lite-05+) or defaults (older
// drafts), so awaiting it never yields a placeholder.
async function resolveInfo(state: TrackState): Promise<Info> {
	for (;;) {
		const info = state.info.peek();
		if (info) return info;

		const closed = state.closed.peek();
		if (closed instanceof Error) throw closed;
		if (closed !== undefined) throw new Error("track closed before info was known");

		await Signal.race(state.info, state.closed);
	}
}

// A source group retained in the producer cache, with the mirror handed to each sink
// so eviction can drop them together.
type CachedGroup = { group: GroupProducer; time: number; mirrors: Map<TrackState, GroupConsumer> };

// Constructs a Subscriber from within this module without exposing a public
// constructor that would leak the unexported TrackState. Assigned in the class's
// static block.
let makeSubscriber: (name: string, state: TrackState) => Subscriber;

/**
 * The write side of a track, mirroring the Rust `Producer`.
 *
 * A producer is a fan-out source: every {@link subscribe} (including each wire
 * subscription the publisher serves from it) gets an independent
 * {@link Subscriber} that receives a full copy of the groups, each with its own
 * read cursor. Groups are mirrored into every live subscriber and retained for the
 * track's `latencyMax` window so a late subscriber replays the recent groups.
 *
 * Obtained from {@link Request.accept} (the wire asks the application for a track to
 * serve) or constructed directly for an in-process track.
 */
export class Producer {
	/** The track name. */
	readonly name: string;

	// The producer's own state is the source of truth (info/closed); subscribers
	// read mirrored sinks, never this state directly.
	#state = new TrackState();

	#next?: number;

	// Recently written source groups, retained for replay to late subscribers and
	// pruned once closed and older than the cache window. Each entry tracks the mirror
	// it handed to every sink so eviction can drop them too: otherwise a slow consumer
	// that never reads would pin old groups (and their frame bytes) forever.
	#cache: CachedGroup[] = [];

	// One independent downstream state per live subscriber.
	#sinks = new Set<TrackState>();

	// Whether any subscriber is currently attached. Exposed as {@link used}; the consumer wire
	// watches it to tear down an idle upstream, and a publisher can watch it for on-demand capture.
	#used = new Signal<boolean>(false);

	constructor(name: string) {
		this.name = name;
	}

	/**
	 * Resolve this track's immutable publisher properties, committed at accept time.
	 * Rejects if the track is closed before the properties are known.
	 */
	info(): Promise<Info> {
		return resolveInfo(this.#state);
	}

	/**
	 * Settles once the track closes: `null` on a clean close, or the abort {@link Error}.
	 * Peek it synchronously (`undefined` while open), observe it reactively, or `await` it.
	 */
	get closed(): GetPromise<Error | null> {
		return this.#state.closed;
	}

	/**
	 * The current subscription, or `undefined` until a subscriber first calls
	 * {@link Subscriber.update}. The wire layer watches this to emit SUBSCRIBE_UPDATE.
	 */
	get subscription(): Getter<Subscription | undefined> {
		return this.#state.update;
	}

	/** Commit the immutable publisher properties, resolving {@link info}. Returns `this`. */
	accept(info: Partial<Info> = {}): this {
		const resolved = infoDefaults(info);
		this.#state.info.set(resolved);
		// Propagate to any sink handed out before accept (the on-demand path).
		for (const sink of this.#sinks) sink.info.set(resolved);
		return this;
	}

	/** An independent {@link Subscriber} receiving a full copy of this track's groups. */
	subscribe(): Subscriber {
		const sink = new TrackState();
		this.#addSink(sink);
		return makeSubscriber(this.name, sink);
	}

	/**
	 * Whether the track currently has any subscribers.
	 *
	 * Watch it (`effect.get` / `.peek()`) to drive on-demand work: a publisher can start and stop
	 * capture with demand, and the consumer wire watches it to tear an idle upstream subscription
	 * down instead of downloading to nobody. Pairs with {@link unused}. Mirrors the Rust `Demand`.
	 */
	get used(): Getter<boolean> {
		return this.#used;
	}

	/** Resolves once the track has no subscribers (or has closed). Await it to react to demand ending. */
	async unused(): Promise<void> {
		while (this.#used.peek() && this.#state.closed.peek() === undefined) {
			await Signal.race(this.#used, this.#state.closed);
		}
	}

	// Register a downstream sink: seed its info, replay the retained window, and (while
	// the track is open) mirror future groups into it. A late subscriber to a closed
	// track still drains the buffered groups before seeing the end.
	#addSink(sink: TrackState): void {
		const info = this.#state.info.peek();
		if (info) sink.info.set(info);

		const closed = this.#state.closed.peek();
		if (closed === undefined) {
			this.#sinks.add(sink);
			this.#used.set(true);

			// Forward subscription updates from the sink's Subscriber to the producer's own
			// state, which the wire layer (or the serving application) watches.
			const forward = sink.update.subscribe((update) => {
				if (update) this.#state.update.set(update, true);
			});

			// Drop the sink once its consumer goes away, closing its mirrors so source
			// groups stop teeing into them, so a long-lived producer doesn't leak. This
			// covers mirrors already handed out via recvGroup (no longer in sink.groups)
			// by closing them through the cache's per-sink tracking.
			const dispose = sink.closed.subscribe((c) => {
				if (c === undefined) return;
				const abort = c instanceof Error ? c : undefined;
				forward();
				this.#sinks.delete(sink);
				for (const entry of this.#cache) {
					const mirror = entry.mirrors.get(sink);
					if (mirror) {
						mirror.close(abort);
						entry.mirrors.delete(sink);
					}
				}
				for (const group of sink.groups.peek()) group.close(abort);
				dispose();

				// Update demand: once the last subscriber leaves, the consumer wire (watching
				// {@link unused}) tears the upstream down instead of downloading to nobody.
				this.#used.set(this.#sinks.size > 0);
			});
		}

		this.#prune();
		for (const entry of this.#cache) this.#mirror(entry, sink);

		if (closed !== undefined) closeTrackState(sink, closed instanceof Error ? closed : undefined);
	}

	// Mirror a cached source group into a sink. The mirror fills synchronously as the
	// source is written and keeps its own read cursor; frame bytes are shared by
	// reference. Tracked on the entry so eviction can drop it from the sink.
	#mirror(entry: CachedGroup, sink: TrackState): void {
		const dst = entry.group.mirror();
		entry.mirrors.set(sink, dst);
		sink.groups.mutate((groups) => {
			groups.push(dst);
			groups.sort((a, b) => a.sequence - b.sequence);
		});
	}

	// Evict cached groups that are closed and older than the cache window, dropping
	// each evicted group's mirror from every sink so no consumer can pin it.
	#prune(): void {
		const latencyMaxMs = this.#state.info.peek()?.latencyMax ?? DEFAULT_LATENCY_MAX_MS;
		const cutoff = Date.now() - latencyMaxMs;

		const retained: CachedGroup[] = [];
		for (const entry of this.#cache) {
			if (entry.time > cutoff || entry.group.closed.peek() === undefined) {
				retained.push(entry);
				continue;
			}

			for (const [sink, mirror] of entry.mirrors) {
				sink.groups.mutate((groups) => {
					const i = groups.indexOf(mirror);
					if (i >= 0) groups.splice(i, 1);
				});
				mirror.close();
			}
			entry.mirrors.clear();
		}
		this.#cache = retained;
	}

	// Retain a source group and fan it out to every live sink.
	#publish(group: GroupProducer): void {
		const entry: CachedGroup = { group, time: Date.now(), mirrors: new Map<TrackState, GroupConsumer>() };
		this.#cache.push(entry);
		this.#prune();
		for (const sink of this.#sinks) this.#mirror(entry, sink);
	}

	/** Append a new group with the next sequence number. */
	appendGroup(): GroupProducer {
		if (this.#state.closed.peek() !== undefined) throw new Error("track is closed");

		const group = new GroupProducer(this.#next ?? 0);
		this.#next = group.sequence + 1;
		this.#publish(group);

		return group;
	}

	/** Insert an existing group into the track. */
	writeGroup(group: GroupProducer) {
		if (this.#state.closed.peek() !== undefined) throw new Error("track is closed");

		// Only advance #next upward (for appendGroup auto-increment).
		if (group.sequence >= (this.#next ?? 0)) {
			this.#next = group.sequence + 1;
		}

		this.#publish(group);
	}

	// Fan a datagram out to every live subscriber, dropping the oldest once the ring is full.
	// Late subscribers do NOT replay old datagrams (best-effort, unlike the group cache).
	#publishDatagram(datagram: Datagram): void {
		const now = performance.now();
		for (const sink of this.#sinks) {
			sink.datagrams.mutate((list) => {
				list.push({ datagram, time: now });
				// Drop anything older than the send-buffer window.
				while (list.length > 0 && now - list[0].time > MAX_DATAGRAM_AGE_MS) list.shift();
			});
		}
	}

	/**
	 * Append a datagram with the next sequence number, returning the assigned sequence.
	 *
	 * A datagram is delivered best-effort over a single QUIC datagram, parallel to the track's
	 * groups but drawing from the same sequence namespace (interleaving with {@link appendGroup}
	 * never reuses a number). The payload must fit the negotiated transport datagram size minus
	 * a small header; an oversize payload is dropped at each hop (there is no group fallback), so
	 * keep datagram payloads small (e.g. a single audio frame). Datagrams are never delivered
	 * over IETF moq-transport or stream-only transports (the WebSocket fallback). A payload over
	 * 65535 bytes (the QUIC datagram frame ceiling) throws. An origin publisher uses this; a
	 * relay preserving upstream numbering uses {@link writeDatagram}.
	 */
	appendDatagram(timestamp: Timestamp, payload: Uint8Array): number {
		if (this.#state.closed.peek() !== undefined) throw new Error("track is closed");
		if (payload.byteLength > MAX_DATAGRAM_BYTES) throw new Error("datagram payload too large");

		const sequence = this.#next ?? 0;
		this.#next = sequence + 1;
		this.#publishDatagram({ sequence, timestamp, payload });
		return sequence;
	}

	/**
	 * Write a datagram with an explicit sequence number.
	 *
	 * Preserves the supplied sequence (advancing the shared counter if needed) so a relay can
	 * forward a datagram without renumbering it. The size limits of {@link appendDatagram}
	 * apply. Most origin publishers want {@link appendDatagram} instead.
	 */
	writeDatagram(datagram: Datagram) {
		if (this.#state.closed.peek() !== undefined) throw new Error("track is closed");
		if (datagram.payload.byteLength > MAX_DATAGRAM_BYTES) throw new Error("datagram payload too large");

		if (datagram.sequence >= (this.#next ?? 0)) {
			this.#next = datagram.sequence + 1;
		}
		this.#publishDatagram(datagram);
	}

	/** Close the track and every subscriber, mirroring the abort to their groups. Idempotent. */
	close(abort?: Error) {
		closeTrackState(this.#state, abort);
		for (const { group } of this.#cache) group.close(abort);
		for (const sink of this.#sinks) {
			for (const group of sink.groups.peek()) group.close(abort);
			closeTrackState(sink, abort);
		}
		this.#sinks.clear();
	}

	/** Append a frame as its own single-frame group. */
	writeFrame(frame: Frame) {
		const group = this.appendGroup();
		group.writeFrame(frame);
		group.close();
	}

	/** Appends a string to the track as its own single-frame group. */
	writeString(str: string) {
		const group = this.appendGroup();
		group.writeString(str);
		group.close();
	}

	/** Appends a JSON value to the track as its own single-frame group. */
	writeJson(json: unknown) {
		const group = this.appendGroup();
		group.writeJson(json);
		group.close();
	}

	/** Appends a boolean to the track as its own single-frame group. */
	writeBool(bool: boolean) {
		const group = this.appendGroup();
		group.writeBool(bool);
		group.close();
	}
}

/**
 * The read side of a live track subscription, mirroring the Rust `Subscriber`.
 *
 * Obtained from `Broadcast.Consumer.subscribe` / `Track.Consumer.subscribe`, or from
 * {@link Producer.subscribe} for an in-process track. Reads the groups a
 * {@link Producer} on the same underlying state writes.
 */
export class Subscriber {
	/** The track name. */
	readonly name: string;

	#state: TrackState;
	#nextSequence = 0;

	private constructor(name: string, state: TrackState) {
		this.name = name;
		this.#state = state;
	}

	static {
		makeSubscriber = (name, state) => new Subscriber(name, state);
	}

	/**
	 * Resolve this track's immutable publisher properties.
	 *
	 * Resolves once the wire layer commits the TRACK_INFO it received (lite-05+) or
	 * defaults (older drafts), so awaiting it never yields a placeholder. Rejects if
	 * the track is closed before the properties are known (e.g. a rejected subscription).
	 */
	info(): Promise<Info> {
		return resolveInfo(this.#state);
	}

	/** Settles once the track closes; see {@link Producer.closed}. */
	get closed(): GetPromise<Error | null> {
		return this.#state.closed;
	}

	/** The last {@link update} requested on this subscriber, or `undefined` if none yet. */
	get subscription(): Getter<Subscription | undefined> {
		return this.#state.update;
	}

	/** Close the track (optionally with an error), closing any pending groups. Idempotent. */
	close(abort?: Error) {
		if (!closeTrackState(this.#state, abort)) return;
		for (const group of this.#state.groups.peek()) {
			group.close(abort);
		}
	}

	/**
	 * Receive the next group available on this track, in arrival order.
	 *
	 * Groups may arrive out of order or with gaps due to network conditions.
	 * Use {@link nextGroup} for sequence order, skipping those that arrive too late.
	 */
	async recvGroup(): Promise<GroupConsumer | undefined> {
		for (;;) {
			const groups = this.#state.groups.peek();
			if (groups.length > 0) {
				return groups.shift();
			}

			const closed = this.#state.closed.peek();
			if (closed instanceof Error) throw closed;
			if (closed !== undefined) return undefined;

			await Signal.race(this.#state.groups, this.#state.closed);
		}
	}

	/**
	 * Receive the next datagram in arrival order.
	 *
	 * Datagrams are a separate best-effort channel from groups (see
	 * {@link Producer.appendDatagram}); they share only the sequence namespace. A consumer
	 * that falls too far behind silently loses the oldest datagrams. Read this alongside
	 * {@link recvGroup} (e.g. in a separate loop) to receive both channels concurrently. Returning
	 * a datagram advances {@link nextGroup} past that sequence.
	 */
	async recvDatagram(): Promise<Datagram | undefined> {
		for (;;) {
			const datagrams = this.#state.datagrams.peek();

			// Evict datagrams older than the send-buffer window (also enforced on write), so a
			// reader that stalled skips stale datagrams instead of replaying them.
			const cutoff = performance.now() - MAX_DATAGRAM_AGE_MS;
			while (datagrams.length > 0 && datagrams[0].time < cutoff) datagrams.shift();

			if (datagrams.length > 0) {
				const datagram = datagrams.shift()?.datagram;
				if (datagram) {
					this.#nextSequence = Math.max(this.#nextSequence, datagram.sequence + 1);
				}
				return datagram;
			}

			const closed = this.#state.closed.peek();
			if (closed instanceof Error) throw closed;
			if (closed !== undefined) return undefined;

			await Signal.race(this.#state.datagrams, this.#state.closed);
		}
	}

	/**
	 * Return the next group with a strictly-greater sequence number than the last returned.
	 *
	 * Late arrivals (sequence at or below the last returned) are silently skipped.
	 * Use {@link recvGroup} to see every group in arrival order instead.
	 */
	async nextGroup(): Promise<GroupConsumer | undefined> {
		for (;;) {
			const group = await this.recvGroup();
			if (!group) return undefined;
			if (group.sequence < this.#nextSequence) {
				group.close();
				continue;
			}
			this.#nextSequence = group.sequence + 1;
			return group;
		}
	}

	/**
	 * Reads the next frame across all groups, discarding older groups.
	 * Treat the returned frame bytes as read-only; they are shared with other consumers.
	 */
	async readFrame(): Promise<Frame | undefined> {
		const next = await this.readFrameSequence();
		return next ? { payload: next.payload, timestamp: next.timestamp } : undefined;
	}

	/**
	 * Reads the next frame along with its group and frame sequence numbers.
	 * Treat the returned frame bytes as read-only; they are shared with other consumers.
	 */
	async readFrameSequence(): Promise<({ group: number; frame: number } & Frame) | undefined> {
		for (;;) {
			const groups = this.#state.groups.peek();

			// Drain older groups first, dropping each once empty.
			while (groups.length > 1) {
				if (groups[0].skipped) {
					// The reader fell behind this group's eviction window. Drop it and
					// signal the gap; the next read resyncs from the following group.
					groups.shift()?.close();
					throw new Lagged();
				}
				const next = groups[0].tryReadFrameSequence();
				if (next) {
					return {
						group: groups[0].sequence,
						frame: next.sequence,
						payload: next.payload,
						timestamp: next.timestamp,
					};
				}
				groups.shift()?.close();
			}

			if (groups.length === 0) {
				const closed = this.#state.closed.peek();
				if (closed instanceof Error) throw closed;
				if (closed !== undefined) return undefined;
				await Signal.race(this.#state.groups, this.#state.closed);
				continue;
			}

			const group = groups[0];
			if (group.skipped) {
				// Fell behind this group's eviction window. Drop it and signal the gap;
				// the next read resyncs from the following group.
				groups.shift()?.close();
				throw new Lagged();
			}
			const next = group.tryReadFrameSequence();
			if (next)
				return {
					group: group.sequence,
					frame: next.sequence,
					payload: next.payload,
					timestamp: next.timestamp,
				};

			const closed = this.#state.closed.peek();
			if (closed instanceof Error) throw closed;
			if (closed !== undefined) return undefined;

			// A finished (drained + closed) group has nothing left: drop it and loop, rather than
			// busy-waiting on its already-resolved readable() (which would livelock and starve the
			// macrotask that delivers the next group).
			if (group.done) {
				groups.shift()?.close();
				continue;
			}

			// Lone open group with nothing buffered yet: wait for a frame on it, a new group, or
			// the track closing.
			await Promise.race([Signal.race(this.#state.groups, this.#state.closed), group.readable()]);
		}
	}

	/** Reads the next frame and decodes it as a UTF-8 string. */
	async readString(): Promise<string | undefined> {
		const next = await this.readFrame();
		if (!next) return undefined;
		return new TextDecoder().decode(next.payload);
	}

	/** Reads the next frame and parses it as JSON. */
	async readJson(): Promise<unknown | undefined> {
		const next = await this.readString();
		if (!next) return undefined;
		return JSON.parse(next);
	}

	/** Reads the next frame and decodes it as a one-byte boolean, throwing on a malformed frame. */
	async readBool(): Promise<boolean | undefined> {
		const next = await this.readFrame();
		if (!next) return undefined;
		const payload = next.payload;
		if (payload.byteLength !== 1 || !(payload[0] === 0 || payload[0] === 1)) throw new Error("invalid bool frame");
		return payload[0] === 1;
	}

	/**
	 * Update this subscription's options (e.g. priority), triggering a SUBSCRIBE_UPDATE to the
	 * publisher. Mirrors the Rust `Subscriber::update`.
	 */
	update(options: Subscription) {
		this.#state.update.set(options, true);
	}
}

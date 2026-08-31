/**
 * Track role handles: a live stream of groups (and best-effort datagrams) within a broadcast.
 *
 * @module
 */
import { type Dispose, type GetPromise, type Getter, Once, Signal } from "@moq/signals";
import type { Datagram } from "./datagram.ts";
import { type Frame, type Consumer as GroupConsumer, Producer as GroupProducer, Lagged } from "./group.ts";
import { hooks, type Recv, type TrackRequestOptions, type TrackSequence, type TrackSequences } from "./internal.ts";
import { Timescale, type Timestamp } from "./time.ts";

export type { Datagram } from "./datagram.ts";

/** Default {@link Info.maxAge} window (milliseconds) when the publisher does not set one. */
export const DEFAULT_MAX_AGE_MS = 5000;

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
	 * Publisher Max Age: the maximum age (milliseconds) of a non-latest group before
	 * the publisher evicts it. Reported in TRACK_INFO (Lite05+) so relays re-serve with the
	 * same bound. The publisher-side half of the budget a subscriber sets for itself.
	 */
	maxAge: number;
	/** Tie-break priority between subscriptions of equal subscriber priority. */
	priority: number;
}

/** Fill in any unset {@link Info} fields with their defaults. */
export function infoDefaults(info: Partial<Info> = {}): Info {
	return {
		timescale: info.timescale ?? Timescale.MILLI,
		maxAge: info.maxAge ?? DEFAULT_MAX_AGE_MS,
		priority: info.priority ?? 0,
	};
}

/**
 * Per-subscription options, requested when a subscription opens and adjustable later via
 * {@link Subscriber.update}. Mirrors the Rust `Subscription`.
 */
export interface Subscription {
	/** Delivery priority relative to this session's other subscriptions. Defaults to `0`. */
	priority?: number;
	/** Maximum age (milliseconds) of a non-latest group before it is skipped. Defaults to `0`. */
	maxAge?: number;
	/**
	 * The lowest group the publisher may deliver (a floor), or omit for none.
	 *
	 * A floor, not a request: only {@link maxAge} asks for data, and the floor bounds how
	 * far back it may reach. Omitting it and a floor of 0 mean the same thing, and a floor
	 * above the live edge simply waits there (a resumed subscription naming where it left
	 * off).
	 */
	startGroup?: number;
	/** Last group the publisher should deliver (inclusive), or omit for no end. */
	endGroup?: number;
}

// Materialize the defaults at the model boundary so every layer observes a complete
// subscription rather than interpreting an omitted field differently.
function subscriptionDefaults(subscription: Subscription = {}): Subscription {
	return {
		priority: subscription.priority ?? 0,
		maxAge: subscription.maxAge ?? 0,
		startGroup: subscription.startGroup,
		endGroup: subscription.endGroup,
	};
}

// Aggregate the preferences of every live subscriber, matching Rust's Subscription::poll_combined.
function combineSubscriptions(states: Iterable<TrackState>): Subscription | undefined {
	let combined: Subscription | undefined;
	for (const state of states) {
		const subscription = state.update.peek();
		if (!subscription) continue;
		if (!combined) {
			combined = { ...subscription };
			continue;
		}

		combined.priority = Math.max(combined.priority ?? 0, subscription.priority ?? 0);
		combined.maxAge = Math.max(combined.maxAge ?? 0, subscription.maxAge ?? 0);

		// A floor only restricts, so a subscriber without one clears the aggregate:
		// its budget may reach below any floor the others set.
		if (combined.startGroup === undefined || subscription.startGroup === undefined) {
			combined.startGroup = undefined;
		} else {
			combined.startGroup = Math.min(combined.startGroup, subscription.startGroup);
		}

		if (combined.endGroup === undefined || subscription.endGroup === undefined) {
			combined.endGroup = undefined;
		} else {
			combined.endGroup = Math.max(combined.endGroup, subscription.endGroup);
		}
	}
	return combined;
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

	#producer: Producer;
	#sequences: TrackSequences;

	private constructor(options: TrackRequestOptions) {
		this.name = options.name;
		this.#producer = options.producer;
		this.#sequences = options.sequences;
	}

	static {
		hooks.makeRequest = (options) => new Request(options);
	}

	/** The aggregate subscription requested for this track. */
	get subscription(): Readonly<Subscription> {
		return this.#producer.subscription.peek() ?? subscriptionDefaults();
	}

	/** The subscriber's priority for this track. */
	get priority(): number {
		return this.subscription.priority ?? 0;
	}

	/** Accept the request, committing the track's immutable {@link Info}. */
	accept(info: Partial<Info> = {}): Producer {
		bindProducer(this.name, this.#producer, this.#sequences);
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

	/**
	 * Open a live subscription to the track.
	 *
	 * The cursor starts at the group the subscription named (its floor), or 0.
	 * {@link Subscription.maxAge} is what asks for data: delivery skips everything above
	 * the floor that the budget convicts, so the default budget of zero delivers only the
	 * latest group and a larger one reaches back over what it can still use.
	 */
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
	// Every group still in the producer's replay cache, including groups this
	// subscriber already consumed, paired with its source queue time. Drift anchors
	// have the same lifetime as content.
	timeline = new Map<number, { group: GroupConsumer; time: number }>();
	// First timestamps mutate group state rather than track state, so held groups
	// watch this revision as well as arrivals when enforcing latency after handoff.
	timelineChanged = new Signal(0);
	/** Best-effort datagram channel, parallel to {@link groups}; an age-evicted send buffer per subscriber. */
	datagrams = new Signal<BufferedDatagram[]>([]);
	latest?: number;
	/**
	 * The exclusive final boundary, stamped when the producer closes cleanly: one past the
	 * highest sequence produced. Groups and datagrams share the namespace, so this can
	 * exceed `latest + 1` (which only tracks groups). Mirrors the Rust `final_sequence`.
	 */
	final?: number;
	closed = new Once<Error | null>();
	update: Signal<Subscription | undefined>;
	/** Resolved once the producer commits the immutable properties. */
	info = new Signal<Info | undefined>(undefined);

	constructor(subscription?: Subscription) {
		this.update = new Signal(subscription === undefined ? undefined : subscriptionDefaults(subscription));
	}
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

function bindProducer(name: string, producer: Producer, sequences: TrackSequences): void {
	let shared = sequences.get(name);
	if (!shared) {
		shared = { next: 0 };
		sequences.set(name, shared);
	}
	bindProducerSequence(producer, shared);
}

let bindProducerSequence: (producer: Producer, sequence: TrackSequence) => void;

// The sequence-order cursor lives inside `Subscriber` (it shares the group buffer and the
// drift anchor with the arrival cursor), so `Ordered` reaches it through this bridge,
// assigned in the class's static block.
let makeOrdered: (subscriber: Subscriber) => Ordered;
let ordered_: {
	nextGroup(subscriber: Subscriber): Promise<GroupConsumer | undefined>;
	readFrame(subscriber: Subscriber): Promise<Frame | undefined>;
	readFrameSequence(subscriber: Subscriber): Promise<({ group: number; frame: number } & Frame) | undefined>;
	readString(subscriber: Subscriber): Promise<string | undefined>;
	readJson(subscriber: Subscriber): Promise<unknown | undefined>;
	readBool(subscriber: Subscriber): Promise<boolean | undefined>;
	recvDatagram(subscriber: Subscriber): Promise<Datagram | undefined>;
};

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
 * track's `maxAge` window so a late subscriber replays the recent groups.
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
	#sequence: TrackSequence = { next: 0 };

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

	static {
		bindProducerSequence = (producer, sequence) => {
			producer.#sequence = sequence;
		};
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
	 * The aggregate subscription across live subscribers, or `undefined` when there are none.
	 * The wire layer watches this to emit SUBSCRIBE_UPDATE.
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
		this.#updateSubscription();
		return this;
	}

	/**
	 * An independent {@link Subscriber} reading this track's groups.
	 *
	 * Its cursor starts at the group the subscription named (its floor), or 0.
	 * {@link Subscription.maxAge} is what asks for data: delivery skips everything above
	 * the floor that the budget convicts, so the default budget of zero delivers only the
	 * latest group and a larger one reaches back over what it can still use.
	 */
	subscribe(options: Subscription = {}): Subscriber {
		const sink = new TrackState(options);
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
			const forward = sink.update.subscribe(() => this.#updateSubscription());
			this.#updateSubscription();

			// Drop the sink once its consumer goes away, closing its mirrors so source
			// groups stop teeing into them, so a long-lived producer doesn't leak. This
			// covers mirrors already handed out via recvGroup (no longer in sink.groups)
			// by closing them through the cache's per-sink tracking.
			const dispose = sink.closed.subscribe((c) => {
				if (c === undefined) return;
				const abort = c instanceof Error ? c : undefined;
				forward();
				this.#sinks.delete(sink);
				this.#updateSubscription();
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

		if (closed !== undefined) {
			sink.final = this.#state.final;
			closeTrackState(sink, closed instanceof Error ? closed : undefined);
		}
	}

	// Recompute from every live sink because an update or close can narrow as well as widen
	// the aggregate. The wire layer observes this signal and emits SUBSCRIBE_UPDATE.
	#updateSubscription(): void {
		const combined = combineSubscriptions(this.#sinks);
		const retained = this.#state.info.peek()?.maxAge;
		if (combined && retained !== undefined) combined.maxAge = Math.min(combined.maxAge ?? 0, retained);
		this.#state.update.set(combined);
	}

	// Mirror a cached source group into a sink. The mirror fills synchronously as the
	// source is written and keeps its own read cursor; frame bytes are shared by
	// reference. Tracked on the entry so eviction can drop it from the sink.
	#mirror(entry: CachedGroup, sink: TrackState): void {
		const dst = entry.group.mirror();
		entry.mirrors.set(sink, dst);
		sink.timeline.set(dst.sequence, { group: dst, time: entry.time });
		void dst.readable().then(() => sink.timelineChanged.update((revision) => revision + 1));
		sink.latest = Math.max(sink.latest ?? 0, dst.sequence);
		sink.groups.mutate((groups) => {
			groups.push(dst);
			groups.sort((a, b) => a.sequence - b.sequence);
		});
	}

	// Drop a cached group's mirror from every sink so no consumer can pin it.
	#evict(entry: CachedGroup): void {
		for (const [sink, mirror] of entry.mirrors) {
			hooks.evictGroup(mirror);
			sink.groups.mutate((groups) => {
				const i = groups.indexOf(mirror);
				if (i >= 0) groups.splice(i, 1);
			});
			if (sink.timeline.get(mirror.sequence)?.group === mirror) sink.timeline.delete(mirror.sequence);
			mirror.close();
		}
		entry.mirrors.clear();
	}

	// Evict cached groups that are closed and older than the cache window.
	#prune(): void {
		const maxAgeMs = this.#state.info.peek()?.maxAge ?? DEFAULT_MAX_AGE_MS;
		const cutoff = performance.now() - maxAgeMs;

		const retained: CachedGroup[] = [];
		for (const entry of this.#cache) {
			if (entry.time >= cutoff || entry.group.closed.peek() === undefined) {
				retained.push(entry);
				continue;
			}
			this.#evict(entry);
		}
		this.#cache = retained;
	}

	// Retain a source group and fan it out to every live sink.
	#publish(group: GroupProducer): void {
		const entry: CachedGroup = { group, time: performance.now(), mirrors: new Map<TrackState, GroupConsumer>() };
		this.#cache.push(entry);
		for (const sink of this.#sinks) this.#mirror(entry, sink);
		// Give held mirrors the new live edge before pruning their timeline entry,
		// so their latency guard can preserve a terminal expiry verdict.
		this.#prune();
	}

	/** Append a new group with the next sequence number. */
	appendGroup(): GroupProducer {
		if (this.#state.closed.peek() !== undefined) throw new Error("track is closed");

		const sequence = this.#sequence;
		const group = new GroupProducer(sequence.next);
		sequence.next = group.sequence + 1;
		this.#publish(group);

		return group;
	}

	/**
	 * Insert an existing group into the track.
	 *
	 * Throws on a sequence that is still cached: a live duplicate would fan out to every
	 * subscriber twice. An aborted incarnation is evicted so a fresh group can serve the
	 * sequence again. Best effort (mirrors Rust): nothing remembers a sequence whose cache
	 * entry is already gone, so a long-evicted sequence is accepted as new.
	 */
	writeGroup(group: GroupProducer) {
		if (this.#state.closed.peek() !== undefined) throw new Error("track is closed");

		const existing = this.#cache.findIndex((entry) => entry.group.sequence === group.sequence);
		if (existing >= 0) {
			const entry = this.#cache[existing];
			if (!(entry.group.closed.peek() instanceof Error)) {
				throw new Error(`duplicate group: sequence=${group.sequence}`);
			}
			this.#evict(entry);
			this.#cache.splice(existing, 1);
		}

		// Only advance the shared counter upward (for appendGroup auto-increment).
		const sequence = this.#sequence;
		if (group.sequence >= sequence.next) {
			sequence.next = group.sequence + 1;
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

		const counter = this.#sequence;
		const sequence = counter.next;
		counter.next = sequence + 1;
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

		const sequence = this.#sequence;
		if (datagram.sequence >= sequence.next) {
			sequence.next = datagram.sequence + 1;
		}
		this.#publishDatagram(datagram);
	}

	/** Close the track and every subscriber, mirroring the abort to their groups. Idempotent. */
	close(abort?: Error) {
		// A clean close declares the final boundary; an abort ends without one.
		if (abort === undefined && this.#state.closed.peek() === undefined) {
			this.#state.final = this.#sequence.next;
			for (const sink of this.#sinks) sink.final = this.#state.final;
		}
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
	#cursor = new Signal<{ start: number; end?: number }>({ start: 0 });
	#enforceLatency = true;
	// Which cursor owns this subscription. Both cursors draw from one buffer, so the
	// first group read commits the track to arrival order and {@link ordered} commits
	// it to sequence order; whichever wins is the only one allowed from here on.
	// Datagrams are a separate cursor and never commit.
	#mode?: "arrival" | "ordered";
	// The group the frame-level helpers are currently draining, acquired through the
	// sequence cursor so frame reads and {@link Ordered.nextGroup} share one floor.
	#frameGroup?: GroupConsumer;

	#drift(): {
		budget: number;
		presentation?: { sequence: number; timestamp: Timestamp };
		end?: number;
	} {
		const { end } = this.#cursor.peek();
		let presentation: { sequence: number; timestamp: Timestamp } | undefined;
		for (const { group } of this.#state.timeline.values()) {
			if (end !== undefined && group.sequence > end) continue;
			if (group.closed.peek() instanceof Error) continue;
			// The edge wants the newest content that exists, so it takes the newest
			// stamped group's latest frame.
			const timestamp = hooks.groupTimestamp(group);
			if (timestamp !== undefined && (!presentation || group.sequence > presentation.sequence)) {
				presentation = { sequence: group.sequence, timestamp: hooks.groupLatest(group) ?? timestamp };
			}
		}

		const requested = this.#state.update.peek()?.maxAge ?? 0;
		const retained = this.#state.info.peek()?.maxAge;
		return {
			budget: this.#enforceLatency
				? retained === undefined
					? requested
					: Math.min(requested, retained)
				: Number.POSITIVE_INFINITY,
			presentation,
			end,
		};
	}

	// The furthest presentation time the group at `sequence` could still reach: where its
	// immediate servable successor begins, or undefined when nothing proves where it
	// stops. An upper bound, deliberately: a frame's duration is not on the wire, so a
	// group's own last timestamp says where it starts presenting, not where it ends.
	// Only the *immediate* successor counts: timestamps need not rise with sequence (a
	// rewind reorders them), so a later stamped group proves nothing about where an
	// unstamped successor will begin, and shrinking the bound is the unsafe direction.
	// An unstamped successor leaves the reach unbounded until it presents a frame.
	#reach(sequence: number, end?: number): number | undefined {
		let successor: GroupConsumer | undefined;
		for (const { group } of this.#state.timeline.values()) {
			if (group.sequence <= sequence) continue;
			if (end !== undefined && group.sequence > end) continue;
			if (group.closed.peek() instanceof Error) continue;
			if (!successor || group.sequence < successor.sequence) successor = group;
		}
		if (!successor) return undefined;
		return hooks.groupTimestamp(successor)?.asMillis();
	}

	// Whether the drift budget says to give up on `group`.
	//
	// Presentation time measures a group by how far it could still reach, not by how far
	// behind it started. Being behind is survivable: priority transmits newer groups first,
	// so a backlog bursts at whatever rate is left over and closes the gap faster than the
	// live edge advances. A group is abandoned only once everything it could still present
	// falls outside the budget.
	//
	// A group's reach is bounded by its nearest successor: it cannot present past where the
	// next group begins. Its own frames say nothing, since a frame's duration is not on the
	// wire, and the candidate needs no timestamp of its own: an empty group is bounded by
	// its stamped successor the same way. The bound is exclusive, so the comparison is `>=`:
	// the freshest frame a group could still hold sits just below its reach, so an age equal
	// to the budget already puts every frame in it strictly past the budget. A zero budget
	// falls out for free. Only timestamps drive expiry; wall-clock reclamation of idle
	// content is the cache's own policy, not the budget's.
	#isStale(
		group: GroupConsumer,
		drift: {
			budget: number;
			presentation?: { sequence: number; timestamp: Timestamp };
			end?: number;
		},
	): boolean {
		const candidate = this.#state.timeline.get(group.sequence);
		if (candidate?.group !== group) return false;

		const reach = this.#reach(group.sequence, drift.end);
		return (
			drift.presentation !== undefined &&
			drift.presentation.sequence > group.sequence &&
			reach !== undefined &&
			drift.presentation.timestamp.asMillis() - reach >= drift.budget
		);
	}

	#guard(group: GroupConsumer): GroupConsumer {
		if (!this.#enforceLatency) return group;
		hooks.expireGroup(group, {
			expired: () => this.#isStale(group, this.#drift()),
			changed: [
				this.#state.groups,
				this.#state.timelineChanged,
				this.#state.update,
				this.#state.info,
				this.#cursor,
				this.#state.closed,
			],
		});
		return group;
	}

	private constructor(name: string, state: TrackState) {
		this.name = name;
		this.#state = state;
		// The cursor's floor is the group the subscription named, or 0. A floor is the
		// only thing a start contributes; {@link Subscription.maxAge} is what asks for
		// data, and delivery skips everything above the floor that the budget convicts.
		this.#cursor.set({ start: state.update.peek()?.startGroup ?? 0 });
	}

	static {
		makeSubscriber = (name, state) => new Subscriber(name, state);
		hooks.tryRecvGroup = (subscriber) => subscriber.#tryRecvGroup();
		hooks.groupChanged = (subscriber, fn) => subscriber.#groupChanged(fn);
		hooks.exemptFetch = (subscriber) => {
			subscriber.#enforceLatency = false;
		};
		// The sequence cursor lives here (it shares the buffer and the drift anchor with
		// the arrival cursor); `Ordered` is the handle that reaches it.
		ordered_ = {
			nextGroup: (subscriber) => subscriber.#nextGroup(),
			readFrame: (subscriber) => subscriber.#readFrame(),
			readFrameSequence: (subscriber) => subscriber.#readFrameSequence(),
			readString: (subscriber) => subscriber.#readString(),
			readJson: (subscriber) => subscriber.#readJson(),
			readBool: (subscriber) => subscriber.#readBool(),
			// Unordered either way, so both handles reach the same cursor.
			recvDatagram: (subscriber) => subscriber.#recvDatagram(),
		};
	}

	// Refuse a read on this handle once `ordered()` has taken the subscription.
	#live() {
		if (this.#mode === "ordered") throw new Error("track is read in sequence order; use the Ordered handle");
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

	/** This subscriber's current options, including defaults and the last {@link update}. */
	get subscription(): Getter<Subscription | undefined> {
		return this.#state.update;
	}

	/** Return the latest group sequence observed on this track, if any. */
	latest(): number | undefined {
		return this.#state.latest;
	}

	/**
	 * The track's exclusive final boundary, known once the producer closes cleanly:
	 * one past the highest sequence produced, or 0 for a track that produced none.
	 * Groups and datagrams share the sequence namespace, so this can exceed
	 * `latest() + 1`. Undefined while the track is live or after an abort.
	 */
	final(): number | undefined {
		return this.#state.final;
	}

	/** Start this subscriber's local read cursor at `sequence`, without changing its wire request. */
	startAt(sequence: number): void {
		this.#cursor.update((cursor) => ({ ...cursor, start: sequence }));
	}

	/**
	 * Cap {@link recvGroup} at `sequence` inclusively, or omit it to remove the cap. Groups
	 * above the cap remain buffered and become readable if the cap is raised. This local
	 * cursor does not change the subscription's wire request.
	 */
	endAt(sequence?: number): void {
		this.#cursor.update((cursor) => ({ ...cursor, end: sequence }));
	}

	/** Close the track (optionally with an error), closing any pending groups. Idempotent. */
	close(abort?: Error) {
		// Settle if we're first (the producer may already have); either way drop anything
		// still buffered. Groups parked at the endAt cap deliberately outlive a clean
		// producer close, so the subscriber leaving is what must release them: closing
		// and clearing wakes a pending read to observe the end instead of hanging.
		closeTrackState(this.#state, abort);
		this.#state.groups.mutate((groups) => {
			for (const group of groups) group.close(abort);
			groups.length = 0;
		});
		this.#frameGroup?.close(abort);
		this.#frameGroup = undefined;
		this.#state.timeline.clear();
	}

	/**
	 * Receive every group on this track exactly once, as it becomes available.
	 *
	 * Groups may arrive out of order or with gaps due to network conditions; unlike
	 * {@link Ordered.nextGroup}, one that arrives after a newer group was already returned
	 * is still delivered. When several groups are buffered, the lowest sequence is
	 * returned first.
	 *
	 * Honors the floor set by {@link startAt} and the cap set by {@link endAt}: a group
	 * beyond the cap stays buffered (not dropped) and is offered once the cap rises, even
	 * after a clean close, without blocking in-range groups that arrive behind it.
	 * A group whose presentation time is further behind the live edge than this
	 * subscriber's `maxAge` is skipped. The default of zero takes the live edge.
	 * The budget remains attached after return, so a pending frame read rejects if a stalled
	 * group becomes stale while newer data advances.
	 *
	 * The first call commits this track to arrival order: {@link ordered} throws afterwards.
	 */
	async recvGroup(): Promise<GroupConsumer | undefined> {
		this.#live();
		this.#mode = "arrival";
		for (;;) {
			const recv = this.#tryRecvGroup();
			switch (recv.kind) {
				case "group":
					return recv.group;
				case "done":
					return undefined;
				case "error":
					throw recv.error;
			}

			// Idle, or parked at the boundary waiting for the cap to rise.
			await Signal.race(this.#state.groups, this.#cursor, this.#state.closed);
		}
	}

	// Package-internal synchronous half of recvGroup. The lite publisher uses this so applying
	// control state, popping the group, and positioning its frames are one JavaScript turn.
	#tryRecvGroup(): Recv {
		const groups = this.#state.groups.peek();
		const { start, end } = this.#cursor.peek();
		while (groups.length > 0 && groups[0].sequence < start) groups.shift()?.close();
		const drift = this.#drift();

		for (;;) {
			// The buffer is sequence-sorted, so an in-range group that arrives behind a
			// beyond-cap one sorts in front of it and is never blocked by it.
			const group = groups[0];
			if (!group || (end !== undefined && group.sequence > end)) break;
			groups.shift();
			if (this.#isStale(group, drift)) {
				group.close();
				continue;
			}
			return { kind: "group", group: this.#guard(group) };
		}

		const group = groups[0];
		const closed = this.#state.closed.peek();
		if (closed instanceof Error) return { kind: "error", error: closed };
		if (closed === undefined) return { kind: "idle" };
		// A group beyond the cap outlives a clean close: it becomes deliverable if
		// the cap rises, so the track isn't over while any are held.
		return group ? { kind: "boundary" } : { kind: "done" };
	}

	// Package-internal readiness half of recvGroup. Each registration fires at most once, and
	// the caller disposes the losers after whichever source wakes it.
	#groupChanged(fn: () => void): Dispose {
		const dispose = [this.#state.groups.changed(fn), this.#cursor.changed(fn), this.#state.closed.changed(fn)];
		return () => {
			for (const close of dispose) close();
		};
	}

	/**
	 * Receive the next datagram in arrival order.
	 *
	 * Datagrams are a separate best-effort channel from groups (see
	 * {@link Producer.appendDatagram}); they share only the sequence namespace. A consumer
	 * that falls too far behind silently loses the oldest datagrams. Read this alongside
	 * {@link recvGroup} (e.g. in a separate loop) to receive both channels concurrently.
	 * The two cursors are independent: a datagram never moves the group cursor.
	 */
	async recvDatagram(): Promise<Datagram | undefined> {
		this.#live();
		return this.#recvDatagram();
	}

	// The datagram cursor, reachable from either handle: unordered by construction, so the
	// choice of group order says nothing about it.
	async #recvDatagram(): Promise<Datagram | undefined> {
		for (;;) {
			const datagrams = this.#state.datagrams.peek();

			// Evict datagrams older than the send-buffer window (also enforced on write), so a
			// reader that stalled skips stale datagrams instead of replaying them.
			const cutoff = performance.now() - MAX_DATAGRAM_AGE_MS;
			while (datagrams.length > 0 && datagrams[0].time < cutoff) datagrams.shift();

			if (datagrams.length > 0) {
				return datagrams.shift()?.datagram;
			}

			const closed = this.#state.closed.peek();
			if (closed instanceof Error) throw closed;
			if (closed !== undefined) return undefined;

			await Signal.race(this.#state.datagrams, this.#state.closed);
		}
	}

	// The sequence cursor behind {@link Ordered}, which owns the only public door to it.
	async #nextGroup(): Promise<GroupConsumer | undefined> {
		for (;;) {
			const groups = this.#state.groups.peek();
			const cursor = this.#cursor.peek();
			const start = Math.max(cursor.start, this.#nextSequence);
			while (groups.length > 0 && groups[0].sequence < start) groups.shift()?.close();
			// One anchor for the whole pass, so walking a backlog off stays linear.
			const drift = this.#drift();

			for (;;) {
				const group = groups[0];
				if (!group || (cursor.end !== undefined && group.sequence > cursor.end)) break;
				groups.shift();
				this.#nextSequence = group.sequence + 1;
				// Every frame this group could still hold is past the budget, so keep
				// scanning: one pass walks a whole backlog off rather than replaying it.
				if (this.#isStale(group, drift)) {
					group.close();
					continue;
				}
				// One cursor: the frame helpers must not keep draining a group this
				// read just moved past, or interleaved reads would run backwards.
				if (this.#frameGroup && this.#frameGroup.sequence < group.sequence) {
					this.#frameGroup.close();
					this.#frameGroup = undefined;
				}
				return this.#guard(group);
			}

			const closed = this.#state.closed.peek();
			if (closed instanceof Error) throw closed;
			// A group parked above the cap stays deliverable even after a clean close
			// (its frames remain buffered), so keep waiting for a cap raise. Only a
			// drained track reports finished.
			if (closed !== undefined && !groups[0]) return undefined;

			await Signal.race(this.#state.groups, this.#cursor, this.#state.closed);
		}
	}

	/**
	 * Reads the next frame across groups, in sequence order.
	 * Treat the returned frame bytes as read-only; they are shared with other consumers.
	 */
	async #readFrame(): Promise<Frame | undefined> {
		const next = await this.#readFrameSequence();
		return next ? { payload: next.payload, timestamp: next.timestamp } : undefined;
	}

	/**
	 * Reads the next frame along with its group and frame sequence numbers.
	 * Treat the returned frame bytes as read-only; they are shared with other consumers.
	 *
	 * Groups are acquired through the same sequence cursor as {@link Ordered.nextGroup},
	 * so frames never run backwards: a late lower-sequence group is skipped, and so is
	 * one every frame of which `maxAge` proves is too old. A group the budget abandons
	 * mid-stall ends cleanly and the cursor resyncs from the next group; an eviction gap
	 * inside a group still surfaces as {@link Lagged}.
	 */
	async #readFrameSequence(): Promise<({ group: number; frame: number } & Frame) | undefined> {
		for (;;) {
			if (!this.#frameGroup) {
				this.#frameGroup = await this.#nextGroup();
				if (!this.#frameGroup) return undefined;
			}
			const group = this.#frameGroup;

			let next: ({ sequence: number } & Frame) | undefined;
			try {
				next = await group.readFrameSequence();
			} catch (err) {
				// The group failed underneath us: resync from the next one, surfacing
				// only what the caller can act on (a gap, or the track's own abort).
				this.#frameGroup = undefined;
				group.close();
				if (err instanceof Lagged) throw err;
				const closed = this.#state.closed.peek();
				if (closed instanceof Error) throw closed;
				continue;
			}

			if (next) {
				return {
					group: group.sequence,
					frame: next.sequence,
					payload: next.payload,
					timestamp: next.timestamp,
				};
			}

			// The group is exhausted (or the budget abandoned its stall); move on.
			this.#frameGroup = undefined;
			group.close();
		}
	}

	/** Reads the next frame and decodes it as a UTF-8 string. */
	async #readString(): Promise<string | undefined> {
		const next = await this.#readFrame();
		if (!next) return undefined;
		return new TextDecoder().decode(next.payload);
	}

	/** Reads the next frame and parses it as JSON. */
	async #readJson(): Promise<unknown | undefined> {
		const next = await this.#readString();
		if (!next) return undefined;
		return JSON.parse(next);
	}

	/** Reads the next frame and decodes it as a one-byte boolean, throwing on a malformed frame. */
	async #readBool(): Promise<boolean | undefined> {
		const next = await this.#readFrame();
		if (!next) return undefined;
		const payload = next.payload;
		if (payload.byteLength !== 1 || !(payload[0] === 0 || payload[0] === 1)) throw new Error("invalid bool frame");
		return payload[0] === 1;
	}

	/**
	 * Read this track's groups in sequence order instead of arrival order.
	 *
	 * Both cursors draw from the same buffer, so a track is read one way or the other: this
	 * hands the subscription to the returned {@link Ordered} and leaves this handle inert.
	 * {@link recvGroup} and {@link recvDatagram} throw afterwards. Throws once a
	 * {@link recvGroup} call has already committed the track to arrival order.
	 *
	 * Datagrams come along: they are a separate cursor either way, so the choice of group
	 * order says nothing about them, and reading them commits nothing.
	 */
	ordered(): Ordered {
		if (this.#mode === "ordered") throw new Error("track is already read in sequence order");
		if (this.#mode === "arrival") throw new Error("track is already read in arrival order");
		this.#mode = "ordered";
		return makeOrdered(this);
	}

	/**
	 * Update this subscription's options (e.g. priority), triggering a SUBSCRIBE_UPDATE to the
	 * publisher. Mirrors the Rust `Subscriber::update`.
	 */
	update(options: Subscription) {
		this.#state.update.set(subscriptionDefaults(options));
	}
}

/**
 * A {@link Subscriber} that reads groups in sequence order.
 *
 * Created by {@link Subscriber.ordered}, which takes the subscription over: the two
 * cursors draw from one buffer, so a track is read one way or the other and never both.
 * Every group this returns has a higher sequence than the last, so a late arrival is
 * skipped rather than delivered out of turn.
 *
 * `maxAge` applies as this cursor reads, exactly as it does on the arrival cursor: a
 * group is skipped once its reach, where its immediate successor begins, is that far
 * behind the newest frame on the track. Nothing weaker convicts it, since the reach is
 * the only proof that every frame it could still hold is past the budget, so a backlog
 * inside the budget is still delivered whole, as a burst in order. The budget follows a
 * group already handed out too: a stalled group's pending frame read rejects once newer
 * content has pulled that far ahead.
 */
export class Ordered {
	/** The track name. */
	readonly name: string;

	#subscriber: Subscriber;

	private constructor(subscriber: Subscriber) {
		this.name = subscriber.name;
		this.#subscriber = subscriber;
	}

	static {
		makeOrdered = (subscriber) => new Ordered(subscriber);
	}

	/** Resolve this track's immutable publisher properties; see {@link Subscriber.info}. */
	info(): Promise<Info> {
		return this.#subscriber.info();
	}

	/** Settles once the track closes; see {@link Producer.closed}. */
	get closed(): GetPromise<Error | null> {
		return this.#subscriber.closed;
	}

	/** This subscriber's current options, including defaults and the last {@link update}. */
	get subscription(): Getter<Subscription | undefined> {
		return this.#subscriber.subscription;
	}

	/** The latest group sequence observed on this track, if any. */
	latest(): number | undefined {
		return this.#subscriber.latest();
	}

	/** The track's exclusive final boundary; see {@link Subscriber.final}. */
	final(): number | undefined {
		return this.#subscriber.final();
	}

	/** Start this cursor at `sequence`, without changing the subscription's wire request. */
	startAt(sequence: number): void {
		this.#subscriber.startAt(sequence);
	}

	/**
	 * Cap this cursor at `sequence` inclusively, or omit it to remove the cap. Groups above
	 * the cap remain buffered and become readable if the cap is raised.
	 */
	endAt(sequence?: number): void {
		this.#subscriber.endAt(sequence);
	}

	/** Update this subscription's options; see {@link Subscriber.update}. */
	update(options: Subscription) {
		this.#subscriber.update(options);
	}

	/** Close the track (optionally with an error), closing any pending groups. Idempotent. */
	close(abort?: Error) {
		this.#subscriber.close(abort);
	}

	/**
	 * Return the next group with a strictly-greater sequence number than the last returned.
	 *
	 * Late arrivals (sequence at or below the last returned) are silently skipped, as is a
	 * group whose every frame is further behind the live edge than `maxAge` (the default of
	 * zero keeps only what nothing newer has superseded). Honors the bounds set by
	 * {@link startAt} and {@link endAt}.
	 */
	nextGroup(): Promise<GroupConsumer | undefined> {
		return ordered_.nextGroup(this.#subscriber);
	}

	/**
	 * Read the next frame across groups, in sequence order.
	 *
	 * Rides the same cursor as {@link nextGroup} and shares this handle's contract: a
	 * buffered backlog is drained in full up to the point `maxAge` proves it useless.
	 * Treat the returned frame bytes as read-only; they are shared with other consumers.
	 */
	readFrame(): Promise<Frame | undefined> {
		return ordered_.readFrame(this.#subscriber);
	}

	/** The same, plus the group and frame sequence numbers the frame came from. */
	readFrameSequence(): Promise<({ group: number; frame: number } & Frame) | undefined> {
		return ordered_.readFrameSequence(this.#subscriber);
	}

	/** Read the next frame and decode it as a UTF-8 string. */
	readString(): Promise<string | undefined> {
		return ordered_.readString(this.#subscriber);
	}

	/** Read the next frame and parse it as JSON. */
	readJson(): Promise<unknown | undefined> {
		return ordered_.readJson(this.#subscriber);
	}

	/** Read the next frame and decode it as a one-byte boolean, throwing on a malformed frame. */
	readBool(): Promise<boolean | undefined> {
		return ordered_.readBool(this.#subscriber);
	}

	/**
	 * Receive the next datagram in arrival order.
	 *
	 * Datagrams are a separate best-effort channel from groups (see
	 * {@link Producer.appendDatagram}); they share only the sequence namespace, and
	 * neither cursor moves the other. Unordered by construction, so this behaves
	 * identically on either handle; it is here so a track carrying both channels needs
	 * one subscription rather than two.
	 */
	recvDatagram(): Promise<Datagram | undefined> {
		return ordered_.recvDatagram(this.#subscriber);
	}
}

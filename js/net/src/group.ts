/**
 * Group role handles: an ordered stream of frames within a track, delivered over one QUIC stream.
 *
 * @module
 */
import { type Instant, now } from "@moq/clock";
import { type Dispose, type GetPromise, type Getter, Once, race, Signal } from "@moq/signals";
import {
	FrameTooLarge,
	GroupTooLarge,
	ProtocolViolation,
	StreamCode,
	StreamError,
	TimestampMismatch,
	TooFarBehind,
} from "./error.ts";
import { Compression, stored } from "./flate/group.ts";
import { type Expiry, type GroupRecover, hooks, type ReadGroupFrame, type Replacement } from "./internal.ts";
import type { Timestamp } from "./time.ts";

/** Maximum bytes of frames in a group. A write that would exceed this aborts with {@link GroupTooLarge}. */
export const MAX_GROUP_CACHE_BYTES = 32 * 1024 * 1024;

/** Maximum number of frames in a group. 8192 is the largest legal group; the 8193rd write is {@link GroupTooLarge}. */
export const MAX_GROUP_FRAMES = 8192;

/**
 * A frame buffered in a group: its presentation {@link Timestamp}, if any, and payload bytes.
 *
 * The timestamp carries its own scale, so a track can pick its units; the wire layer
 * converts it into the track's negotiated timescale.
 */
export interface Frame {
	/** The plain payload, shared across readers even when the track stores compressed bytes. */
	payload: Uint8Array;
	/**
	 * Presentation timestamp, present exactly when the track has a timescale. A frame on a
	 * track without one is untimed and omits it; a write that disagrees with its track is
	 * refused with {@link TimestampMismatch}.
	 */
	timestamp?: Timestamp;
	/**
	 * When the frame landed in this node's group, on the installed clock.
	 *
	 * Every frame a group returns carries the instant it was written, received, or fetched
	 * into it, so a reader judges lateness on that rather than on when it got around to
	 * reading. A write ignores it and stamps its own.
	 */
	arrived?: Instant;
}

/** Options for a sequence-aware frame read. */
export interface ReadOptions {
	/**
	 * The lowest sequence number the caller wants; defaults to the whole group.
	 *
	 * Frames below it are discarded instead of returned. The group still starts at frame 0,
	 * so this is a reader filter, not a missing prefix.
	 */
	from?: number;
}

/** Immutable group metadata. */
export interface Info {
	/** Sequence number of this group within its track. */
	sequence: number;
}

/** Reactive backing state shared by the group producer and one consumer. */
class GroupState {
	readonly sequence: number;
	frames = new Signal<Frame[]>([]);
	closed = new Once<Error | null>();
	total = new Signal<number>(0); // The total number of frames in the group thus far

	// Absolute sequence of the frame at the front of `frames`, advanced by a read: the
	// sequence the next read returns.
	start = 0;
	// The wire offset this copy began at; unlike `start`, reads never advance it.
	offset = 0;

	cacheBytes = 0;
	compressed?: boolean;
	flate?: Compression;
	// Whether the owning track has a timescale, so every frame must carry a timestamp (true)
	// or none may (false). Undefined for a group no track has claimed yet, which takes either.
	timed?: boolean;
	// The first frame's timestamp, retained after reads. Media time only: an untimed group
	// never has one, so it says nothing about whether a frame was written.
	timestamp?: Timestamp;
	// The highest frame PTS, including when the decode-order tail presents earlier.
	latest?: Timestamp;
	// Accepted below the track's eviction watermark, where nothing proves where it starts,
	// so this mirror drives no media-time expiry.
	unverified = false;
	// When the group joined the track this mirror belongs to, at this hop: the wall clock a
	// superseded predecessor ages from. Per track, since one group can join several. A group
	// no track holds arrived when it was created.
	arrived = now();
	// Called with the frame count on every write, synchronously: a track's largest must not
	// trail a write that already returned, as a batched `total` notification would.
	progress = new Set<(total: number) => void>();

	constructor(sequence: number) {
		this.sequence = sequence;
	}
}

// A written frame and when it landed. It keeps the writer's frame rather than copying it, so a
// lazy payload is read only when a reader takes it.
class Landed implements Frame {
	readonly arrived: Instant;
	#frame: Frame;
	constructor(frame: Frame, arrived: Instant) {
		this.#frame = frame;
		this.arrived = arrived;
	}
	get payload(): Uint8Array {
		return this.#frame.payload;
	}
	get timestamp(): Timestamp | undefined {
		return this.#frame.timestamp;
	}
}

// Refuse a frame whose timedness disagrees with the track's.
function checkTimed(timed: boolean | undefined, frame: Frame) {
	if (timed !== undefined && timed !== (frame.timestamp !== undefined)) throw new TimestampMismatch();
}

function appendFrame(state: GroupState, frame: Frame) {
	if (state.closed.peek() !== undefined) throw new Error("group is closed");

	state.timestamp ??= frame.timestamp;
	if (frame.timestamp === undefined) state.latest = undefined;
	else if (state.latest === undefined || state.latest.isBefore(frame.timestamp)) state.latest = frame.timestamp;
	state.cacheBytes += stored(frame)?.payload.length ?? frame.payload.byteLength;
	state.frames.mutate((frames) => {
		frames.push(frame);
	});

	state.total.update((total) => total + 1);
	for (const fn of state.progress) fn(state.total.peek());
}

let makeDemand: (sequence: number, wanted: Getter<boolean>, state: GroupState) => Demand;

/**
 * A watch-only view of a group's mirror readers, including coalesced fetches.
 *
 * @internal The sessions' own fetch coalescing is its only consumer.
 */
export class Demand {
	/** The group sequence this handle watches. */
	readonly sequence: number;
	#wanted: Getter<boolean>;
	#state: GroupState;

	private constructor(sequence: number, wanted: Getter<boolean>, state: GroupState) {
		this.sequence = sequence;
		this.#wanted = wanted;
		this.#state = state;
	}

	static {
		makeDemand = (sequence, wanted, state) => new Demand(sequence, wanted, state);
	}

	/** Whether any mirror reader is attached. */
	get wanted(): Getter<boolean> {
		return this.#wanted;
	}

	/** The group's clean close or abort error. */
	get closed(): GetPromise<Error | null> {
		return this.#state.closed;
	}
}

/**
 * The write side of an ordered stream of frames within a track.
 *
 * @public
 */
export class Producer {
	/** Sequence number of this group within its track. */
	readonly sequence: number;

	#state: GroupState;
	#mirrors?: Set<GroupState>;

	// Whether any mirror reader is attached (see {@link Demand.wanted}). Fetch coalescing watches it to
	// cancel an abandoned download; a group can stay open indefinitely (a catalog or JSON stream),
	// so this is what stops a reader-less fetch instead of the stream ending on its own.
	#wanted = new Signal<boolean>(false);
	#demand: Demand;

	// Exclusive cap on the frame count, set when the track ends inside this group: a write
	// past the track's end is refused rather than served as content that does not exist.
	#limit?: number;

	// When the group was created or last written. Retention ages a group from this,
	// not from its creation, so a group still being filled is never reclaimed.
	#activity = now();

	// The tracks this group joined before its first frame, each checking that frame's
	// timestamp against its neighbors' starts.
	#admits?: ((start: Timestamp) => () => void)[];

	constructor(sequence: number) {
		this.#state = new GroupState(sequence);
		this.sequence = sequence;
		this.#demand = makeDemand(sequence, this.#wanted, this.#state);
	}

	static {
		hooks.bindGroupCompression = (group, compressed) => {
			const state = group.#state;
			if (state.compressed === compressed) return;
			if (state.compressed !== undefined)
				throw new ProtocolViolation("group compression disagrees with its track");
			if (compressed && state.start !== 0) throw new ProtocolViolation("compressed group lacks its prefix");
			state.compressed = compressed;
			if (!compressed) return;
			const flate = new Compression();
			state.flate = flate;
			const frames = state.frames.peek().map((frame) => flate.push(frame));
			state.frames.set(frames);
			state.cacheBytes = flate.stored;
			if (flate.stored > MAX_GROUP_CACHE_BYTES) group.#overflow();
			for (const mirror of group.#mirrors ?? []) {
				mirror.frames.set(frames.slice(mirror.start - state.start));
				mirror.flate = flate;
				mirror.compressed = true;
			}
			if (state.closed.peek() !== undefined) flate.finish();
		};
		hooks.groupOffset = (group) => group.#state.offset;
		hooks.startGroup = (group, start) => {
			if (!Number.isSafeInteger(start) || start < 0) throw new RangeError("invalid group frame start");
			const state = group.#state;
			if (state.total.peek() !== 0 || state.closed.peek() !== undefined || group.#mirrors?.size)
				throw new Error("group already started");
			if (start !== 0 && state.compressed) throw new ProtocolViolation("compressed group lacks its prefix");
			state.start = state.offset = start;
			state.total.set(start);
		};
		hooks.writeGroupFrame = (group, frame, inflated) => group.#writeFrame(frame, inflated);
		hooks.groupCacheBytes = (group) => group.#state.flate?.bytes ?? group.#state.cacheBytes;
		hooks.groupFrameCount = (group) => group.#state.total.peek();
		hooks.groupProgress = (group, fn) => {
			const state = group.#state;
			fn(state.total.peek());
			state.progress.add(fn);
			return () => state.progress.delete(fn);
		};
		hooks.bindGroupTimed = (group, timed) => {
			const state = group.#state;
			if (state.timed === timed) return;
			// Another track already holds it to the opposite timedness, empty or not.
			if (state.timed !== undefined) throw new TimestampMismatch();
			for (const frame of state.frames.peek()) checkTimed(timed, frame);
			state.timed = timed;
		};
		hooks.admitGroupStart = (group, admit) => {
			group.#admits ??= [];
			group.#admits.push(admit);
		};
		hooks.overRefresh = (group, ratio) => {
			const state = group.#state;
			const first = state.frames.peek()[0];
			// Without its first frame the cache cannot measure the group, so it asks for nothing.
			if (state.start !== 0 || first === undefined) return false;
			const head = stored(first)?.payload.length ?? first.payload.byteLength;
			return state.cacheBytes - head > head * ratio;
		};
		hooks.limitGroup = (group, frames) => {
			// A closed group takes no more frames, but one already past the cap still
			// contradicts the end.
			if (group.#state.total.peek() > frames) {
				throw new ProtocolViolation(`group ${group.sequence} already holds a frame past the track's end`);
			}
			group.#limit = frames;
		};
	}

	/**
	 * When the group was created or last written, on the installed clock.
	 *
	 * @internal Idle cache eviction only.
	 */
	get activity(): Instant {
		return this.#activity;
	}

	/**
	 * Settles once the group closes: `null` on a clean close, or the abort {@link Error}.
	 * Peek it synchronously (`undefined` while open), observe it reactively, or `await` it.
	 */
	get closed(): GetPromise<Error | null> {
		return this.#state.closed;
	}

	/** A read handle for this group. */
	consume(): Consumer {
		return this.mirror();
	}

	/**
	 * Create an independent read handle that receives every frame written here.
	 *
	 * Frames written so far are replayed synchronously; later writes and close are teed
	 * in as they happen.
	 *
	 * @internal Track fan-out and fetch coalescing only. Use {@link consume} instead.
	 */
	mirror(): Consumer {
		const dst = new GroupState(this.sequence);
		dst.timestamp = this.#state.timestamp;
		dst.latest = this.#state.latest;
		dst.flate = this.#state.flate;
		dst.compressed = this.#state.compressed;
		// A track restamps its own mirror with when the group joined it.
		dst.arrived = this.#state.arrived;
		for (const frame of this.#state.frames.peek()) appendFrame(dst, frame);
		dst.start = this.#state.start;
		dst.offset = this.#state.offset;
		// The replay only covers what is still buffered, so the count has to come from the
		// source: it is what names a frame's absolute sequence, and what a publisher reads
		// to resolve the live edge.
		dst.total.set(this.#state.total.peek());

		const closed = this.#state.closed.peek();
		if (closed !== undefined) {
			dst.closed.set(closed);
			return makeConsumer(dst);
		}

		this.#mirrors ??= new Set();
		this.#mirrors.add(dst);
		this.#wanted.set(true);

		// Track this mirror's close eagerly (not just lazily on the next write) so demand drops the
		// moment the last reader leaves, letting an abandoned fetch cancel even with no more frames.
		const dispose = dst.closed.subscribe((c) => {
			if (c === undefined) return;
			this.#mirrors?.delete(dst);
			this.#wanted.set((this.#mirrors?.size ?? 0) > 0);
			dispose();
		});

		return makeConsumer(dst);
	}

	/**
	 * Watch the mirror readers attached to this group.
	 *
	 * @internal The sessions' own fetch coalescing is its only consumer.
	 */
	demand(): Demand {
		return this.#demand;
	}

	/** Writes a frame to the group. */
	writeFrame(frame: Frame) {
		this.#writeFrame(frame);
	}

	#writeFrame(frame: Frame, inflated?: number) {
		// Closed first: a write after a clean close at the cap would otherwise take the
		// overflow path, wipe committed frames, and leave readers with an empty success.
		if (this.#state.closed.peek() !== undefined) throw new Error("group is closed");
		// A frame past the cache cap would make the group exceed its budget by itself, so
		// accepting it would report success for a write nothing can ever read. Rust rejects
		// it up front with `Error::FrameTooLarge`; do the same rather than silently dropping it.
		if (frame.payload.byteLength > MAX_GROUP_CACHE_BYTES || (inflated ?? 0) > MAX_GROUP_CACHE_BYTES)
			throw new FrameTooLarge();
		if (inflated !== undefined && (!this.#state.flate || !Number.isSafeInteger(inflated) || inflated < 0))
			throw new ProtocolViolation("unexpected inflated frame size");
		if (this.#state.total.peek() === Number.MAX_SAFE_INTEGER)
			throw new ProtocolViolation("group frame index exceeds the safe integer range");
		checkTimed(this.#state.timed, frame);
		if (this.#limit !== undefined && this.#state.total.peek() >= this.#limit) {
			throw new Error(`frame ${this.#state.total.peek()} is past the track's end`);
		}
		// Frame 0 must fit between its neighbors in every track it joined, checked before
		// any of them records it, so a refusal leaves the group and its tracks as they were.
		let commits: (() => void)[] | undefined;
		if (this.#admits && frame.timestamp !== undefined && this.#state.total.peek() === 0) {
			const start = frame.timestamp;
			commits = this.#admits.map((admit) => admit(start));
		}
		if (
			this.#state.total.peek() - this.#state.offset >= MAX_GROUP_FRAMES ||
			((!this.#state.flate || inflated !== undefined) &&
				this.#state.cacheBytes + frame.payload.byteLength > MAX_GROUP_CACHE_BYTES) ||
			(this.#state.flate?.declared ?? this.#state.cacheBytes) + (inflated ?? frame.payload.length) >
				MAX_GROUP_CACHE_BYTES
		) {
			this.#overflow();
		}

		const arrived = now();
		frame = new Landed(frame, arrived);
		if (this.#state.flate) {
			frame = this.#state.flate.push(frame, inflated);
			if (this.#state.flate.stored > MAX_GROUP_CACHE_BYTES) {
				this.#overflow();
			}
		}
		this.#activity = arrived;
		appendFrame(this.#state, frame);
		if (commits) {
			this.#admits = undefined;
			for (const commit of commits) commit();
		}

		if (this.#mirrors) {
			for (const mirror of this.#mirrors) {
				if (mirror.closed.peek() !== undefined) this.#mirrors.delete(mirror);
				else appendFrame(mirror, frame);
			}
		}
	}

	#overflow(): never {
		const error = new GroupTooLarge();
		this.#state.frames.set([]);
		this.#state.cacheBytes = 0;
		this.#state.flate?.abort(error);
		for (const mirror of this.#mirrors ?? []) {
			mirror.frames.set([]);
			mirror.cacheBytes = 0;
		}
		this.abort(error);
		throw error;
	}

	/** True once the group has been closed. */
	get isClosed(): boolean {
		return this.#state.closed.peek() !== undefined;
	}

	/** Marks the group complete: no more frames will be written. Idempotent. */
	finish() {
		this.#end(null);
	}

	/**
	 * Ends the group with `error`, which readers that have not drained it see. A no-op once the
	 * group finished or aborted: the outcome is already settled.
	 */
	abort(error: Error) {
		this.#end(error);
	}

	#end(outcome: Error | null) {
		if (this.#state.closed.peek() !== undefined) return;
		this.#state.flate?.finish();
		this.#state.closed.set(outcome);

		if (this.#mirrors) {
			for (const mirror of this.#mirrors) {
				if (mirror.closed.peek() === undefined) mirror.closed.set(outcome);
			}
			this.#mirrors.clear();
		}
	}
}

let makeConsumer: (state: GroupState) => Consumer;

// A replacement copy asked for on behalf of one consumer, settled or still out.
interface Attempt {
	/** Asked for a copy that failed, rather than one that stalled. */
	failed: boolean;
	abort: AbortController;
	result: Promise<void>;
	settled: boolean;
	replacement?: Replacement;
}

// `Consumer.expired`: judges the drift budget only while something listens, so racing it once
// per write leaks nothing, and a group nobody watches pays nothing. The verdict latches.
class Expired implements GetPromise<Error> {
	#judge: () => Error | undefined;
	#changed: () => readonly Getter<unknown>[];
	#listeners = new Set<(value: Error | undefined) => void>();
	// The drift inputs watched while anything listens.
	#unwatch?: Dispose[];

	constructor(judge: () => Error | undefined, changed: () => readonly Getter<unknown>[]) {
		this.#judge = judge;
		this.#changed = changed;
	}

	peek(): Error | undefined {
		return this.#judge();
	}

	subscribe(fn: (value: Error | undefined) => void): Dispose {
		this.#listeners.add(fn);
		this.#watch();
		return () => {
			this.#listeners.delete(fn);
			if (this.#listeners.size === 0) this.#stop();
		};
	}

	changed(): Promise<Error | undefined>;
	changed(fn: (value: Error | undefined) => void): Dispose;
	changed(fn?: (value: Error | undefined) => void): Promise<Error | undefined> | Dispose {
		if (!fn) return new Promise((resolve) => this.changed(resolve));
		const dispose = this.subscribe((value) => {
			dispose();
			fn(value);
		});
		return dispose;
	}

	// biome-ignore lint/suspicious/noThenProperty: Expired is intentionally awaitable.
	then<R1 = Error, R2 = never>(
		onFulfilled?: ((value: Error) => R1 | PromiseLike<R1>) | null,
		onRejected?: ((reason: unknown) => R2 | PromiseLike<R2>) | null,
	): PromiseLike<R1 | R2> {
		return new Promise<Error>((resolve) => {
			const value = this.peek();
			if (value) resolve(value);
			else this.changed((value) => value && resolve(value));
		}).then(onFulfilled, onRejected);
	}

	// Watch the current drift inputs again, since an adopted copy brings its own.
	refresh(): void {
		if (!this.#unwatch) return;
		this.#stop();
		this.#watch();
	}

	#watch(): void {
		if (this.#unwatch) return;
		this.#unwatch = this.#changed().map((changed) => changed.subscribe(() => this.#check()));
		// Judging arms the wall-clock wakeup; a verdict already reached notifies after `subscribe`
		// returns, as a signal would.
		if (this.peek()) queueMicrotask(() => this.#check());
	}

	#check(): void {
		const value = this.peek();
		if (!value) return;
		this.#stop();
		const listeners = [...this.#listeners];
		this.#listeners.clear();
		for (const fn of listeners) fn(value);
	}

	#stop(): void {
		for (const dispose of this.#unwatch ?? []) dispose();
		this.#unwatch = undefined;
	}
}

// A consumer can finish from its own expiry verdict or from the shared group state. Keep
// one stable public handle while letting peek() observe either source synchronously, before
// signal subscribers run in the next microtask. The source is re-read on every check, since a
// recovered group moves to another copy's state, and `final` says whether its value ends the
// consumer: a failure recovery can still continue does not.
class CombinedClosed implements GetPromise<Error | null> {
	#preferred: Once<Error | null>;
	#source: () => Once<Error | null>;
	#final: (value: Error | null) => boolean;
	#value = new Once<Error | null>();
	#watched?: Once<Error | null>;
	// The watcher on `#watched`, replaced when a recovery moves to another copy's state.
	#unwatch?: Dispose;
	#dispose: Dispose[] = [];

	constructor(
		preferred: Once<Error | null>,
		source: () => Once<Error | null>,
		final: (value: Error | null) => boolean,
	) {
		this.#preferred = preferred;
		this.#source = source;
		this.#final = final;
		if (this.#sync()) return;
		this.#dispose.push(preferred.changed(() => this.#sync()));
	}

	#sync(): boolean {
		if (this.#value.peek() !== undefined) return true;
		const preferred = this.#preferred.peek();
		const source = this.#source();
		const current = source.peek();
		const value =
			preferred !== undefined ? preferred : current !== undefined && this.#final(current) ? current : undefined;
		if (value === undefined) {
			if (this.#watched !== source) {
				this.#unwatch?.();
				this.#watched = source;
				this.#unwatch = source.changed(() => this.#sync());
			}
			return false;
		}

		this.#value.set(value);
		for (const dispose of this.#dispose) dispose();
		this.#dispose = [];
		this.#unwatch?.();
		this.#unwatch = undefined;
		this.#watched = undefined;
		return true;
	}

	refresh() {
		this.#sync();
	}

	peek(): Error | null | undefined {
		this.#sync();
		return this.#value.peek();
	}

	changed(): Promise<Error | null | undefined>;
	changed(fn: (value: Error | null | undefined) => void): Dispose;
	changed(fn?: (value: Error | null | undefined) => void): Promise<Error | null | undefined> | Dispose {
		this.#sync();
		return fn ? this.#value.changed(fn) : this.#value.changed();
	}

	subscribe(fn: (value: Error | null | undefined) => void): Dispose {
		this.#sync();
		return this.#value.subscribe(fn);
	}

	// biome-ignore lint/suspicious/noThenProperty: CombinedClosed is intentionally awaitable.
	then<R1 = Error | null, R2 = never>(
		onFulfilled?: ((value: Error | null) => R1 | PromiseLike<R1>) | null,
		onRejected?: ((reason: unknown) => R2 | PromiseLike<R2>) | null,
	): PromiseLike<R1 | R2> {
		this.#sync();
		return this.#value.then(onFulfilled, onRejected);
	}
}

/**
 * The read side of an ordered stream of frames within a track.
 *
 * Created internally: obtain one from {@link Producer.consume} or a track subscriber's
 * group reads.
 *
 * @public
 */
export class Consumer {
	/** Sequence number of this group within its track. */
	readonly sequence: number;

	#state: GroupState;
	// The copy it was mirrored from, which that copy's cache evicts.
	readonly #mirrored: GroupState;
	#expiry?: Expiry;
	// Carries the group across its logical track's route changes; see `resume.ts`.
	#recover?: GroupRecover;
	// A replacement asked for while the copy being read stalls, raced against that copy.
	#attempt?: Attempt;
	// The frame a replacement copy resumes at: frames below it were read from an earlier copy.
	#skip = 0;
	// Sticky failure, set once, including a budget that gave up on the group.
	#terminal?: Error;
	#verdict = new Once<Error | null>();
	#closed?: CombinedClosed;
	#expired?: Expired;
	// Closed by its reader, which ends it whatever is left unread.
	#released = false;
	// The arrival of the copy first read, kept once a replacement copy is adopted.
	#arrived?: Instant;

	private constructor(state: GroupState) {
		this.#state = state;
		this.#mirrored = state;
		this.sequence = state.sequence;
	}

	/**
	 * When the group joined its track at this hop, before its first frame, on the installed
	 * clock: its header was received, or it was fetched or created here.
	 *
	 * A reader that gets to the group late still measures from when it landed, as
	 * {@link Frame.arrived} does for each frame. A consumer resumed onto a replacement copy
	 * keeps the original group's arrival.
	 */
	get arrived(): Instant {
		return this.#arrived ?? this.#state.arrived;
	}

	/**
	 * Settles when this consumer reaches a terminal state: `null` after a clean close has no
	 * unread or in-flight frames, or the terminal {@link Error}.
	 * Peek it synchronously (`undefined` while open), observe it reactively, or `await` it.
	 */
	get closed(): GetPromise<Error | null> {
		this.#closed ??= new CombinedClosed(
			this.#verdict,
			() => this.#state.closed,
			(value) =>
				value instanceof Error ? !this.#recovers(value) : this.#released || this.#bufferedFrames().length === 0,
		);
		return this.#closed;
	}

	/**
	 * Settles with a `StreamError` of code `Old` once the subscription's `maxDelay` gives up
	 * on this group, even after every frame was read or the group closed: a publisher still
	 * writing or draining it races its writes against this, and against {@link closed}
	 * settling with an error, and abandons the stream once either does. Peek it synchronously
	 * (`undefined` while within budget), or race it, which stops watching the budget once the
	 * race ends.
	 *
	 * @internal Wire publishers only.
	 */
	get expired(): GetPromise<Error> {
		this.#expired ??= new Expired(
			() => {
				this.#expire();
				return this.#terminal instanceof StreamError && this.#terminal.code === StreamCode.Old
					? this.#terminal
					: undefined;
			},
			() => this.#expiry?.changed ?? [],
		);
		return this.#expired;
	}

	// Whether a failure of the copy being read may still be continued from another route.
	#recovers(failed: Error): boolean {
		return !this.#terminal && this.#recover?.wants(failed) === true;
	}

	static {
		makeConsumer = (state) => new Consumer(state);
		hooks.releaseGroupReader = (group, abort) => group.#release(abort);
		hooks.readsMirrored = (group) => group.#state === group.#mirrored;
		hooks.groupTimestamp = (group) =>
			group.#state.unverified || group.#state.offset !== 0 ? undefined : group.#state.timestamp;
		hooks.groupLatest = (group) => (group.#state.unverified ? undefined : group.#state.latest);
		hooks.unverifyGroup = (group) => {
			group.#state.unverified = true;
		};
		hooks.groupArrived = (group) => group.#state.arrived;
		hooks.groupIndex = (group) => group.#index();
		hooks.groupStarted = (group) =>
			group.#state.total.peek() > group.#state.offset || group.#state.closed.peek() !== undefined;
		hooks.groupFate = (group) => {
			const state = group.#state;
			return () => {
				const closed = state.closed.peek();
				return closed instanceof Error ? closed : undefined;
			};
		};
		hooks.arriveGroup = (group, at) => {
			group.#state.arrived = at;
		};
		hooks.expireGroup = (group, expiry) => {
			group.#expiry = expiry;
		};
		hooks.recoverGroup = (group, recover) => {
			group.#recover = recover;
			// A track's drift scan may have read `closed` already, latching a failure recovery can
			// now continue: derive it again with the recovery in view.
			if (group.#closed?.peek() instanceof Error && group.#verdict.peek() === undefined)
				group.#closed = undefined;
		};
		hooks.readGroupFrame = (group, compressed) => group.#readFrame(0, compressed);
		hooks.evictGroup = (group) => group.#evict();
	}

	// Evaluate the drift budget for a read that has nothing buffered and is about to
	// wait. A group with frames in hand is always drained: the budget bounds a group
	// that has stalled while the live edge moved on, not a reader slower than the wire.
	// Returns true if a verdict was just reached, so the caller re-checks.
	//
	// A reader that took every frame still fails with Old: the producer never finished
	// the group, so a clean end would pass the prefix off as the whole group.
	#expire(): boolean {
		if (this.#terminal) return false;
		if (this.#state.closed.peek() instanceof Error) return false;
		if (!this.#expiry?.expired()) return false;

		this.#terminal = new StreamError(StreamCode.Old, {
			message: "group exceeded the subscription max delay budget",
		});
		if (this.#verdict.peek() === undefined) this.#verdict.set(this.#terminal);
		return true;
	}

	#evict(): boolean {
		// A group read on from another copy is that copy's to reclaim.
		if (this.#state !== this.#mirrored) return false;
		const frames = this.#bufferedFrames();
		if (frames.length > 0) {
			// Unread content is going away either way. If the budget had already given up
			// on this group that is the more specific reason, so ask it first; otherwise
			// the reader simply lagged behind the cache.
			this.#expire();
			if (!this.#terminal) {
				this.#terminal = new TooFarBehind();
				if (this.#verdict.peek() === undefined) this.#verdict.set(this.#terminal);
			}
		}
		if (frames.length > 0) {
			this.#state.start += frames.length;
		}
		this.#state.cacheBytes = 0;
		this.#state.flate = undefined;
		this.#state.frames.set([]);
		// A failed copy holds nothing more to reclaim, and its recovery may still continue the group.
		const closed = this.#state.closed.peek();
		return !(closed instanceof Error && this.#recovers(closed));
	}

	// The next change a parked read waits for.
	#changed(): GetPromise<unknown> {
		const changed: Getter<unknown>[] = [this.#state.frames, this.#state.closed];
		if (this.#expiry) changed.push(...this.#expiry.changed);
		if (this.#recover) changed.push(this.#recover.changed);
		return Signal.race(...changed);
	}

	// Continue from the serving route's copy once the one being read failed with `failed`, or
	// stalled (no `failed`) while a newer route serves. Returns whether the read should look
	// again; throws the error that ends the group once nothing can continue it.
	async #resume(failed: Error | undefined): Promise<boolean> {
		const recover = this.#recover;
		if (!recover?.wants(failed)) return false;
		// A stall's attempt learns nothing of a failure since: ask again with it.
		if (failed && this.#attempt && !this.#attempt.failed) this.#abandon();
		let attempt = this.#attempt;
		if (!attempt) {
			const abort = new AbortController();
			const started: Attempt = { failed: failed !== undefined, abort, result: Promise.resolve(), settled: false };
			started.result = recover.replace(this.#index(), failed, abort.signal).then(
				(replacement) => {
					started.settled = true;
					// Abandoned while it was out: nobody reads it.
					if (abort.signal.aborted) replacement.discard();
					else started.replacement = replacement;
				},
				(err: unknown) => {
					started.settled = true;
					if (abort.signal.aborted) return;
					this.#terminal ??= err instanceof Error ? err : new Error(String(err));
					if (this.#verdict.peek() === undefined) this.#verdict.set(this.#terminal);
				},
			);
			attempt = started;
			this.#attempt = attempt;
		}
		// A stalled copy may still deliver first; a failed one never will.
		if (failed) await attempt.result;
		else await race([attempt.result, this.#changed()]);
		// The copy being read moved first: look again, keeping the attempt out.
		if (!attempt.settled) return true;
		if (this.#attempt === attempt) this.#attempt = undefined;
		// Taken once: a concurrent read awaiting the same attempt looks again instead.
		const replacement = attempt.replacement;
		attempt.replacement = undefined;
		if (!replacement) return true;
		if (!failed && (this.#bufferedFrames().length > 0 || this.#state.closed.peek() !== undefined)) {
			// The copy being read delivered or ended meanwhile; keep reading it.
			replacement.discard();
			return true;
		}
		this.#adopt(replacement);
		return true;
	}

	// The next frame this consumer reads: past what it read, and what a copy it adopted repeats.
	#index(): number {
		return Math.max(this.#state.start, this.#skip);
	}

	// Give up a replacement still being asked for, or one nobody adopted: the group ended without it.
	#abandon(): void {
		const attempt = this.#attempt;
		if (!attempt) return;
		this.#attempt = undefined;
		attempt.abort.abort();
		attempt.replacement?.discard();
	}

	// Read the rest of the group from `replacement`, from the frame this one reached.
	#adopt(replacement: Replacement): void {
		const next = replacement.group.#state;
		const index = this.#index();
		if (next.start > index) {
			replacement.discard();
			this.#terminal ??= new Error(`group ${this.sequence} cannot resume at frame ${index}`);
			if (this.#verdict.peek() === undefined) this.#verdict.set(this.#terminal);
			return;
		}
		replacement.adopt();
		// The copy left behind stops teeing into the state this consumer read.
		if (this.#state.closed.peek() === undefined) this.#state.closed.set(null);
		this.#arrived ??= this.#state.arrived;
		this.#state = next;
		this.#expiry = replacement.expiry;
		this.#skip = index;
		this.#closed?.refresh();
		this.#expired?.refresh();
	}

	// The unread frames. Every look at the buffer goes through here, so readiness, reads and
	// `done` agree on what is left.
	#bufferedFrames(): Frame[] {
		const frames = this.#state.frames.peek();
		// A resumed copy repeats the frames an earlier one already delivered.
		while (this.#state.start < this.#skip && frames.length > 0) {
			frames.shift();
			this.#state.start++;
		}
		return frames;
	}

	#readBufferedFrame(compressed = false): ReadGroupFrame | undefined {
		const frames = this.#bufferedFrames();
		const buffered = frames.shift();
		if (!buffered) return undefined;

		const slice = compressed ? stored(buffered) : undefined;
		let frame: Frame;
		try {
			frame = {
				payload: slice?.payload ?? buffered.payload,
				timestamp: buffered.timestamp,
				arrived: buffered.arrived,
			};
		} catch (error) {
			this.#terminal = error instanceof Error ? error : new Error(String(error));
			if (this.#verdict.peek() === undefined) this.#verdict.set(this.#terminal);
			this.#closed?.refresh();
			throw this.#terminal;
		}
		if (frames.length === 0 && this.#state.closed.peek() !== undefined) this.#state.flate = undefined;
		const sequence = this.#state.start++;
		this.#closed?.refresh();
		return { sequence, frame, inflated: slice?.inflated };
	}

	/** True once no further frames can be read: this handle or the group has closed, and every buffered frame is read. */
	get done(): boolean {
		return (
			this.#terminal !== undefined || (this.#bufferedFrames().length === 0 && (this.#released || this.#settled()))
		);
	}

	/** True once this handle or the group has been closed, regardless of whether buffered frames remain unread. Synchronous complement to the {@link closed} promise. */
	get isClosed(): boolean {
		return this.#terminal !== undefined || this.#released || this.#settled();
	}

	// Whether the copy being read ended, for good: a failure recovery may continue is not an end.
	#settled(): boolean {
		const closed = this.#state.closed.peek();
		return closed !== undefined && !(closed instanceof Error && this.#recovers(closed));
	}

	/**
	 * How many frames the group has held, including any already read.
	 *
	 * It is also the next frame's sequence number, so `frameCount - 1` names the newest
	 * frame written so far. A publisher snapshots it to resolve the track's live edge.
	 */
	get frameCount(): number {
		return this.#state.total.peek();
	}

	/**
	 * Reads the next already-buffered frame without blocking.
	 * Treat the returned frame bytes as read-only; they are shared with other consumers.
	 *
	 * Returns `undefined` when nothing is buffered right now. That is not by itself
	 * end-of-group: check {@link done} to tell "no frame buffered yet" from "finished".
	 */
	tryReadFrame(): Frame | undefined {
		if (this.#terminal) return undefined;
		return this.#readBufferedFrame()?.frame;
	}

	/** Like {@link tryReadFrame} but also reports the frame's sequence number within the group. */
	tryReadFrameSequence(): ({ sequence: number } & Frame) | undefined {
		if (this.#terminal) return undefined;
		const read = this.#readBufferedFrame();
		if (!read) return undefined;
		return { sequence: read.sequence, ...read.frame };
	}

	/** Resolves once {@link readFrame} would not block. */
	async readable(): Promise<void> {
		for (;;) {
			if (this.#terminal) {
				this.#abandon();
				return;
			}
			if (this.#bufferedFrames().length > 0) return;
			const closed = this.#state.closed.peek();
			if (closed instanceof Error && (await this.#resume(closed))) continue;
			if (closed !== undefined) {
				this.#abandon();
				this.#closed?.refresh();
				return;
			}
			if (this.#released || this.#expire()) return;
			if (this.#recover?.wants(undefined)) {
				await this.#resume(undefined);
				continue;
			}
			await this.#changed();
		}
	}

	/**
	 * Reads the next frame from the group.
	 * Treat the returned frame bytes as read-only; they are shared with other consumers.
	 */
	async readFrame(): Promise<Frame | undefined> {
		return (await this.#readFrame(0))?.frame;
	}

	/**
	 * Reads the next frame along with its sequence number within the group.
	 * Treat the returned frame bytes as read-only; they are shared with other consumers.
	 */
	async readFrameSequence(options?: ReadOptions): Promise<({ sequence: number } & Frame) | undefined> {
		const read = await this.#readFrame(options?.from ?? 0);
		if (!read) return undefined;
		return { sequence: read.sequence, ...read.frame };
	}

	async #readFrame(from: number, compressed = false): Promise<ReadGroupFrame | undefined> {
		for (;;) {
			if (this.#terminal) {
				this.#abandon();
				throw this.#terminal;
			}

			const read = this.#readBufferedFrame(compressed);
			if (read) {
				if (read.sequence < from) continue;
				return read;
			}

			const closed = this.#state.closed.peek();
			if (closed instanceof Error) {
				if (await this.#resume(closed)) continue;
				this.#abandon();
				throw closed;
			}
			if (closed !== undefined) {
				// The copy finished the group: a replacement asked for meanwhile is moot.
				this.#abandon();
				this.#closed?.refresh();
				return;
			}

			// Nothing buffered and the group is still open: this wait is the stall the
			// drift budget bounds, and the only place it applies.
			if (this.#expire()) continue;
			// A stall while a newer route serves looks there too.
			if (this.#recover?.wants(undefined)) {
				await this.#resume(undefined);
				continue;
			}
			await this.#changed();
		}
	}

	/** Reads the next frame and decodes its payload as a UTF-8 string. */
	async readString(): Promise<string | undefined> {
		const frame = await this.readFrame();
		return frame ? new TextDecoder().decode(frame.payload) : undefined;
	}

	/** Reads the next frame and parses its payload as JSON. */
	async readJson(): Promise<unknown | undefined> {
		const frame = await this.readString();
		return frame !== undefined ? JSON.parse(frame) : undefined;
	}

	/** Reads the next frame and decodes its payload as a one-byte boolean. */
	async readBool(): Promise<boolean | undefined> {
		const frame = await this.readFrame();
		if (!frame) return undefined;
		const payload = frame.payload;
		if (payload.byteLength !== 1 || !(payload[0] === 0 || payload[0] === 1)) throw new Error("invalid bool frame");
		return payload[0] === 1;
	}

	/**
	 * Release this handle, like dropping a Rust consumer. Never changes the group for other
	 * readers or the producer. It drains what it buffered and then ends. Idempotent.
	 */
	close(): void {
		this.#release(undefined);
	}

	// Release this reader, failing it with `abort` if given. Every reader owns its mirrored
	// state, so closing that state drops its demand without touching the producer.
	#release(abort: Error | undefined): void {
		// A copy failure recovery was continuing past ends with the abort instead.
		if (abort && this.#recover && this.#state.closed.peek() instanceof Error) {
			this.#terminal ??= abort;
			if (this.#verdict.peek() === undefined) this.#verdict.set(this.#terminal);
		}
		this.#abandon();
		this.#recover?.close?.();
		this.#recover = undefined;
		// Closing is terminal for this reader, even with frames still unread.
		this.#released = true;
		if (this.#state.closed.peek() === undefined) this.#state.closed.set(abort ?? null);
		this.#closed?.refresh();
	}
}

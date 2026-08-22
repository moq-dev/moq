/**
 * Group role handles: an ordered stream of frames within a track, delivered over one QUIC stream.
 *
 * @module
 */
import { type Dispose, type GetPromise, type Getter, Once, Signal } from "@moq/signals";
import { type GroupPosition, hooks, type ReadGroupFrame } from "./internal.ts";
import { Timestamp } from "./time.ts";

/** Maximum bytes of frames cached in a group before old frames are evicted from the front. */
export const MAX_GROUP_CACHE_BYTES = 32 * 1024 * 1024;

/** Maximum number of frames cached in a group before old frames are evicted from the front. */
export const MAX_GROUP_FRAMES = 1024;

/**
 * A frame buffered in a group: its presentation {@link Timestamp} and payload bytes.
 *
 * The timestamp carries its own scale, so a track can pick its units; the wire layer
 * converts it into the track's negotiated timescale.
 */
export interface Frame {
	/** The frame payload. */
	payload: Uint8Array;
	/**
	 * Presentation timestamp. Required: for a payload with no presentation time of its own
	 * (a JSON catalog, control state) pass {@link Timestamp.now} explicitly.
	 */
	timestamp: Timestamp;
}

/** A frame paired with its arrival clock for wall-clock drift measurement. */
interface BufferedFrame {
	frame: Frame;
	activity: number;
}

/** Immutable group metadata. */
export interface Info {
	/** Sequence number of this group within its track. */
	sequence: number;
}

/**
 * Thrown by a frame read when the reader fell behind the group's eviction window: frames
 * it had not yet read were dropped to stay under the cache cap, so the stream has a gap.
 */
export class Lagged extends Error {
	constructor() {
		super("lagged: frames were evicted before being read");
		this.name = "Lagged";
	}
}

/** Reactive backing state shared by the group producer and one consumer. */
class GroupState {
	readonly sequence: number;
	frames = new Signal<BufferedFrame[]>([]);
	closed = new Once<Error | null>();
	total = new Signal<number>(0); // The total number of frames in the group thus far

	// Frames evicted from the front by the cache cap. A reader that had not consumed
	// them has a gap, so its next read throws Lagged rather than skipping silently.
	offset = 0;
	cacheBytes = 0;
	// The first frame's timestamp, retained after reads and front eviction.
	timestamp?: Timestamp;
	// The newest frame's timestamp: where a reader that has taken every frame sits.
	latest?: Timestamp;
	// When the group last accepted a frame. A group is silent from here, not from
	// whenever it was created.
	activity?: number;

	constructor(sequence: number) {
		this.sequence = sequence;
	}
}

function appendFrame(state: GroupState, frame: Frame, activity: number) {
	if (state.closed.peek() !== undefined) throw new Error("group is closed");

	state.timestamp ??= frame.timestamp;
	state.latest = frame.timestamp;
	state.activity = activity;
	state.cacheBytes += frame.payload.byteLength;
	state.frames.mutate((frames) => {
		frames.push({ frame, activity });

		while (frames.length > MAX_GROUP_FRAMES || state.cacheBytes > MAX_GROUP_CACHE_BYTES) {
			const evicted = frames.shift();
			if (!evicted) break;
			state.cacheBytes -= evicted.frame.payload.byteLength;
			state.offset++;
		}
	});

	state.total.update((total) => total + 1);
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

	// Whether any mirror reader is attached (see {@link used}). Fetch coalescing watches it to
	// cancel an abandoned download; a group can stay open indefinitely (a catalog or JSON stream),
	// so this is what stops a reader-less fetch instead of the stream ending on its own.
	#used = new Signal<boolean>(false);

	constructor(sequence: number) {
		this.#state = new GroupState(sequence);
		this.sequence = sequence;
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
		return makeConsumer(this.#state);
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
		dst.activity = this.#state.activity;
		for (const { frame, activity } of this.#state.frames.peek()) appendFrame(dst, frame, activity);
		dst.offset = this.#state.offset;

		const closed = this.#state.closed.peek();
		if (closed !== undefined) {
			dst.closed.set(closed);
			return makeConsumer(dst);
		}

		this.#mirrors ??= new Set();
		this.#mirrors.add(dst);
		this.#used.set(true);

		// Track this mirror's close eagerly (not just lazily on the next write) so demand drops the
		// moment the last reader leaves, letting an abandoned fetch cancel even with no more frames.
		const dispose = dst.closed.subscribe((c) => {
			if (c === undefined) return;
			this.#mirrors?.delete(dst);
			this.#used.set((this.#mirrors?.size ?? 0) > 0);
			dispose();
		});

		return makeConsumer(dst);
	}

	/**
	 * Whether any mirror reader is currently attached.
	 *
	 * Pairs with {@link unused}. Fetch coalescing watches it to cancel a download once every reader
	 * has gone: a group can stay open indefinitely (a catalog track, a JSON stream), so it can't
	 * rely on the stream ending on its own.
	 *
	 * @internal Track fan-out and fetch coalescing only.
	 */
	get used(): Getter<boolean> {
		return this.#used;
	}

	/**
	 * Resolves once no mirror reader remains (or the group closes).
	 *
	 * @internal Track fan-out and fetch coalescing only.
	 */
	async unused(): Promise<void> {
		while (this.#used.peek() && this.#state.closed.peek() === undefined) {
			await Signal.race(this.#used, this.#state.closed);
		}
	}

	/** Writes a frame to the group. */
	writeFrame(frame: Frame) {
		const activity = performance.now();
		appendFrame(this.#state, frame, activity);

		if (this.#mirrors) {
			for (const mirror of this.#mirrors) {
				if (mirror.closed.peek() !== undefined) this.#mirrors.delete(mirror);
				else appendFrame(mirror, frame, activity);
			}
		}
	}

	/** Write a string as a single UTF-8 encoded frame, stamped with {@link Timestamp.now}. */
	writeString(str: string) {
		this.writeFrame({ payload: new TextEncoder().encode(str), timestamp: Timestamp.now() });
	}

	/** Write a value as a single JSON-encoded frame, stamped with {@link Timestamp.now}. */
	writeJson(json: unknown) {
		this.writeString(JSON.stringify(json));
	}

	/** Write a boolean as a single one-byte frame, stamped with {@link Timestamp.now}. */
	writeBool(bool: boolean) {
		this.writeFrame({ payload: new Uint8Array([bool ? 1 : 0]), timestamp: Timestamp.now() });
	}

	/** True once the group has been closed. */
	get isClosed(): boolean {
		return this.#state.closed.peek() !== undefined;
	}

	/** Closes the group, optionally with an error to abort readers. */
	close(abort?: Error) {
		if (this.#state.closed.peek() !== undefined) return;
		this.#state.closed.set(abort ?? null);

		if (this.#mirrors) {
			for (const mirror of this.#mirrors) {
				if (mirror.closed.peek() === undefined) mirror.closed.set(abort ?? null);
			}
			this.#mirrors.clear();
		}
	}
}

let makeConsumer: (state: GroupState) => Consumer;

// A consumer can finish from its own expiry verdict or from the shared group state. Keep
// one stable public handle while letting peek() observe either source synchronously, before
// signal subscribers run in the next microtask.
class CombinedClosed implements GetPromise<Error | null> {
	#preferred: Once<Error | null>;
	#source: Once<Error | null>;
	#sourceCleanReady: () => boolean;
	#value = new Once<Error | null>();
	#dispose?: Dispose;

	constructor(preferred: Once<Error | null>, source: Once<Error | null>, sourceCleanReady: () => boolean) {
		this.#preferred = preferred;
		this.#source = source;
		this.#sourceCleanReady = sourceCleanReady;
		if (this.#sync()) return;

		const dispose = [preferred.changed(() => this.#sync()), source.changed(() => this.#sync())];
		this.#dispose = () => {
			for (const close of dispose) close();
		};
	}

	#sync(): boolean {
		if (this.#value.peek() !== undefined) return true;
		const preferred = this.#preferred.peek();
		const source = this.#source.peek();
		const value =
			preferred !== undefined
				? preferred
				: source instanceof Error || this.#sourceCleanReady()
					? source
					: undefined;
		if (value === undefined) return false;

		this.#value.set(value);
		this.#dispose?.();
		this.#dispose = undefined;
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
	#expiry?: { expired: (at: GroupPosition) => boolean; changed: readonly Getter<unknown>[] };
	// Sticky verdicts, set once. `#ended` is a drained group the budget gave up on,
	// which is indistinguishable from one that ended: nothing was lost, so it reads as
	// the end of the group. `#terminal` is a failure, including a budget that gave up
	// while content was still unread.
	#ended = false;
	#terminal?: Error;
	#verdict = new Once<Error | null>();
	#closed?: CombinedClosed;
	#pendingFrames = 0;

	private constructor(state: GroupState) {
		this.#state = state;
		this.sequence = state.sequence;
	}

	/**
	 * Settles when this consumer reaches a terminal state: `null` after a clean close has no
	 * unread or in-flight frames, or the terminal {@link Error}.
	 * Peek it synchronously (`undefined` while open), observe it reactively, or `await` it.
	 */
	get closed(): GetPromise<Error | null> {
		this.#closed ??= new CombinedClosed(
			this.#verdict,
			this.#state.closed,
			() => this.#state.frames.peek().length === 0 && this.#pendingFrames === 0,
		);
		return this.#closed;
	}

	static {
		makeConsumer = (state) => new Consumer(state);
		hooks.groupTimestamp = (group) => group.#state.timestamp;
		hooks.expireGroup = (group, expiry) => {
			group.#expiry = expiry;
		};
		hooks.guardGroup = (group, operation, at) => group.#guard(operation, at);
		hooks.readGroupFrame = (group) => group.#readFramePosition(true);
		hooks.evictGroup = (group) => {
			group.#evict();
		};
	}

	/// Where this cursor sits, for the budget to measure against the live edge.
	//
	// A group is not late because it *started* long ago; what can be late is the content
	// its reader has yet to take. So a reader that has drained the group sits at the
	// group's newest frame, not its first.
	#position(): GroupPosition {
		const next = this.#state.frames.peek()[0];
		return {
			presentation: next?.frame.timestamp ?? this.#state.latest,
			activity: next?.activity ?? this.#state.activity,
		};
	}

	// Evaluate the drift budget for a read that has nothing buffered and is about to
	// wait. A group with frames in hand is always drained: the budget bounds a group
	// that has stalled while the live edge moved on, not a reader slower than the wire.
	// Returns true if a verdict was just reached, so the caller re-checks.
	//
	// `unread` is for a caller that still holds content the reader has not seen (a
	// publisher part-way through writing a frame): giving up there is a truncation, so
	// it fails rather than ending.
	#expire(unread = false, at = this.#position()): boolean {
		if (this.#terminal || this.#ended) return false;
		if (this.#state.closed.peek() instanceof Error) return false;
		if (!this.#expiry?.expired(at)) return false;

		if (unread) {
			this.#terminal = new Error("group exceeded the subscription latency budget");
		} else {
			this.#ended = true;
		}
		if (this.#verdict.peek() === undefined) this.#verdict.set(this.#terminal ?? null);
		return true;
	}

	#guard<T>(operation: Promise<T>, at = this.#position()): Promise<T> {
		if (this.#expire(true, at)) return Promise.reject(this.#terminal);
		if (this.#terminal) return Promise.reject(this.#terminal);
		const expiry = this.#expiry;
		if (!expiry) return operation;

		return new Promise<T>((resolve, reject) => {
			let settled = false;
			const disposes: Dispose[] = [];
			const finish = (fn: () => void) => {
				if (settled) return;
				settled = true;
				for (const dispose of disposes) dispose();
				fn();
			};
			const check = () => {
				this.#expire(true, at);
				if (this.#terminal) {
					const error = this.#terminal;
					finish(() => reject(error));
				}
			};

			for (const changed of expiry.changed) disposes.push(changed.subscribe(check));
			operation.then(
				(value) => finish(() => resolve(value)),
				(error: unknown) => finish(() => reject(error)),
			);
			check();
		});
	}

	#evict() {
		const frames = this.#state.frames.peek();
		if (frames.length > 0) {
			// Unread content is going away either way. If the budget had already given up
			// on this group that is the more specific reason, so ask it first; otherwise
			// the reader simply lagged behind the cache.
			this.#expire(true);
			if (!this.#terminal && !this.#ended) {
				this.#terminal = new Lagged();
				if (this.#verdict.peek() === undefined) this.#verdict.set(this.#terminal);
			}
		}
		this.#state.offset += frames.length;
		this.#state.cacheBytes = 0;
		this.#state.frames.set([]);
	}

	async #changed(): Promise<void> {
		if (!this.#expiry) {
			await Signal.race(this.#state.frames, this.#state.closed);
			return;
		}
		await Signal.race(this.#state.frames, this.#state.closed, ...this.#expiry.changed);
	}

	#readBufferedFrame(pending = false): ReadGroupFrame | undefined {
		const frames = this.#state.frames.peek();
		const buffered = frames.shift();
		if (!buffered) return undefined;

		this.#state.cacheBytes -= buffered.frame.payload.byteLength;
		if (pending) this.#pendingFrames++;
		let completed = false;
		return {
			sequence: this.#state.total.peek() - frames.length - 1,
			frame: buffered.frame,
			position: { presentation: buffered.frame.timestamp, activity: buffered.activity },
			complete: () => {
				if (completed) return;
				completed = true;
				if (pending) this.#pendingFrames--;
				this.#closed?.refresh();
			},
		};
	}

	/** True once no further frames can be read: the group has closed and every buffered frame is read. */
	get done(): boolean {
		return (
			this.#terminal !== undefined ||
			this.#ended ||
			(this.#state.frames.peek().length === 0 && this.#state.closed.peek() !== undefined)
		);
	}

	/** True once the group has been closed, regardless of whether buffered frames remain unread. Synchronous complement to the {@link closed} promise. */
	get isClosed(): boolean {
		return this.#terminal !== undefined || this.#ended || this.#state.closed.peek() !== undefined;
	}

	/** True if frames were evicted from the front of this group before being read. */
	get skipped(): boolean {
		return this.#state.offset > 0;
	}

	/**
	 * Reads the next already-buffered frame without blocking.
	 * Treat the returned frame bytes as read-only; they are shared with other consumers.
	 *
	 * Returns `undefined` when nothing is buffered right now. That is not by itself
	 * end-of-group: check {@link done} to tell "no frame buffered yet" from "finished".
	 */
	tryReadFrame(): Frame | undefined {
		if (this.#terminal || this.#ended) return undefined;
		const read = this.#readBufferedFrame();
		read?.complete();
		return read?.frame;
	}

	/** Like {@link tryReadFrame} but also reports the frame's sequence number within the group. */
	tryReadFrameSequence(): ({ sequence: number } & Frame) | undefined {
		if (this.#terminal || this.#ended) return undefined;
		const read = this.#readBufferedFrame();
		if (!read) return undefined;
		read.complete();
		return { sequence: read.sequence, payload: read.frame.payload, timestamp: read.frame.timestamp };
	}

	/** Resolves once {@link readFrame} would not block. */
	async readable(): Promise<void> {
		for (;;) {
			if (this.#terminal || this.#ended) return;
			if (this.#state.frames.peek().length > 0) return;
			if (this.#state.closed.peek() !== undefined) {
				this.#closed?.refresh();
				return;
			}
			if (this.#expire()) return;
			await this.#changed();
		}
	}

	/**
	 * Reads the next frame from the group.
	 * Treat the returned frame bytes as read-only; they are shared with other consumers.
	 */
	async readFrame(): Promise<Frame | undefined> {
		return (await this.#readFramePosition())?.frame;
	}

	async #readFramePosition(pending = false): Promise<ReadGroupFrame | undefined> {
		for (;;) {
			if (this.#terminal) throw this.#terminal;
			if (this.#ended) return;
			if (this.#state.offset > 0) throw new Lagged();

			const read = this.#readBufferedFrame(pending);
			if (read) {
				if (!pending) read.complete();
				return read;
			}

			const closed = this.#state.closed.peek();
			if (closed instanceof Error) throw closed;
			if (closed !== undefined) {
				this.#closed?.refresh();
				return;
			}

			// Nothing buffered and the group is still open: this wait is the stall the
			// drift budget bounds, and the only place it applies.
			if (this.#expire()) continue;
			await this.#changed();
		}
	}

	/**
	 * Reads the next frame along with its sequence number within the group.
	 * Treat the returned frame bytes as read-only; they are shared with other consumers.
	 */
	async readFrameSequence(): Promise<({ sequence: number } & Frame) | undefined> {
		const read = await this.#readFramePosition();
		if (!read) return undefined;
		return { sequence: read.sequence, payload: read.frame.payload, timestamp: read.frame.timestamp };
	}

	/** Reads the next frame and decodes its payload as a UTF-8 string. */
	async readString(): Promise<string | undefined> {
		const frame = await this.readFrame();
		return frame ? new TextDecoder().decode(frame.payload) : undefined;
	}

	/** Reads the next frame and parses its payload as JSON. */
	async readJson(): Promise<unknown | undefined> {
		const frame = await this.readString();
		return frame ? JSON.parse(frame) : undefined;
	}

	/** Reads the next frame and decodes its payload as a one-byte boolean. */
	async readBool(): Promise<boolean | undefined> {
		const frame = await this.readFrame();
		return frame ? frame.payload[0] === 1 : undefined;
	}

	/** Closes the group, optionally with an error to abort readers. Idempotent. */
	close(abort?: Error) {
		if (this.#state.closed.peek() !== undefined) return; // already closed
		this.#state.closed.set(abort ?? null);
	}
}

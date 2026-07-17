/**
 * Group role handles: an ordered stream of frames within a track, delivered over one QUIC stream.
 *
 * @module
 */
import { type GetPromise, Once, Signal } from "@moq/signals";
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

/** Immutable group metadata. */
export interface Info {
	/** Sequence number of this group within its track. */
	sequence: number;
}

/**
 * Thrown by a frame read when the reader fell behind the group's eviction window: frames
 * it had not yet read were dropped to stay under the cache cap, so the stream has a gap.
 */
export class CacheFull extends Error {
	constructor() {
		super("group cache full: frames were evicted before being read");
		this.name = "CacheFull";
	}
}

/** Reactive backing state shared by the group producer and one consumer. */
class GroupState {
	readonly sequence: number;
	frames = new Signal<Frame[]>([]);
	closed = new Once<Error | null>();
	total = new Signal<number>(0); // The total number of frames in the group thus far

	// Frames evicted from the front by the cache cap. A reader that had not consumed
	// them has a gap, so its next read throws CacheFull rather than skipping silently.
	offset = 0;
	cacheBytes = 0;

	constructor(sequence: number) {
		this.sequence = sequence;
	}
}

function appendFrame(state: GroupState, frame: Frame) {
	if (state.closed.peek() !== undefined) throw new Error("group is closed");

	state.cacheBytes += frame.payload.byteLength;
	state.frames.mutate((frames) => {
		frames.push(frame);

		while (frames.length > MAX_GROUP_FRAMES || state.cacheBytes > MAX_GROUP_CACHE_BYTES) {
			const evicted = frames.shift();
			if (!evicted) break;
			state.cacheBytes -= evicted.payload.byteLength;
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
		for (const frame of this.#state.frames.peek()) appendFrame(dst, frame);
		dst.offset = this.#state.offset;

		const closed = this.#state.closed.peek();
		if (closed !== undefined) {
			dst.closed.set(closed);
			return makeConsumer(dst);
		}

		this.#mirrors ??= new Set();
		this.#mirrors.add(dst);
		return makeConsumer(dst);
	}

	/** Writes a frame to the group. */
	writeFrame(frame: Frame) {
		appendFrame(this.#state, frame);

		if (this.#mirrors) {
			for (const mirror of this.#mirrors) {
				if (mirror.closed.peek() !== undefined) this.#mirrors.delete(mirror);
				else appendFrame(mirror, frame);
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

	private constructor(state: GroupState) {
		this.#state = state;
		this.sequence = state.sequence;
	}

	/**
	 * Settles once the group closes: `null` on a clean close, or the abort {@link Error}.
	 * Peek it synchronously (`undefined` while open), observe it reactively, or `await` it.
	 */
	get closed(): GetPromise<Error | null> {
		return this.#state.closed;
	}

	static {
		makeConsumer = (state) => new Consumer(state);
	}

	#readBufferedFrame(): { sequence: number; frame: Frame } | undefined {
		const frames = this.#state.frames.peek();
		const frame = frames.shift();
		if (!frame) return undefined;

		this.#state.cacheBytes -= frame.payload.byteLength;
		return { sequence: this.#state.total.peek() - frames.length - 1, frame };
	}

	/** True once no further frames can be read: the group has closed and every buffered frame is read. */
	get done(): boolean {
		return this.#state.frames.peek().length === 0 && this.#state.closed.peek() !== undefined;
	}

	/** True once the group has been closed, regardless of whether buffered frames remain unread. Synchronous complement to the {@link closed} promise. */
	get isClosed(): boolean {
		return this.#state.closed.peek() !== undefined;
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
		const read = this.#readBufferedFrame();
		return read?.frame;
	}

	/** Like {@link tryReadFrame} but also reports the frame's sequence number within the group. */
	tryReadFrameSequence(): ({ sequence: number } & Frame) | undefined {
		const read = this.#readBufferedFrame();
		if (!read) return undefined;
		return { sequence: read.sequence, payload: read.frame.payload, timestamp: read.frame.timestamp };
	}

	/** Resolves once {@link readFrame} would not block. */
	async readable(): Promise<void> {
		for (;;) {
			if (this.#state.frames.peek().length > 0) return;
			if (this.#state.closed.peek() !== undefined) return;
			await Signal.race(this.#state.frames, this.#state.closed);
		}
	}

	/**
	 * Reads the next frame from the group.
	 * Treat the returned frame bytes as read-only; they are shared with other consumers.
	 */
	async readFrame(): Promise<Frame | undefined> {
		for (;;) {
			if (this.#state.offset > 0) throw new CacheFull();

			const read = this.#readBufferedFrame();
			if (read) return read.frame;

			const closed = this.#state.closed.peek();
			if (closed instanceof Error) throw closed;
			if (closed !== undefined) return;

			await Signal.race(this.#state.frames, this.#state.closed);
		}
	}

	/**
	 * Reads the next frame along with its sequence number within the group.
	 * Treat the returned frame bytes as read-only; they are shared with other consumers.
	 */
	async readFrameSequence(): Promise<({ sequence: number } & Frame) | undefined> {
		for (;;) {
			if (this.#state.offset > 0) throw new CacheFull();

			const read = this.#readBufferedFrame();
			if (read) return { sequence: read.sequence, payload: read.frame.payload, timestamp: read.frame.timestamp };

			const closed = this.#state.closed.peek();
			if (closed instanceof Error) throw closed;
			if (closed !== undefined) return;

			await Signal.race(this.#state.frames, this.#state.closed);
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

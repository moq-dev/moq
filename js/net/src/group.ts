import { Signal } from "@moq/signals";
import { Timestamp } from "./time.ts";

/** Maximum bytes of frames cached in a group before old frames are evicted from the front. */
export const MAX_GROUP_CACHE_BYTES = 32 * 1024 * 1024;

/** Maximum number of frames cached in a group before old frames are evicted from the front. */
export const MAX_GROUP_FRAMES = 1024;

/**
 * A frame buffered in a {@link Group}: its presentation {@link Timestamp} and payload bytes.
 *
 * The timestamp carries its own scale, so a track can pick its units; the wire layer
 * converts it into the track's negotiated timescale.
 */
export interface Frame {
	/** The frame payload. */
	data: Uint8Array;
	/**
	 * Presentation timestamp. Required: for data with no presentation time of its own
	 * (a JSON catalog, control state) pass {@link Timestamp.now} explicitly.
	 */
	timestamp: Timestamp;
}

/**
 * Thrown by a frame read when the reader fell behind the group's eviction window: frames
 * it had not yet read were dropped to stay under the cache cap, so the stream has a gap.
 * Mirrors the Rust `Error::CacheFull`. Skipping the gap silently would corrupt decoding,
 * so the reader must surface this instead.
 */
export class CacheFull extends Error {
	constructor() {
		super("group cache full: frames were evicted before being read");
		this.name = "CacheFull";
	}
}

/** Reactive backing state for a {@link Group}: buffered frames, a closed flag, and the running frame count. */
export class GroupState {
	frames = new Signal<Frame[]>([]);
	closed = new Signal<boolean | Error>(false);
	total = new Signal<number>(0); // The total number of frames in the group thus far

	// Frames evicted from the front by the cache cap. A reader that had not consumed
	// them has a gap, so its next read throws CacheFull rather than skipping silently.
	offset = 0;
}

/** An ordered stream of frames within a track, delivered over a single QUIC stream. */
export class Group {
	/** Sequence number of this group within its track. */
	readonly sequence: number;

	/** Reactive backing state. */
	state = new GroupState();

	/** Resolves with the abort error (or undefined) once closed. */
	readonly closed: Promise<Error | undefined>;

	// Downstream copies that receive every frame written here, synchronously. Used by
	// TrackProducer to fan one source group out to per-subscriber groups.
	#mirrors?: Set<Group>;

	// Running byte total of the frames currently cached, for the eviction cap.
	#cacheBytes = 0;

	constructor(sequence: number) {
		this.sequence = sequence;

		// Cache the closed promise to avoid recreating it every time.
		this.closed = new Promise((resolve) => {
			const dispose = this.state.closed.subscribe((closed) => {
				if (!closed) return;
				resolve(closed instanceof Error ? closed : undefined);
				dispose();
			});
		});
	}

	/** Writes a frame to the group. */
	writeFrame(frame: Frame) {
		if (this.state.closed.peek()) throw new Error("group is closed");

		const { data } = frame;

		this.#cacheBytes += data.byteLength;
		this.state.frames.mutate((frames) => {
			frames.push(frame);

			// Bound an unbounded (e.g. never-closed) group: drop the oldest frames once
			// over either cap. A consumer too far behind silently skips them.
			while (frames.length > MAX_GROUP_FRAMES || this.#cacheBytes > MAX_GROUP_CACHE_BYTES) {
				const evicted = frames.shift();
				if (!evicted) break;
				this.#cacheBytes -= evicted.data.byteLength;
				this.state.offset++;
			}
		});

		this.state.total.update((total) => total + 1);

		// Tee into live mirrors, dropping any the consumer has already closed.
		if (this.#mirrors) {
			for (const mirror of this.#mirrors) {
				if (mirror.state.closed.peek()) this.#mirrors.delete(mirror);
				else mirror.writeFrame(frame);
			}
		}
	}

	/**
	 * Create an independent copy that receives every frame written to this group.
	 *
	 * Frames written so far are replayed synchronously; later writes (and the close)
	 * are teed in as they happen. The copy has its own read cursor, so consumers never
	 * steal frames from each other. Internal to {@link TrackProducer} fan-out.
	 */
	mirror(): Group {
		const dst = new Group(this.sequence);
		for (const frame of this.state.frames.peek()) dst.writeFrame(frame);
		// Inherit the evicted prefix: frames dropped before this copy was made are a gap
		// for its reader too, so reading them throws CacheFull.
		dst.state.offset = this.state.offset;

		const closed = this.state.closed.peek();
		if (closed) {
			dst.close(closed instanceof Error ? closed : undefined);
			return dst;
		}

		this.#mirrors ??= new Set();
		this.#mirrors.add(dst);
		return dst;
	}

	/** Write a string as a single UTF-8 encoded frame, stamped with wall-clock now. */
	writeString(str: string) {
		this.writeFrame({ data: new TextEncoder().encode(str), timestamp: Timestamp.now() });
	}

	/** Write a value as a single JSON-encoded frame, stamped with wall-clock now. */
	writeJson(json: unknown) {
		this.writeString(JSON.stringify(json));
	}

	/** Write a boolean as a single one-byte frame, stamped with wall-clock now. */
	writeBool(bool: boolean) {
		this.writeFrame({ data: new Uint8Array([bool ? 1 : 0]), timestamp: Timestamp.now() });
	}

	/** True once no further frames can be read: the group has closed and every buffered frame is read. */
	get done(): boolean {
		return this.state.frames.peek().length === 0 && this.state.closed.peek() !== false;
	}

	/**
	 * Reads the next already-buffered frame's payload without blocking.
	 *
	 * Returns `undefined` when nothing is buffered right now. That is *not* by itself end-of-group:
	 * check {@link done} to tell "no frame buffered yet" (more may arrive) from "the group finished".
	 * Drain a backlog by looping until this returns `undefined`, then branch on {@link done}: if not
	 * done, {@link readable} resolves when the next frame arrives.
	 *
	 * Non-throwing: unlike {@link readFrame} it does not raise {@link CacheFull} on an evicted prefix.
	 */
	tryReadFrame(): Uint8Array | undefined {
		return this.tryReadFrameSequence()?.data;
	}

	/**
	 * Like {@link tryReadFrame} but also reports the frame's sequence number within the group.
	 *
	 * Non-throwing: it does not raise {@link CacheFull} on an evicted prefix. The eviction check
	 * lives only in the blocking {@link readFrame}/{@link readFrameSequence} paths.
	 */
	tryReadFrameSequence(): { sequence: number; data: Uint8Array } | undefined {
		const frames = this.state.frames.peek();
		const frame = frames.shift();
		if (frame === undefined) return undefined;
		return { sequence: this.state.total.peek() - frames.length - 1, data: frame.data };
	}

	/**
	 * Resolves once {@link readFrame} would not block: a frame is buffered, or the group has closed.
	 * Always settles (never hangs), so on a finished group it resolves immediately; pair it with
	 * {@link done} to avoid re-waiting on a group that has nothing left.
	 *
	 * Lets a caller fold "this group has a frame" into a larger wait (e.g. racing it against a new
	 * group arriving) without touching the group's internal signals.
	 */
	async readable(): Promise<void> {
		for (;;) {
			if (this.state.frames.peek().length > 0) return;
			if (this.state.closed.peek()) return;
			await Signal.race(this.state.frames, this.state.closed);
		}
	}

	/**
	 * Reads the next frame (timestamp + payload) from the group.
	 * @returns A promise that resolves to the next frame or undefined
	 */
	async readFrame(): Promise<Frame | undefined> {
		for (;;) {
			if (this.state.offset > 0) throw new CacheFull();

			const frames = this.state.frames.peek();
			const frame = frames.shift();
			if (frame) return frame;

			const closed = this.state.closed.peek();
			if (closed instanceof Error) throw closed;
			if (closed) return;

			await Signal.race(this.state.frames, this.state.closed);
		}
	}

	/** Reads the next frame's payload along with its sequence number within the group. */
	async readFrameSequence(): Promise<{ sequence: number; data: Uint8Array } | undefined> {
		for (;;) {
			if (this.state.offset > 0) throw new CacheFull();

			const frames = this.state.frames.peek();
			const frame = frames.shift();
			if (frame) return { sequence: this.state.total.peek() - frames.length - 1, data: frame.data };

			const closed = this.state.closed.peek();
			if (closed instanceof Error) throw closed;
			if (closed) return;

			await Signal.race(this.state.frames, this.state.closed);
		}
	}

	/** Reads the next frame and decodes its payload as a UTF-8 string. */
	async readString(): Promise<string | undefined> {
		const frame = await this.readFrame();
		return frame ? new TextDecoder().decode(frame.data) : undefined;
	}

	/** Reads the next frame and parses its payload as JSON. */
	async readJson(): Promise<unknown | undefined> {
		const frame = await this.readString();
		return frame ? JSON.parse(frame) : undefined;
	}

	/** Reads the next frame and decodes its payload as a one-byte boolean. */
	async readBool(): Promise<boolean | undefined> {
		const frame = await this.readFrame();
		return frame ? frame.data[0] === 1 : undefined;
	}

	/** Closes the group, optionally with an error to abort readers. */
	close(abort?: Error) {
		this.state.closed.set(abort ?? true);

		if (this.#mirrors) {
			for (const mirror of this.#mirrors) mirror.close(abort);
			this.#mirrors.clear();
		}
	}
}

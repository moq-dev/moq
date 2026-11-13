/**
 * Group role handles: an ordered stream of frames within a track, delivered over one QUIC stream.
 *
 * @module
 */
import { type Instant, now } from "@moq/clock";
import { type Dispose, type GetPromise, type Getter, Once, race, Signal } from "@moq/signals";
import type { Charge } from "./cache.ts";
import { FrameTooLarge, ProtocolViolation, StreamCode, StreamError, TimestampMismatch } from "./error.ts";
import { Compression, encodable, stored } from "./flate/group.ts";
import {
	Chunked,
	chunkChanged,
	EMPTY,
	evicted,
	type Consumer as FrameConsumer,
	type Info as FrameInfo,
	type Producer as FrameProducer,
	joined,
	makeConsumer as makeFrameConsumer,
	makeProducer as makeFrameProducer,
	payloadBytes,
	pollChunk,
	WAIT,
} from "./frame.ts";
import { type Expiry, type GroupRecover, hooks, type ReadGroupFrame, type Replacement } from "./internal.ts";
import { Queue } from "./queue.ts";
import { rescale } from "./rescale.ts";
import type { Timestamp } from "./time.ts";
import type { Info as TrackInfo } from "./track.ts";
import { MAX_FRAME_BYTES } from "./util/limits.ts";
import { checkSequence, RESERVED } from "./util/sequence.ts";

// A live tail budget, including a conservative charge for each frame's object and queue slot.
const TAIL_BYTES = 32 * 1024 * 1024;
const FRAME_BYTES = 256;

function frameBytes(frame: Frame): number {
	if (frame instanceof Chunked) return FRAME_BYTES + frame.retained;
	const slice = stored(frame);
	return FRAME_BYTES + (slice ? slice.payload.length + slice.inflated : frame.payload.byteLength);
}

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

// Whether a frame's head was shed, so it can no longer be read from its start.
function headShed(frame: Frame | undefined): boolean {
	return frame instanceof Chunked && frame.base > 0;
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
	sequence: bigint;
}

/** Reactive backing state shared by the group producer and one consumer. */
class GroupState {
	readonly sequence: bigint;
	frames = new Signal(new Queue<Frame>());
	closed = new Once<Error | null>();
	total = new Signal<number>(0); // The total number of frames in the group thus far

	// Absolute sequence of the frame at the front of `frames`, advanced by a read: the
	// sequence the next read returns.
	start = 0;
	// The wire offset this copy began at; unlike `start`, reads never advance it.
	offset = 0;

	cacheBytes = 0;
	// What the source copy bills its track's cache pool, while cached. A mirror shares its
	// source's, so its reads count as accesses.
	charge?: Charge;
	compressed?: boolean;
	flate?: Compression;
	// The owning track's info, which every frame's timestamp must `rescale` to: a timed track's frames
	// carry a timestamp its scale holds, an untimed track's carry none. Undefined for a group
	// no track has claimed yet, which takes either.
	track?: TrackInfo;
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
	shedRead?: () => void;
	// Tells the group this copy was mirrored from whether its reader will still read it, so a
	// copy buffered for a reader that passed it stops keeping the group wanted.
	reads?: (reads: boolean) => void;
	// Stops the group teeing into this mirror from watching it close.
	unwatch?: () => void;
	// The first frame the group teeing into this mirror wrote here. A copy that took over a failed
	// one tees from where that one stopped, so the frames below came from the failed copy.
	from = 0;

	constructor(sequence: bigint) {
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

// Refuse a frame whose timestamp the track's timeline refuses.
function checkTimed(track: TrackInfo | undefined, frame: Frame) {
	if (track) rescale(track, frame.timestamp);
}

function appendFrame(state: GroupState, frame: Frame) {
	if (state.closed.peek() !== undefined) throw new Error("group is closed");

	state.timestamp ??= frame.timestamp;
	if (frame.timestamp === undefined) state.latest = undefined;
	else if (state.latest === undefined || state.latest.isBefore(frame.timestamp)) state.latest = frame.timestamp;
	state.frames.mutate((frames) => {
		frames.push(frame);
	});

	state.total.update((total) => total + 1);
	for (const fn of state.progress) fn(state.total.peek());
}

let makeDemand: (sequence: bigint, wanted: Getter<boolean>, state: GroupState) => Demand;

/**
 * A watch-only view of a group's mirror readers, including coalesced fetches.
 *
 * @internal The sessions' own fetch coalescing is its only consumer.
 */
export class Demand {
	/** The group sequence this handle watches. */
	readonly sequence: bigint;
	#wanted: Getter<boolean>;
	#state: GroupState;

	private constructor(sequence: bigint, wanted: Getter<boolean>, state: GroupState) {
		this.sequence = sequence;
		this.#wanted = wanted;
		this.#state = state;
	}

	static {
		makeDemand = (sequence, wanted, state) => new Demand(sequence, wanted, state);
	}

	/**
	 * Whether a reader will still read the group: a mirror handed out or fetched, or one a track
	 * buffers for a reader that has yet to reach it. Mirrors the Rust fill's held runs plus its
	 * group consumers.
	 */
	get wanted(): Getter<boolean> {
		return this.#wanted;
	}

	/** The group's clean close or abort error. */
	get closed(): GetPromise<Error | null> {
		return this.#state.closed;
	}
}

/**
 * A group its producer finished: every frame is written, and it can still be read, but not
 * written to. {@link Producer.finish} returns it, mirroring the Rust `group::Finished`; the
 * producer itself still refuses a write after the finish.
 *
 * @public
 */
export interface Finished {
	/** Sequence number of this group within its track. */
	readonly sequence: bigint;
	/** Settles once the group closes: `null` on a clean close, or the abort {@link Error}. */
	readonly closed: GetPromise<Error | null>;
	/** A read handle for this group. */
	consume(): Consumer;
}

/**
 * The write side of an ordered stream of frames within a track.
 *
 * @public
 */
export class Producer {
	/** Sequence number of this group within its track. */
	readonly sequence: bigint;

	#state: GroupState;
	// Each open mirror, with whether its reader will still read it.
	#mirrors?: Map<GroupState, boolean>;
	// How many open mirrors a reader will still read.
	#readers = 0;

	// Whether a reader will still read it (see {@link Demand.wanted}). Fetch coalescing watches it to
	// cancel an abandoned download; a group can stay open indefinitely (a catalog or JSON stream),
	// so this is what stops a reader-less fetch instead of the stream ending on its own.
	#wanted = new Signal<boolean>(false);
	#demand: Demand;

	// Exclusive cap on the frame count, set when the track ends inside this group: a write
	// past the track's end is refused rather than served as content that does not exist.
	#limit?: number;

	// The frame being written in pieces, if any; no other frame is written until it ends.
	#open?: FrameProducer;

	// The tracks this group joined before its first frame, each checking that frame's
	// timestamp against its neighbors' starts.
	#admits?: ((start: Timestamp) => () => void)[];

	// Called as the group aborts, before any signal subscriber runs (see `hooks.onGroupAbort`).
	#aborted?: ((error: Error) => void)[];

	// Duplicates waiting on this received copy (see `hooks.holdGroup`). Failing while held, it
	// keeps its mirrors open for the duplicate that takes over to tee into.
	#holds = 0;

	constructor(sequence: bigint) {
		if (checkSequence(sequence) >= RESERVED) throw new RangeError(`group sequence out of range: ${sequence}`);
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
			if (compressed && Array.from(state.frames.peek()).some((frame) => frame instanceof Chunked))
				throw new ProtocolViolation("compressed group holds a streamed frame");
			// A frame written before the track chose compression is encoded and sent whole too.
			if (compressed && Array.from(state.frames.peek()).some((frame) => !encodable(frame.payload.byteLength)))
				throw new FrameTooLarge();
			state.compressed = compressed;
			if (!compressed) return;
			const flate = new Compression();
			flate.failed = (error) => group.abort(error);
			state.flate = flate;
			const frames = Array.from(state.frames.peek(), (frame) => flate.push(frame));
			state.frames.set(new Queue(frames));
			const bytes = frames.reduce((bytes, frame) => bytes + frameBytes(frame), 0);
			if (bytes > state.cacheBytes) state.charge?.bill(bytes - state.cacheBytes);
			else state.charge?.sub(state.cacheBytes - bytes);
			state.cacheBytes = bytes;
			for (const mirror of group.#mirrors?.keys() ?? []) {
				mirror.frames.set(new Queue(frames.slice(mirror.start - state.start)));
				mirror.flate = flate;
				mirror.compressed = true;
			}
			group.#trim();
			if (state.closed.peek() !== undefined) flate.finish();
		};
		hooks.groupOffset = (group) => group.#state.offset;
		hooks.holdGroup = (group) => {
			group.#holds++;
			let held = true;
			return () => {
				if (!held) return;
				held = false;
				if (--group.#holds === 0) group.#failHeld();
			};
		};
		hooks.succeedGroup = (next, failed) => {
			const error = failed.#state.closed.peek();
			if (!(error instanceof Error)) throw new Error("group has not failed");
			const state = next.#state;
			// The failed copy's frames the continued mirrors hold, once each though mirrors share them.
			const kept = new Set<Frame>();
			for (const [mirror, reads] of failed.#mirrors ?? []) {
				failed.#unmirror(mirror);
				// The successor continues a mirror from the frame it reached, so it must hold that
				// frame from its start. Compression binds a group to its prefix, so a compressed
				// successor starts at 0.
				if (mirror.closed.peek() !== undefined) continue;
				const lost = state.start === mirror.total.peek() && headShed(state.frames.peek().first);
				if (state.closed.peek() !== undefined || state.start > mirror.total.peek() || lost) {
					mirror.closed.set(error);
					continue;
				}
				// Its unread frames count against the tail it continues in, as they would on one copy, so a
				// chain of takeovers cannot grow a stalled reader's buffer: past the tail, it is behind it.
				let unread = 0;
				for (const frame of mirror.frames.peek()) unread += frameBytes(frame);
				if (unread > TAIL_BYTES) {
					mirror.frames.set(new Queue());
					mirror.cacheBytes = 0;
					mirror.flate = undefined;
					mirror.closed.set(evicted());
					continue;
				}
				for (const frame of mirror.frames.peek()) kept.add(frame);
				mirror.from = mirror.total.peek();
				mirror.charge = state.charge;
				if (state.compressed) mirror.flate = state.flate;
				let index = state.start;
				for (const frame of state.frames.peek()) if (index++ >= mirror.from) appendFrame(mirror, frame);
				next.#attach(mirror, reads);
			}
			let bytes = 0;
			for (const frame of kept) bytes += frameBytes(frame);
			return bytes;
		};
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
		hooks.bindGroupTimed = (group, track) => {
			const state = group.#state;
			const timed = track.timescale != null;
			if (state.track !== undefined && (state.track.timescale != null) === timed) return;
			// Another track already holds it to the opposite timedness, empty or not.
			if (state.track !== undefined) throw new TimestampMismatch();
			for (const frame of state.frames.peek()) checkTimed(track, frame);
			state.track = track;
		};
		hooks.admitGroupStart = (group, admit) => {
			group.#admits ??= [];
			group.#admits.push(admit);
		};
		hooks.onGroupAbort = (group, fn) => {
			group.#aborted ??= [];
			group.#aborted.push(fn);
		};
		hooks.overRefresh = (group, ratio) => {
			const state = group.#state;
			if (state.offset !== 0) return false;
			const first = state.frames.peek().first;
			// A shed front, whole frames or the first frame's head, cannot be replayed.
			if (state.start > 0 || headShed(first)) return true;
			if (first === undefined) return false;
			const head = payloadBytes(first);
			const bytes = state.flate?.stored ?? state.cacheBytes - state.frames.peek().length * FRAME_BYTES;
			return bytes - head > head * ratio;
		};
		hooks.chargeGroup = (group, charge) => {
			const state = group.#state;
			state.charge = charge;
			charge?.bill(state.cacheBytes);
		};
		hooks.shedGroup = (group, debt, cap, readers) => group.#shed(debt, cap, readers);
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
		dst.charge = this.#state.charge;
		// A track restamps its own mirror with when the group joined it.
		dst.arrived = this.#state.arrived;
		if (this.#state.start === this.#state.offset && !headShed(this.#state.frames.peek().first)) {
			for (const frame of this.#state.frames.peek()) appendFrame(dst, frame);
		} else {
			dst.flate = undefined;
			const closed = this.#state.closed.peek();
			dst.closed.set(closed instanceof Error ? closed : evicted());
		}
		dst.start = dst.closed.peek() instanceof Error ? this.#state.offset : this.#state.start;
		dst.offset = this.#state.offset;
		// The replay only covers what is still buffered, so the count has to come from the
		// source: it is what names a frame's absolute sequence, and what a publisher reads
		// to resolve the live edge.
		dst.total.set(this.#state.total.peek());

		if (dst.closed.peek() !== undefined) return makeConsumer(dst);
		const closed = this.#state.closed.peek();
		if (closed !== undefined) {
			dst.closed.set(closed);
			return makeConsumer(dst);
		}

		this.#attach(dst, true);
		return makeConsumer(dst);
	}

	// Tee later writes into `dst`, counting its reader if it `reads`.
	#attach(dst: GroupState, reads: boolean): void {
		this.#mirrors ??= new Map();
		const mirrors = this.#mirrors;
		mirrors.set(dst, reads);
		if (reads) this.#readers++;
		this.#wanted.set(this.#readers > 0);
		dst.reads = (reads) => {
			const was = mirrors.get(dst);
			if (was === undefined || was === reads) return;
			mirrors.set(dst, reads);
			this.#readers += reads ? 1 : -1;
			this.#wanted.set(this.#readers > 0);
		};

		// Track this mirror's close eagerly (not just lazily on the next write) so demand drops the
		// moment the last reader leaves, letting an abandoned fetch cancel even with no more frames.
		dst.unwatch = dst.closed.subscribe((c) => {
			if (c !== undefined) this.#unmirror(dst);
		});
	}

	/**
	 * Watch the mirror readers attached to this group.
	 *
	 * @internal The sessions' own fetch coalescing is its only consumer.
	 */
	demand(): Demand {
		return this.#demand;
	}

	// Stop teeing into `mirror`, which closed, and stop counting its reader.
	#unmirror(mirror: GroupState): void {
		const reads = this.#mirrors?.get(mirror);
		if (reads === undefined) return;
		this.#mirrors?.delete(mirror);
		// A consumer that outlives the group must not pin it, and its buffers, through this.
		mirror.reads = undefined;
		mirror.unwatch?.();
		mirror.unwatch = undefined;
		if (reads) this.#readers--;
		this.#wanted.set(this.#readers > 0);
	}

	/** Writes a frame to the group. */
	writeFrame(frame: Frame) {
		this.#writeFrame(frame);
	}

	/**
	 * Start a frame of `info.size` bytes, written in pieces, as Rust's `create_frame`.
	 *
	 * Readers can stream it as it is written (see {@link Consumer.nextFrame}), so a frame
	 * of any size is held only within the group's retained tail. No other frame can be
	 * written until this one finishes.
	 */
	createFrame(info: FrameInfo): FrameProducer {
		const { size, timestamp } = info;
		if (!Number.isSafeInteger(size) || size < 0) throw new RangeError("invalid frame size");
		const frame = { payload: EMPTY, timestamp };
		if (this.#state.flate) {
			// A compressed frame is encoded whole once written, as `writeFrame` does.
			if (!encodable(size)) throw new FrameTooLarge();
			this.#admit(frame);
			const payload = new Uint8Array(size);
			let written = 0;
			const open = makeFrameProducer(info, {
				written: () => written,
				write: (chunk) => {
					if (this.#open !== open) throw new Error("group is closed");
					payload.set(chunk, written);
					written += chunk.byteLength;
				},
				finish: () => {
					if (this.#open !== open) throw new Error("group is closed");
					this.#open = undefined;
					this.#writeFrame({ payload, timestamp });
				},
				abort: (error) => this.abort(error),
			});
			this.#open = open;
			return open;
		}

		const commits = this.#admit(frame);
		const entry = new Chunked(info);
		this.#append(entry, commits);
		const open = makeFrameProducer(info, {
			written: () => entry.written.peek(),
			write: (chunk) => {
				if (this.#open !== open) throw new Error("group is closed");
				const bytes = entry.write(chunk);
				this.#state.cacheBytes += bytes;
				this.#state.charge?.add(bytes);
				this.#trim();
				this.#state.charge?.settle();
			},
			finish: () => {
				if (this.#open !== open) throw new Error("group is closed");
				this.#open = undefined;
				entry.arrived = now();
				this.#state.charge?.recordWrite();
				entry.done.set(null);
				// Wake whole-frame readers waiting for it to complete.
				this.#state.frames.mutate(() => {});
				for (const mirror of this.#mirrors?.keys() ?? []) mirror.frames.mutate(() => {});
			},
			abort: (error) => this.abort(error),
		});
		this.#open = open;
		return open;
	}

	#writeFrame(frame: Frame, inflated?: number) {
		// A plain frame written whole is the writer's own buffer, at any size. A compressed one
		// is sent whole, so its slice must fit: a received one as declared, a local one encoded.
		const size = frame.payload.byteLength;
		if (this.#state.flate && (inflated === undefined ? !encodable(size) : size > MAX_FRAME_BYTES))
			throw new FrameTooLarge();
		if ((inflated ?? 0) > MAX_FRAME_BYTES) throw new FrameTooLarge();
		const commits = this.#admit(frame);
		if (inflated !== undefined && (!this.#state.flate || !Number.isSafeInteger(inflated) || inflated < 0))
			throw new ProtocolViolation("unexpected inflated frame size");

		const arrived = now();
		frame = new Landed(frame, arrived);
		if (this.#state.flate) {
			frame = this.#state.flate.push(frame, inflated);
		}
		this.#append(frame, commits);
	}

	// Check that the next frame may be written, returning what records its admission.
	#admit(frame: Frame): (() => void)[] | undefined {
		// Refuse further writes before inspecting or changing committed content.
		if (this.#state.closed.peek() !== undefined) throw new Error("group is closed");
		if (this.#open) throw new Error("a frame is still being written");
		if (this.#state.total.peek() === Number.MAX_SAFE_INTEGER)
			throw new ProtocolViolation("group frame index exceeds the safe integer range");
		checkTimed(this.#state.track, frame);
		if (this.#limit !== undefined && this.#state.total.peek() >= this.#limit) {
			throw new Error(`frame ${this.#state.total.peek()} is past the track's end`);
		}
		// Frame 0 must fit between its neighbors in every track it joined, checked before
		// any of them records it, so a refusal leaves the group and its tracks as they were.
		if (this.#admits && frame.timestamp !== undefined && this.#state.total.peek() === 0) {
			const start = frame.timestamp;
			return this.#admits.map((admit) => admit(start));
		}
		return undefined;
	}

	#append(frame: Frame, commits: (() => void)[] | undefined) {
		const bytes = frameBytes(frame);
		this.#state.cacheBytes += bytes;
		this.#state.charge?.add(bytes);
		appendFrame(this.#state, frame);
		if (commits) {
			this.#admits = undefined;
			for (const commit of commits) commit();
		}

		const index = this.#state.total.peek() - 1;
		for (const mirror of this.#mirrors?.keys() ?? []) {
			if (mirror.closed.peek() !== undefined) this.#unmirror(mirror);
			// A mirror taken over from a failed copy already holds the frames that copy wrote.
			else if (index >= mirror.total.peek()) appendFrame(mirror, frame);
		}
		this.#trim();
		// Past the frame's own bookkeeping: paying the track's debt may evict other groups.
		this.#state.charge?.settle();
	}

	// Shed the front past the live tail budget.
	#trim(): void {
		const state = this.#state;
		const frames = state.frames.peek();
		const bytes = state.cacheBytes;
		while (state.cacheBytes > TAIL_BYTES) {
			const front = frames.first;
			if (!front) break;
			// A chunked front frame sheds its sealed segments first, keeping only its newest.
			const freed = front instanceof Chunked ? front.shed() : 0;
			if (freed > 0) {
				state.cacheBytes -= freed;
				continue;
			}
			// The newest frame stays readable: its writer holds it whole anyway.
			if (frames.length === 1) break;
			this.#shift();
		}
		this.#shedMirrors(bytes);
	}

	// Shed whole frames from the front toward the cache pool's `debt`, freeing about `cap` at
	// most, for a group the debt cannot evict whole: one being read, or the live edge. A frame
	// larger than what is left of the debt stays, with everything after it. An open group keeps
	// its newest committed frame, for the reader about to take it, and any frame still being
	// written. A finished group no longer tees into its mirrors, so it ends `readers`, the copies
	// its track still holds, that had yet to take a shed frame. Returns the bytes freed. Mirrors
	// Rust's `GroupState::shed`.
	#shed(debt: number, cap: number, readers: Iterable<Consumer>): number {
		const state = this.#state;
		const closed = state.closed.peek();
		if (closed instanceof Error) return 0;
		const frames = state.frames.peek();
		const last = frames.last;
		const writing = last instanceof Chunked && last.done.peek() === undefined;
		const keep = closed === null ? 0 : writing ? 2 : 1;
		const bytes = state.cacheBytes;
		while (frames.length > keep && bytes - state.cacheBytes < cap) {
			const front = frames.first;
			if (!front || (front instanceof Chunked && front.done.peek() === undefined)) break;
			if (frameBytes(front) > debt - (bytes - state.cacheBytes)) break;
			this.#shift();
		}
		this.#shedMirrors(bytes);
		if (closed === null) for (const reader of readers) shedReader(reader, state.start);
		return bytes - state.cacheBytes;
	}

	// Drop the front frame.
	#shift(): void {
		const state = this.#state;
		const front = state.frames.peek().shift() as Frame;
		state.cacheBytes -= frameBytes(front);
		if (front instanceof Chunked) front.release();
		state.start++;
	}

	// Release what the front shed since it held `bytes`, and end the mirrors that can no longer
	// read from their start.
	#shedMirrors(bytes: number): void {
		const state = this.#state;
		const frames = state.frames.peek();
		if (state.cacheBytes === bytes) return;
		state.charge?.sub(bytes - state.cacheBytes);
		state.flate?.shed(state.start);
		const lost = headShed(frames.first);
		for (const mirror of this.#mirrors?.keys() ?? []) {
			// A mirror that has not taken the front frame can no longer read it from its start.
			if (mirror.closed.peek() !== undefined) continue;
			const front = Math.max(mirror.start, mirror.from);
			if (front > state.start || (front === state.start && !lost)) continue;
			mirror.frames.set(new Queue());
			mirror.cacheBytes = 0;
			mirror.flate = undefined;
			mirror.closed.set(evicted());
			this.#unmirror(mirror);
		}
	}

	/** True once the group has been closed. */
	get isClosed(): boolean {
		return this.#state.closed.peek() !== undefined;
	}

	/**
	 * Marks the group complete, returning it as {@link Finished}: no more frames will be written.
	 * Idempotent.
	 */
	finish(): Finished {
		this.#end(null);
		return this;
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
		// A frame cut short leaves the group incomplete, so even a clean end fails it.
		let cut: Chunked | undefined;
		if (this.#open) {
			this.#open = undefined;
			outcome ??= new Error("group finished while a frame was being written");
			const last = this.#state.frames.peek().last;
			if (last instanceof Chunked && last.done.peek() === undefined) cut = last;
		}
		const held = outcome !== null && this.#holds > 0;
		// A held copy's duplicate continues each mirror from the frame it reached, so a mirror
		// drops the frame cut short before its reader takes it. One whose reader took it lost it.
		if (held && cut) {
			const index = this.#state.total.peek() - 1;
			for (const mirror of this.#mirrors?.keys() ?? []) {
				// A mirror taken over past the cut frame never got it from this copy.
				if (index < mirror.from) continue;
				if (mirror.frames.peek().last === cut) {
					mirror.frames.mutate((frames) => frames.pop());
					mirror.total.update((total) => total - 1);
				} else if (mirror.closed.peek() === undefined) mirror.closed.set(outcome);
			}
		}
		cut?.done.set(outcome);
		this.#state.flate?.finish();
		this.#state.closed.set(outcome);

		if (!held) this.#failHeld();
		if (outcome) for (const fn of this.#aborted ?? []) fn(outcome);
		this.#aborted = undefined;
	}

	// End the open mirrors, once this copy closed and no duplicate held for it can take over.
	#failHeld(): void {
		const outcome = this.#state.closed.peek();
		if (outcome === undefined) return;
		for (const mirror of this.#mirrors?.keys() ?? []) {
			if (mirror.closed.peek() === undefined) mirror.closed.set(outcome);
			this.#unmirror(mirror);
		}
	}
}

let makeConsumer: (state: GroupState) => Consumer;
// End a track-held copy of a finished group that had yet to take frame `start - 1`, now shed.
let shedReader: (group: Consumer, start: number) => void;

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

// A consumer can finish from its own verdict or from the shared group state. Keep one stable
// public handle while letting peek() observe either source synchronously, before signal
// subscribers run in the next microtask. The source is re-read on every check, since a
// recovered group moves to another copy's state, and `final` says whether its value ends the
// consumer: a failure recovery can still continue does not, so it never reaches a caller. Built
// on first read, which only a caller holding the group makes: a reader attaches its recovery
// (`hooks.recoverGroup`) before handing the group out, and the track's own scans read
// `hooks.groupFailed` instead. A front serving another front is the exception: the inner front
// holds the handle before the outer attaches its recovery, so attaching rebuilds a handle that
// latched a failure, and the inner front alone keeps the failure it saw.
class CombinedClosed implements GetPromise<Error | null> {
	#preferred: Once<Error>;
	#source: () => Once<Error | null>;
	#final: (value: Error | null) => boolean;
	#value = new Once<Error | null>();
	#watched?: Once<Error | null>;
	// The watcher on `#watched`, replaced when a recovery moves to another copy's state.
	#unwatch?: Dispose;
	#dispose: Dispose[] = [];

	constructor(preferred: Once<Error>, source: () => Once<Error | null>, final: (value: Error | null) => boolean) {
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
	readonly sequence: bigint;

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
	// This reader's own failure, set once, including a budget that gave up on the group: it
	// ends the consumer whatever its copy does. See `#fail`.
	#verdict = new Once<Error>();
	#closed?: CombinedClosed;
	// Settles once {@link done}: its reader read or released everything it holds.
	#finished?: CombinedClosed;
	#expired?: Expired;
	// Closed by its reader, which ends it whatever is left unread.
	#released = false;
	// The arrival of the copy first read, kept once a replacement copy is adopted.
	#arrived?: Instant;
	// The last frame handed out to stream before it completed. Cut short, it is lost, so no
	// other copy can continue the group past it.
	#streamed?: Chunked;

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

	// Settles with its terminal state once {@link done}, unlike {@link closed}, which a failure
	// settles while frames are still unread.
	#done(): GetPromise<Error | null> {
		this.#finished ??= new CombinedClosed(
			this.#verdict,
			() => this.#state.closed,
			(value) =>
				(!(value instanceof Error) || !this.#recovers(value)) &&
				(this.#released || this.#bufferedFrames().length === 0),
		);
		return this.#finished;
	}

	#refresh(): void {
		this.#closed?.refresh();
		this.#finished?.refresh();
	}

	/**
	 * Settles with a `StreamError` of code `Old` once the subscription's `delay.max` gives up
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
				const verdict = this.#verdict.peek();
				return verdict instanceof StreamError && verdict.code === StreamCode.Old ? verdict : undefined;
			},
			() => this.#expiry?.changed ?? [],
		);
		return this.#expired;
	}

	// The failure that ends this reader, as `closed` would settle with it, without building that
	// handle: a track's scans read this, so none latches a failure before a reader attaches the
	// recovery that continues past it.
	#failed(): Error | undefined {
		const verdict = this.#verdict.peek();
		if (verdict) return verdict;
		const closed = this.#state.closed.peek();
		return closed instanceof Error && !this.#recovers(closed) ? closed : undefined;
	}

	// Whether a failure of the copy being read may still be continued from another route.
	#recovers(failed: Error): boolean {
		return !this.#verdict.peek() && !this.#torn() && this.#recover?.wants(failed) === true;
	}

	// Whether a frame handed out to stream was cut short.
	#torn(): boolean {
		const done = this.#streamed?.done.peek();
		return done !== undefined && done !== null;
	}

	static {
		makeConsumer = (state) => new Consumer(state);
		hooks.releaseGroupReader = (group, abort) => group.#release(abort);
		hooks.groupDone = (group) => group.#done();
		hooks.readsMirrored = (group) => group.#state === group.#mirrored;
		hooks.groupReads = (group, reads) => group.#mirrored.reads?.(reads);
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
		hooks.groupFailed = (group) => group.#failed();
		hooks.recoverGroup = (group, recover) => {
			group.#recover = recover;
			// A front serving another front attaches its recovery after the inner one handed the
			// group out, whose reads may have latched a failure this recovery continues past.
			if (group.#closed?.peek() instanceof Error && group.#verdict.peek() === undefined)
				group.#closed = undefined;
		};
		hooks.readGroupFrame = (group, compressed, from = 0) => group.#readFrame(from, compressed, true);
		hooks.evictGroup = (group, lost) => group.#evict(lost);
		shedReader = (group, start) => {
			// A copy that took every shed frame keeps reading; one that had not lost them.
			const state = group.#state;
			if (state === group.#mirrored && Math.max(state.start, state.from) < start) group.#evict(evicted());
		};
		hooks.refreshGroup = (group) => group.#state.charge?.refresh();
		hooks.onShedRead = (group, notify) => {
			group.#state.shedRead = notify;
		};
	}

	// Evaluate the drift budget for a read that has nothing buffered and is about to
	// wait. A group with frames in hand is always drained: the budget bounds a group
	// that has stalled while the live edge moved on, not a reader slower than the wire.
	// Returns true if a verdict was just reached, so the caller re-checks.
	//
	// A reader that took every frame still fails with Old: the producer never finished
	// the group, so a clean end would pass the prefix off as the whole group.
	#expire(): boolean {
		if (this.#verdict.peek()) return false;
		if (this.#state.closed.peek() instanceof Error) return false;
		if (!this.#expiry?.expired()) return false;

		this.#fail(
			new StreamError(StreamCode.Old, {
				message: "group exceeded the subscription max delay budget",
			}),
		);
		return true;
	}

	// Fail this reader with `error`, unless it already failed: the first verdict stands.
	#fail(error: unknown): Error {
		if (this.#verdict.peek() === undefined)
			this.#verdict.set(error instanceof Error ? error : new Error(String(error)));
		return this.#verdict.peek() as Error;
	}

	#evict(lost: Error): boolean {
		// A group read on from another copy is that copy's to reclaim.
		if (this.#state !== this.#mirrored) return false;
		const frames = this.#bufferedFrames();
		if (frames.length > 0) {
			// Unread content is going away either way. If the budget had already given up
			// on this group that is the more specific reason, so ask it first; otherwise
			// the cache reclaimed it before the reader got to it.
			this.#expire();
			this.#fail(lost);
		}
		if (frames.length > 0) {
			this.#state.start += frames.length;
		}
		this.#state.cacheBytes = 0;
		this.#state.flate = undefined;
		this.#state.frames.set(new Queue());
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
	async #resume(failed: Error | undefined, preferBuffered = true): Promise<boolean> {
		const recover = this.#recover;
		if (this.#torn() || !recover?.wants(failed)) return false;
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
					this.#fail(err);
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
		if (
			!failed &&
			((preferBuffered && this.#bufferedFrames().length > 0) || this.#state.closed.peek() !== undefined)
		) {
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
			this.#fail(new Error(`group ${this.sequence} cannot resume at frame ${index}`));
			return;
		}
		replacement.adopt();
		// The copy left behind stops teeing into the state this consumer read.
		if (this.#state.closed.peek() === undefined) this.#state.closed.set(null);
		this.#arrived ??= this.#state.arrived;
		this.#state = next;
		this.#expiry = replacement.expiry;
		this.#skip = index;
		this.#refresh();
		this.#expired?.refresh();
	}

	// The unread frames. Every look at the buffer goes through here, so readiness, reads and
	// `done` agree on what is left.
	#bufferedFrames(): Queue<Frame> {
		const frames = this.#state.frames.peek();
		// A resumed copy repeats the frames an earlier one already delivered.
		while (this.#state.start < this.#skip && frames.length > 0) {
			frames.shift();
			this.#state.start++;
		}
		return frames;
	}

	// Whether the next frame can be read whole without waiting.
	#ready(): boolean {
		const front = this.#bufferedFrames().first;
		return front !== undefined && !(front instanceof Chunked && front.done.peek() === undefined);
	}

	// Take the next frame whole, or as chunks when it is a chunked frame `streaming` reads, or
	// one still being written that a `collect` read gathers itself. A plain whole read leaves an
	// incomplete frame buffered.
	#readBufferedFrame(compressed = false, take: "whole" | "collect" | "stream" = "whole"): ReadGroupFrame | undefined {
		const frames = this.#bufferedFrames();
		const closed = this.#state.closed.peek();
		if (closed instanceof StreamError && closed.code === StreamCode.Evicted) {
			const notify = this.#state.shedRead;
			this.#state.shedRead = undefined;
			notify?.();
		}
		const front = frames.first;
		if (!front) return undefined;
		if (front instanceof Chunked) {
			if (take === "stream" || (take === "collect" && front.done.peek() === undefined)) {
				frames.shift();
				const sequence = this.#state.start++;
				this.#streamed = front;
				this.#refresh();
				this.#state.charge?.refresh();
				return { sequence, frame: front, chunks: makeFrameConsumer(sequence, front) };
			}
			if (front.done.peek() === undefined) return undefined;
		}
		const buffered = frames.shift() as Frame;

		const slice = compressed ? stored(buffered) : undefined;
		let frame: Frame;
		try {
			frame = {
				payload: slice?.payload ?? buffered.payload,
				timestamp: buffered.timestamp,
				arrived: buffered.arrived,
			};
		} catch (error) {
			const verdict = this.#fail(error);
			this.#refresh();
			throw verdict;
		}
		if (frames.length === 0 && this.#state.closed.peek() !== undefined) this.#state.flate = undefined;
		const sequence = this.#state.start++;
		this.#refresh();
		this.#state.charge?.refresh();
		return { sequence, frame, inflated: slice?.inflated };
	}

	/** True once no further frames can be read: this handle or the group has closed, and every buffered frame is read. */
	get done(): boolean {
		return (
			this.#verdict.peek() !== undefined ||
			(this.#bufferedFrames().length === 0 && (this.#released || this.#settled()))
		);
	}

	/** True once this handle or the group has been closed, regardless of whether buffered frames remain unread. Synchronous complement to the {@link closed} promise. */
	get isClosed(): boolean {
		return this.#verdict.peek() !== undefined || this.#released || this.#settled();
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
		if (this.#verdict.peek()) return undefined;
		return this.#readBufferedFrame()?.frame;
	}

	/** Like {@link tryReadFrame} but also reports the frame's sequence number within the group. */
	tryReadFrameSequence(): ({ sequence: number } & Frame) | undefined {
		if (this.#verdict.peek()) return undefined;
		const read = this.#readBufferedFrame();
		if (!read) return undefined;
		return { sequence: read.sequence, ...read.frame };
	}

	/** Resolves once {@link readFrame} would not block. */
	async readable(): Promise<void> {
		for (;;) {
			if (this.#verdict.peek()) {
				this.#abandon();
				return;
			}
			if (this.#ready()) return;
			const closed = this.#state.closed.peek();
			if (closed instanceof Error && (await this.#resume(closed))) continue;
			if (closed !== undefined) {
				this.#abandon();
				this.#refresh();
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

	/**
	 * Reads the next frame as it arrives, as Rust's `next_frame`: resolves once its header is
	 * buffered, without waiting for its payload. Use it for a frame too large to hold whole.
	 */
	async nextFrame(): Promise<FrameConsumer | undefined> {
		const read = await this.#readFrame(0, false, true);
		if (!read) return undefined;
		return read.chunks ?? makeFrameConsumer(read.sequence, read.frame);
	}

	async #readFrame(from: number, compressed = false, streaming = false): Promise<ReadGroupFrame | undefined> {
		for (;;) {
			const verdict = this.#verdict.peek();
			if (verdict) {
				this.#abandon();
				throw verdict;
			}

			// A whole read streams a frame still being written into its own buffer, rather
			// than waiting for it whole while the group sheds its head.
			const read = this.#readBufferedFrame(compressed, streaming ? "stream" : "collect");
			if (read) {
				if (read.sequence < from) {
					if (read.chunks) this.#streamed = undefined;
					continue;
				}
				if (read.chunks && !streaming) return this.#join(read.sequence, read.chunks);
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
				this.#refresh();
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

	// Read a streamed frame whole. Losing any of it ends this reader, since the frame is gone.
	async #join(sequence: number, chunks: FrameConsumer): Promise<ReadGroupFrame | undefined> {
		const parts: Uint8Array[] = [];
		let size = 0;
		try {
			for (;;) {
				const verdict = this.#verdict.peek();
				if (verdict) throw verdict;
				const chunk = pollChunk(chunks);
				if (chunk === undefined) break;
				if (chunk !== WAIT) {
					parts.push(chunk);
					size += chunk.byteLength;
					continue;
				}
				// A released reader stops waiting on a frame its producer may never finish.
				if (this.#released) {
					const closed = this.#state.closed.peek();
					if (closed instanceof Error) throw closed;
					return undefined;
				}
				// A frame stalled partway is the stall the drift budget bounds, as an empty group is.
				if (this.#expire()) continue;
				await Signal.race(...chunkChanged(chunks), this.#state.closed, ...(this.#expiry?.changed ?? []));
			}
			const payload = parts.length === 0 ? EMPTY : joined(parts, size);
			return { sequence, frame: { payload, timestamp: chunks.timestamp, arrived: chunks.arrived } };
		} catch (error) {
			const verdict = this.#fail(error);
			this.#refresh();
			throw verdict;
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
			this.#fail(abort);
		}
		this.#abandon();
		this.#recover?.close?.();
		this.#recover = undefined;
		// Closing is terminal for this reader, even with frames still unread.
		this.#released = true;
		if (this.#state.closed.peek() === undefined) this.#state.closed.set(abort ?? null);
		this.#refresh();
	}
}

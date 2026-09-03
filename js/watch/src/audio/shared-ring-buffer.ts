import { Time } from "@moq/net";

// Control array slot indices. The playhead is not here: see `state`.
const WRITE = 0;
const LATENCY = 1;
const STALLED = 2;
const CONTROL_SLOTS = 3;

/**
 * The playhead and the timeline it belongs to, packed into one 64-bit word: epoch in the high
 * half, read cursor in the low half.
 *
 * They have to move together. The reader samples the ring, copies, and only then publishes, and
 * the writer can rebase the timeline anywhere in between. Checking an epoch and then writing a
 * cursor leaves a window between the two no ordering closes, which is how a stale cursor ends up
 * on a fresh timeline: the playhead lands past WRITE, `read` returns nothing, and `insert` drops
 * everything as too old until WRITE catches up. Packed, the check *is* the write: the reader
 * compare-exchanges the whole word, so a rebase makes its publish fail rather than land.
 */
const CURSOR_MASK = 0xffffffffn;

function pack(epoch: number, read: number): bigint {
	return (BigInt(epoch >>> 0) << 32n) | BigInt(read >>> 0);
}

/** The timeline half. Bumped by the writer on every re-anchor. */
function epochOf(state: bigint): number {
	return Number(state >> 32n) | 0;
}

/** The playhead half, back as an i32 so the modular comparisons elsewhere still hold. */
function readOf(state: bigint): number {
	return Number(state & CURSOR_MASK) | 0;
}

export interface SharedRingBufferInit {
	channels: number;
	capacity: number; // samples per channel, always a power of two
	rate: number;
	samples: SharedArrayBuffer; // channels * capacity * Float32Array.BYTES_PER_ELEMENT bytes
	control: SharedArrayBuffer; // CONTROL_SLOTS * Int32Array.BYTES_PER_ELEMENT bytes
	state: SharedArrayBuffer; // one BigInt64Array element: epoch and read cursor, packed
	// Buffered mode: never skip ahead on read, so we play through everything buffered.
	buffered: boolean;
}

/** Rounds up to a power of two, which is what makes `slot` wrap-invariant. */
function ceilPow2(n: number): number {
	return n <= 1 ? 1 : 1 << (32 - Math.clz32(n - 1));
}

/**
 * Allocate the shared memory for a ring holding at least `capacity` samples per channel.
 *
 * The capacity is rounded up to a power of two so `slot` can mask instead of taking a
 * remainder; read `init.capacity` back rather than assuming the requested value.
 */
export function allocSharedRingBuffer(
	channels: number,
	capacity: number,
	rate: number,
	buffered = false,
): SharedRingBufferInit {
	if (channels <= 0) throw new Error("invalid channels");
	if (capacity <= 0 || capacity > 2 ** 30) throw new Error("invalid capacity");
	if (rate <= 0) throw new Error("invalid sample rate");

	capacity = ceilPow2(capacity);

	const samples = new SharedArrayBuffer(channels * capacity * Float32Array.BYTES_PER_ELEMENT);
	const control = new SharedArrayBuffer(CONTROL_SLOTS * Int32Array.BYTES_PER_ELEMENT);
	const state = new SharedArrayBuffer(BigInt64Array.BYTES_PER_ELEMENT);

	// Initialize STALLED to 1
	const ctrl = new Int32Array(control);
	Atomics.store(ctrl, STALLED, 1);

	return { channels, capacity, rate, samples, control, state, buffered };
}

/** Modular i32 max: returns a if a is ahead of b, else b. */
function i32Max(a: number, b: number): number {
	return ((a - b) | 0) > 0 ? a : b;
}

/**
 * Maps a sample index to a [0, capacity) array slot.
 *
 * `capacity` is a power of two, so masking is exact for negative indexes and, unlike a
 * remainder, survives the i32 wrap: consecutive indexes stay consecutive slots across
 * 0x7fffffff -> 0x80000000, which is what keeps unread samples from aliasing there.
 */
function slot(idx: number, capacity: number): number {
	return idx & (capacity - 1);
}

export class SharedRingBuffer {
	readonly channels: number;
	readonly capacity: number;
	readonly rate: number;
	readonly buffered: boolean;
	readonly init: SharedRingBufferInit;

	#control: Int32Array;
	#state: BigInt64Array;
	#samples: Float32Array[];

	// Whether READ/WRITE have been anchored to the first inserted sample.
	#anchored = false;

	// Absolute sample index of that first sample. READ/WRITE are stored relative to it, so
	// `timestamp` adds it back to recover media time. Main-thread only: the worklet reads by
	// difference and never needs an absolute position.
	#anchor = 0;

	// Unwrapped READ, in anchor-relative samples, plus the raw i32 it was last derived from.
	// READ is Int32 and wraps every ~13.5h at 44.1kHz. That is harmless for the modular
	// comparisons the ring runs on, but reading the negative half as a magnitude would report
	// the playhead jumping back 2^32 samples, so accumulate deltas here instead. Main-thread
	// only, and only accurate while `timestamp` is observed more often than READ can advance
	// 2^31 samples, which the 50ms poll in `buffer.ts` satisfies by six orders of magnitude.
	#position = 0;
	#lastRead = 0;

	/**
	 * Wrap the shared memory described by `init`.
	 *
	 * Pass `previous` in the worklet when this ring replaces one already being read. `resize`
	 * snapshots the playhead on the main thread and hands the replacement over by message, so the
	 * reader keeps draining the old ring in the meantime. Carrying its cursor across means those
	 * samples are not played twice.
	 */
	constructor(init: SharedRingBufferInit, previous?: SharedRingBuffer) {
		this.channels = init.channels;
		this.capacity = init.capacity;
		this.rate = init.rate;
		this.buffered = init.buffered;
		this.init = init;

		this.#control = new Int32Array(init.control);
		this.#state = new BigInt64Array(init.state);
		this.#samples = [];
		for (let i = 0; i < this.channels; i++) {
			this.#samples.push(
				new Float32Array(init.samples, i * this.capacity * Float32Array.BYTES_PER_ELEMENT, this.capacity),
			);
		}

		if (previous !== undefined) this.#handoff(previous);
	}

	/**
	 * Carry `source`'s playhead across, if the replacement is still the timeline it belongs to.
	 *
	 * `resize` copies the epoch, so a destination that re-anchored since has a different one and
	 * the cursor is dropped rather than applied to audio the reader has never played. The check
	 * and the write are the same exchange, so a re-anchor landing mid-handoff makes it fail
	 * instead of slipping through the gap that separate operations would leave.
	 */
	#handoff(source: SharedRingBuffer): void {
		if (source.channels !== this.channels || source.rate !== this.rate) return;

		const from = Atomics.load(source.#state, 0);

		for (;;) {
			const state = Atomics.load(this.#state, 0);
			if (epochOf(state) !== epochOf(from)) return;
			if (((readOf(from) - readOf(state)) | 0) <= 0) return;

			const next = pack(epochOf(state), readOf(from));
			if (Atomics.compareExchange(this.#state, 0, state, next) === state) return;
		}
	}

	/**
	 * Move the playhead forward to `candidate`, staying on whatever timeline is current.
	 *
	 * Retries while the word changes under it, and never steps backwards. Used by the writer's
	 * overflow path; the reader publishes with its own exchange so it can tell a rebase apart
	 * from losing a race.
	 */
	#advance(candidate: number): void {
		for (;;) {
			const state = Atomics.load(this.#state, 0);
			if (((candidate - readOf(state)) | 0) <= 0) return;

			const next = pack(epochOf(state), candidate);
			if (Atomics.compareExchange(this.#state, 0, state, next) === state) return;
		}
	}

	/**
	 * Insert audio samples at the given timestamp.
	 * Main thread only. Handles out-of-order writes, gap filling, and overflow.
	 */
	insert(timestamp: Time.Micro, data: Float32Array[]): void {
		if (data.length !== this.channels) throw new Error("wrong number of channels");

		let start = Math.round(Time.Second.fromMicro(timestamp) * this.rate);
		const originalLength = data[0].length;
		let offset = 0;

		// Anchor to the first sample so playback starts at its timestamp rather than gap-filling
		// from index 0.
		if (!this.#anchored) {
			this.#anchor = start;
			// One store rebases both halves, so a reader's compare-exchange against the old word
			// cannot land afterwards.
			const epoch = epochOf(Atomics.load(this.#state, 0));
			Atomics.store(this.#state, 0, pack((epoch + 1) | 0, 0));
			Atomics.store(this.#control, WRITE, 0);
			this.#anchored = true;
			this.#position = 0;
			this.#lastRead = 0;
		}

		// Positions are relative to the anchor. READ/WRITE are Int32, so an absolute sample index
		// wraps once a stream has been broadcasting a while, and the wrapped value reads as far
		// ahead of READ, so every insert would be discarded as too old and the ring would go
		// silent for good. Relative positions start at 0 instead. They still wrap, after ~13.5h at
		// 44.1kHz, but nothing here reads them as magnitudes: the comparisons are modular, `slot`
		// masks a power-of-two capacity, and `timestamp` keeps its own unwrapped position.
		start = (start - this.#anchor) | 0;

		const end = (start + originalLength) | 0;

		// Trim old: discard samples before the read index
		const read = readOf(Atomics.load(this.#state, 0));
		const behind = (read - start) | 0;
		if (behind > 0) {
			if (behind >= originalLength) {
				// All samples are too old
				return;
			}
			offset = behind;
			start = (start + behind) | 0;
		}

		const samples = originalLength - offset;

		// Overflow: if the write would exceed capacity from current READ, advance READ.
		// Use CAS so a concurrent reader advance isn't clobbered backward.
		if (((end - read) | 0) > this.capacity) {
			this.#advance((end - this.capacity) | 0);
		}

		// Gap fill: zero-fill from current WRITE to start if there's a discontinuity
		const write = Atomics.load(this.#control, WRITE);
		const gap = (start - write) | 0;
		if (gap > 0) {
			const gapSize = Math.min(gap, this.capacity);
			for (let channel = 0; channel < this.channels; channel++) {
				const dst = this.#samples[channel];
				for (let i = 0; i < gapSize; i++) {
					dst[slot((write + i) | 0, this.capacity)] = 0;
				}
			}
		}

		// Write sample data
		for (let channel = 0; channel < this.channels; channel++) {
			const src = data[channel];
			const dst = this.#samples[channel];
			for (let i = 0; i < samples; i++) {
				dst[slot((start + i) | 0, this.capacity)] = src[offset + i];
			}
		}

		// Advance WRITE (only forward)
		Atomics.store(this.#control, WRITE, i32Max(Atomics.load(this.#control, WRITE), end));

		// Un-stall: if buffered data >= LATENCY
		const currentRead = readOf(Atomics.load(this.#state, 0));
		const currentWrite = Atomics.load(this.#control, WRITE);
		const latency = Atomics.load(this.#control, LATENCY);
		if (((currentWrite - currentRead) | 0) >= latency && latency > 0) {
			Atomics.store(this.#control, STALLED, 0);
		}
	}

	/**
	 * Read audio samples into the output buffers.
	 * AudioWorklet only. Returns the number of samples read.
	 */
	read(output: Float32Array[]): number {
		// Sample the word before admission, not after. A re-anchor publishes the new state and
		// only then clears WRITE, so a reader admitted in between would pair a fresh epoch with
		// the previous timeline's WRITE and its exchange would succeed. Taken first, a reader
		// admitted before the rebase still holds the old word and its exchange fails, while one
		// arriving after sees the STALLED that `reset` raised and never starts.
		const state = Atomics.load(this.#state, 0);
		if (Atomics.load(this.#control, STALLED) === 1) return 0;

		let read = readOf(state);
		const write = Atomics.load(this.#control, WRITE);
		const latency = Atomics.load(this.#control, LATENCY);

		// Latency skip: if buffered data exceeds LATENCY, skip ahead.
		// CAS ensures we never step backward relative to a concurrent writer advance.
		// Disabled in buffered mode, where we deliberately play through the whole buffer.
		const buffered = (write - read) | 0;
		if (!this.buffered && latency > 0 && buffered > latency) {
			const skipTo = (write - latency) | 0;
			if (((skipTo - read) | 0) > 0) read = skipTo;
		}

		const available = (write - read) | 0;
		const count = Math.min(available, output[0].length);
		if (count <= 0) {
			// A latency skip still has to be published, and still only if nothing moved.
			if (((read - readOf(state)) | 0) > 0) {
				Atomics.compareExchange(this.#state, 0, state, pack(epochOf(state), read));
			}
			return 0;
		}

		// Copy samples
		for (let channel = 0; channel < this.channels; channel++) {
			const src = this.#samples[channel];
			const dst = output[channel];
			for (let i = 0; i < count; i++) {
				dst[i] = src[slot((read + i) | 0, this.capacity)];
			}
		}

		// Publish. The exchange fails if anything moved the word since the snapshot above, which
		// covers both a rebase and a concurrent overflow, so these samples are only ever counted
		// as played on the timeline they came from.
		const next = pack(epochOf(state), (read + count) | 0);
		if (Atomics.compareExchange(this.#state, 0, state, next) !== state) {
			// The worklet takes the return value as an underflow count rather than clearing the
			// buffer it handed over, so silence the prefix or the discarded audio renders anyway.
			for (let channel = 0; channel < this.channels; channel++) output[channel].fill(0, 0, count);
			return 0;
		}

		return count;
	}

	/** Update the target latency in samples. */
	setLatency(samples: number): void {
		Atomics.store(this.#control, LATENCY, samples);
	}

	/**
	 * Drop buffered samples at or after `timestamp`, keeping whatever is already due.
	 * Main thread only.
	 *
	 * A successor track overwrites the slots its own samples land on, but anything the previous
	 * track wrote beyond them would otherwise still play once the successor runs out.
	 */
	truncate(timestamp: Time.Micro): void {
		const target = (Math.round(Time.Second.fromMicro(timestamp) * this.rate) - this.#anchor) | 0;
		for (;;) {
			const write = Atomics.load(this.#control, WRITE);
			if (((write - target) | 0) <= 0) return; // nothing buffered past the new timeline
			// Never retreat past the playhead: those samples are already due. The worklet can still
			// advance READ past the value read here, leaving READ ahead of WRITE. That reads as an
			// empty ring (read() returns nothing, insert() trims what the worklet already played) and
			// heals on the first successor sample past the playhead, so it costs a quantum of silence
			// rather than replaying anything.
			const clamped = i32Max(target, readOf(Atomics.load(this.#state, 0)));
			if (((write - clamped) | 0) <= 0) return;
			if (Atomics.compareExchange(this.#control, WRITE, write, clamped) === write) return;
		}
	}

	/**
	 * Flush buffered samples and re-stall, ready to anchor the next utterance (buffered mode).
	 * Main thread only. The worklet reader sees STALLED and stops until the next insert.
	 */
	reset(): void {
		this.#anchored = false;
		Atomics.store(this.#control, STALLED, 1);
		const write = Atomics.load(this.#control, WRITE);
		const state = Atomics.load(this.#state, 0);
		Atomics.store(this.#state, 0, pack(epochOf(state), write));
	}

	/**
	 * Allocate a new ring with `newCapacity` samples and copy the unread window
	 * [READ, WRITE) plus control state into it. Used when growing capacity so
	 * we don't drop buffered audio. If `newCapacity` is smaller than the unread
	 * span, the oldest samples are truncated.
	 *
	 * Main thread only. `resize()` reads from the source `SharedRingBuffer` and
	 * writes into a freshly allocated buffer from `allocSharedRingBuffer`, so it
	 * relies on the same invariant as `insert()`: no concurrent main-thread
	 * writers. The AudioWorklet reader is tolerated via the CAS discipline used
	 * by READ/WRITE elsewhere.
	 */
	resize(newCapacity: number): SharedRingBuffer {
		const init = allocSharedRingBuffer(this.channels, newCapacity, this.rate, this.buffered);
		const dst = new SharedRingBuffer(init);
		dst.#anchored = this.#anchored;
		dst.#anchor = this.#anchor;

		const state = Atomics.load(this.#state, 0);
		const read = readOf(state);
		const write = Atomics.load(this.#control, WRITE);
		const latency = Atomics.load(this.#control, LATENCY);
		const stalled = Atomics.load(this.#control, STALLED);

		const available = (write - read) | 0;
		const copyCount = Math.max(0, Math.min(available, dst.capacity));
		const copyStart = (write - copyCount) | 0;

		for (let channel = 0; channel < this.channels; channel++) {
			const src = this.#samples[channel];
			const out = dst.#samples[channel];
			for (let i = 0; i < copyCount; i++) {
				const idx = (copyStart + i) | 0;
				out[slot(idx, dst.capacity)] = src[slot(idx, this.capacity)];
			}
		}

		Atomics.store(dst.#state, 0, pack(epochOf(state), copyStart));
		Atomics.store(dst.#control, WRITE, write);
		Atomics.store(dst.#control, LATENCY, latency);
		Atomics.store(dst.#control, STALLED, stalled);

		// Carry the unwrapped playhead over, rebased onto dst's READ. Fold the same `read`
		// snapshot the copy used so both sides agree on one observation; `copyStart` is at or
		// ahead of it whenever the copy dropped the oldest samples.
		dst.#position = this.#foldRead(read) + ((copyStart - read) | 0);
		dst.#lastRead = copyStart;

		return dst;
	}

	/**
	 * Fold an observed READ into `#position`, so the returned offset keeps counting up across
	 * the i32 wrap. Idempotent: folding the same value twice adds a zero delta.
	 */
	#foldRead(read: number): number {
		this.#position += (read - this.#lastRead) | 0;
		this.#lastRead = read;
		return this.#position;
	}

	/** `#foldRead` against the current READ. */
	#unwrapRead(): number {
		return this.#foldRead(readOf(Atomics.load(this.#state, 0)));
	}

	/**
	 * Current playback timestamp derived from READ position.
	 *
	 * Main thread only, and stateful: it advances the unwrapped read position, so it has to be
	 * polled rather than sampled once. See `#position`.
	 */
	get timestamp(): Time.Micro {
		return Time.Micro.fromSecond(((this.#anchor + this.#unwrapRead()) / this.rate) as Time.Second);
	}

	/** Whether the buffer is stalled (waiting to fill). */
	get stalled(): boolean {
		return Atomics.load(this.#control, STALLED) === 1;
	}

	/**
	 * Number of buffered samples (WRITE - READ).
	 *
	 * Non-atomic: WRITE and READ are loaded separately, so a concurrent
	 * writer/reader can make the two loads inconsistent. Intended for
	 * tests and diagnostics, not control-flow decisions.
	 */
	get length(): number {
		return (Atomics.load(this.#control, WRITE) - readOf(Atomics.load(this.#state, 0))) | 0;
	}
}

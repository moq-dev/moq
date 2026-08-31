import { Time } from "@moq/net";

// Control array slot indices
const WRITE = 0;
const READ = 1;
const LATENCY = 2;
const STALLED = 3;
// Bumped every time `insert` re-anchors, so a replacement ring can tell whether it still
// shares an index space with the ring it is replacing. See the `SharedRingBuffer` constructor.
const GENERATION = 4;
const CONTROL_SLOTS = 5;

export interface SharedRingBufferInit {
	channels: number;
	capacity: number; // samples per channel, always a power of two
	rate: number;
	samples: SharedArrayBuffer; // channels * capacity * Float32Array.BYTES_PER_ELEMENT bytes
	control: SharedArrayBuffer; // CONTROL_SLOTS * Int32Array.BYTES_PER_ELEMENT bytes
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

	// Initialize STALLED to 1
	const ctrl = new Int32Array(control);
	Atomics.store(ctrl, STALLED, 1);

	return { channels, capacity, rate, samples, control, buffered };
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

/**
 * Atomically advance `arr[idx]` to `candidate` iff `candidate` is strictly ahead
 * (in modular i32 ordering). Retries under contention so the slot only ever
 * moves forward and concurrent writers/readers can't clobber each other.
 */
function casAdvance(arr: Int32Array, idx: number, candidate: number): number {
	for (;;) {
		const current = Atomics.load(arr, idx);
		if (((candidate - current) | 0) <= 0) return current;
		const witnessed = Atomics.compareExchange(arr, idx, current, candidate);
		if (witnessed === current) return candidate;
	}
}

export class SharedRingBuffer {
	readonly channels: number;
	readonly capacity: number;
	readonly rate: number;
	readonly buffered: boolean;
	readonly init: SharedRingBufferInit;

	#control: Int32Array;
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
	 * snapshots READ on the main thread and then hands the replacement over by message, so the
	 * worklet keeps draining the old ring in the meantime. Reconciling here advances past those
	 * samples instead of replaying them, and is skipped when the rings no longer share an index
	 * space (a re-anchor, or a different stream entirely).
	 */
	constructor(init: SharedRingBufferInit, previous?: SharedRingBuffer) {
		this.channels = init.channels;
		this.capacity = init.capacity;
		this.rate = init.rate;
		this.buffered = init.buffered;
		this.init = init;

		this.#control = new Int32Array(init.control);
		this.#samples = [];
		for (let i = 0; i < this.channels; i++) {
			this.#samples.push(
				new Float32Array(init.samples, i * this.capacity * Float32Array.BYTES_PER_ELEMENT, this.capacity),
			);
		}

		if (previous !== undefined) this.#reconcile(previous);
	}

	/**
	 * Advance past samples the reader consumed from `source` after `resize` snapshotted READ.
	 *
	 * Only valid while both rings share an index space, which is what the generation counter
	 * proves. `#anchor` is main-thread state that never crosses the message boundary, so both
	 * worklet-side wrappers read it as 0 and comparing it cannot tell a re-anchor apart. Copying
	 * READ across a re-anchor parks the new timeline behind a playhead measured against the old
	 * one, swallowing however much of the next utterance the previous one had played. A mismatch
	 * skips the reconcile rather than throwing: there is nothing to carry over, and the caller is
	 * mid-swap with no way to recover from an exception.
	 *
	 * This runs on the audio thread while the main thread may be re-anchoring, so the generation
	 * cannot merely be checked once up front: single-word atomics can't read it together with
	 * READ and WRITE. The loop below closes that in three ways, none of which blocks the audio
	 * thread. See `#advanceRead`. All three rest on `insert` bumping the generation before it
	 * rebases the cursors, so no reconcile can complete inside a re-anchor without noticing.
	 */
	#reconcile(source: SharedRingBuffer): void {
		if (source.channels !== this.channels || source.rate !== this.rate) return;

		const candidate = Atomics.load(source.#control, READ);

		for (;;) {
			const generation = Atomics.load(this.#control, GENERATION);
			if (Atomics.load(source.#control, GENERATION) !== generation) return;
			if (this.#advanceRead(candidate, generation)) return;
		}
	}

	/**
	 * One attempt at moving READ to `candidate`, valid only while GENERATION is still
	 * `generation`. Returns false if a concurrent writer moved READ and the caller should retry.
	 *
	 * A re-anchor rebases READ and WRITE and bumps GENERATION, and can land anywhere in here:
	 *
	 * - Clamping to a freshly loaded WRITE keeps an advance from parking READ seconds beyond the
	 *   new timeline, where `read` returns nothing and `insert` drops everything as too old. The
	 *   clamp never binds on the normal path, since `candidate` came from the same index space.
	 * - Exchanging from an exact observed READ, rather than advancing unconditionally, fails
	 *   against the re-anchor's store and retries against the new generation.
	 * - Re-reading GENERATION after a successful exchange catches the case the exchange cannot:
	 *   a re-anchor stores READ back to 0, so an observed 0 is indistinguishable from the 0 a
	 *   fresh ring starts on. Undoing is itself an exchange, so a writer that moved READ in the
	 *   meantime keeps its value.
	 *
	 * What survives is bounded: READ at most at WRITE, which is the empty ring `truncate`
	 * already documents, costing a quantum of silence that heals on the next insert.
	 */
	#advanceRead(candidate: number, generation: number): boolean {
		const current = Atomics.load(this.#control, READ);
		const write = Atomics.load(this.#control, WRITE);

		const target = ((candidate - write) | 0) > 0 ? write : candidate;
		if (((target - current) | 0) <= 0) return true;

		if (Atomics.compareExchange(this.#control, READ, current, target) !== current) return false;

		if (Atomics.load(this.#control, GENERATION) !== generation) {
			Atomics.compareExchange(this.#control, READ, target, current);
		}

		return true;
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
			// Invalidate the generation before rebasing the cursors, never after. A reconcile on
			// the audio thread that observes a stale generation and a rebased READ would commit an
			// old-timeline cursor and escape every check it makes, since nothing it reads has
			// changed yet. Bumping first means such a reconcile either sees the mismatch outright
			// or catches it on the re-read after its exchange.
			Atomics.add(this.#control, GENERATION, 1);

			this.#anchor = start;
			Atomics.store(this.#control, READ, 0);
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
		const read = Atomics.load(this.#control, READ);
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
			casAdvance(this.#control, READ, (end - this.capacity) | 0);
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
		const currentRead = Atomics.load(this.#control, READ);
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
		if (Atomics.load(this.#control, STALLED) === 1) return 0;

		let read = Atomics.load(this.#control, READ);
		const write = Atomics.load(this.#control, WRITE);
		const latency = Atomics.load(this.#control, LATENCY);

		// Latency skip: if buffered data exceeds LATENCY, skip ahead.
		// CAS ensures we never step backward relative to a concurrent writer advance.
		// Disabled in buffered mode, where we deliberately play through the whole buffer.
		const buffered = (write - read) | 0;
		if (!this.buffered && latency > 0 && buffered > latency) {
			const skipTo = (write - latency) | 0;
			read = casAdvance(this.#control, READ, skipTo);
		}

		const available = (write - read) | 0;
		const count = Math.min(available, output[0].length);
		if (count <= 0) return 0;

		// Copy samples
		for (let channel = 0; channel < this.channels; channel++) {
			const src = this.#samples[channel];
			const dst = output[channel];
			for (let i = 0; i < count; i++) {
				dst[i] = src[slot((read + i) | 0, this.capacity)];
			}
		}

		// Advance READ via CAS so a concurrent writer overflow can't be undone.
		casAdvance(this.#control, READ, (read + count) | 0);

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
			const clamped = i32Max(target, Atomics.load(this.#control, READ));
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
		Atomics.store(this.#control, READ, write);
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

		const read = Atomics.load(this.#control, READ);
		const write = Atomics.load(this.#control, WRITE);
		const latency = Atomics.load(this.#control, LATENCY);
		const stalled = Atomics.load(this.#control, STALLED);
		const generation = Atomics.load(this.#control, GENERATION);

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

		Atomics.store(dst.#control, READ, copyStart);
		Atomics.store(dst.#control, WRITE, write);
		Atomics.store(dst.#control, LATENCY, latency);
		Atomics.store(dst.#control, STALLED, stalled);
		Atomics.store(dst.#control, GENERATION, generation);

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
		return this.#foldRead(Atomics.load(this.#control, READ));
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
		return (Atomics.load(this.#control, WRITE) - Atomics.load(this.#control, READ)) | 0;
	}
}

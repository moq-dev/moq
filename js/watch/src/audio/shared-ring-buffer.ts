import { Time } from "@moq/lite";

// Control array slot indices
const WRITE = 0;
const READ = 1;
const LATENCY = 2;
const STALLED = 3;
const CONTROL_SLOTS = 4;

export interface SharedRingBufferInit {
	channels: number;
	capacity: number; // samples per channel
	rate: number;
	samples: SharedArrayBuffer; // channels * capacity * Float32Array.BYTES_PER_ELEMENT bytes
	control: SharedArrayBuffer; // CONTROL_SLOTS * Int32Array.BYTES_PER_ELEMENT bytes
}

export function allocSharedRingBuffer(channels: number, capacity: number, rate: number): SharedRingBufferInit {
	if (channels <= 0) throw new Error("invalid channels");
	if (capacity <= 0) throw new Error("invalid capacity");
	if (rate <= 0) throw new Error("invalid sample rate");

	const samples = new SharedArrayBuffer(channels * capacity * Float32Array.BYTES_PER_ELEMENT);
	const control = new SharedArrayBuffer(CONTROL_SLOTS * Int32Array.BYTES_PER_ELEMENT);

	// Initialize STALLED to 1
	const ctrl = new Int32Array(control);
	Atomics.store(ctrl, STALLED, 1);

	return { channels, capacity, rate, samples, control };
}

/** Modular i32 max: returns a if a is ahead of b, else b. */
function i32Max(a: number, b: number): number {
	return ((a - b) | 0) > 0 ? a : b;
}

/** Maps an absolute sample index to a [0, capacity) array slot. */
function slot(idx: number, capacity: number): number {
	return ((idx % capacity) + capacity) % capacity;
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
	readonly init: SharedRingBufferInit;

	#control: Int32Array;
	#samples: Float32Array[];

	constructor(init: SharedRingBufferInit) {
		this.channels = init.channels;
		this.capacity = init.capacity;
		this.rate = init.rate;
		this.init = init;

		this.#control = new Int32Array(init.control);
		this.#samples = [];
		for (let i = 0; i < this.channels; i++) {
			this.#samples.push(
				new Float32Array(init.samples, i * this.capacity * Float32Array.BYTES_PER_ELEMENT, this.capacity),
			);
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
		const buffered = (write - read) | 0;
		if (latency > 0 && buffered > latency) {
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
		const init = allocSharedRingBuffer(this.channels, newCapacity, this.rate);
		const dst = new SharedRingBuffer(init);

		const read = Atomics.load(this.#control, READ);
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

		Atomics.store(dst.#control, READ, copyStart);
		Atomics.store(dst.#control, WRITE, write);
		Atomics.store(dst.#control, LATENCY, latency);
		Atomics.store(dst.#control, STALLED, stalled);

		return dst;
	}

	/** Current playback timestamp derived from READ position. */
	get timestamp(): Time.Micro {
		const read = Atomics.load(this.#control, READ);
		return Time.Micro.fromSecond((read / this.rate) as Time.Second);
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

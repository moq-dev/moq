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

export class SharedRingBuffer {
	readonly channels: number;
	readonly capacity: number;
	readonly rate: number;

	#control: Int32Array;
	#samples: Float32Array[];

	constructor(init: SharedRingBufferInit) {
		this.channels = init.channels;
		this.capacity = init.capacity;
		this.rate = init.rate;

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

		// Overflow: if the write would exceed capacity from current READ, advance READ
		if (((end - read) | 0) > this.capacity) {
			Atomics.store(this.#control, READ, (end - this.capacity) | 0);
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

		// Latency skip: if buffered data exceeds LATENCY, skip ahead
		const buffered = (write - read) | 0;
		if (latency > 0 && buffered > latency) {
			read = (write - latency) | 0;
			// Store the skip — but don't go backwards if writer overflowed concurrently
			const current = Atomics.load(this.#control, READ);
			if (((read - current) | 0) > 0) {
				Atomics.store(this.#control, READ, read);
			}
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

		// Advance READ, but don't undo a concurrent writer overflow advance
		const desired = (read + count) | 0;
		const current = Atomics.load(this.#control, READ);
		if (((desired - current) | 0) > 0) {
			Atomics.store(this.#control, READ, desired);
		}

		return count;
	}

	/** Update the target latency in samples. */
	setLatency(samples: number): void {
		Atomics.store(this.#control, LATENCY, samples);
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

	/** Number of buffered samples (WRITE - READ). */
	get length(): number {
		return (Atomics.load(this.#control, WRITE) - Atomics.load(this.#control, READ)) | 0;
	}
}

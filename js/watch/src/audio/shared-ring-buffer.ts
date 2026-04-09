// SharedArrayBuffer-based ring buffer for lock-free audio between main thread and AudioWorklet.
// Uses wrapping indices in [0, capacity) with Atomics for cross-thread visibility.
// The same class is used on both sides; the main thread calls write(), the worklet calls read().

// Control array layout
const WRITE_INDEX = 0;
const READ_INDEX = 1;
const STALLED = 2;

export interface SharedRingBufferInit {
	channels: number;
	capacity: number;
	samples: SharedArrayBuffer;
	control: SharedArrayBuffer;
}

// Allocate the SharedArrayBuffers for a ring buffer with the given parameters.
export function allocSharedRingBuffer(channels: number, capacity: number): SharedRingBufferInit {
	const samples = new SharedArrayBuffer(channels * capacity * Float32Array.BYTES_PER_ELEMENT);
	const control = new SharedArrayBuffer(3 * Int32Array.BYTES_PER_ELEMENT);
	return { channels, capacity, samples, control };
}

export class SharedRingBuffer {
	readonly channels: number;
	readonly capacity: number;

	// Per-channel Float32Array views into the shared samples buffer.
	#samples: Float32Array[];

	// [WRITE_INDEX, READ_INDEX, STALLED] — accessed via Atomics.
	#control: Int32Array;

	constructor(init: SharedRingBufferInit) {
		this.channels = init.channels;
		this.capacity = init.capacity;

		this.#samples = [];
		for (let i = 0; i < this.channels; i++) {
			this.#samples[i] = new Float32Array(
				init.samples,
				i * init.capacity * Float32Array.BYTES_PER_ELEMENT,
				init.capacity,
			);
		}

		this.#control = new Int32Array(init.control);
	}

	get stalled(): boolean {
		return Atomics.load(this.#control, STALLED) === 1;
	}

	get length(): number {
		const write = Atomics.load(this.#control, WRITE_INDEX);
		const read = Atomics.load(this.#control, READ_INDEX);
		return (write - read + this.capacity) % this.capacity;
	}

	// Push decoded audio samples into the buffer (main thread / producer).
	// Returns the number of samples that were overwritten (overflow).
	write(data: Float32Array[]): number {
		if (data.length !== this.channels) throw new Error("wrong number of channels");

		const count = data[0].length;
		if (count === 0) return 0;
		if (count >= this.capacity) throw new Error("data exceeds buffer capacity");

		const writeIdx = Atomics.load(this.#control, WRITE_INDEX);
		let readIdx = Atomics.load(this.#control, READ_INDEX);

		// Available samples in the buffer (unread).
		const available = (writeIdx - readIdx + this.capacity) % this.capacity;

		// Space left: capacity - 1 to distinguish full from empty.
		const space = this.capacity - 1 - available;

		let overflow = 0;

		if (count > space) {
			// Overflow: advance readIndex to make room.
			overflow = count - space;
			readIdx = (readIdx + overflow) % this.capacity;
			Atomics.store(this.#control, READ_INDEX, readIdx);
			Atomics.store(this.#control, STALLED, 0);
		}

		// Write samples, splitting at the buffer boundary.
		for (let ch = 0; ch < this.channels; ch++) {
			const src = data[ch];
			const dst = this.#samples[ch];

			const firstPart = Math.min(count, this.capacity - writeIdx);
			dst.set(src.subarray(0, firstPart), writeIdx);

			if (firstPart < count) {
				dst.set(src.subarray(firstPart), 0);
			}
		}

		// Update write index after data is written.
		const newWriteIdx = (writeIdx + count) % this.capacity;
		Atomics.store(this.#control, WRITE_INDEX, newWriteIdx);

		return overflow;
	}

	// Pull samples from the buffer into the output (worklet / consumer).
	read(output: Float32Array[]): number {
		if (output.length !== this.channels) throw new Error("wrong number of channels");
		if (Atomics.load(this.#control, STALLED) === 1) return 0;

		const writeIdx = Atomics.load(this.#control, WRITE_INDEX);
		const readIdx = Atomics.load(this.#control, READ_INDEX);

		const available = (writeIdx - readIdx + this.capacity) % this.capacity;
		const count = Math.min(available, output[0].length);
		if (count === 0) return 0;

		// Read samples, splitting at the buffer boundary.
		for (let ch = 0; ch < this.channels; ch++) {
			const src = this.#samples[ch];
			const dst = output[ch];

			const firstPart = Math.min(count, this.capacity - readIdx);
			dst.set(src.subarray(readIdx, readIdx + firstPart));

			if (firstPart < count) {
				dst.set(src.subarray(0, count - firstPart), firstPart);
			}
		}

		// Advance read index. Use compareExchange so an overflow on the main thread
		// (which also writes readIndex) isn't reverted by a stale worklet store.
		const newReadIdx = (readIdx + count) % this.capacity;
		const prev = Atomics.compareExchange(this.#control, READ_INDEX, readIdx, newReadIdx);
		if (prev !== readIdx) {
			// Main thread advanced readIndex (overflow) since we loaded it.
			// Our read data may be stale, but the audio will self-correct next quantum.
		}

		return count;
	}
}

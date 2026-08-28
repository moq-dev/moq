import { describe, expect, it } from "bun:test";
import { Time } from "@moq/net";
import { allocSharedRingBuffer, SharedRingBuffer } from "./shared-ring-buffer";

function create(props?: { rate?: number; channels?: number; capacity?: number; latency?: number }) {
	const rate = props?.rate ?? 1000;
	const channels = props?.channels ?? 2;
	const capacity = props?.capacity ?? 100;
	const init = allocSharedRingBuffer(channels, capacity, rate);
	const buffer = new SharedRingBuffer(init);
	if (props?.latency !== undefined) {
		buffer.setLatency(props.latency);
	} else {
		buffer.setLatency(capacity);
	}
	return buffer;
}

function insert(
	buffer: SharedRingBuffer,
	timestampMs: number,
	samples: number,
	opts?: { channels?: number; value?: number },
): void {
	const channelCount = opts?.channels ?? buffer.channels;
	const data: Float32Array[] = [];
	for (let i = 0; i < channelCount; i++) {
		const channel = new Float32Array(samples);
		channel.fill(opts?.value ?? 1.0);
		data.push(channel);
	}
	buffer.insert(Time.Micro.fromMilli(timestampMs as Time.Milli), data);
}

function read(buffer: SharedRingBuffer, samples: number, channelCount?: number): Float32Array[] {
	const ch = channelCount ?? buffer.channels;
	const output: Float32Array[] = [];
	for (let i = 0; i < ch; i++) {
		output.push(new Float32Array(samples));
	}
	const samplesRead = buffer.read(output);
	if (samplesRead < samples) {
		return output.map((channel) => channel.slice(0, samplesRead));
	}
	return output;
}

describe("initialization", () => {
	it("should allocate correct SAB sizes", () => {
		const init = allocSharedRingBuffer(2, 128, 1000);
		expect(init.channels).toBe(2);
		expect(init.capacity).toBe(128);
		expect(init.rate).toBe(1000);
		expect(init.samples.byteLength).toBe(2 * 128 * 4); // 2 channels * 128 samples * Float32
		expect(init.control.byteLength).toBe(4 * 4); // 4 control slots * Int32
	});

	it("rounds capacity up to a power of two", () => {
		// slot() masks rather than taking a remainder, which is what keeps it wrap-invariant.
		expect(allocSharedRingBuffer(1, 100, 1000).capacity).toBe(128);
		expect(allocSharedRingBuffer(1, 128, 1000).capacity).toBe(128);
		expect(allocSharedRingBuffer(1, 129, 1000).capacity).toBe(256);
		expect(allocSharedRingBuffer(1, 1, 1000).capacity).toBe(1);
	});

	it("should start stalled", () => {
		const buffer = create();
		expect(buffer.stalled).toBe(true);
		expect(buffer.length).toBe(0);
	});

	it("should throw on invalid channels", () => {
		expect(() => allocSharedRingBuffer(0, 100, 1000)).toThrow(/invalid channels/);
	});

	it("should throw on invalid capacity", () => {
		expect(() => allocSharedRingBuffer(2, 0, 1000)).toThrow(/invalid capacity/);
	});

	it("should throw on invalid sample rate", () => {
		expect(() => allocSharedRingBuffer(2, 100, 0)).toThrow(/invalid sample rate/);
	});
});

describe("insert", () => {
	it("should write continuous data", () => {
		const buffer = create({ rate: 1000, channels: 2, capacity: 100, latency: 100 });

		insert(buffer, 0, 10, { value: 1.0 });
		expect(buffer.length).toBe(10);

		insert(buffer, 10, 10, { value: 2.0 });
		expect(buffer.length).toBe(20);
	});

	it("should handle gaps by filling with zeros", () => {
		const buffer = create({ rate: 1000, channels: 2, capacity: 100, latency: 100 });

		insert(buffer, 0, 10, { value: 1.0 });

		// Write at timestamp 20ms (sample 20), creating a 10-sample gap
		insert(buffer, 20, 10, { value: 2.0 });

		expect(buffer.length).toBe(30); // 10 + 10 (gap) + 10

		// Fill to exit stalled mode
		insert(buffer, 30, 70, { value: 0.0 });
		expect(buffer.stalled).toBe(false);

		// Read and verify the gap was filled with zeros
		const output = read(buffer, 30);
		expect(output[0].length).toBe(30);

		for (let i = 0; i < 10; i++) {
			expect(output[0][i]).toBe(1.0);
			expect(output[1][i]).toBe(1.0);
		}
		for (let i = 10; i < 20; i++) {
			expect(output[0][i]).toBe(0);
			expect(output[1][i]).toBe(0);
		}
		for (let i = 20; i < 30; i++) {
			expect(output[0][i]).toBe(2.0);
			expect(output[1][i]).toBe(2.0);
		}
	});

	it("should handle out-of-order writes", () => {
		const buffer = create({ rate: 1000, channels: 1, capacity: 100, latency: 100 });

		// Fill buffer to exit stalled mode
		insert(buffer, 0, 100, { channels: 1, value: 0.0 });
		expect(buffer.stalled).toBe(false);

		// Read 50 samples to advance read pointer to 50
		read(buffer, 50, 1);

		// Write at timestamp 120ms — creates a gap from 100-120
		insert(buffer, 120, 10, { channels: 1, value: 1.0 });

		// Now fill part of the gap at timestamp 110ms
		insert(buffer, 110, 10, { channels: 1, value: 2.0 });

		expect(buffer.length).toBe(80); // 130 - 50

		// Skip the old samples and gap
		read(buffer, 60, 1); // Read samples 50-109

		// Read and verify the out-of-order writes
		const output = read(buffer, 20, 1);
		expect(output[0].length).toBe(20);

		for (let i = 0; i < 10; i++) {
			expect(output[0][i]).toBe(2.0);
		}
		for (let i = 10; i < 20; i++) {
			expect(output[0][i]).toBe(1.0);
		}
	});

	it("should discard samples that are too old", () => {
		const buffer = create({ rate: 1000, channels: 2, capacity: 100, latency: 100 });

		// Fill and exit stalled mode
		insert(buffer, 0, 100, { value: 0.0 });
		expect(buffer.stalled).toBe(false);

		// Read 60 samples, readIndex now at 60
		read(buffer, 60);

		// Write 50 new samples at timestamp 100
		insert(buffer, 100, 50, { value: 1.0 });
		expect(buffer.length).toBe(90); // 150 - 60

		// Read 10 more, readIndex now at 70
		read(buffer, 10);
		expect(buffer.length).toBe(80); // 150 - 70

		// Write data before read index — should be discarded
		insert(buffer, 50, 5, { value: 2.0 });
		expect(buffer.length).toBe(80); // unchanged
	});

	it("should throw on wrong channel count", () => {
		const buffer = create({ channels: 2 });
		expect(() => {
			buffer.insert(0 as Time.Micro, [new Float32Array(10)]); // only 1 channel
		}).toThrow(/wrong number of channels/);
	});
});

describe("reading", () => {
	it("should read available data", () => {
		const buffer = create({ rate: 1000, channels: 2, capacity: 100, latency: 100 });

		// Fill to exit stalled mode
		insert(buffer, 0, 100, { value: 0.0 });
		expect(buffer.stalled).toBe(false);

		read(buffer, 80);
		expect(buffer.length).toBe(20);

		insert(buffer, 100, 20, { value: 1.5 });
		expect(buffer.length).toBe(40);

		// Read old samples first
		const output1 = read(buffer, 20);
		expect(output1[0].length).toBe(20);
		for (let i = 0; i < 20; i++) {
			expect(output1[0][i]).toBe(0.0);
		}

		// Read the new samples
		const output2 = read(buffer, 10);
		expect(output2[0].length).toBe(10);
		for (let i = 0; i < 10; i++) {
			expect(output2[0][i]).toBe(1.5);
		}
	});

	it("should handle partial reads", () => {
		const buffer = create({ rate: 1000, channels: 2, capacity: 100, latency: 100 });

		// Fill and exit stalled
		insert(buffer, 0, 100, { value: 0.0 });
		read(buffer, 80);

		insert(buffer, 100, 20, { value: 1.0 });
		expect(buffer.length).toBe(40);

		// Try to read 50 (only 40 available)
		const output = read(buffer, 50);
		expect(output[0].length).toBe(40);
		expect(buffer.length).toBe(0);
	});

	it("should return 0 when stalled", () => {
		const buffer = create();
		const output = read(buffer, 10);
		expect(output[0].length).toBe(0);
	});

	it("should return 0 when empty", () => {
		const buffer = create({ rate: 1000, channels: 2, capacity: 100, latency: 100 });

		// Fill and drain to exit stalled
		insert(buffer, 0, 100, { value: 0.0 });
		read(buffer, 100);
		expect(buffer.length).toBe(0);

		// Try to read — empty but not stalled
		const output = read(buffer, 10);
		expect(output[0].length).toBe(0);
	});
});

describe("stall behavior", () => {
	it("should start stalled and not output data", () => {
		const buffer = create({ rate: 1000, channels: 2, capacity: 100, latency: 100 });
		expect(buffer.stalled).toBe(true);

		insert(buffer, 0, 50, { value: 1.0 });
		const output = read(buffer, 10);
		expect(output[0].length).toBe(0);
	});

	it("should un-stall when buffer reaches LATENCY", () => {
		const buffer = create({ rate: 1000, channels: 2, capacity: 100, latency: 50 });

		// Write 49 samples — not enough
		insert(buffer, 0, 49, { value: 1.0 });
		expect(buffer.stalled).toBe(true);

		// Write 1 more to reach 50 = LATENCY
		insert(buffer, 49, 1, { value: 1.0 });
		expect(buffer.stalled).toBe(false);
	});

	it("should un-stall on overflow (buffer reaches capacity)", () => {
		const buffer = create({ rate: 1000, channels: 2, capacity: 100, latency: 100 });

		// Fill buffer completely
		insert(buffer, 0, 100, { value: 1.0 });
		expect(buffer.stalled).toBe(false);

		// Overflow — should still not be stalled
		insert(buffer, 100, 10, { value: 2.0 });
		expect(buffer.stalled).toBe(false);
	});
});

describe("ring wrapping", () => {
	it("should wrap around when buffer is full", () => {
		const buffer = create({ rate: 1000, channels: 1, capacity: 128, latency: 128 });

		// Fill buffer
		insert(buffer, 0, 128, { channels: 1, value: 1.0 });
		expect(buffer.stalled).toBe(false);

		// Read 64 to make room
		read(buffer, 64, 1);

		// Write 64 more
		insert(buffer, 128, 64, { channels: 1, value: 2.0 });
		expect(buffer.length).toBe(128);

		// Write 64 more — wraps around
		insert(buffer, 192, 64, { channels: 1, value: 3.0 });
		expect(buffer.length).toBe(128);

		const output = read(buffer, 128, 1);
		expect(output[0].length).toBe(128);

		for (let i = 0; i < 64; i++) {
			expect(output[0][i]).toBe(2.0);
		}
		for (let i = 64; i < 128; i++) {
			expect(output[0][i]).toBe(3.0);
		}
	});
});

describe("multi-channel", () => {
	it("should handle stereo data correctly", () => {
		const buffer = create({ rate: 1000, channels: 2, capacity: 100, latency: 100 });

		// Fill and exit stalled
		insert(buffer, 0, 100, { value: 0.5 });
		expect(buffer.stalled).toBe(false);

		read(buffer, 80);

		insert(buffer, 100, 20, { value: 1.5 });

		// Read old data
		const output = read(buffer, 20);
		expect(output[0].length).toBe(20);
		expect(output[1].length).toBe(20);

		for (let i = 0; i < 20; i++) {
			expect(output[0][i]).toBe(0.5);
			expect(output[1][i]).toBe(0.5);
		}

		// Read new data
		const output2 = read(buffer, 20);
		for (let i = 0; i < 20; i++) {
			expect(output2[0][i]).toBe(1.5);
			expect(output2[1][i]).toBe(1.5);
		}
	});
});

describe("edge cases", () => {
	it("should handle zero-length output buffers", () => {
		const buffer = create({ latency: 50 });
		insert(buffer, 0, 50, { value: 1.0 });

		const output = [new Float32Array(0), new Float32Array(0)];
		const samplesRead = buffer.read(output);
		expect(samplesRead).toBe(0);
	});

	it("should handle fractional timestamps", () => {
		const buffer = create({ rate: 1000, channels: 2, capacity: 200, latency: 200 });

		// Fill buffer to exit stalled
		insert(buffer, 0, 200, { value: 0.0 });
		read(buffer, 200);

		// Fractional timestamp that rounds
		insert(buffer, 1105, 10, { value: 1.0 }); // 110.5 samples → rounds to 1105
		insert(buffer, 1204, 10, { value: 2.0 }); // 120.4 samples → rounds to 1204

		const output = read(buffer, 20);
		expect(output[0].length).toBeGreaterThan(0);
	});
});

describe("overflow", () => {
	it("should advance READ when exceeding capacity", () => {
		const buffer = create({ rate: 1000, channels: 1, capacity: 128, latency: 64 });

		// Fill buffer to 64 (LATENCY) to un-stall
		insert(buffer, 0, 64, { channels: 1, value: 1.0 });
		expect(buffer.stalled).toBe(false);

		// Write way past capacity — should advance READ
		insert(buffer, 0, 256, { channels: 1, value: 2.0 });

		// Buffer should still have <= capacity samples
		expect(buffer.length).toBeLessThanOrEqual(buffer.capacity);
		expect(buffer.stalled).toBe(false);
	});

	it("should handle oversized frames", () => {
		const buffer = create({ rate: 1000, channels: 1, capacity: 128, latency: 64 });

		// Write a frame larger than the buffer capacity
		insert(buffer, 0, 192, { channels: 1, value: 1.0 });

		expect(buffer.length).toBeLessThanOrEqual(buffer.capacity);
		// Should un-stall due to overflow advancing READ
		expect(buffer.stalled).toBe(false);
	});
});

describe("latency skip", () => {
	it("should skip READ when buffered exceeds LATENCY", () => {
		const buffer = create({ rate: 1000, channels: 1, capacity: 100, latency: 20 });

		// Fill 60 samples — exceeds LATENCY of 20
		insert(buffer, 0, 60, { channels: 1, value: 1.0 });
		expect(buffer.stalled).toBe(false);

		// Read should skip ahead to maintain LATENCY distance from WRITE
		const output = read(buffer, 128, 1);

		// Should only get LATENCY (20) samples, skipping the first 40
		expect(output[0].length).toBe(20);
	});

	it("should not skip when buffered is within LATENCY", () => {
		const buffer = create({ rate: 1000, channels: 1, capacity: 100, latency: 50 });

		// Fill exactly 50 samples = LATENCY
		insert(buffer, 0, 50, { channels: 1, value: 1.0 });
		expect(buffer.stalled).toBe(false);

		// Read should get all 50 — no skip needed
		const output = read(buffer, 128, 1);
		expect(output[0].length).toBe(50);
	});
});

describe("timestamp getter", () => {
	it("should track READ position as timestamp", () => {
		const buffer = create({ rate: 1000, channels: 1, capacity: 100, latency: 100 });

		// Fill and un-stall
		insert(buffer, 0, 100, { channels: 1, value: 0.0 });
		expect(buffer.stalled).toBe(false);

		// Initially at 0
		expect(buffer.timestamp).toBe(0 as Time.Micro);

		// Read 50 samples
		read(buffer, 50, 1);

		// Timestamp should reflect READ = 50 at rate 1000
		// 50 / 1000 = 0.05 seconds = 50000 microseconds
		expect(buffer.timestamp).toBe(50000 as Time.Micro);
	});
});

describe("setLatency", () => {
	it("should dynamically change latency affecting skip behavior", () => {
		const buffer = create({ rate: 1000, channels: 1, capacity: 100, latency: 50 });

		// Fill 80 samples
		insert(buffer, 0, 80, { channels: 1, value: 1.0 });
		expect(buffer.stalled).toBe(false);

		// With LATENCY=50, reading should skip to 30 (80-50)
		const output1 = read(buffer, 128, 1);
		expect(output1[0].length).toBe(50);

		// Write more
		insert(buffer, 80, 80, { channels: 1, value: 2.0 });

		// Change latency to 20
		buffer.setLatency(20);

		// Now reading should skip more aggressively
		const output2 = read(buffer, 128, 1);
		expect(output2[0].length).toBe(20);
	});
});

describe("stalled getter", () => {
	it("should reflect STALLED flag", () => {
		const buffer = create({ rate: 1000, channels: 1, capacity: 100, latency: 50 });
		expect(buffer.stalled).toBe(true);

		insert(buffer, 0, 50, { channels: 1, value: 1.0 });
		expect(buffer.stalled).toBe(false);
	});
});

describe("length getter", () => {
	it("should report buffered samples", () => {
		const buffer = create({ rate: 1000, channels: 1, capacity: 100, latency: 100 });

		expect(buffer.length).toBe(0);

		insert(buffer, 0, 30, { channels: 1, value: 1.0 });
		expect(buffer.length).toBe(30);

		// Fill to un-stall and read
		insert(buffer, 30, 70, { channels: 1, value: 0.0 });
		read(buffer, 50, 1);
		expect(buffer.length).toBe(50);
	});
});

describe("i32 wrap epoch", () => {
	// READ/WRITE live in an Int32Array because they are shared with the worklet, so they wrap
	// every ~13.5h at 44.1kHz. The modular comparisons cope; these cover the two places that
	// used to read the raw signed value as a magnitude.
	const WRITE = 0;
	const READ = 1;

	it("keeps the playhead advancing across the i32 wrap", () => {
		const init = allocSharedRingBuffer(1, 64, 1000);
		const control = new Int32Array(init.control);
		const buffer = new SharedRingBuffer(init);
		buffer.setLatency(32);

		insert(buffer, 0, 32, { channels: 1, value: 0.5 });
		expect(Time.Milli.fromMicro(buffer.timestamp)).toBe(0 as Time.Milli);

		// Step READ up to the boundary and over it, observing each time. Production gets these
		// observations from the 50ms poll in `buffer.ts`.
		control[READ] = (0x7fffffff - 1) | 0;
		const before = buffer.timestamp;
		control[READ] = 0x7fffffff | 0;
		const at = buffer.timestamp;
		control[READ] = -0x80000000;
		const after = buffer.timestamp;

		// One sample at 1000Hz is 1ms. Reading the negative half as a magnitude used to report
		// the playhead jumping back 2^32 samples here. The loose tolerance is double rounding
		// in the micro/second conversion at this magnitude, not drift in the position.
		expect(at - before).toBeCloseTo(1000, 2);
		expect(after - at).toBeCloseTo(1000, 2);
		expect(after).toBeGreaterThan(0);
	});

	it("reads back a frame that straddles the i32 wrap", () => {
		// Ask for 100, get 128: a power-of-two capacity divides 2^32, so the slot mapping stays
		// continuous across the boundary. At 100 the frame below aliased onto unread slots.
		const init = allocSharedRingBuffer(1, 100, 1000);
		expect(init.capacity).toBe(128);

		const control = new Int32Array(init.control);
		const buffer = new SharedRingBuffer(init);
		buffer.setLatency(16);

		insert(buffer, 0, 16, { channels: 1, value: 0.25 });
		read(buffer, 16, 1);

		// Park both cursors 8 samples below the boundary so the next frame straddles it.
		const edge = 0x7fffffff - 8;
		control[READ] = edge | 0;
		control[WRITE] = edge | 0;

		insert(buffer, edge, 16, { channels: 1, value: 0.75 });
		expect(buffer.length).toBe(16);

		const out = read(buffer, 16, 1);
		expect(out[0].length).toBe(16);
		for (let i = 0; i < 16; i++) {
			expect(out[0][i]).toBe(0.75);
		}
	});
});

describe("i32 wrapping", () => {
	it("should handle modular arithmetic with large sample indices", () => {
		const buffer = create({ rate: 1000, channels: 1, capacity: 100, latency: 100 });

		// At rate 1000: sample = round(seconds * 1000)
		// Use 2_000_000 ms = 2000 seconds → sample index 2_000_000
		insert(buffer, 2_000_000, 100, { channels: 1, value: 42.0 });
		expect(buffer.stalled).toBe(false);

		const output = read(buffer, 100, 1);
		expect(output[0].length).toBe(100);
		for (let i = 0; i < 100; i++) {
			expect(output[0][i]).toBe(42.0);
		}
	});

	it("plays a stream whose sample index truncates to a negative i32", () => {
		// Regression: an unanchored index from a long-running stream truncates to a negative
		// Int32, which pinned WRITE at 0 and left the ring silent forever.
		const rate = 44100;
		const buffer = create({ rate, channels: 1, capacity: rate, latency: 4410 });

		// 743975s in: sample 32_809_297_500, negative as an i32.
		const startMs = 743_975_000;
		expect(Math.round((startMs / 1000) * rate) | 0).toBeLessThan(0);

		insert(buffer, startMs, 4410, { channels: 1, value: 0.5 });
		insert(buffer, startMs + 100, 4410, { channels: 1, value: 0.5 });

		expect(buffer.stalled).toBe(false);
		// The anchor is preserved, so media time survives the wrap rather than reading negative.
		expect(buffer.timestamp).toBe(Time.Micro.fromMilli(startMs as Time.Milli));
		const output = read(buffer, 128, 1);
		expect(output[0].length).toBe(128);
		expect(output[0][0]).toBeCloseTo(0.5, 5);
	});

	it("should handle slot indexing past capacity boundary", () => {
		// capacity=10, start at sample 97 → wraps across boundary
		const buffer = create({ rate: 1000, channels: 1, capacity: 10, latency: 10 });

		insert(buffer, 97, 10, { channels: 1, value: 7.0 });
		expect(buffer.stalled).toBe(false);

		const output = read(buffer, 10, 1);
		expect(output[0].length).toBe(10);
		for (let i = 0; i < 10; i++) {
			expect(output[0][i]).toBe(7.0);
		}
	});
});

describe("SharedRingBuffer.resize", () => {
	it("preserves the unread window when growing capacity", () => {
		const src = create({ rate: 1000, channels: 2, capacity: 64, latency: 30 });
		insert(src, 0, 30, { value: 3.5 });
		expect(src.stalled).toBe(false);

		const dst = src.resize(256);
		expect(dst.capacity).toBe(256);
		expect(dst.channels).toBe(2);
		expect(dst.rate).toBe(1000);

		// The 30 unread samples should be readable from the new buffer.
		const out = read(dst, 30);
		expect(out[0].length).toBe(30);
		for (let i = 0; i < 30; i++) {
			expect(out[0][i]).toBe(3.5);
			expect(out[1][i]).toBe(3.5);
		}
		// Unstalled state carried across.
		expect(dst.stalled).toBe(false);
	});

	it("truncates to the newest samples when shrinking below the unread span", () => {
		const src = create({ rate: 1000, channels: 1, capacity: 64, latency: 64 });
		// Fill [0, 48) with value 1, then [48, 64) with value 2.
		insert(src, 0, 48, { channels: 1, value: 1.0 });
		insert(src, 48, 16, { channels: 1, value: 2.0 });

		const dst = src.resize(16);
		expect(dst.capacity).toBe(16);

		// Only the most recent 16 samples fit.
		const out = read(dst, 16, 1);
		for (let i = 0; i < 16; i++) {
			expect(out[0][i]).toBe(2.0);
		}
	});
});

describe("buffered mode", () => {
	function createBuffered(latency: number) {
		// Capacity large enough to hold a whole utterance without overflow.
		const init = allocSharedRingBuffer(1, 10000, 1000, true);
		const buffer = new SharedRingBuffer(init);
		buffer.setLatency(latency);
		return buffer;
	}

	it("anchors to the first frame instead of index 0", () => {
		const buffer = createBuffered(50);
		// First frame at a future timestamp; READ should snap to it, not gap-fill from 0.
		insert(buffer, 2000, 100, { channels: 1, value: 0.1 });
		expect(Time.Milli.fromMicro(buffer.timestamp)).toBe(2000 as Time.Milli);
		expect(buffer.stalled).toBe(false); // 100 buffered >= 50 latency
	});

	it("plays through the whole buffer without skipping ahead", () => {
		const buffer = createBuffered(50);
		// Dump a 1s utterance faster than real-time with consecutive future timestamps.
		for (let i = 0; i < 10; i++) {
			insert(buffer, 2000 + i * 100, 100, { channels: 1, value: (i + 1) / 10 });
		}
		expect(buffer.length).toBe(1000);

		// Read the oldest frame first; a non-buffered ring would have skipped to write-latency.
		const first = read(buffer, 100, 1);
		expect(first[0][0]).toBeCloseTo(0.1, 5);
		expect(Time.Milli.fromMicro(buffer.timestamp)).toBe(2100 as Time.Milli);

		const second = read(buffer, 100, 1);
		expect(second[0][0]).toBeCloseTo(0.2, 5);
	});

	it("reset re-stalls and re-anchors to the next utterance", () => {
		const buffer = createBuffered(50);
		insert(buffer, 2000, 100, { channels: 1, value: 0.1 });
		read(buffer, 50, 1);

		buffer.reset();
		expect(buffer.stalled).toBe(true);

		// A new utterance with its own timestamps anchors fresh.
		insert(buffer, 500, 100, { channels: 1, value: 0.9 });
		expect(Time.Milli.fromMicro(buffer.timestamp)).toBe(500 as Time.Milli);
		const out = read(buffer, 100, 1);
		expect(out[0][0]).toBeCloseTo(0.9, 5);
	});

	it("drops the oldest samples once the buffer exceeds the cap", () => {
		// 256-sample capacity at 1000Hz = a 256ms cap.
		const init = allocSharedRingBuffer(1, 256, 1000, true);
		const buffer = new SharedRingBuffer(init);
		buffer.setLatency(50);

		insert(buffer, 0, 128, { channels: 1, value: 0.1 }); // [0, 128)
		insert(buffer, 128, 128, { channels: 1, value: 0.2 }); // [128, 256)
		insert(buffer, 256, 128, { channels: 1, value: 0.3 }); // exceeds cap; drops [0, 128)

		// READ skipped forward to stay within the cap; oldest frame is gone.
		expect(Time.Milli.fromMicro(buffer.timestamp)).toBe(128 as Time.Milli);
		expect(read(buffer, 128, 1)[0][0]).toBeCloseTo(0.2, 5);
		expect(read(buffer, 128, 1)[0][0]).toBeCloseTo(0.3, 5);
	});

	it("truncate drops the write-ahead tail a successor supersedes", () => {
		const buffer = createBuffered(50);
		for (let i = 0; i < 10; i++) {
			insert(buffer, 2000 + i * 100, 100, { channels: 1, value: 0.1 });
		}
		read(buffer, 100, 1); // playhead at 2100

		// A successor track takes over at 2200 with 100ms of its own audio. Writing it only overwrites
		// [2200, 2300); without the truncate, [2300, 3000) of the old track still plays after it.
		buffer.truncate(Time.Micro.fromMilli(2200 as Time.Milli));
		insert(buffer, 2200, 100, { channels: 1, value: 0.9 });

		expect(buffer.stalled).toBe(false); // truncate keeps playing, unlike reset()
		expect(read(buffer, 100, 1)[0][0]).toBeCloseTo(0.1, 5); // [2100, 2200) was already due
		expect(read(buffer, 100, 1)[0][0]).toBeCloseTo(0.9, 5); // the successor
		expect(read(buffer, 100, 1)[0].length).toBe(0); // and nothing after it
	});

	it("truncate never rewinds past the playhead", () => {
		const buffer = createBuffered(50);
		insert(buffer, 2000, 1000, { channels: 1, value: 0.1 });
		read(buffer, 100, 1); // playhead at 2100

		// A successor whose first frame predates the playhead: those samples are already due, so WRITE
		// floors there rather than going backwards.
		buffer.truncate(Time.Micro.fromMilli(1000 as Time.Milli));
		expect(buffer.length).toBe(0);
		expect(read(buffer, 100, 1)[0].length).toBe(0);
	});
});

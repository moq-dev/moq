import { describe, expect, it } from "bun:test";
import { Time } from "@moq/lite";
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
		const init = allocSharedRingBuffer(2, 100, 1000);
		expect(init.channels).toBe(2);
		expect(init.capacity).toBe(100);
		expect(init.rate).toBe(1000);
		expect(init.samples.byteLength).toBe(2 * 100 * 4); // 2 channels * 100 samples * Float32
		expect(init.control.byteLength).toBe(4 * 4); // 4 control slots * Int32
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
		const buffer = create({ rate: 1000, channels: 1, capacity: 100, latency: 100 });

		// Fill buffer
		insert(buffer, 0, 100, { channels: 1, value: 1.0 });
		expect(buffer.stalled).toBe(false);

		// Read 50 to make room
		read(buffer, 50, 1);

		// Write 50 more
		insert(buffer, 100, 50, { channels: 1, value: 2.0 });
		expect(buffer.length).toBe(100);

		// Write 50 more — wraps around
		insert(buffer, 150, 50, { channels: 1, value: 3.0 });
		expect(buffer.length).toBe(100);

		const output = read(buffer, 100, 1);
		expect(output[0].length).toBe(100);

		for (let i = 0; i < 50; i++) {
			expect(output[0][i]).toBe(2.0);
		}
		for (let i = 50; i < 100; i++) {
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
		const buffer = create({ rate: 1000, channels: 1, capacity: 100, latency: 50 });

		// Fill buffer to 50 (LATENCY) to un-stall
		insert(buffer, 0, 50, { channels: 1, value: 1.0 });
		expect(buffer.stalled).toBe(false);

		// Write way past capacity — should advance READ
		insert(buffer, 0, 200, { channels: 1, value: 2.0 });

		// Buffer should still have <= capacity samples
		expect(buffer.length).toBeLessThanOrEqual(100);
		expect(buffer.stalled).toBe(false);
	});

	it("should handle oversized frames", () => {
		const buffer = create({ rate: 1000, channels: 1, capacity: 100, latency: 50 });

		// Write a frame larger than the buffer capacity
		insert(buffer, 0, 150, { channels: 1, value: 1.0 });

		expect(buffer.length).toBeLessThanOrEqual(100);
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
		const src = create({ rate: 1000, channels: 2, capacity: 50, latency: 30 });
		insert(src, 0, 30, { value: 3.5 });
		expect(src.stalled).toBe(false);

		const dst = src.resize(200);
		expect(dst.capacity).toBe(200);
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
		const src = create({ rate: 1000, channels: 1, capacity: 50, latency: 40 });
		// Fill [0, 30) with value 1, then [30, 40) with value 2.
		insert(src, 0, 30, { channels: 1, value: 1.0 });
		insert(src, 30, 10, { channels: 1, value: 2.0 });

		const dst = src.resize(10);
		expect(dst.capacity).toBe(10);

		// Only the most recent 10 samples fit.
		const out = read(dst, 10, 1);
		for (let i = 0; i < 10; i++) {
			expect(out[0][i]).toBe(2.0);
		}
	});
});

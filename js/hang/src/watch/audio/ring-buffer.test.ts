import { describe, expect, it } from "vitest";
import { Time } from "@moq/lite";
import { AudioRingBuffer } from "./ring-buffer";

function read(buffer: AudioRingBuffer, samples: number, channelCount = 2): Float32Array[] {
	const output: Float32Array[] = [];
	for (let i = 0; i < channelCount; i++) {
		output.push(new Float32Array(samples));
	}
	const samplesRead = buffer.read(output);
	// Return the output arrays with only the read samples
	if (samplesRead < samples) {
		return output.map((channel) => channel.slice(0, samplesRead));
	}
	return output;
}

function write(
	buffer: AudioRingBuffer,
	timestamp: Time.Milli,
	samples: number,
	props?: { channels?: number; value?: number },
): void {
	const channelCount = props?.channels ?? buffer.channels;
	const data: Float32Array[] = [];
	for (let i = 0; i < channelCount; i++) {
		const channel = new Float32Array(samples);
		channel.fill(props?.value ?? 1.0);
		data.push(channel);
	}
	buffer.write(Time.Micro.fromMilli(timestamp), data);
}

describe("initialization", () => {
	it("should initialize with valid parameters", () => {
		const buffer = new AudioRingBuffer({ rate: 48000, channels: 2, latency: 100 as Time.Milli });

		expect(buffer.capacity).toBe(4800); // 48000 * 0.1
		expect(buffer.length).toBe(0);
	});

	it("should throw on invalid channel count", () => {
		expect(() => new AudioRingBuffer({ rate: 48000, channels: 0, latency: 100 as Time.Milli })).toThrow(
			/invalid channels/,
		);
	});

	it("should throw on invalid sample rate", () => {
		expect(() => new AudioRingBuffer({ rate: 0, channels: 2, latency: 100 as Time.Milli })).toThrow(
			/invalid sample rate/,
		);
	});

	it("should throw on invalid latency", () => {
		expect(() => new AudioRingBuffer({ rate: 48000, channels: 2, latency: 0 as Time.Milli })).toThrow(
			/invalid latency/,
		);
	});
});

describe("writing data", () => {
	it("should write continuous data", () => {
		const buffer = new AudioRingBuffer({ rate: 1000, channels: 2, latency: 100 as Time.Milli });

		// Write 10 samples at timestamp 0
		write(buffer, 0 as Time.Milli, 10, { channels: 2, value: 1.0 });
		expect(buffer.length).toBe(10);

		// Write 10 more samples at timestamp 10ms
		write(buffer, 10 as Time.Milli, 10, { channels: 2, value: 2.0 });
		expect(buffer.length).toBe(20);
	});

	it("should handle gaps by filling with zeros", () => {
		const buffer = new AudioRingBuffer({ rate: 1000, channels: 2, latency: 100 as Time.Milli }); // 100 samples buffer

		// Write at timestamp 0
		write(buffer, 0 as Time.Milli, 10, { channels: 2, value: 1.0 });

		// Write at timestamp 20ms (sample 20), creating a 10 sample gap
		write(buffer, 20 as Time.Milli, 10, { channels: 2, value: 2.0 });

		// Should have filled the gap with zeros
		expect(buffer.length).toBe(30); // 10 + 10 (gap) + 10

		// Exit refill mode by filling buffer
		write(buffer, 30 as Time.Milli, 70, { channels: 2, value: 0.0 });
		expect(buffer.refilling).toBe(false);

		// Read and verify the gap was filled with zeros
		const output = read(buffer, 30, 2);
		expect(output[0].length).toBe(30);

		// First 10 samples should be 1.0
		for (let i = 0; i < 10; i++) {
			expect(output[0][i]).toBe(1.0);
			expect(output[1][i]).toBe(1.0);
		}
		// Next 10 samples should be 0 (the gap)
		for (let i = 10; i < 20; i++) {
			expect(output[0][i]).toBe(0);
			expect(output[1][i]).toBe(0);
		}
		// Last 10 samples should be 2.0
		for (let i = 20; i < 30; i++) {
			expect(output[0][i]).toBe(2.0);
			expect(output[1][i]).toBe(2.0);
		}
	});

	it("should handle late-arriving samples (out-of-order writes)", () => {
		const buffer = new AudioRingBuffer({ rate: 1000, channels: 1, latency: 100 as Time.Milli });

		// Fill buffer to exit refill mode
		write(buffer, 0 as Time.Milli, 100, { channels: 1, value: 0.0 });
		expect(buffer.refilling).toBe(false);

		// Read 50 samples to advance read pointer to 50
		read(buffer, 50, 1);

		// Write at timestamp 120ms (sample 120) - this creates a gap from 100-120
		write(buffer, 120 as Time.Milli, 10, { channels: 1, value: 1.0 });

		// Now write data that fills part of the gap at timestamp 110ms (sample 110)
		// This should work because readIndex is at 50, so sample 110 is still ahead
		write(buffer, 110 as Time.Milli, 10, { channels: 1, value: 2.0 });

		// We should have samples from 50-99 (original), gap 100-109 (zeros), 110-119 (2.0), 120-129 (1.0)
		expect(buffer.length).toBe(80); // 130 - 50

		// Skip the old samples and gap
		read(buffer, 60, 1); // Read samples 50-109

		// Read and verify the out-of-order writes
		const output = read(buffer, 20, 1);
		expect(output[0].length).toBe(20);

		// First 10 samples should be 2.0 (the late-arriving data at 110-119)
		for (let i = 0; i < 10; i++) {
			expect(output[0][i]).toBe(2.0);
		}
		// Next 10 samples should be 1.0 (the earlier write at 120-129)
		for (let i = 10; i < 20; i++) {
			expect(output[0][i]).toBe(1.0);
		}
	});

	it("should discard samples that are too old", () => {
		const buffer = new AudioRingBuffer({ rate: 1000, channels: 2, latency: 100 as Time.Milli });

		// Exit refill mode by filling buffer
		write(buffer, 0 as Time.Milli, 100, { channels: 2, value: 0.0 });
		expect(buffer.refilling).toBe(false);

		// Read 60 samples, readIndex now at 60
		read(buffer, 60, 2);

		// Write 50 new samples at timestamp 100
		write(buffer, 100 as Time.Milli, 50, { channels: 2, value: 1.0 });
		expect(buffer.length).toBe(90); // 150 - 60

		// Read 10 more samples, readIndex now at 70
		read(buffer, 10, 2);
		expect(buffer.length).toBe(80); // 150 - 70

		// Try to write data that's before the read index (at sample 50, which is before 70)
		write(buffer, 50 as Time.Milli, 5, { channels: 2, value: 2.0 }); // These should be ignored

		// Available should still be 80 (unchanged because old samples were discarded)
		expect(buffer.length).toBe(80);
	});

	it("should not throw when writing to buffer", () => {
		const buffer = new AudioRingBuffer({ rate: 1000, channels: 2, latency: 100 as Time.Milli });
		// The current implementation doesn't require initialization
		expect(() => write(buffer, 0 as Time.Milli, 10, { channels: 2, value: 0.0 })).not.toThrow();
	});
});

describe("reading data", () => {
	it("should read available data", () => {
		const buffer = new AudioRingBuffer({ rate: 1000, channels: 2, latency: 100 as Time.Milli });

		// Exit refill mode by filling the buffer
		write(buffer, 0 as Time.Milli, 100, { channels: 2, value: 0.0 });
		// Buffer should now be out of refill mode
		expect(buffer.refilling).toBe(false);

		// Read some samples to make room (readIndex at 80)
		read(buffer, 80, 2);
		expect(buffer.length).toBe(20); // 100 - 80

		// Write 20 samples at the current position
		write(buffer, 100 as Time.Milli, 20, { channels: 2, value: 1.5 });
		expect(buffer.length).toBe(40); // 120 - 80

		// First read the remaining old samples (80-99)
		const output1 = read(buffer, 20, 2);
		expect(output1[0].length).toBe(20);
		for (let channel = 0; channel < 2; channel++) {
			for (let i = 0; i < 20; i++) {
				expect(output1[channel][i]).toBe(0.0);
			}
		}

		// Now read the new samples (100-109)
		const output2 = read(buffer, 10, 2);
		expect(output2[0].length).toBe(10);
		expect(buffer.length).toBe(10); // 120 - 110

		// Verify the new data
		for (let channel = 0; channel < 2; channel++) {
			for (let i = 0; i < 10; i++) {
				expect(output2[channel][i]).toBe(1.5);
			}
		}
	});

	it("should handle partial reads", () => {
		const buffer = new AudioRingBuffer({ rate: 1000, channels: 2, latency: 100 as Time.Milli });

		// Exit refill mode by filling the buffer
		write(buffer, 0 as Time.Milli, 100, { channels: 2, value: 0.0 });
		expect(buffer.refilling).toBe(false);

		// Read some to make room (readIndex at 80)
		read(buffer, 80, 2);

		// Write 20 samples
		write(buffer, 100 as Time.Milli, 20, { channels: 2, value: 1.0 });
		expect(buffer.length).toBe(40); // 120 - 80

		// Try to read 50 samples (only 40 available)
		const output = read(buffer, 50, 2);

		expect(output[0].length).toBe(40);
		expect(buffer.length).toBe(0);
	});

	it("should return 0 when no data available", () => {
		const buffer = new AudioRingBuffer({ rate: 1000, channels: 2, latency: 100 as Time.Milli });

		const output = read(buffer, 10, 2);
		expect(output[0].length).toBe(0);
	});

	it("should return 0 when not initialized", () => {
		const buffer = new AudioRingBuffer({ rate: 1000, channels: 2, latency: 100 as Time.Milli });
		const output = read(buffer, 10, 2);
		expect(output[0].length).toBe(0);
	});
});

describe("refill behavior", () => {
	it("should start in refill mode", () => {
		const buffer = new AudioRingBuffer({ rate: 1000, channels: 2, latency: 100 as Time.Milli });
		expect(buffer.refilling).toBe(true);

		// Should not output anything in refill mode
		write(buffer, 0 as Time.Milli, 50, { channels: 2, value: 1.0 });
		const output = read(buffer, 10, 2);
		expect(output[0].length).toBe(0);
	});

	it("should exit refill mode when buffer is full", () => {
		const buffer = new AudioRingBuffer({ rate: 1000, channels: 2, latency: 100 as Time.Milli });

		// Fill the buffer completely
		write(buffer, 0 as Time.Milli, 100, { channels: 2, value: 1.0 });

		// Write more data to trigger overflow handling
		write(buffer, 10 as Time.Milli, 50, { channels: 2, value: 2.0 }); // This should exit refill mode

		expect(buffer.refilling).toBe(false);

		// Now we should be able to read
		const output = read(buffer, 10, 2);
		expect(output[0].length).toBe(10);
	});
});

describe("ring buffer wrapping", () => {
	it("should wrap around when buffer is full", () => {
		const buffer = new AudioRingBuffer({ rate: 1000, channels: 1, latency: 100 as Time.Milli });

		// Fill the buffer
		write(buffer, 0 as Time.Milli, 100, { channels: 1, value: 1.0 });
		expect(buffer.refilling).toBe(false);

		// Read 50 samples to make room (readIndex at 50)
		const output1 = read(buffer, 50, 1);
		expect(output1[0].length).toBe(50);

		// Write 50 more samples at timestamp 100 (fills from sample 100-149)
		write(buffer, 100 as Time.Milli, 50, { channels: 1, value: 2.0 });

		// Now we have 100 samples available (50-149)
		expect(buffer.length).toBe(100);

		// Write 50 more samples at timestamp 150, this will wrap around
		write(buffer, 150 as Time.Milli, 50, { channels: 1, value: 3.0 });

		// Should still have 100 samples (buffer is at capacity)
		expect(buffer.length).toBe(100);

		// Read all 100 samples
		const output2 = read(buffer, 100, 1);
		expect(output2[0].length).toBe(100);

		// First 50 should be 2.0, next 50 should be 3.0
		for (let i = 0; i < 50; i++) {
			expect(output2[0][i]).toBe(2.0);
		}
		for (let i = 50; i < 100; i++) {
			expect(output2[0][i]).toBe(3.0);
		}
	});
});

describe("multi-channel handling", () => {
	it("should handle stereo data correctly", () => {
		const buffer = new AudioRingBuffer({ rate: 1000, channels: 2, latency: 100 as Time.Milli });

		// Exit refill mode by filling buffer
		write(buffer, 0 as Time.Milli, 100, { channels: 2, value: 0.5 });
		expect(buffer.refilling).toBe(false);

		// Read some to make room
		read(buffer, 80, 2);

		// Write stereo data with same value for both channels
		write(buffer, 100 as Time.Milli, 20, { channels: 2, value: 1.5 });

		// Read and verify
		const output = read(buffer, 20, 2);
		expect(output[0].length).toBe(20);
		expect(output[1].length).toBe(20);

		// Both channels should have the same data
		for (let i = 0; i < 20; i++) {
			expect(output[0][i]).toBe(0.5);
			expect(output[1][i]).toBe(0.5);
		}

		// Read the next batch
		const output2 = read(buffer, 20, 2);
		for (let i = 0; i < 20; i++) {
			expect(output2[0][i]).toBe(1.5);
			expect(output2[1][i]).toBe(1.5);
		}
	});
});

describe("edge cases", () => {
	it("should throw when output array has wrong channel count", () => {
		const buffer = new AudioRingBuffer({ rate: 1000, channels: 2, latency: 100 as Time.Milli });
		write(buffer, 0 as Time.Milli, 50, { channels: 2, value: 1.0 });

		const output: Float32Array[] = [];
		// Current implementation throws when channel count doesn't match
		expect(() => buffer.read(output)).toThrow(/wrong number of channels/);
	});

	it("should handle zero-length output buffers", () => {
		const buffer = new AudioRingBuffer({ rate: 1000, channels: 2, latency: 100 as Time.Milli });
		write(buffer, 0 as Time.Milli, 50, { channels: 2, value: 1.0 });

		const output = [new Float32Array(0), new Float32Array(0)];
		const samplesRead = buffer.read(output);
		expect(samplesRead).toBe(0);
	});

	it("should handle fractional timestamps", () => {
		const buffer = new AudioRingBuffer({ rate: 1000, channels: 2, latency: 100 as Time.Milli });

		// Exit refill mode first
		write(buffer, 0 as Time.Milli, 100, { channels: 2, value: 0.0 });
		write(buffer, 10 as Time.Milli, 10, { channels: 2, value: 0.0 }); // This exits refill mode
		read(buffer, 110, 2);

		// Write with fractional timestamp that rounds
		write(buffer, 1105 as Time.Milli, 10, { channels: 2, value: 1.0 }); // 110.5 samples, rounds to 111
		write(buffer, 1204 as Time.Milli, 10, { channels: 2, value: 2.0 }); // 120.4 samples, rounds to 120

		const output = read(buffer, 20, 2);
		expect(output[0].length).toBeGreaterThan(0);
	});
});

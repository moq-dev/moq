import { describe, expect, it } from "bun:test";
import { Micro, Milli } from "@moq/clock";
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
	timestamp: Milli,
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
	buffer.write(Micro.fromMilli(timestamp), data);
}

describe("initialization", () => {
	it("should initialize with valid parameters", () => {
		const buffer = new AudioRingBuffer({ rate: 48000, channels: 2, latency: 100 as Milli });

		// The receiver fixes the capacity at twice the deepest floor, never from the target: whatever
		// of the target is above the floor stays encoded upstream.
		expect(buffer.capacity).toBe(48000); // 48000 * 0.5 * 2
		expect(buffer.length).toBe(0);
	});

	it("should throw on invalid channel count", () => {
		expect(() => new AudioRingBuffer({ rate: 48000, channels: 0, latency: 100 as Milli })).toThrow(
			/invalid channels/,
		);
	});

	it("should throw on invalid sample rate", () => {
		expect(() => new AudioRingBuffer({ rate: 0, channels: 2, latency: 100 as Milli })).toThrow(
			/invalid sample rate/,
		);
	});

	it("should throw on invalid latency", () => {
		expect(() => new AudioRingBuffer({ rate: 48000, channels: 2, latency: 0 as Milli })).toThrow(/invalid latency/);
	});
});

describe("writing data", () => {
	it("should write continuous data", () => {
		const buffer = new AudioRingBuffer({ rate: 1000, channels: 2, latency: 100 as Milli });

		// Write 10 samples at timestamp 0
		write(buffer, 0 as Milli, 10, { channels: 2, value: 1.0 });
		expect(buffer.length).toBe(10);

		// Write 10 more samples at timestamp 10ms
		write(buffer, 10 as Milli, 10, { channels: 2, value: 2.0 });
		expect(buffer.length).toBe(20);
	});

	it("should handle gaps by filling with zeros", () => {
		const buffer = new AudioRingBuffer({ rate: 1000, channels: 2, latency: 100 as Milli }); // 100 samples buffer

		// Write at timestamp 0
		write(buffer, 0 as Milli, 10, { channels: 2, value: 1.0 });

		// Write at timestamp 20ms (sample 20), creating a 10 sample gap
		write(buffer, 20 as Milli, 10, { channels: 2, value: 2.0 });

		// Should have filled the gap with zeros
		expect(buffer.length).toBe(30); // 10 + 10 (gap) + 10

		// Exit stalled mode by filling buffer
		write(buffer, 30 as Milli, 70, { channels: 2, value: 0.0 });
		expect(buffer.stalled).toBe(false);

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
		const buffer = new AudioRingBuffer({ rate: 1000, channels: 1, latency: 100 as Milli });

		// Fill buffer to exit stalled mode
		write(buffer, 0 as Milli, 100, { channels: 1, value: 0.0 });
		expect(buffer.stalled).toBe(false);

		// Read 50 samples to advance read pointer to 50
		read(buffer, 50, 1);

		// Write at timestamp 120ms (sample 120) - this creates a gap from 100-120
		write(buffer, 120 as Milli, 10, { channels: 1, value: 1.0 });

		// Now write data that fills part of the gap at timestamp 110ms (sample 110)
		// This should work because readIndex is at 50, so sample 110 is still ahead
		write(buffer, 110 as Milli, 10, { channels: 1, value: 2.0 });

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
		const buffer = new AudioRingBuffer({ rate: 1000, channels: 2, latency: 100 as Milli });

		// Exit stalled mode by filling buffer
		write(buffer, 0 as Milli, 100, { channels: 2, value: 0.0 });
		expect(buffer.stalled).toBe(false);

		// Read 60 samples, readIndex now at 60
		read(buffer, 60, 2);

		// Write 50 new samples at timestamp 100
		write(buffer, 100 as Milli, 50, { channels: 2, value: 1.0 });
		expect(buffer.length).toBe(90); // 150 - 60

		// Read 10 more samples, readIndex now at 70
		read(buffer, 10, 2);
		expect(buffer.length).toBe(80); // 150 - 70

		// Try to write data that's before the read index (at sample 50, which is before 70)
		write(buffer, 50 as Milli, 5, { channels: 2, value: 2.0 }); // These should be ignored

		// Available should still be 80 (unchanged because old samples were discarded)
		expect(buffer.length).toBe(80);
	});
});

describe("reading data", () => {
	it("should read available data", () => {
		const buffer = new AudioRingBuffer({ rate: 1000, channels: 2, latency: 100 as Milli });

		// Exit stalled mode by filling the buffer
		write(buffer, 0 as Milli, 100, { channels: 2, value: 0.0 });
		// Buffer should now be out of stalled mode
		expect(buffer.stalled).toBe(false);

		// Read some samples to make room (readIndex at 80)
		read(buffer, 80, 2);
		expect(buffer.length).toBe(20); // 100 - 80

		// Write 20 samples at the current position
		write(buffer, 100 as Milli, 20, { channels: 2, value: 1.5 });
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
		const buffer = new AudioRingBuffer({ rate: 1000, channels: 2, latency: 100 as Milli });

		// Exit stalled mode by filling the buffer
		write(buffer, 0 as Milli, 100, { channels: 2, value: 0.0 });
		expect(buffer.stalled).toBe(false);

		// Read some to make room (readIndex at 80)
		read(buffer, 80, 2);

		// Write 20 samples
		write(buffer, 100 as Milli, 20, { channels: 2, value: 1.0 });
		expect(buffer.length).toBe(40); // 120 - 80

		// Try to read 50 samples (only 40 available)
		const output = read(buffer, 50, 2);

		expect(output[0].length).toBe(40);
		expect(buffer.length).toBe(0);
	});

	it("should return 0 when no data available", () => {
		const buffer = new AudioRingBuffer({ rate: 1000, channels: 2, latency: 100 as Milli });

		const output = read(buffer, 10, 2);
		expect(output[0].length).toBe(0);
	});
});

describe("stall behavior", () => {
	it("should start in stalled mode", () => {
		const buffer = new AudioRingBuffer({ rate: 1000, channels: 2, latency: 100 as Milli });
		expect(buffer.stalled).toBe(true);

		// Should not output anything in stalled mode
		write(buffer, 0 as Milli, 50, { channels: 2, value: 1.0 });
		const output = read(buffer, 10, 2);
		expect(output[0].length).toBe(0);
	});

	it("should exit stalled mode when buffer is full", () => {
		const buffer = new AudioRingBuffer({ rate: 1000, channels: 2, latency: 100 as Milli });

		// Fill the buffer completely
		write(buffer, 0 as Milli, 100, { channels: 2, value: 1.0 });

		// Write more data to trigger overflow handling
		write(buffer, 10 as Milli, 50, { channels: 2, value: 2.0 }); // This should exit stalled mode

		expect(buffer.stalled).toBe(false);

		// Now we should be able to read
		const output = read(buffer, 10, 2);
		expect(output[0].length).toBe(10);
	});
});

describe("ring buffer wrapping", () => {
	it("should wrap around when buffer is full", () => {
		// 100 sample target, so 200 samples of capacity.
		const buffer = new AudioRingBuffer({ rate: 1000, channels: 1, latency: 100 as Milli });

		// Fill to the target
		write(buffer, 0 as Milli, 100, { channels: 1, value: 1.0 });
		expect(buffer.stalled).toBe(false);

		// Read 50 samples to make room (readIndex at 50)
		const output1 = read(buffer, 50, 1);
		expect(output1[0].length).toBe(50);

		// Write 50 more samples at timestamp 100 (fills from sample 100-149)
		write(buffer, 100 as Milli, 50, { channels: 1, value: 2.0 });

		// Now we have 100 samples available (50-149)
		expect(buffer.length).toBe(100);

		// One chunk above the target is tolerated rather than dropped (50-199).
		write(buffer, 150 as Milli, 50, { channels: 1, value: 3.0 });
		expect(buffer.length).toBe(150);

		// Past the band: drop back to the target, wrapping abs 200-249 onto slots 0-49.
		write(buffer, 200 as Milli, 50, { channels: 1, value: 4.0 });
		expect(buffer.length).toBe(100);

		// Read all 100 samples: abs 150-199 = 3.0, abs 200-249 = 4.0.
		const output2 = read(buffer, 100, 1);
		expect(output2[0].length).toBe(100);

		for (let i = 0; i < 50; i++) {
			expect(output2[0][i]).toBe(3.0);
		}
		for (let i = 50; i < 100; i++) {
			expect(output2[0][i]).toBe(4.0);
		}
	});
});

describe("multi-channel handling", () => {
	it("should handle stereo data correctly", () => {
		const buffer = new AudioRingBuffer({ rate: 1000, channels: 2, latency: 100 as Milli });

		// Exit stalled mode by filling buffer
		write(buffer, 0 as Milli, 100, { channels: 2, value: 0.5 });
		expect(buffer.stalled).toBe(false);

		// Read some to make room
		read(buffer, 80, 2);

		// Write stereo data with same value for both channels
		write(buffer, 100 as Milli, 20, { channels: 2, value: 1.5 });

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
		const buffer = new AudioRingBuffer({ rate: 1000, channels: 2, latency: 100 as Milli });
		write(buffer, 0 as Milli, 50, { channels: 2, value: 1.0 });

		const output: Float32Array[] = [];
		// Current implementation throws when channel count doesn't match
		expect(() => buffer.read(output)).toThrow(/wrong number of channels/);
	});

	it("should handle zero-length output buffers", () => {
		const buffer = new AudioRingBuffer({ rate: 1000, channels: 2, latency: 100 as Milli });
		write(buffer, 0 as Milli, 50, { channels: 2, value: 1.0 });

		const output = [new Float32Array(0), new Float32Array(0)];
		const samplesRead = buffer.read(output);
		expect(samplesRead).toBe(0);
	});

	it("should handle fractional timestamps", () => {
		const buffer = new AudioRingBuffer({ rate: 1000, channels: 2, latency: 100 as Milli });

		// Exit stalled mode first
		write(buffer, 0 as Milli, 100, { channels: 2, value: 0.0 });
		write(buffer, 10 as Milli, 10, { channels: 2, value: 0.0 }); // This exits stalled mode
		read(buffer, 110, 2);

		// Write with fractional timestamp that rounds
		write(buffer, 1105 as Milli, 10, { channels: 2, value: 1.0 }); // 110.5 samples, rounds to 111
		write(buffer, 1204 as Milli, 10, { channels: 2, value: 2.0 }); // 120.4 samples, rounds to 120

		const output = read(buffer, 20, 2);
		expect(output[0].length).toBeGreaterThan(0);
	});
});

describe("setLatency", () => {
	it("keeps the receiver's capacity whatever the floor", () => {
		const buffer = new AudioRingBuffer({ rate: 1000, channels: 1, latency: 100 as Milli });
		const capacity = buffer.capacity;
		write(buffer, 0 as Milli, 50, { channels: 1, value: 1.0 });

		buffer.setLatency(400 as Milli);
		expect(buffer.capacity).toBe(capacity);
		expect(buffer.length).toBe(50);
		expect(buffer.stalled).toBe(true);
	});

	// Buffered playback promises to play through everything queued, so a lower floor must not drop
	// the oldest unread audio the way a live ring does.
	it("keeps every unread sample when a buffered ring's floor drops", () => {
		const buffer = new AudioRingBuffer({ rate: 1000, channels: 1, latency: 100 as Milli, buffered: true });
		write(buffer, 0 as Milli, 100, { channels: 1, value: 1.0 });
		write(buffer, 100 as Milli, 80, { channels: 1, value: 2.0 });

		buffer.setLatency(30 as Milli);
		write(buffer, 180 as Milli, 20, { channels: 1, value: 3.0 });
		expect(buffer.length).toBe(200);
		expect(Milli.fromMicro(buffer.timestamp)).toBe(0 as Milli);
		const [output] = read(buffer, 200, 1);
		expect([...output]).toEqual([...Array(100).fill(1), ...Array(80).fill(2), ...Array(20).fill(3)]);
	});

	it("refuses an empty floor or one past the capacity", () => {
		const buffer = new AudioRingBuffer({ rate: 1000, channels: 1, latency: 100 as Milli });
		expect(() => buffer.setLatency(0 as Milli)).toThrow(/empty buffer/);
		expect(() => buffer.setLatency((buffer.capacity + 1) as Milli)).toThrow(/exceeds capacity/);
		expect(() => new AudioRingBuffer({ rate: 1000, channels: 1, latency: 60_000 as Milli })).toThrow(
			/exceeds capacity/,
		);
	});
});

describe("park", () => {
	it("holds a covered ring until the next floor update, keeping what it buffered", () => {
		const buffer = new AudioRingBuffer({ rate: 1000, channels: 1, latency: 50 as Milli });
		write(buffer, 0 as Milli, 100, { channels: 1, value: 1.0 });
		expect(read(buffer, 10, 1)[0].length).toBe(10);

		buffer.park();
		expect(buffer.stalled).toBe(true);
		expect(read(buffer, 10, 1)[0].length).toBe(0);

		// Output decoded before the park can still land, so a write does not resume it.
		write(buffer, 100 as Milli, 10, { channels: 1, value: 2.0 });
		expect(buffer.stalled).toBe(true);

		// The main thread knows when the media it holds encoded covers the target.
		buffer.setLatency(50 as Milli);
		expect(buffer.stalled).toBe(false);
		expect(read(buffer, 10, 1)[0]).toEqual(new Float32Array(10).fill(1.0));
	});
});

describe("decoded rate must match the ring (#2352)", () => {
	// The ring maps a frame's timestamp to a sample index using its own rate, so the ring must run at
	// the rate the decoder actually output. A 20ms Opus frame decodes to 960 samples (48kHz); if the
	// ring runs at that rate they land contiguously, but if it runs at a mismatched rate (e.g. 44100,
	// the machine default a file publish leaked into the catalog) each frame overruns its slot and
	// every frame after the first is misplaced, which is the noise reported in #2352.
	function writeOpusFrames(rate: number): AudioRingBuffer {
		// 1s floor so nothing overflows while we fill 10 frames.
		const buffer = new AudioRingBuffer({ rate, channels: 1, latency: 1000 as Milli });
		for (let i = 0; i < 10; i++) {
			write(buffer, (i * 20) as Milli, 960, { channels: 1, value: (i + 1) / 10 });
		}
		return buffer;
	}

	it("places 20ms/960-sample frames contiguously at 48kHz", () => {
		// 20ms at 48kHz is exactly 960 samples, so 10 frames pack into 9600 with no gaps or overlap.
		expect(writeOpusFrames(48000).length).toBe(9600);
	});
});

describe("live anchoring", () => {
	// Without anchoring, a first frame at a large timestamp gap-filled the whole ring with zeros,
	// un-stalled on that overflow, and left the playhead a floor before the frame.
	it("anchors a live ring to the first frame instead of gap-filling from zero", () => {
		const buffer = new AudioRingBuffer({ rate: 1000, channels: 1, latency: 100 as Milli });

		write(buffer, 2000 as Milli, 40, { channels: 1, value: 0.5 });
		// Still short of the floor, so nothing plays yet and the ring holds only real samples.
		expect(buffer.stalled).toBe(true);
		expect(buffer.length).toBe(40);
		expect(Milli.fromMicro(buffer.timestamp)).toBe(2000 as Milli);

		write(buffer, 2040 as Milli, 60, { channels: 1, value: 0.5 });
		expect(buffer.stalled).toBe(false);

		// Playback starts on the first real sample, not on gap-filled silence.
		const output = read(buffer, 40, 1);
		expect(output[0].length).toBe(40);
		expect(output[0][0]).toBeCloseTo(0.5, 5);
	});
});

describe("re-buffer", () => {
	it("re-stalls and refills to the target after running dry", () => {
		const buffer = new AudioRingBuffer({ rate: 1000, channels: 1, latency: 40 as Milli });

		write(buffer, 0 as Milli, 20, { channels: 1, value: 1.0 });
		write(buffer, 20 as Milli, 20, { channels: 1, value: 1.0 });
		expect(buffer.stalled).toBe(false);

		// Drain it.
		expect(read(buffer, 40, 1)[0].length).toBe(40);
		expect(buffer.length).toBe(0);

		// The next read finds nothing and parks playback rather than handing the worklet a quantum
		// of silence and trying again on the next one.
		expect(read(buffer, 20, 1)[0].length).toBe(0);
		expect(buffer.stalled).toBe(true);
		expect(buffer.underruns).toBe(1);

		// A single chunk is not the target, so playback stays parked.
		write(buffer, 40 as Milli, 20, { channels: 1, value: 2.0 });
		expect(buffer.stalled).toBe(true);
		expect(read(buffer, 20, 1)[0].length).toBe(0);

		// Reaching the target resumes it, and nothing buffered in the meantime was lost.
		write(buffer, 60 as Milli, 20, { channels: 1, value: 2.0 });
		expect(buffer.stalled).toBe(false);
		expect(read(buffer, 40, 1)[0].length).toBe(40);
	});

	it("counts a quantum it could only partly fill as an underrun", () => {
		const buffer = new AudioRingBuffer({ rate: 1000, channels: 1, latency: 40 as Milli });

		write(buffer, 0 as Milli, 20, { channels: 1, value: 1.0 });
		write(buffer, 20 as Milli, 20, { channels: 1, value: 1.0 });
		expect(read(buffer, 30, 1)[0].length).toBe(30);

		// Ten samples remain against a quantum of twenty: the rest of the quantum is silence, which
		// is an underrun whether or not a chunk lands before the next read. It is not a stall: a
		// refill would cost the whole target to cover a gap shorter than one quantum.
		expect(read(buffer, 20, 1)[0].length).toBe(10);
		expect(buffer.stalled).toBe(false);
		expect(buffer.underruns).toBe(1);

		// A chunk landing before the next read keeps playback going.
		write(buffer, 40 as Milli, 20, { channels: 1, value: 2.0 });
		expect(read(buffer, 20, 1)[0].length).toBe(20);
		expect(buffer.underruns).toBe(1);
	});

	it("does not count an underrun while parked", () => {
		const buffer = new AudioRingBuffer({ rate: 1000, channels: 1, latency: 40 as Milli });
		read(buffer, 20, 1);
		read(buffer, 20, 1);
		expect(buffer.underruns).toBe(0);
	});
});

describe("buffered mode", () => {
	function createBuffered(latency: number) {
		return new AudioRingBuffer({ rate: 1000, channels: 1, latency: latency as Milli, buffered: true });
	}

	it("anchors to the first frame and un-stalls at the latency target", () => {
		const buffer = createBuffered(50);
		expect(buffer.stalled).toBe(true);
		write(buffer, 2000 as Milli, 100, { channels: 1, value: 0.1 });
		expect(buffer.stalled).toBe(false);
		expect(Milli.fromMicro(buffer.timestamp)).toBe(2000 as Milli);
	});

	it("plays frames in order within the floor-sized ring without skipping ahead", () => {
		// 600ms floor at 1000Hz -> 1200-sample ring, comfortably holding the 1000-sample utterance
		// the backpressure loop would feed it. A non-buffered ring would have skipped to write-latency.
		const buffer = createBuffered(600);
		for (let i = 0; i < 10; i++) {
			write(buffer, (2000 + i * 100) as Milli, 100, { channels: 1, value: (i + 1) / 10 });
		}
		expect(buffer.length).toBe(1000);

		const first = read(buffer, 100, 1);
		expect(first[0][0]).toBeCloseTo(0.1, 5);
		const second = read(buffer, 100, 1);
		expect(second[0][0]).toBeCloseTo(0.2, 5);
	});

	it("reset re-stalls and re-anchors to the next utterance", () => {
		const buffer = createBuffered(50);
		write(buffer, 2000 as Milli, 100, { channels: 1, value: 0.1 });
		read(buffer, 50, 1);

		buffer.reset();
		expect(buffer.stalled).toBe(true);

		write(buffer, 500 as Milli, 100, { channels: 1, value: 0.9 });
		expect(Milli.fromMicro(buffer.timestamp)).toBe(500 as Milli);
		const out = read(buffer, 100, 1);
		expect(out[0][0]).toBeCloseTo(0.9, 5);
	});

	it("drops the oldest samples once the ring fills beyond its capacity", () => {
		// 50ms floor at 1000Hz in a 1000-sample ring. Backpressure normally keeps the decode loop from
		// running this far ahead; if it slips, the ring drops its oldest as a backstop.
		const buffer = createBuffered(50);

		write(buffer, 0 as Milli, 1000, { channels: 1, value: 0.1 }); // [0, 1000)
		write(buffer, 1000 as Milli, 100, { channels: 1, value: 0.2 }); // past the ring; drops [0, 100)

		expect(Milli.fromMicro(buffer.timestamp)).toBe(100 as Milli);
		expect(buffer.length).toBe(1000);
	});

	it("truncate drops the write-ahead tail a successor supersedes", () => {
		// 600ms floor at 1000Hz -> 1200-sample ring, holding a 1s write-ahead utterance.
		const buffer = createBuffered(600);
		for (let i = 0; i < 10; i++) {
			write(buffer, (2000 + i * 100) as Milli, 100, { channels: 1, value: 0.1 });
		}
		read(buffer, 100, 1); // playhead at 2100

		// A successor track takes over at 2200 with 100ms of its own audio. Writing it only overwrites
		// [2200, 2300); without the truncate, [2300, 3000) of the old track still plays after it.
		buffer.truncate(Micro.fromMilli(2200 as Milli));
		write(buffer, 2200 as Milli, 100, { channels: 1, value: 0.9 });

		expect(buffer.stalled).toBe(false); // truncate keeps playing, unlike reset()
		expect(read(buffer, 100, 1)[0][0]).toBeCloseTo(0.1, 5); // [2100, 2200) was already due
		expect(read(buffer, 100, 1)[0][0]).toBeCloseTo(0.9, 5); // the successor
		expect(read(buffer, 100, 1)[0].length).toBe(0); // and nothing after it
	});

	it("truncate never rewinds past the playhead", () => {
		const buffer = createBuffered(600);
		write(buffer, 2000 as Milli, 1000, { channels: 1, value: 0.1 });
		read(buffer, 100, 1); // playhead at 2100

		// A successor whose first frame predates the playhead: those samples are already due, so the
		// write index floors there rather than going backwards.
		buffer.truncate(Micro.fromMilli(1000 as Milli));
		expect(buffer.length).toBe(0);
		expect(read(buffer, 100, 1)[0].length).toBe(0);
	});
});

describe("latency increase", () => {
	// Video holds a deeper floor on its own, so audio has to park for the difference or it runs
	// ahead. Flushing instead cost the whole floor in silence for every rise, however small.
	it("parks a playing ring until the deeper floor refills, keeping what it buffered", () => {
		const buffer = new AudioRingBuffer({ rate: 1000, channels: 1, latency: 100 as Milli });

		// Fill to the floor so playback starts, then play half of it.
		write(buffer, 0 as Milli, 100, { channels: 1, value: 1.0 });
		expect(buffer.stalled).toBe(false);
		read(buffer, 50, 1);

		buffer.setLatency(110 as Milli);
		expect(buffer.stalled).toBe(true);
		expect(buffer.length).toBe(50);

		// Refilling to the 110-sample floor takes 60 more samples, not the whole floor.
		write(buffer, 100 as Milli, 59, { channels: 1, value: 2.0 });
		expect(buffer.stalled).toBe(true);
		write(buffer, 159 as Milli, 1, { channels: 1, value: 2.0 });
		expect(buffer.stalled).toBe(false);

		// Playback resumes where it parked rather than at the refill.
		expect(Milli.fromMicro(buffer.timestamp)).toBe(50 as Milli);
		expect(read(buffer, 1, 1)[0][0]).toBe(1.0);
	});

	it("keeps playing through a shallower floor", () => {
		const buffer = new AudioRingBuffer({ rate: 1000, channels: 1, latency: 100 as Milli });
		write(buffer, 0 as Milli, 100, { channels: 1, value: 1.0 });
		read(buffer, 50, 1);

		buffer.setLatency(90 as Milli);
		expect(buffer.stalled).toBe(false);
	});

	it("resumes a refill once a shallower floor is already covered", () => {
		const buffer = new AudioRingBuffer({ rate: 1000, channels: 1, latency: 100 as Milli });
		write(buffer, 0 as Milli, 100, { channels: 1, value: 1.0 });
		read(buffer, 50, 1);

		buffer.setLatency(110 as Milli);
		expect(buffer.stalled).toBe(true);

		// No further frame arrives, as when the source stops, so only the lower floor can resume it.
		buffer.setLatency(50 as Milli);
		expect(buffer.stalled).toBe(false);
		expect(read(buffer, 1, 1)[0][0]).toBe(1.0);
	});
});

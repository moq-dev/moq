import { describe, expect, it } from "bun:test";
import { AudioRingBuffer } from "./ring-buffer";

// Rings that play exactly what they hold: on a shared clock pinned to the playhead with no depth to
// keep, the stretch has nothing to steer toward, so every window holds.
const exacts = new WeakSet<AudioRingBuffer>();
function exact(props: { rate: number; channels: number; latency: number }): AudioRingBuffer {
	const buffer = new AudioRingBuffer({ ...props, target: 0 });
	exacts.add(buffer);
	return buffer;
}

function read(buffer: AudioRingBuffer, samples: number, channelCount = 2): Float32Array[] {
	const output: Float32Array[] = [];
	for (let i = 0; i < channelCount; i++) {
		output.push(new Float32Array(samples));
	}
	if (exacts.has(buffer)) buffer.clock(buffer.timestamp);
	const samplesRead = buffer.read(output);
	// Return the output arrays with only the read samples
	if (samplesRead < samples) {
		return output.map((channel) => channel.slice(0, samplesRead));
	}
	return output;
}

function write(
	buffer: AudioRingBuffer,
	timestamp: number,
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
	buffer.write(timestamp * 1000, data);
}

describe("initialization", () => {
	it("should initialize with valid parameters", () => {
		const buffer = new AudioRingBuffer({ rate: 48000, channels: 2, latency: 100 });

		// The receiver fixes the capacity at twice the deepest floor, never from the target: whatever
		// of the target is above the floor stays encoded upstream.
		expect(buffer.capacity).toBe(48000); // 48000 * 0.5 * 2
		expect(buffer.length).toBe(0);
	});

	it("should throw on invalid channel count", () => {
		expect(() => new AudioRingBuffer({ rate: 48000, channels: 0, latency: 100 })).toThrow(/invalid channels/);
	});

	it("should throw on invalid sample rate", () => {
		expect(() => new AudioRingBuffer({ rate: 0, channels: 2, latency: 100 })).toThrow(/invalid sample rate/);
	});

	it("should throw on invalid latency", () => {
		expect(() => new AudioRingBuffer({ rate: 48000, channels: 2, latency: 0 })).toThrow(/invalid latency/);
	});
});

describe("writing data", () => {
	it("should write continuous data", () => {
		const buffer = new AudioRingBuffer({ rate: 1000, channels: 2, latency: 100 });

		// Write 10 samples at timestamp 0
		write(buffer, 0, 10, { channels: 2, value: 1.0 });
		expect(buffer.length).toBe(10);

		// Write 10 more samples at timestamp 10ms
		write(buffer, 10, 10, { channels: 2, value: 2.0 });
		expect(buffer.length).toBe(20);
	});

	it("should handle gaps by filling with zeros", () => {
		const buffer = new AudioRingBuffer({ rate: 1000, channels: 2, latency: 100 }); // 100 samples buffer

		// Write at timestamp 0
		write(buffer, 0, 10, { channels: 2, value: 1.0 });

		// Write at timestamp 20ms (sample 20), creating a 10 sample gap
		write(buffer, 20, 10, { channels: 2, value: 2.0 });

		// Should have filled the gap with zeros
		expect(buffer.length).toBe(30); // 10 + 10 (gap) + 10

		// Exit stalled mode by filling buffer
		write(buffer, 30, 70, { channels: 2, value: 0.0 });
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

	it("plays frames stamped within a millisecond of contiguous as contiguous", () => {
		const buffer = exact({ rate: 48000, channels: 1, latency: 10 });
		// 480 samples is 10 ms. The second frame is stamped 1 ms late, the third 2 samples early (of
		// the second's actual end), and the fourth after a real 2 ms hole.
		write(buffer, 0, 480, { value: 1 });
		write(buffer, 11, 480, { value: 2 });
		write(buffer, (960 - 2) / 48, 480, { value: 3 });
		write(buffer, (1440 + 96) / 48, 480, { value: 4 });
		expect(buffer.length).toBe(4 * 480 + 96);

		const output = read(buffer, 4 * 480 + 96, 1)[0];
		expect(output.subarray(0, 1440).every((v, i) => v === 1 + Math.floor(i / 480))).toBe(true);
		expect(output.subarray(1440, 1536).every((v) => v === 0)).toBe(true);
		expect(output.subarray(1536).every((v) => v === 4)).toBe(true);
	});

	// These drain the ring below its floor without refilling it, which the stretch slows down for, so
	// they run exact.
	it("should handle late-arriving samples (out-of-order writes)", () => {
		const buffer = exact({ rate: 1000, channels: 1, latency: 100 });

		// Fill buffer to exit stalled mode
		write(buffer, 0, 100, { channels: 1, value: 0.0 });
		expect(buffer.stalled).toBe(false);

		// Read 50 samples to advance read pointer to 50
		read(buffer, 50, 1);

		// Write at timestamp 120ms (sample 120) - this creates a gap from 100-120
		write(buffer, 120, 10, { channels: 1, value: 1.0 });

		// Now write data that fills part of the gap at timestamp 110ms (sample 110)
		// This should work because readIndex is at 50, so sample 110 is still ahead
		write(buffer, 110, 10, { channels: 1, value: 2.0 });

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
		const buffer = exact({ rate: 1000, channels: 2, latency: 100 });

		// Exit stalled mode by filling buffer
		write(buffer, 0, 100, { channels: 2, value: 0.0 });
		expect(buffer.stalled).toBe(false);

		// Read 60 samples, readIndex now at 60
		read(buffer, 60, 2);

		// Write 50 new samples at timestamp 100
		write(buffer, 100, 50, { channels: 2, value: 1.0 });
		expect(buffer.length).toBe(90); // 150 - 60

		// Read 10 more samples, readIndex now at 70
		read(buffer, 10, 2);
		expect(buffer.length).toBe(80); // 150 - 70

		// Try to write data that's before the read index (at sample 50, which is before 70)
		write(buffer, 50, 5, { channels: 2, value: 2.0 }); // These should be ignored

		// Available should still be 80 (unchanged because old samples were discarded)
		expect(buffer.length).toBe(80);
	});
});

// Exact, for the same reason as the writes above.
describe("reading data", () => {
	it("should read available data", () => {
		const buffer = exact({ rate: 1000, channels: 2, latency: 100 });

		// Exit stalled mode by filling the buffer
		write(buffer, 0, 100, { channels: 2, value: 0.0 });
		// Buffer should now be out of stalled mode
		expect(buffer.stalled).toBe(false);

		// Read some samples to make room (readIndex at 80)
		read(buffer, 80, 2);
		expect(buffer.length).toBe(20); // 100 - 80

		// Write 20 samples at the current position
		write(buffer, 100, 20, { channels: 2, value: 1.5 });
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
		const buffer = exact({ rate: 1000, channels: 2, latency: 100 });

		// Exit stalled mode by filling the buffer
		write(buffer, 0, 100, { channels: 2, value: 0.0 });
		expect(buffer.stalled).toBe(false);

		// Read some to make room (readIndex at 80)
		read(buffer, 80, 2);

		// Write 20 samples
		write(buffer, 100, 20, { channels: 2, value: 1.0 });
		expect(buffer.length).toBe(40); // 120 - 80

		// Try to read 50 samples (only 40 available)
		const output = read(buffer, 50, 2);

		expect(output[0].length).toBe(40);
		expect(buffer.length).toBe(0);
	});

	it("should return 0 when no data available", () => {
		const buffer = new AudioRingBuffer({ rate: 1000, channels: 2, latency: 100 });

		const output = read(buffer, 10, 2);
		expect(output[0].length).toBe(0);
	});
});

describe("stall behavior", () => {
	it("should start in stalled mode", () => {
		const buffer = new AudioRingBuffer({ rate: 1000, channels: 2, latency: 100 });
		expect(buffer.stalled).toBe(true);

		// Should not output anything in stalled mode
		write(buffer, 0, 50, { channels: 2, value: 1.0 });
		const output = read(buffer, 10, 2);
		expect(output[0].length).toBe(0);
	});

	it("should exit stalled mode when buffer is full", () => {
		const buffer = new AudioRingBuffer({ rate: 1000, channels: 2, latency: 100 });

		// Fill the buffer completely
		write(buffer, 0, 100, { channels: 2, value: 1.0 });

		// Write more data to trigger overflow handling
		write(buffer, 10, 50, { channels: 2, value: 2.0 }); // This should exit stalled mode

		expect(buffer.stalled).toBe(false);

		// Now we should be able to read
		const output = read(buffer, 10, 2);
		expect(output[0].length).toBe(10);
	});
});

describe("ring buffer wrapping", () => {
	it("should wrap around when buffer is full", () => {
		// A 1000-sample ring, exact so it plays through everything it holds.
		const buffer = exact({ rate: 1000, channels: 1, latency: 100 });
		write(buffer, 0, 600, { channels: 1, value: 1.0 });
		expect(read(buffer, 500, 1)[0].length).toBe(500);

		// Abs 600-1199 wraps onto slots 600-999 and 0-199.
		write(buffer, 600, 600, { channels: 1, value: 2.0 });
		expect(buffer.length).toBe(700);

		const [output] = read(buffer, 700, 1);
		expect([...output]).toEqual([...Array(100).fill(1), ...Array(600).fill(2)]);
	});
});

describe("multi-channel handling", () => {
	it("should handle stereo data correctly", () => {
		const buffer = exact({ rate: 1000, channels: 2, latency: 100 });

		// Exit stalled mode by filling buffer
		write(buffer, 0, 100, { channels: 2, value: 0.5 });
		expect(buffer.stalled).toBe(false);

		// Read some to make room
		read(buffer, 80, 2);

		// Write stereo data with same value for both channels
		write(buffer, 100, 20, { channels: 2, value: 1.5 });

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
		const buffer = new AudioRingBuffer({ rate: 1000, channels: 2, latency: 100 });
		write(buffer, 0, 50, { channels: 2, value: 1.0 });

		const output: Float32Array[] = [];
		// Current implementation throws when channel count doesn't match
		expect(() => buffer.read(output)).toThrow(/wrong number of channels/);
	});

	it("should handle zero-length output buffers", () => {
		const buffer = new AudioRingBuffer({ rate: 1000, channels: 2, latency: 100 });
		write(buffer, 0, 50, { channels: 2, value: 1.0 });

		const output = [new Float32Array(0), new Float32Array(0)];
		const samplesRead = buffer.read(output);
		expect(samplesRead).toBe(0);
	});

	it("should handle fractional timestamps", () => {
		const buffer = new AudioRingBuffer({ rate: 1000, channels: 2, latency: 100 });

		// Exit stalled mode first
		write(buffer, 0, 100, { channels: 2, value: 0.0 });
		write(buffer, 10, 10, { channels: 2, value: 0.0 }); // This exits stalled mode
		read(buffer, 110, 2);

		// Write with fractional timestamp that rounds
		write(buffer, 1105, 10, { channels: 2, value: 1.0 }); // 110.5 samples, rounds to 111
		write(buffer, 1204, 10, { channels: 2, value: 2.0 }); // 120.4 samples, rounds to 120

		const output = read(buffer, 20, 2);
		expect(output[0].length).toBeGreaterThan(0);
	});
});

describe("setLatency", () => {
	it("keeps the receiver's capacity whatever the floor", () => {
		const buffer = new AudioRingBuffer({ rate: 1000, channels: 1, latency: 100 });
		const capacity = buffer.capacity;
		write(buffer, 0, 50, { channels: 1, value: 1.0 });

		buffer.setLatency(400);
		expect(buffer.capacity).toBe(capacity);
		expect(buffer.length).toBe(50);
		expect(buffer.stalled).toBe(true);
	});

	// A lower floor moves the clock, which the ring speeds up onto, so it drops no unread audio.
	it("keeps every unread sample when the floor drops", () => {
		const buffer = exact({ rate: 1000, channels: 1, latency: 100 });
		write(buffer, 0, 100, { channels: 1, value: 1.0 });
		write(buffer, 100, 80, { channels: 1, value: 2.0 });

		buffer.setLatency(30, { target: 0, bound: 0 });
		write(buffer, 180, 20, { channels: 1, value: 3.0 });
		expect(buffer.length).toBe(200);
		expect(buffer.timestamp / 1000).toBe(0);
		const [output] = read(buffer, 200, 1);
		expect([...output]).toEqual([...Array(100).fill(1), ...Array(80).fill(2), ...Array(20).fill(3)]);
	});

	it("refuses an empty floor or one past the capacity", () => {
		const buffer = new AudioRingBuffer({ rate: 1000, channels: 1, latency: 100 });
		expect(() => buffer.setLatency(0)).toThrow(/empty buffer/);
		expect(() => buffer.setLatency(buffer.capacity + 1)).toThrow(/exceeds capacity/);
		expect(() => new AudioRingBuffer({ rate: 1000, channels: 1, latency: 60_000 })).toThrow(/exceeds capacity/);
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
		const buffer = new AudioRingBuffer({ rate, channels: 1, latency: 1000 });
		for (let i = 0; i < 10; i++) {
			write(buffer, i * 20, 960, { channels: 1, value: (i + 1) / 10 });
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
		const buffer = new AudioRingBuffer({ rate: 1000, channels: 1, latency: 100 });

		write(buffer, 2000, 40, { channels: 1, value: 0.5 });
		// Still short of the floor, so nothing plays yet and the ring holds only real samples.
		expect(buffer.stalled).toBe(true);
		expect(buffer.length).toBe(40);
		expect(buffer.timestamp / 1000).toBe(2000);

		write(buffer, 2040, 60, { channels: 1, value: 0.5 });
		expect(buffer.stalled).toBe(false);

		// Playback starts on the first real sample, not on gap-filled silence.
		const output = read(buffer, 40, 1);
		expect(output[0].length).toBe(40);
		expect(output[0][0]).toBeCloseTo(0.5, 5);
	});
});

describe("re-buffer", () => {
	it("re-stalls after running dry and resumes on a cushion, slowing down to rebuild", () => {
		const buffer = new AudioRingBuffer({ rate: 1000, channels: 1, latency: 100, bound: 2000 });

		write(buffer, 0, 100, { channels: 1, value: 1.0 });
		expect(buffer.stalled).toBe(false);

		// Drain it.
		expect(read(buffer, 100, 1)[0].length).toBe(100);
		expect(buffer.length).toBe(0);

		// The next read finds nothing and stalls playback rather than handing the worklet a quantum
		// of silence and trying again on the next one.
		expect(read(buffer, 20, 1)[0].length).toBe(0);
		expect(buffer.stalled).toBe(true);
		expect(buffer.underruns).toBe(1);

		// The cushion is one 30 ms window and a 20 ms read, not the 100 ms floor.
		write(buffer, 100, 40, { channels: 1, value: 2.0 });
		expect(buffer.stalled).toBe(true);
		expect(read(buffer, 20, 1)[0].length).toBe(0);
		write(buffer, 140, 20, { channels: 1, value: 2.0 });
		expect(buffer.stalled).toBe(false);

		// It resumed 60 ms short of its target, so it slows down: 60 ms of audio outlasts 60 ms of reads,
		// and nothing buffered in the meantime was lost.
		expect(read(buffer, 60, 1)[0].length).toBe(60);
		expect(buffer.length).toBeGreaterThan(0);
		expect(buffer.timestamp / 1000).toBeLessThan(160);
		expect(buffer.underruns).toBe(1);
	});

	it("counts a quantum it could only partly fill as an underrun", () => {
		const buffer = new AudioRingBuffer({ rate: 1000, channels: 1, latency: 40 });

		write(buffer, 0, 20, { channels: 1, value: 1.0 });
		write(buffer, 20, 20, { channels: 1, value: 1.0 });
		expect(read(buffer, 30, 1)[0].length).toBe(30);

		// Ten samples remain against a quantum of twenty: the rest of the quantum is silence, which
		// is an underrun whether or not a chunk lands before the next read. It is not a stall: a
		// refill would cost the whole target to cover a gap shorter than one quantum.
		expect(read(buffer, 20, 1)[0].length).toBe(10);
		expect(buffer.stalled).toBe(false);
		expect(buffer.underruns).toBe(1);

		// A chunk landing before the next read keeps playback going.
		write(buffer, 40, 20, { channels: 1, value: 2.0 });
		expect(read(buffer, 20, 1)[0].length).toBe(20);
		expect(buffer.underruns).toBe(1);
	});

	it("does not count an underrun before it starts", () => {
		const buffer = new AudioRingBuffer({ rate: 1000, channels: 1, latency: 40 });
		read(buffer, 20, 1);
		read(buffer, 20, 1);
		expect(buffer.underruns).toBe(0);
	});
});

// A publisher that stops sending, such as a push-to-talk mute, declares where its audio ends. Running
// dry there is the silence it announced, not an underrun, through the refill of the next utterance.
describe("declared end", () => {
	it("is declared silence before any audio, and the next utterance anchors", () => {
		const buffer = new AudioRingBuffer({ rate: 1000, channels: 1, latency: 40 });
		buffer.end(5_000_000);
		expect(buffer.silent).toBe(true);
		expect(read(buffer, 20, 1)[0].length).toBe(0);
		expect(buffer.underruns).toBe(0);

		buffer.next();
		expect(buffer.silent).toBe(true);
		write(buffer, 6000, 40, { channels: 1 });
		expect(read(buffer, 20, 1)[0].length).toBe(20);
		expect(buffer.silent).toBe(false);
		expect(buffer.timestamp / 1000).toBe(6020);
	});

	it("clears declared silence on a timeline reset", () => {
		const buffer = new AudioRingBuffer({ rate: 1000, channels: 1, latency: 40 });
		write(buffer, 0, 40, { channels: 1 });
		buffer.end(30_000);
		read(buffer, 50, 1);
		expect(buffer.silent).toBe(true);
		buffer.reset();
		expect(buffer.silent).toBe(false);
		expect(buffer.stalled).toBe(true);
	});

	it("runs dry at the end without an underrun, and stays silent across the next utterance", () => {
		const buffer = new AudioRingBuffer({ rate: 1000, channels: 1, latency: 40 });
		write(buffer, 0, 40, { channels: 1 });
		buffer.end(30_000);

		expect(read(buffer, 20, 1)[0].length).toBe(20);
		expect(buffer.silent).toBe(false);
		// A partial quantum past the end is silence too, so playback waits for the next utterance.
		expect(read(buffer, 30, 1)[0].length).toBe(20);
		expect(buffer.silent).toBe(true);
		expect(buffer.stalled).toBe(true);
		expect(read(buffer, 20, 1)[0].length).toBe(0);
		expect(buffer.underruns).toBe(0);

		// The next utterance re-anchors and refills, still silent until it plays.
		buffer.next();
		expect(buffer.silent).toBe(true);
		write(buffer, 5000, 40, { channels: 1 });
		expect(read(buffer, 20, 1)[0].length).toBe(20);
		expect(buffer.silent).toBe(false);

		// The reset forgot the old end, so running dry now is an underrun.
		read(buffer, 20, 1);
		read(buffer, 20, 1);
		expect(buffer.underruns).toBe(1);
		expect(buffer.silent).toBe(false);
	});

	it("ends where contiguous frames end, not at their jittered stamp", () => {
		const buffer = new AudioRingBuffer({ rate: 48000, channels: 1, latency: 10 });
		// The second frame is stamped 1 ms late, so it plays from sample 480 and ends at 960, not 1008.
		buffer.end(21_000);
		write(buffer, 0, 480, { channels: 1 });
		write(buffer, 11, 480, { channels: 1 });
		expect(read(buffer, 1024, 1)[0].length).toBe(960);
		expect(buffer.silent).toBe(true);
		expect(buffer.underruns).toBe(0);
	});

	// A finished track shorter than the floor has nothing more coming to fill it.
	for (const declared of ["before", "after"] as const) {
		it(`plays audio short of its floor up to an end declared ${declared} it`, () => {
			const buffer = new AudioRingBuffer({ rate: 1000, channels: 1, latency: 100 });
			if (declared === "before") buffer.end(40_000);
			write(buffer, 0, 40, { channels: 1 });
			if (declared === "after") {
				expect(read(buffer, 20, 1)[0].length).toBe(0);
				buffer.end(40_000);
			}
			expect(buffer.stalled).toBe(false);
			expect(read(buffer, 20, 1)[0].length).toBe(20);
			expect(read(buffer, 30, 1)[0].length).toBe(20);
			expect(buffer.silent).toBe(true);
			expect(buffer.underruns).toBe(0);
		});
	}

	it("counts running dry short of the end as an underrun", () => {
		const buffer = new AudioRingBuffer({ rate: 1000, channels: 1, latency: 20 });
		write(buffer, 0, 20, { channels: 1 });
		buffer.end(40_000);
		read(buffer, 20, 1);
		read(buffer, 20, 1);
		expect(buffer.underruns).toBe(1);
		expect(buffer.silent).toBe(false);
	});

	// Back-to-back presses: the next utterance arrives while the last one's tail is still buffered.
	it("plays the next utterance after the end's tail without flushing it", () => {
		const buffer = new AudioRingBuffer({ rate: 1000, channels: 1, latency: 40 });
		write(buffer, 0, 40, { channels: 1, value: 1 });
		buffer.end(40_000);
		expect(read(buffer, 20, 1)[0].length).toBe(20);

		buffer.next();
		// One 20 ms frame, so the ring stays within a frame of its floor and plays it as received.
		write(buffer, 50, 20, { channels: 1, value: 2 });
		// The rest of the tail, the short gap to the next utterance, then the utterance itself.
		const [played] = read(buffer, 50, 1);
		expect([...played.slice(0, 20)]).toEqual(Array(20).fill(1));
		expect([...played.slice(20, 30)]).toEqual(Array(10).fill(0));
		expect([...played.slice(30)]).toEqual(Array(20).fill(2));
		// The gap is the declared silence, for the reader to fill with comfort noise.
		expect(buffer.quiet).toEqual([20, 30]);
		expect(buffer.underruns).toBe(0);
		expect(buffer.silent).toBe(false);

		// The end no longer applies, so running dry now is an underrun.
		read(buffer, 20, 1);
		expect(buffer.underruns).toBe(1);
	});

	// A playhead 30 ms behind the clock plays the end's tail as received; only then does it catch up,
	// by skipping that much of the declared silence, which drops no audio.
	it("keeps the end's tail and skips the declared silence after it by the lag", () => {
		const buffer = new AudioRingBuffer({ rate: 1000, channels: 1, latency: 40, bound: 1000 });
		write(buffer, 0, 40, { channels: 1, value: 1 });
		buffer.clock(30_000);
		expect(read(buffer, 20, 1)[0].length).toBe(20);
		buffer.end(40_000);
		buffer.next();

		write(buffer, 100, 20, { channels: 1, value: 2 });
		buffer.clock(50_000);
		expect([...read(buffer, 20, 1)[0]]).toEqual(Array(20).fill(1));

		write(buffer, 120, 20, { channels: 1, value: 2 });
		buffer.clock(70_000);
		expect([...read(buffer, 40, 1)[0]]).toEqual([...Array(30).fill(0), ...Array(10).fill(2)]);
		expect(buffer.underruns).toBe(0);
		expect(buffer.skips).toBe(0);
		expect(buffer.trimmed).toBe(0);
	});

	it("reports a declared silence across reads until the next utterance plays", () => {
		const buffer = new AudioRingBuffer({ rate: 1000, channels: 1, latency: 40 });
		write(buffer, 0, 40, { channels: 1, value: 1 });
		buffer.end(40_000);
		read(buffer, 30, 1);
		buffer.next();
		write(buffer, 70, 20, { channels: 1, value: 2 });

		read(buffer, 20, 1);
		expect(buffer.quiet).toEqual([10, 20]);
		read(buffer, 20, 1);
		expect(buffer.quiet).toEqual([0, 20]);
		read(buffer, 20, 1);
		expect(buffer.quiet).toBeUndefined();
	});

	// The next utterance's audio may land a beat after the tail has played, which is still the
	// silence the publisher declared.
	it("treats running dry before the next utterance lands as declared silence", () => {
		const buffer = new AudioRingBuffer({ rate: 1000, channels: 1, latency: 20 });
		write(buffer, 0, 40, { channels: 1 });
		buffer.end(40_000);
		read(buffer, 20, 1);
		buffer.next();
		read(buffer, 20, 1);
		read(buffer, 20, 1);
		expect(buffer.underruns).toBe(0);
		expect(buffer.silent).toBe(true);

		write(buffer, 60, 20, { channels: 1, value: 2 });
		read(buffer, 20, 1);
		expect(buffer.quiet).toEqual([0, 20]);
		expect([...read(buffer, 20, 1)[0]]).toEqual(Array(20).fill(2));
		expect(buffer.quiet).toBeUndefined();
		expect(buffer.underruns).toBe(0);
	});

	it("re-anchors the next utterance once the end's audio has played out", () => {
		const buffer = new AudioRingBuffer({ rate: 1000, channels: 1, latency: 20 });
		write(buffer, 0, 20, { channels: 1 });
		buffer.end(20_000);
		read(buffer, 20, 1);
		read(buffer, 20, 1);
		expect(buffer.silent).toBe(true);
		buffer.next();
		expect(buffer.anchored).toBe(false);
		expect(buffer.silent).toBe(true);
	});

	it("takes an end that arrives after the playhead ran out there", () => {
		const buffer = new AudioRingBuffer({ rate: 1000, channels: 1, latency: 20 });
		write(buffer, 0, 20, { channels: 1 });
		read(buffer, 20, 1);
		read(buffer, 20, 1);
		expect(buffer.underruns).toBe(1);
		buffer.end(20_000);
		expect(buffer.silent).toBe(true);
	});
});

// Backpressure keeps a ring near its floor, but nothing in the ring depends on it.
describe("holding more than its floor", () => {
	function createBuffered(latency: number) {
		return exact({ rate: 1000, channels: 1, latency });
	}

	it("anchors to the first frame and un-stalls at the latency target", () => {
		const buffer = createBuffered(50);
		expect(buffer.stalled).toBe(true);
		write(buffer, 2000, 100, { channels: 1, value: 0.1 });
		expect(buffer.stalled).toBe(false);
		expect(buffer.timestamp / 1000).toBe(2000);
	});

	it("plays frames in order within the floor-sized ring without skipping ahead", () => {
		// A 1000-sample utterance in a ring that holds it.
		const buffer = createBuffered(600);
		for (let i = 0; i < 10; i++) {
			write(buffer, 2000 + i * 100, 100, { channels: 1, value: (i + 1) / 10 });
		}
		expect(buffer.length).toBe(1000);

		const first = read(buffer, 100, 1);
		expect(first[0][0]).toBeCloseTo(0.1, 5);
		const second = read(buffer, 100, 1);
		expect(second[0][0]).toBeCloseTo(0.2, 5);
	});

	it("reset re-stalls and re-anchors to the next utterance", () => {
		const buffer = createBuffered(50);
		write(buffer, 2000, 100, { channels: 1, value: 0.1 });
		read(buffer, 50, 1);

		buffer.reset();
		expect(buffer.stalled).toBe(true);

		write(buffer, 500, 100, { channels: 1, value: 0.9 });
		expect(buffer.timestamp / 1000).toBe(500);
		const out = read(buffer, 100, 1);
		expect(out[0][0]).toBeCloseTo(0.9, 5);
	});

	it("drops the oldest samples once the ring fills beyond its capacity", () => {
		// 50ms floor at 1000Hz in a 1000-sample ring. Backpressure normally keeps the decode loop from
		// running this far ahead; if it slips, the ring drops its oldest as a backstop.
		const buffer = createBuffered(50);

		write(buffer, 0, 1000, { channels: 1, value: 0.1 }); // [0, 1000)
		write(buffer, 1000, 100, { channels: 1, value: 0.2 }); // past the ring; drops [0, 100)

		expect(buffer.timestamp / 1000).toBe(100);
		expect(buffer.length).toBe(1000);
	});

	it("truncate drops the write-ahead tail a successor supersedes", () => {
		// 600ms floor at 1000Hz -> 1200-sample ring, holding a 1s write-ahead utterance.
		const buffer = createBuffered(600);
		for (let i = 0; i < 10; i++) {
			write(buffer, 2000 + i * 100, 100, { channels: 1, value: 0.1 });
		}
		read(buffer, 100, 1); // playhead at 2100

		// A successor track takes over at 2200 with 100ms of its own audio. Writing it only overwrites
		// [2200, 2300); without the truncate, [2300, 3000) of the old track still plays after it.
		buffer.truncate(2200 * 1000);
		write(buffer, 2200, 100, { channels: 1, value: 0.9 });

		expect(buffer.stalled).toBe(false); // truncate keeps playing, unlike reset()
		expect(read(buffer, 100, 1)[0][0]).toBeCloseTo(0.1, 5); // [2100, 2200) was already due
		expect(read(buffer, 100, 1)[0][0]).toBeCloseTo(0.9, 5); // the successor
		expect(read(buffer, 100, 1)[0].length).toBe(0); // and nothing after it
	});

	it("truncate never rewinds past the playhead", () => {
		const buffer = createBuffered(600);
		write(buffer, 2000, 1000, { channels: 1, value: 0.1 });
		read(buffer, 100, 1); // playhead at 2100

		// A successor whose first frame predates the playhead: those samples are already due, so the
		// write index floors there rather than going backwards.
		buffer.truncate(1000 * 1000);
		expect(buffer.length).toBe(0);
		expect(read(buffer, 100, 1)[0].length).toBe(0);
	});
});

describe("latency increase", () => {
	// A deeper target moves the clock later, which the ring slows down onto rather than stopping for:
	// parking cost a gap of silence for every rise, however small.
	it("keeps playing through a deeper floor, keeping what it buffered", () => {
		const buffer = new AudioRingBuffer({ rate: 1000, channels: 1, latency: 100 });

		// Fill to the floor so playback starts, then play half of it.
		write(buffer, 0, 100, { channels: 1, value: 1.0 });
		expect(buffer.stalled).toBe(false);
		read(buffer, 50, 1);

		buffer.setLatency(110);
		expect(buffer.stalled).toBe(false);
		expect(buffer.length).toBe(50);
		expect(buffer.timestamp / 1000).toBe(50);
		expect(read(buffer, 1, 1)[0][0]).toBe(1.0);
	});

	it("keeps playing through a shallower floor", () => {
		const buffer = new AudioRingBuffer({ rate: 1000, channels: 1, latency: 100 });
		write(buffer, 0, 100, { channels: 1, value: 1.0 });
		read(buffer, 50, 1);

		buffer.setLatency(90);
		expect(buffer.stalled).toBe(false);
	});

	it("starts once a lowered floor is already covered", () => {
		const buffer = new AudioRingBuffer({ rate: 1000, channels: 1, latency: 100 });
		write(buffer, 0, 60, { channels: 1, value: 1.0 });
		expect(buffer.stalled).toBe(true);

		// No further frame arrives, as when the source stops, so only the lower floor can start it.
		buffer.setLatency(50);
		expect(buffer.stalled).toBe(false);
		expect(read(buffer, 1, 1)[0][0]).toBe(1.0);
	});
});

describe("time stretch", () => {
	it("truncate replaces the unplayed part of a staged window", () => {
		// Live, so the first read stages a whole 30 ms window though it plays only 128 samples.
		const buffer = new AudioRingBuffer({ rate: 48000, channels: 1, latency: 100 });
		write(buffer, 0, 4800, { channels: 1, value: 0.1 });
		read(buffer, 128, 1);

		buffer.truncate(10_000);
		write(buffer, 10, 960, { channels: 1, value: 0.9 });

		// The predecessor plays to the truncation, then the successor in full.
		const out = read(buffer, 480 - 128 + 960, 1)[0];
		expect(out.length).toBe(480 - 128 + 960);
		expect(out[480 - 128 - 1]).toBeCloseTo(0.1, 5);
		expect(out[480 - 128]).toBeCloseTo(0.9, 5);
		expect(buffer.length).toBe(0);
	});

	// 48 kHz: 30 ms windows of 1440 frames, and a constant signal stretches by its shortest pitch
	// period, 2.5 ms or 120 frames.
	const RATE = 48000;
	const QUANTUM = 128;
	const live = (latency: number, terms: { bound?: number } = {}) =>
		new AudioRingBuffer({ rate: RATE, channels: 1, latency, ...terms });
	const frames = (ms: number) => (RATE * ms) / 1000;
	// A bound turns the stretch on; without a clock nothing trails it far enough to skip.
	const STRETCHING = { bound: 2000 };

	// Without a clock, the target is where the playhead belongs, so audio landing 40 ms short of it
	// plays slower: the ring stretches what it holds rather than run dry on time.
	it("slows down once arrivals land short of its target", () => {
		const buffer = live(100, STRETCHING);
		write(buffer, 0, frames(100), { value: 0.5 });
		read(buffer, frames(60), 1);
		write(buffer, 100, frames(20), { value: 0.5 });
		let played = 0;
		for (;;) {
			const [output] = read(buffer, QUANTUM, 1);
			played += output.length;
			if (output.length < QUANTUM) break;
		}
		expect(played).toBeGreaterThan(frames(60));
		expect(buffer.accelerated).toBe(0);
	});

	it("drains the tail before a declared end as received, since running dry there is silence", () => {
		const buffer = live(100);
		write(buffer, 0, frames(100), { value: 0.5 });
		buffer.end(100_000);
		let played = 0;
		for (;;) {
			const [output] = read(buffer, QUANTUM, 1);
			played += output.length;
			if (output.length < QUANTUM) break;
		}
		expect(played).toBe(frames(100));
		expect(buffer.underruns).toBe(0);
		expect(buffer.silent).toBe(true);
	});

	// Fed `burst` ms of 20 ms frames at a time, at the real-time rate, so only the stretch can move the
	// ring's depth. Returns the depth right after each burst lands, where it reads the clock.
	async function converge(buffer: AudioRingBuffer, start: number, seconds: number, burst = 20) {
		const timestamps: number[] = [];
		const peaks: number[] = [];
		let written = start;
		for (let rendered = 0; rendered < frames(seconds * 1000); rendered += QUANTUM) {
			if (written < start + rendered + QUANTUM) {
				for (const end = written + frames(burst); written < end; written += frames(20)) {
					write(buffer, (written * 1000) / RATE, frames(20), { value: 0.5 });
				}
				peaks.push(buffer.length);
			}
			const [output] = read(buffer, QUANTUM, 1);
			if (burst === 20) expect(output.length).toBe(QUANTUM);
			timestamps.push(buffer.timestamp / 1000);
		}
		return { timestamps, peaks, written };
	}

	// A constant signal accelerates by one 2.5 ms pitch period at a time, and only by a period that fits
	// in the excess, so the ring rests within one period of its floor and never under it.
	const PERIOD = frames(2.5);
	const settled = (peaks: number[], floor: number) => {
		for (const peak of peaks.slice(-20)) {
			expect(peak).toBeGreaterThanOrEqual(frames(floor));
			expect(peak).toBeLessThan(frames(floor) + PERIOD);
		}
	};

	it("accelerates a ring above its floor back onto it, dropping nothing", async () => {
		const buffer = live(100, STRETCHING);
		write(buffer, 0, frames(300), { value: 0.5 });
		const { timestamps, peaks, written } = await converge(buffer, frames(300), 8);

		settled(peaks, 100);
		// Every sample written either played or is still buffered, so nothing was skipped.
		expect(buffer.skips).toBe(0);
		expect(buffer.trimmed).toBe(0);
		// At least the 200 ms it started above the floor, less the period it may rest above it.
		expect(buffer.accelerated).toBeGreaterThan(200 - 2.5);
		expect(Math.round((buffer.timestamp * RATE) / 1_000_000) + buffer.length).toBeGreaterThan(written - frames(30));
		for (let i = 1; i < timestamps.length; i++) {
			const step = timestamps[i] - timestamps[i - 1];
			expect(step).toBeGreaterThan(0);
			expect(step).toBeLessThanOrEqual((2 * QUANTUM * 1000) / RATE + 1e-9);
		}
		expect(buffer.underruns).toBe(0);
	});

	// Five 20 ms frames flushed at once, every 100 ms. Between bursts the ring drains a whole burst
	// below its floor, which is arrivals running late rather than audio ahead of the clock, so steering
	// on that depth would settle a burst behind it.
	it("rests on its floor right after each burst, never a burst above it", async () => {
		const buffer = live(120, STRETCHING);
		write(buffer, 0, frames(120), { value: 0.5 });
		const { peaks } = await converge(buffer, frames(120), 8, 100);
		settled(peaks, 120);
		expect(buffer.underruns).toBe(0);
	});

	// A floor short of the arrival spread underruns, and that is the cost of the floor: each burst
	// lands above the target, which leaves nothing to slow down for.
	it("underruns every burst that outruns its target", async () => {
		const buffer = live(60, STRETCHING);
		write(buffer, 0, frames(60), { value: 0.5 });
		const { peaks } = await converge(buffer, frames(60), 8, 100);
		// Each burst lands on an empty ring, never on audio left from the one before.
		for (const peak of peaks.slice(-20)) expect(peak).toBe(frames(100));
		expect(buffer.underruns).toBeGreaterThanOrEqual(70);
	});

	it("fast-accelerates a ring four times past its high limit, even straight after a stretch", async () => {
		const buffer = live(100, STRETCHING);
		write(buffer, 0, frames(600), { value: 0.5 });
		await converge(buffer, frames(600), 1);
		// Fast accelerate removes up to 15 ms of every 30 ms window until the ring is under 400 ms.
		expect(buffer.length).toBeLessThan(frames(400 + 20));
	});

	// A playhead trailing the clock past its bound and a window skips back to the bound, dropping the
	// staged window with the audio it passes.
	it("drops the staged window with the audio a skip passes", () => {
		const buffer = live(100);
		write(buffer, 0, frames(100), { value: 0.25 });
		read(buffer, QUANTUM, 1);

		write(buffer, 100, frames(400), { value: 0.5 });
		buffer.clock(400_000);
		const [output] = read(buffer, QUANTUM, 1);
		expect(buffer.skips).toBe(1);
		expect(buffer.timestamp / 1000).toBeCloseTo(400 + (QUANTUM * 1000) / RATE, 6);
		// Everything before the new playhead but the quantum that played.
		expect(buffer.trimmed).toBeCloseTo(400 - (QUANTUM * 1000) / RATE, 6);
		expect(buffer.accelerated).toBe(0);
		expect(output.length).toBe(QUANTUM);
		expect(output.every((sample) => sample === 0.5)).toBe(true);
	});

	it("trims a gap's zeros as media", () => {
		const buffer = live(100);
		write(buffer, 0, frames(100), { value: 0.25 });
		read(buffer, QUANTUM, 1);
		// A 100 ms gap, then audio past it, with the clock at 410 ms.
		write(buffer, 200, frames(10), { value: 0.5 });
		write(buffer, 210, frames(300), { value: 0.5 });
		buffer.clock(410_000);
		read(buffer, QUANTUM, 1);
		expect(buffer.skips).toBe(1);
		expect(buffer.timestamp / 1000).toBeCloseTo(410 + (QUANTUM * 1000) / RATE, 6);
		expect(buffer.trimmed).toBeCloseTo(410 - (QUANTUM * 1000) / RATE, 6);
	});

	it("trims no declared silence", () => {
		const buffer = live(100);
		write(buffer, 0, frames(100), { value: 0.25 });
		read(buffer, QUANTUM, 1);
		// A declared silence over [100, 200) ms, then more than the ring's 1 s capacity, which skips to
		// 250 ms through the silence and the incoming chunk's first 50 ms.
		buffer.end(100_000);
		buffer.next();
		write(buffer, 200, frames(1050), { value: 0.5 });
		expect(buffer.skips).toBe(1);
		expect(buffer.timestamp / 1000).toBeCloseTo(250, 6);
		expect(buffer.trimmed).toBeCloseTo(250 - 100 - (QUANTUM * 1000) / RATE, 6);
	});

	it("trims only what the ring held, never the gap a re-anchor jumps", () => {
		const buffer = live(100);
		write(buffer, 0, frames(100), { value: 0.25 });
		read(buffer, QUANTUM, 1);
		// A group past its age budget: the next audio lands 2 s on, past the ring's capacity.
		write(buffer, 2000, frames(20), { value: 0.5 });
		expect(buffer.skips).toBe(1);
		expect(buffer.trimmed).toBeCloseTo(100 - (QUANTUM * 1000) / RATE, 6);
	});

	it("counts nothing when an empty ring re-anchors", () => {
		const buffer = live(100);
		write(buffer, 0, frames(100), { value: 0.25 });
		while (buffer.length > 0) read(buffer, QUANTUM, 1);
		write(buffer, 3000, frames(20), { value: 0.5 });
		expect(buffer.skips).toBe(0);
		expect(buffer.trimmed).toBe(0);
	});

	// A playhead behind the clock skips a declared silence rather than stretching across it, and only
	// once the staged window before it has played.
	it("skips a declared silence by the lag only after the staged window plays", () => {
		const buffer = live(100, { bound: 1000 });
		write(buffer, 0, frames(120), { value: 0.25 });
		while (buffer.timestamp / 1000 <= 90) {
			buffer.clock(buffer.timestamp);
			read(buffer, QUANTUM, 1);
		}
		const staged = buffer.length;
		expect(staged).toBeGreaterThan(0);
		expect(staged).toBeLessThan(frames(30));

		// The next utterance lands 80 ms after the last, 10 ms long, with the playhead 40 ms behind.
		buffer.next();
		write(buffer, 200, frames(10), { value: 0.5 });
		buffer.clock(buffer.timestamp + 40_000);
		const [output] = read(buffer, staged + frames(50), 1);

		// The staged window, then the 80 ms silence less the 40 ms lag, then the utterance.
		expect(output.length).toBe(staged + frames(50));
		expect(output.slice(0, staged).every((sample) => sample === 0.25)).toBe(true);
		expect(output.slice(staged, staged + frames(40)).every((sample) => sample === 0)).toBe(true);
		expect(output.slice(staged + frames(40)).every((sample) => sample === 0.5)).toBe(true);
		expect(buffer.skips).toBe(0);
		expect(buffer.trimmed).toBe(0);
	});

	// A jump counts only as far as the ring holds audio past its target, so one that left no audio to
	// skip, as a reference moving on jitter while the buffer runs low, is the stretch's.
	it("never skips a pending jump below its target", () => {
		const buffer = live(20, { bound: 2000 });
		write(buffer, 0, frames(100), { value: 0.5 });
		buffer.clock(0);
		read(buffer, QUANTUM, 1);

		// 25 ms pending, short of a window, then 15 ms more once only 35 ms is held: one 20 ms target and
		// 15 ms past it.
		buffer.jumped(25_000);
		buffer.clock(buffer.timestamp + 25_000);
		read(buffer, QUANTUM, 1);
		expect(buffer.skips).toBe(0);
		while (buffer.length > frames(35) + QUANTUM) {
			buffer.clock(buffer.timestamp + 25_000);
			read(buffer, QUANTUM, 1);
		}
		buffer.jumped(15_000);
		buffer.clock(buffer.timestamp + 40_000);
		read(buffer, QUANTUM, 1);
		expect(buffer.skips).toBe(0);
		expect(buffer.stalled).toBe(false);
	});

	it("ignores a jump while it holds no audio past its target", () => {
		const buffer = live(100, { bound: 2000 });
		write(buffer, 0, frames(100), { value: 0.5 });
		buffer.clock(0);
		read(buffer, QUANTUM, 1);

		buffer.jumped(50_000);
		buffer.clock(buffer.timestamp + 50_000);
		read(buffer, QUANTUM, 1);
		expect(buffer.skips).toBe(0);
	});

	// A clock whose reference moved earlier skips as much, as far as the playhead trails it, where the
	// same lag from a fallen target is caught up by speeding up.
	for (const jumped of [false, true]) {
		it(`${jumped ? "skips" : "catches up on"} a clock moved ahead by ${jumped ? "its reference" : "a fall in its target"}`, () => {
			const buffer = live(200, { bound: 2000 });
			write(buffer, 0, frames(1000), { value: 0.5 });
			// Joined on the clock.
			buffer.clock(0);
			read(buffer, QUANTUM, 1);

			// The clock moves 100 ms ahead.
			if (jumped) buffer.jumped(150_000);
			else buffer.setLatency(100, { target: 100, bound: 2000 });
			const quantum = (QUANTUM * 1_000_000) / RATE;
			let clock = buffer.timestamp + 100_000;
			buffer.clock(clock);
			read(buffer, QUANTUM, 1);
			if (jumped) {
				// Never past the clock, however far the reference moved.
				expect(buffer.skips).toBe(1);
				expect(Math.abs(buffer.timestamp - clock - quantum)).toBeLessThan(50);
				return;
			}
			for (let i = 0; i < 40; i++) {
				clock += quantum;
				buffer.clock(clock);
				read(buffer, QUANTUM, 1);
			}
			expect(buffer.skips).toBe(0);
			expect(buffer.accelerated).toBeGreaterThan(0);
		});
	}

	// A successor's arrivals count from where it cut the predecessor, so audio the predecessor had
	// received past the cut never reads as depth to speed through.
	it("counts a successor's arrivals from the cut", () => {
		const buffer = live(100, { bound: 2000 });
		write(buffer, 0, frames(200), { value: 0.5 });
		buffer.received(10_000_000);
		buffer.clock(0);
		read(buffer, QUANTUM, 1);

		buffer.truncate(100_000);
		write(buffer, 100, frames(60), { value: 0.9 });
		// 20 ms behind the clock, on 60 ms of the successor's audio: too little to speed through.
		for (let i = 0; i < 20; i++) {
			buffer.clock(buffer.timestamp + 20_000);
			read(buffer, QUANTUM, 1);
		}
		expect(buffer.accelerated).toBe(0);
	});

	// A jump past what is decoded skips the rest once it lands, whether or not the lag past the bound
	// skipped first.
	for (const bound of [2000, 200]) {
		it(`skips the rest of a jump past what is decoded once it lands, at a ${bound} ms bound`, () => {
			const buffer = live(100, { bound });
			write(buffer, 0, frames(100), { value: 0.5 });
			buffer.clock(0);
			read(buffer, QUANTUM, 1);

			// The backlog that moved the reference arrived, still encoded past what is decoded.
			buffer.received(900_000);
			buffer.jumped(500_000);
			const clock = buffer.timestamp + 500_000;
			buffer.clock(clock);
			read(buffer, QUANTUM, 1);
			expect(buffer.skips).toBe(1);

			write(buffer, 100, frames(800), { value: 0.9 });
			buffer.clock(clock);
			read(buffer, QUANTUM, 1);
			expect(Math.abs(buffer.timestamp - clock - (QUANTUM * 1_000_000) / RATE)).toBeLessThan(50);
		});
	}

	// Nothing trailing the clock was heard before the first read on it, so a join lands on the clock
	// past a backlog the bound would otherwise keep.
	it("joins on the clock past a backlog inside its bound", () => {
		const buffer = live(100, { bound: 1000 });
		write(buffer, 0, frames(800), { value: 0.5 });
		buffer.clock(500_000);
		read(buffer, QUANTUM, 1);
		expect(buffer.skips).toBe(1);
		expect(buffer.timestamp / 1000).toBeCloseTo(500 + (QUANTUM * 1000) / RATE, 6);

		// A reset starts a new timeline, which joins again.
		buffer.reset();
		write(buffer, 2000, frames(800), { value: 0.5 });
		buffer.clock(2_300_000);
		read(buffer, QUANTUM, 1);
		expect(buffer.skips).toBe(2);
	});

	// A zero bound turns the stretch off: a lag under a window that the stretch would speed through
	// stays, more skips onto the clock, and a playhead more than a window ahead of it pads silence until
	// the clock reaches it.
	it("with a zero bound, plays unaltered and skips or pads onto the clock", () => {
		const buffer = live(100, { bound: 0 });
		write(buffer, 0, frames(800), { value: 0.5 });
		buffer.clock(0);
		read(buffer, QUANTUM, 1);
		const quantum = (QUANTUM * 1_000_000) / RATE;

		// 20 ms behind the clock, with far more than the target buffered.
		let clock = buffer.timestamp + 20_000;
		const start = buffer.timestamp;
		for (let i = 0; i < 20; i++) {
			buffer.clock(clock);
			expect(read(buffer, QUANTUM, 1)[0].every((sample) => sample === 0.5)).toBe(true);
			clock += quantum;
		}
		expect(buffer.timestamp - start).toBeCloseTo(20 * quantum, 6);
		expect(buffer.accelerated).toBe(0);
		expect(buffer.skips).toBe(0);

		// 100 ms more skips onto the clock.
		clock += 100_000;
		buffer.clock(clock);
		read(buffer, QUANTUM, 1);
		expect(buffer.skips).toBe(1);
		expect(buffer.timestamp).toBeCloseTo(clock + quantum, -1);
		clock += quantum;

		// 50 ms ahead of it pads silence until the clock reaches the playhead.
		clock -= 50_000;
		const playhead = buffer.timestamp;
		let padded = 0;
		for (;;) {
			buffer.clock(clock);
			const [output] = read(buffer, QUANTUM, 1);
			clock += quantum;
			if (output.some((sample) => sample !== 0)) break;
			padded++;
		}
		expect(padded).toBe(Math.floor(50_000 / quantum));
		expect(buffer.timestamp).toBeGreaterThan(playhead);
		expect(buffer.accelerated).toBe(0);
		expect(buffer.skips).toBe(1);
		expect(buffer.underruns).toBe(0);
	});

	// A playhead ahead of the clock within the bound is the stretch's to slow down onto; past it, as a
	// target rise leaves it, it pads silence until the clock reaches it, dropping nothing.
	it("pads a playhead ahead of the clock past its bound onto it", () => {
		const buffer = live(100, { bound: 200 });
		write(buffer, 0, frames(800), { value: 0.5 });
		buffer.clock(0);
		read(buffer, QUANTUM, 1);
		const quantum = (QUANTUM * 1_000_000) / RATE;

		// 100 ms ahead: within the bound, so audio keeps playing.
		let clock = buffer.timestamp - 100_000;
		buffer.clock(clock);
		expect(read(buffer, QUANTUM, 1)[0].some((sample) => sample !== 0)).toBe(true);
		clock += quantum;

		// 300 ms ahead: silence until the clock reaches the playhead.
		clock = buffer.timestamp - 300_000;
		const playhead = buffer.timestamp;
		let padded = 0;
		for (;;) {
			buffer.clock(clock);
			const [output] = read(buffer, QUANTUM, 1);
			clock += quantum;
			if (output.some((sample) => sample !== 0)) break;
			padded++;
		}
		expect(padded).toBe(Math.floor(300_000 / quantum));
		expect(buffer.timestamp).toBeGreaterThan(playhead);
		expect(buffer.skips).toBe(0);
		expect(buffer.underruns).toBe(0);
	});

	// Audio the stretch could still catch up on plays; what trails the clock past the bound, or one
	// window when that is larger, skips onto the clock.
	for (const bound of [0, 200]) {
		it(`skips audio trailing the clock past a ${bound} ms bound onto it`, () => {
			const threshold = Math.max(bound, 30);
			const buffer = live(100, { bound });
			write(buffer, 0, frames(800), { value: 0.5 });
			// Joined on the clock.
			buffer.clock(0);
			read(buffer, QUANTUM, 1);

			buffer.clock(buffer.timestamp + threshold * 1000);
			read(buffer, QUANTUM, 1);
			expect(buffer.skips).toBe(0);

			const clock = buffer.timestamp + (threshold + 1) * 1000;
			buffer.clock(clock);
			read(buffer, QUANTUM, 1);
			expect(buffer.skips).toBe(1);
			// Onto the clock, then a quantum on.
			const quantum = (QUANTUM * 1_000_000) / RATE;
			expect(buffer.timestamp).toBeGreaterThanOrEqual(clock + quantum - 1);
			expect(buffer.timestamp).toBeLessThan(clock + quantum + 1);
		});
	}
});

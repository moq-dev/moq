import { describe, expect, it } from "bun:test";
import { type Micro, Milli } from "@moq/clock";
import { AudioBuffer } from "./buffer";
import type { Message, State } from "./render";

// A buffered buffer over a real port, with the worklet's end of it left to the test.
function open(): {
	buffer: AudioBuffer;
	state: (timestamp: number, stalled: boolean, epoch: number) => Promise<void>;
	close: () => void;
} {
	const channel = new MessageChannel();
	const buffer = new AudioBuffer({ port: channel.port1 } as unknown as AudioWorkletNode, {
		channels: 1,
		rate: 1000,
		latency: 100,
		buffered: true,
	});
	return {
		buffer,
		// Posts a worklet state report and resolves once the buffer has handled it: this
		// listener is added after the buffer's, so it runs after it for the same message.
		state: (timestamp, stalled, epoch) => {
			const msg: State = { type: "state", timestamp: timestamp as Micro, stalled, underruns: 0, epoch };
			const handled = new Promise<void>((resolve) =>
				channel.port1.addEventListener("message", () => resolve(), { once: true }),
			);
			channel.port2.postMessage(msg);
			return handled;
		},
		close: () => {
			buffer.close();
			channel.port1.close();
			channel.port2.close();
		},
	};
}

describe("AudioBuffer", () => {
	it("transfers PCM ownership and orders control messages after the data they change", async () => {
		const channel = new MessageChannel();
		const buffer = new AudioBuffer({ port: channel.port1 } as unknown as AudioWorkletNode, {
			channels: 2,
			rate: 1000,
			latency: 100,
			buffered: true,
		});
		const messages: Message[] = [];
		const received = new Promise<void>((resolve) => {
			channel.port2.onmessage = ({ data }: MessageEvent<Message>) => {
				messages.push(data);
				if (data.type === "reset") resolve();
			};
		});
		try {
			const left = new Float32Array([1, 2, 3]);
			const right = new Float32Array([4, 5, 6]);
			buffer.insert(500_000 as Micro, [left, right]);
			expect(left.byteLength).toBe(0);
			expect(right.byteLength).toBe(0);
			buffer.setLatency(200);
			buffer.truncate(502_000 as Micro);
			buffer.reset();
			await received;
			expect(messages).toEqual([
				{ type: "init", channels: 2, rate: 1000, latency: Milli(100), buffered: true },
				{
					type: "data",
					timestamp: 500_000 as Micro,
					data: [new Float32Array([1, 2, 3]), new Float32Array([4, 5, 6])],
				},
				{ type: "latency", latency: Milli(200) },
				{ type: "truncate", timestamp: 502_000 as Micro },
				{ type: "reset", epoch: 1 },
			]);
		} finally {
			buffer.close();
			channel.port1.close();
			channel.port2.close();
		}
	});

	it("holds the lookahead before the worklet reports", () => {
		const { buffer, close } = open();
		const signal = new AbortController().signal;
		try {
			expect(Bun.peek.status(buffer.wait(0 as Micro, signal))).toBe("fulfilled");
			expect(Bun.peek.status(buffer.wait(100_000 as Micro, signal))).toBe("fulfilled");
			expect(Bun.peek.status(buffer.wait(120_000 as Micro, signal))).toBe("pending");
		} finally {
			close();
		}
	});

	// A decode loop can reach `wait` on a buffer a shape change already closed. Nothing advances a
	// closed buffer, so a gated wait would never settle and would hold the decoder effect's rerun.
	it("releases its waiters and stops observing the port when closed", async () => {
		const { buffer, state, close } = open();
		try {
			await state(100_000, false, 0);
			const held = buffer.wait(10_000_000 as Micro, new AbortController().signal);
			expect(Bun.peek.status(held)).toBe("pending");
			buffer.close();
			expect(Bun.peek.status(held)).toBe("fulfilled");
			await state(5_000_000, false, 0);
			expect(buffer.timestamp.peek()).toBe(100_000 as Micro);
			expect(Bun.peek.status(buffer.wait(10_000_000 as Micro, new AbortController().signal))).toBe("fulfilled");
		} finally {
			close();
		}
	});

	// A decode loop retired mid-wait (a rendition change while the playhead is parked) must not leave
	// its waiter queued, or every such change strands one more for the life of the buffer.
	it("releases a gated wait when its signal aborts", async () => {
		const { buffer, state, close } = open();
		try {
			await state(0, false, 0);
			const controller = new AbortController();
			const wait = buffer.wait(10_000_000 as Micro, controller.signal);
			await Promise.resolve();
			expect(Bun.peek.status(wait)).toBe("pending");

			controller.abort();
			expect(Bun.peek.status(wait)).toBe("fulfilled");
			expect(Bun.peek.status(buffer.wait(10_000_000 as Micro, controller.signal))).toBe("fulfilled");
		} finally {
			close();
		}
	});

	// The decoder's output reaches the ring after the decode loop has moved on, so a stalled ring
	// still looks stalled while the loop releases frame after frame. Releasing all of them pushes
	// the whole lookahead into a floor-sized ring, which drops the oldest unplayed audio.
	it("holds the lookahead while a stalled ring refills", async () => {
		const { buffer, state, close } = open();
		const signal = new AbortController().signal;
		try {
			expect(Bun.peek.status(buffer.wait(0 as Micro, signal))).toBe("fulfilled");
			expect(Bun.peek.status(buffer.wait(100_000 as Micro, signal))).toBe("fulfilled");
			const ahead = buffer.wait(120_000 as Micro, signal);
			expect(Bun.peek.status(ahead)).toBe("pending");

			// The floor lands and plays past the frame's headroom.
			await state(60_000, false, 0);
			expect(Bun.peek.status(buffer.wait(170_000 as Micro, signal))).toBe("pending");
			expect(Bun.peek.status(ahead)).toBe("fulfilled");
		} finally {
			close();
		}
	});

	// Decoded audio can land short of the timestamp its frame was released at (Opus pre-skip), so a
	// refill that stops at the floor exactly leaves the ring a sample short of un-stalling, with the
	// gate waiting on the ring.
	it("refills until a frame reaches the floor", async () => {
		const { buffer, state, close } = open();
		const signal = new AbortController().signal;
		try {
			// The ring anchored on a first frame at 6 ms, still short of the floor.
			await state(6_000, true, 0);
			expect(Bun.peek.status(buffer.wait(100_000 as Micro, signal))).toBe("fulfilled");
			expect(Bun.peek.status(buffer.wait(120_000 as Micro, signal))).toBe("fulfilled");
			expect(Bun.peek.status(buffer.wait(140_000 as Micro, signal))).toBe("pending");
		} finally {
			close();
		}
	});

	// An underrun parks the playhead where the audio ran out. A frame past a gap is further than the
	// floor ahead of it, and holding it would leave the ring stalled with nothing to play.
	it("refills from a frame past a gap", async () => {
		const { buffer, state, close } = open();
		const signal = new AbortController().signal;
		try {
			await state(100_000, true, 0);
			expect(Bun.peek.status(buffer.wait(10_000_000 as Micro, signal))).toBe("fulfilled");
			const next = buffer.wait(10_050_000 as Micro, signal);
			expect(Bun.peek.status(next)).toBe("pending");

			// The frame past the gap un-stalls the ring, which plays the gap out up to it.
			await state(10_000_000, false, 0);
			expect(Bun.peek.status(next)).toBe("fulfilled");
			expect(Bun.peek.status(buffer.wait(10_090_000 as Micro, signal))).toBe("fulfilled");
			expect(Bun.peek.status(buffer.wait(10_110_000 as Micro, signal))).toBe("pending");
		} finally {
			close();
		}
	});

	// A state message the worklet posted before it applied a reset carries the old timeline's
	// playhead, which is no bound at all on the new one.
	it("ignores state from before a reset", async () => {
		const { buffer, state, close } = open();
		const signal = new AbortController().signal;
		try {
			buffer.reset();
			await state(10_000_000, false, 0);
			expect(Bun.peek.status(buffer.wait(500_000 as Micro, signal))).toBe("fulfilled");
			expect(Bun.peek.status(buffer.wait(600_000 as Micro, signal))).toBe("fulfilled");
			const ahead = buffer.wait(700_000 as Micro, signal);
			await state(10_000_000, false, 0);
			expect(Bun.peek.status(ahead)).toBe("pending");

			await state(600_000, false, 1);
			expect(Bun.peek.status(ahead)).toBe("fulfilled");
		} finally {
			close();
		}
	});
});

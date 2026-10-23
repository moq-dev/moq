import { describe, expect, it } from "bun:test";
import type { Time } from "@moq/net";
import { createAudioBuffer } from "./buffer";
import type { InitShared, State } from "./render";
import { SharedRingBuffer } from "./shared-ring-buffer";

describe("SharedAudioBuffer", () => {
	// The worklet keeps reading the old ring until `init-shared` lands, so a rise that grows the ring
	// has to park that one too, or audio drains ahead of video in between.
	it("parks the ring the worklet still reads when a rise grows it", () => {
		const sent: InitShared[] = [];
		const worklet = { port: { postMessage: (msg: InitShared) => sent.push(msg) } } as unknown as AudioWorkletNode;
		const buffer = createAudioBuffer(worklet, 1, 1000, 100);
		try {
			const old = new SharedRingBuffer(sent[0]);
			buffer.insert(0 as Time.Micro, [new Float32Array(100).fill(1.0)]);
			expect(old.read([new Float32Array(50)])).toBe(50);

			buffer.setLatency(1000);
			expect(sent.length).toBe(2);
			expect(old.stalled).toBe(true);
			expect(old.read([new Float32Array(50)])).toBe(0);
			expect(new SharedRingBuffer(sent[1]).stalled).toBe(true);
		} finally {
			buffer.close();
		}
	});

	// A decode loop can reach `wait` on a buffer a shape change already closed. Nothing advances a
	// closed buffer, so a gated wait would never settle and would hold the decoder effect's rerun.
	it("does not gate a wait on a closed buffer", () => {
		const worklet = { port: { postMessage: () => {} } } as unknown as AudioWorkletNode;
		const buffer = createAudioBuffer(worklet, 1, 1000, 100, true);
		buffer.insert(0 as Time.Micro, [new Float32Array(200)]);
		const signal = new AbortController().signal;

		// Open, a frame this far ahead of the playhead is held.
		expect(Bun.peek.status(buffer.wait(10_000_000 as Time.Micro, signal))).toBe("pending");

		buffer.close();
		expect(Bun.peek.status(buffer.wait(10_000_000 as Time.Micro, signal))).toBe("fulfilled");
	});

	// A decode loop retired mid-wait (a rendition change while the playhead is parked) must not leave
	// its waiter queued, or every such change strands one more for the life of the buffer.
	it("releases a gated wait when its signal aborts", async () => {
		const worklet = { port: { postMessage: () => {} } } as unknown as AudioWorkletNode;
		const buffer = createAudioBuffer(worklet, 1, 1000, 100, true);
		try {
			buffer.insert(0 as Time.Micro, [new Float32Array(200)]);
			const controller = new AbortController();
			const wait = buffer.wait(10_000_000 as Time.Micro, controller.signal);
			await Promise.resolve();
			expect(Bun.peek.status(wait)).toBe("pending");

			controller.abort();
			expect(Bun.peek.status(wait)).toBe("fulfilled");
			expect(Bun.peek.status(buffer.wait(10_000_000 as Time.Micro, controller.signal))).toBe("fulfilled");
		} finally {
			buffer.close();
		}
	});

	// The decoder's output reaches the ring after the decode loop has moved on, so a stalled ring
	// still looks stalled while the loop releases frame after frame. Releasing all of them pushes
	// the whole lookahead into a floor-sized ring, which drops the oldest unplayed audio.
	it("holds the lookahead while a stalled ring refills", () => {
		const sent: InitShared[] = [];
		const worklet = { port: { postMessage: (msg: InitShared) => sent.push(msg) } } as unknown as AudioWorkletNode;
		const buffer = createAudioBuffer(worklet, 1, 1000, 100, true);
		const signal = new AbortController().signal;
		try {
			expect(Bun.peek.status(buffer.wait(0 as Time.Micro, signal))).toBe("fulfilled");
			expect(Bun.peek.status(buffer.wait(100_000 as Time.Micro, signal))).toBe("fulfilled");
			const ahead = buffer.wait(120_000 as Time.Micro, signal);
			expect(Bun.peek.status(ahead)).toBe("pending");

			// The floor lands and plays past the frame's headroom.
			buffer.insert(0 as Time.Micro, [new Float32Array(120)]);
			expect(new SharedRingBuffer(sent[0]).read([new Float32Array(60)])).toBe(60);
			expect(Bun.peek.status(buffer.wait(170_000 as Time.Micro, signal))).toBe("pending");
			expect(Bun.peek.status(ahead)).toBe("fulfilled");
		} finally {
			buffer.close();
		}
	});

	// Decoded audio can land short of the timestamp its frame was released at (Opus pre-skip), so a
	// refill that stops at the floor exactly leaves the ring a sample short of un-stalling, with the
	// gate waiting on the ring.
	it("refills until a frame reaches the floor", () => {
		const worklet = { port: { postMessage: () => {} } } as unknown as AudioWorkletNode;
		const buffer = createAudioBuffer(worklet, 1, 1000, 100, true);
		const signal = new AbortController().signal;
		try {
			buffer.insert(6_000 as Time.Micro, [new Float32Array(74)]);
			expect(Bun.peek.status(buffer.wait(100_000 as Time.Micro, signal))).toBe("fulfilled");
			expect(Bun.peek.status(buffer.wait(120_000 as Time.Micro, signal))).toBe("fulfilled");
			expect(Bun.peek.status(buffer.wait(140_000 as Time.Micro, signal))).toBe("pending");
		} finally {
			buffer.close();
		}
	});
});

describe("PostAudioBuffer", () => {
	// Only the postMessage path exists outside cross-origin isolation.
	function post(): {
		buffer: ReturnType<typeof createAudioBuffer>;
		// An undefined timestamp reports a ring no frame has anchored yet, whose playhead reads 0.
		state: (timestamp: number | undefined, stalled: boolean, epoch: number) => Promise<void>;
		close: () => void;
	} {
		const channel = new MessageChannel();
		const global = globalThis as { crossOriginIsolated?: boolean };
		const previous = global.crossOriginIsolated;
		global.crossOriginIsolated = false;
		try {
			const buffer = createAudioBuffer(
				{ port: channel.port1 } as unknown as AudioWorkletNode,
				1,
				1000,
				100,
				true,
			);
			return {
				buffer,
				// Posts a worklet state report and resolves once the buffer has handled it: this
				// listener is added after the buffer's, so it runs after it for the same message.
				state: (timestamp, stalled, epoch) => {
					const msg: State = {
						type: "state",
						timestamp: (timestamp ?? 0) as Time.Micro,
						stalled,
						anchored: timestamp !== undefined,
						epoch,
					};
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
		} finally {
			global.crossOriginIsolated = previous;
		}
	}

	it("holds the lookahead before the worklet reports", () => {
		const { buffer, close } = post();
		const signal = new AbortController().signal;
		try {
			expect(Bun.peek.status(buffer.wait(0 as Time.Micro, signal))).toBe("fulfilled");
			expect(Bun.peek.status(buffer.wait(100_000 as Time.Micro, signal))).toBe("fulfilled");
			expect(Bun.peek.status(buffer.wait(120_000 as Time.Micro, signal))).toBe("pending");
		} finally {
			close();
		}
	});

	// A state message the worklet posted before it applied a reset carries the old timeline's
	// playhead, which is no bound at all on the new one.
	it("ignores state from before a reset", async () => {
		const { buffer, state, close } = post();
		const signal = new AbortController().signal;
		try {
			buffer.reset();
			await state(10_000_000, false, 0);
			expect(Bun.peek.status(buffer.wait(500_000 as Time.Micro, signal))).toBe("fulfilled");
			expect(Bun.peek.status(buffer.wait(600_000 as Time.Micro, signal))).toBe("fulfilled");
			const ahead = buffer.wait(700_000 as Time.Micro, signal);
			await state(10_000_000, false, 0);
			expect(Bun.peek.status(ahead)).toBe("pending");

			await state(600_000, false, 1);
			expect(Bun.peek.status(ahead)).toBe("fulfilled");
		} finally {
			close();
		}
	});

	// A reset ring reads 0 until a frame anchors it, which is no bound on a timeline far from 0.
	it("ignores the playhead before the ring anchors", async () => {
		const { buffer, state, close } = post();
		const signal = new AbortController().signal;
		try {
			buffer.reset();
			await state(undefined, true, 1);
			expect(Bun.peek.status(buffer.wait(10_000_000 as Time.Micro, signal))).toBe("fulfilled");
			expect(Bun.peek.status(buffer.wait(10_050_000 as Time.Micro, signal))).toBe("fulfilled");
			expect(Bun.peek.status(buffer.wait(10_100_000 as Time.Micro, signal))).toBe("fulfilled");
			expect(Bun.peek.status(buffer.wait(10_120_000 as Time.Micro, signal))).toBe("pending");
		} finally {
			close();
		}
	});
});

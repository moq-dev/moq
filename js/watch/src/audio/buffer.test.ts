import { describe, expect, it } from "bun:test";
import { Micro, Milli } from "@moq/clock";
import { Backpressure, createAudioBuffer } from "./buffer";
import type { InitShared, Message, State } from "./render";
import { SharedRingBuffer } from "./shared-ring-buffer";

const HOUR = Milli(60 * 60 * 1000);
const ms = (value: number) => Micro.fromMilli(Milli(value));

/** A worklet that records what the buffer posts to it. */
function worklet(): { node: AudioWorkletNode; sent: InitShared[] } {
	const sent: InitShared[] = [];
	const node = { port: { postMessage: (msg: InitShared) => sent.push(msg) } } as unknown as AudioWorkletNode;
	return { node, sent };
}

describe("Backpressure", () => {
	it("releases every live frame when the whole target fits the floor", () => {
		const gate = new Backpressure(1000, false, Milli(100));
		gate.observe(ms(0), false);
		expect(Bun.peek.status(gate.wait(ms(10_000), new AbortController().signal))).toBe("fulfilled");
	});

	it("holds buffered playback to the floor ahead of the playhead", () => {
		const gate = new Backpressure(1000, true, Milli(200));
		gate.observe(ms(0), false);
		const wait = gate.wait(ms(1000), new AbortController().signal);
		gate.observe(ms(799), false);
		expect(Bun.peek.status(wait)).toBe("pending");
		gate.observe(ms(800), false);
		expect(Bun.peek.status(wait)).toBe("fulfilled");
	});

	// Two seconds is a 500 ms floor and 1.5 s held encoded.
	it("releases a live frame once the queued media covers the hold behind it", () => {
		const gate = new Backpressure(1000, false, Milli(2000));
		gate.observe(ms(0), false);
		const wait = gate.wait(ms(1000), new AbortController().signal);
		gate.queued(ms(2499));
		expect(Bun.peek.status(wait)).toBe("pending");
		gate.queued(ms(2500));
		expect(Bun.peek.status(wait)).toBe("fulfilled");
	});

	it("plays buffered media through instead of releasing it ahead of the floor", () => {
		const gate = new Backpressure(1000, true, Milli(2000));
		gate.observe(ms(0), false);
		const wait = gate.wait(ms(1000), new AbortController().signal);
		gate.queued(ms(60_000));
		expect(Bun.peek.status(wait)).toBe("pending");
		gate.observe(ms(500), false);
		expect(Bun.peek.status(wait)).toBe("fulfilled");
	});

	it("starts a stalled ring only once the queued media covers the hold", () => {
		const gate = new Backpressure(1000, true, Milli(2000));
		const wait = gate.wait(ms(0), new AbortController().signal);
		gate.queued(ms(1499));
		expect(Bun.peek.status(wait)).toBe("pending");
		gate.queued(ms(1500));
		expect(Bun.peek.status(wait)).toBe("fulfilled");
	});

	// An utterance that arrived whole before the reset changes nothing queued, so nothing would
	// report it again: forgetting it would hold its first frame until the next utterance.
	it("keeps what is queued across a reset", () => {
		const gate = new Backpressure(1000, true, Milli(2000));
		gate.queued(ms(5000));
		gate.observe(ms(0), false);
		gate.reset();
		expect(Bun.peek.status(gate.wait(ms(0), new AbortController().signal))).toBe("fulfilled");
	});
});

describe("SharedAudioBuffer", () => {
	// A catalog advertising an hour of jitter once sized the ring to two hours of stereo PCM, 4 GiB.
	it("allocates the same ring for an hour of delay as for a tenth of a second", () => {
		const bytes = [Milli(100), HOUR].map((delay) => {
			const { node, sent } = worklet();
			createAudioBuffer(node, 2, 48_000, delay).close();
			return sent[0].samples.byteLength;
		});
		expect(bytes[1]).toBe(bytes[0]);
		expect(bytes[1]).toBeLessThanOrEqual(2 ** 20);
	});

	it("holds a frame encoded until an hour of queued media is behind it", () => {
		const { node } = worklet();
		const buffer = createAudioBuffer(node, 1, 1000, HOUR);
		try {
			// The ring holds 500 ms decoded; the rest of the hour waits upstream.
			const hold = HOUR - 500;
			const wait = buffer.wait(ms(0), new AbortController().signal);
			buffer.queued(ms(hold - 1));
			expect(Bun.peek.status(wait)).toBe("pending");
			buffer.queued(ms(hold));
			expect(Bun.peek.status(wait)).toBe("fulfilled");
		} finally {
			buffer.close();
		}
	});

	// One second is a 500 ms floor and 500 ms held encoded. Playing 100 ms in, a rise to 1.5 s needs
	// queued media through 1600 ms to keep playing.
	for (const [queued, parked] of [
		[1599, true],
		[1600, false],
	] as const) {
		it(`${parked ? "parks" : "keeps playing"} on a rise with ${queued} ms queued`, () => {
			const { node, sent } = worklet();
			const buffer = createAudioBuffer(node, 1, 1000, Milli(1000));
			try {
				const reader = new SharedRingBuffer(sent[0]);
				buffer.insert(ms(0), [new Float32Array(600).fill(1)]);
				expect(reader.read([new Float32Array(100)])).toBe(100);
				buffer.queued(ms(queued));

				buffer.setLatency(Milli(1500));
				expect(reader.stalled).toBe(parked);
				expect(sent.length).toBe(1);
			} finally {
				buffer.close();
			}
		});
	}

	// The postMessage ring learns the playhead from the worklet's state messages, and parks the same way.
	for (const [queued, parked] of [
		[1599, true],
		[1600, false],
	] as const) {
		it(`posts ${parked ? "a park" : "no park"} on a rise with ${queued} ms queued`, async () => {
			const global = globalThis as { crossOriginIsolated?: boolean };
			global.crossOriginIsolated = false;
			const channel = new MessageChannel();
			const sent: Message[] = [];
			channel.port2.onmessage = (event: MessageEvent<Message>) => sent.push(event.data);
			const buffer = createAudioBuffer(
				{ port: channel.port1 } as unknown as AudioWorkletNode,
				1,
				1000,
				Milli(1000),
			);
			try {
				const state: State = { type: "state", timestamp: ms(100), stalled: false, underruns: 0 };
				channel.port2.postMessage(state);
				await new Promise((resolve) => setTimeout(resolve, 0));
				buffer.queued(ms(queued));

				buffer.setLatency(Milli(1500));
				await new Promise((resolve) => setTimeout(resolve, 0));
				expect(sent.map((msg) => msg.type)).toEqual(
					parked ? ["init-post", "latency", "park"] : ["init-post", "latency"],
				);
			} finally {
				buffer.close();
				channel.port1.close();
				delete global.crossOriginIsolated;
			}
		});
	}

	// A decode loop can reach `wait` on a buffer a shape change already closed. Nothing advances a
	// closed buffer, so a gated wait would never settle and would hold the decoder effect's rerun.
	it("does not gate a wait on a closed buffer", async () => {
		const { node } = worklet();
		const buffer = createAudioBuffer(node, 1, 1000, Milli(100), true);
		buffer.insert(0 as Micro, [new Float32Array(200)]);
		const settles = (wait: Promise<void>) =>
			Promise.race([
				wait.then(() => true),
				new Promise<boolean>((resolve) => setTimeout(() => resolve(false), 0)),
			]);

		// Open, a frame this far ahead of the playhead is held.
		expect(await settles(buffer.wait(10_000_000 as Micro, new AbortController().signal))).toBe(false);

		buffer.close();
		expect(await settles(buffer.wait(10_000_000 as Micro, new AbortController().signal))).toBe(true);
	});

	// A decode loop retired mid-wait (a rendition change while the playhead is parked) must not leave
	// its waiter queued, or every such change strands one more for the life of the buffer.
	it("releases a gated wait when its signal aborts", async () => {
		const { node } = worklet();
		const buffer = createAudioBuffer(node, 1, 1000, Milli(100), true);
		try {
			buffer.insert(0 as Micro, [new Float32Array(200)]);
			const controller = new AbortController();
			const wait = buffer.wait(10_000_000 as Micro, controller.signal);
			await Promise.resolve();
			expect(Bun.peek.status(wait)).toBe("pending");

			controller.abort();
			expect(Bun.peek.status(wait)).toBe("fulfilled");
			expect(Bun.peek.status(buffer.wait(10_000_000 as Micro, controller.signal))).toBe("fulfilled");
		} finally {
			buffer.close();
		}
	});
});

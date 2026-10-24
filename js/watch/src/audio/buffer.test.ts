import { describe, expect, it } from "bun:test";
import { Micro, Milli } from "@moq/clock";
import { AudioBuffer, Backpressure } from "./buffer";
import type { Init, Message, State } from "./render";
import { AudioRingBuffer } from "./ring-buffer";

const HOUR = Milli(60 * 60 * 1000);
const ms = (value: number) => Micro.fromMilli(Milli(value));

// A buffered buffer over a real port, with the worklet's end of it left to the test.
function open(): {
	buffer: AudioBuffer;
	// An undefined timestamp reports a ring no frame has anchored yet, whose playhead reads 0.
	state: (timestamp: number | undefined, stalled: boolean, epoch: number) => Promise<void>;
	close: () => void;
} {
	const channel = new MessageChannel();
	const buffer = new AudioBuffer({ port: channel.port1 } as unknown as AudioWorkletNode, {
		channels: 1,
		rate: 48_000,
		delay: Milli(100),
		buffered: true,
	});
	return {
		buffer,
		// Posts a worklet state report and resolves once the buffer has handled it: this
		// listener is added after the buffer's, so it runs after it for the same message.
		state: (timestamp, stalled, epoch) => {
			const msg: State = {
				type: "state",
				timestamp: (timestamp ?? 0) as Micro,
				stalled,
				anchored: timestamp !== undefined,
				underruns: 0,
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
}

describe("AudioBuffer", () => {
	it("transfers PCM ownership and orders control messages after the data they change", async () => {
		const channel = new MessageChannel();
		const buffer = new AudioBuffer({ port: channel.port1 } as unknown as AudioWorkletNode, {
			channels: 2,
			rate: 48_000,
			delay: Milli(100),
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
			buffer.setLatency(Milli(200));
			buffer.truncate(502_000 as Micro);
			buffer.reset();
			await received;
			expect(messages).toEqual([
				{ type: "init", channels: 2, rate: 48_000, latency: Milli(100), buffered: true },
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

	// Media timestamps are absolute microseconds, so they pass 2^31 after ~36 minutes of broadcast
	// and 2^32 after ~72. The gate has to hold a frame ahead of the playhead at any of them.
	it("holds a frame one second ahead at any timestamp", async () => {
		for (const base of [0, 2 ** 31, 2 ** 32, 3_600_000_000]) {
			const { buffer, state, close } = open();
			try {
				await state(base, false, 0);
				const wait = buffer.wait((base + 1_000_000) as Micro, new AbortController().signal);
				expect(Bun.peek.status(wait)).toBe("pending");

				// Still more than the floor behind the frame.
				await state(base + 800_000, false, 0);
				expect(Bun.peek.status(wait)).toBe("pending");

				// Within the floor of the frame.
				await state(base + 900_000, false, 0);
				expect(Bun.peek.status(wait)).toBe("fulfilled");
			} finally {
				close();
			}
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

	// A reset ring reads 0 until a frame anchors it, which is no bound on a timeline far from 0.
	it("ignores the playhead before the ring anchors", async () => {
		const { buffer, state, close } = open();
		const signal = new AbortController().signal;
		try {
			buffer.reset();
			await state(undefined, true, 1);
			expect(Bun.peek.status(buffer.wait(10_000_000 as Micro, signal))).toBe("fulfilled");
			expect(Bun.peek.status(buffer.wait(10_050_000 as Micro, signal))).toBe("fulfilled");
			expect(Bun.peek.status(buffer.wait(10_100_000 as Micro, signal))).toBe("fulfilled");
			expect(Bun.peek.status(buffer.wait(10_120_000 as Micro, signal))).toBe("pending");
		} finally {
			close();
		}
	});
});

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
		gate.received(ms(2499));
		expect(Bun.peek.status(wait)).toBe("pending");
		gate.received(ms(2500));
		expect(Bun.peek.status(wait)).toBe("fulfilled");
	});

	it("plays buffered media through instead of releasing it ahead of the floor", () => {
		const gate = new Backpressure(1000, true, Milli(2000));
		gate.observe(ms(0), false);
		const wait = gate.wait(ms(1000), new AbortController().signal);
		gate.received(ms(60_000));
		expect(Bun.peek.status(wait)).toBe("pending");
		gate.observe(ms(500), false);
		expect(Bun.peek.status(wait)).toBe("fulfilled");
	});

	it("starts a stalled ring only once the queued media covers the hold", () => {
		const gate = new Backpressure(1000, true, Milli(2000));
		const wait = gate.wait(ms(0), new AbortController().signal);
		gate.received(ms(1499));
		expect(Bun.peek.status(wait)).toBe("pending");
		gate.received(ms(1500));
		expect(Bun.peek.status(wait)).toBe("fulfilled");
	});

	// The ring un-stalls only once the decoder's output reaches it, so covered media released
	// outright would overflow the fixed ring before the worklet reports it playing.
	it("refills a covered buffered ring only to its floor", () => {
		const gate = new Backpressure(1000, true, Milli(2000));
		gate.received(ms(60_000));
		const signal = new AbortController().signal;
		expect(Bun.peek.status(gate.wait(ms(0), signal))).toBe("fulfilled");
		expect(Bun.peek.status(gate.wait(ms(500), signal))).toBe("fulfilled");
		expect(Bun.peek.status(gate.wait(ms(520), signal))).toBe("pending");
	});

	// A park holds the ring until its next write, so it starts a fresh refill: the frames a refill
	// released before the ring reported playing must not hold back the write that resumes it.
	it("releases the frame that resumes a buffered ring parked after a refill", () => {
		const gate = new Backpressure(1000, true, Milli(1000));
		gate.received(ms(1000));
		const signal = new AbortController().signal;
		for (let t = 0; t <= 500; t += 20) expect(Bun.peek.status(gate.wait(ms(t), signal))).toBe("fulfilled");
		gate.observe(ms(0), true);
		expect(gate.setLatency(Milli(2000))).toBe(true);
		const wait = gate.wait(ms(520), signal);
		gate.received(ms(2019));
		expect(Bun.peek.status(wait)).toBe("pending");
		gate.received(ms(2020));
		expect(Bun.peek.status(wait)).toBe("fulfilled");
	});

	// An utterance that arrived whole before the reset changes nothing queued, so nothing would
	// report it again: forgetting it would hold its first frame until the next utterance.
	it("keeps what is queued across a reset", () => {
		const gate = new Backpressure(1000, true, Milli(2000));
		gate.received(ms(5000));
		gate.observe(ms(0), false);
		gate.reset();
		expect(Bun.peek.status(gate.wait(ms(0), new AbortController().signal))).toBe("fulfilled");
	});
});

// A worklet whose port records what the buffer posts and delivers state reports synchronously.
function fake(delay: Milli, buffered = false) {
	const sent: Message[] = [];
	const port = Object.assign(new EventTarget(), {
		postMessage: (msg: Message) => sent.push(msg),
		start() {},
	});
	const buffer = new AudioBuffer({ port } as unknown as AudioWorkletNode, {
		channels: 1,
		rate: 1000,
		delay,
		buffered,
	});
	const state = (timestamp: Micro, stalled: boolean, epoch: number) => {
		const data: State = { type: "state", timestamp, stalled, anchored: true, underruns: 0, epoch };
		port.dispatchEvent(new MessageEvent("message", { data }));
	};
	return { buffer, sent, state };
}

describe("AudioBuffer past its decoded floor", () => {
	// A catalog advertising an hour of jitter once sized the ring to two hours of stereo PCM, 4 GiB.
	it("allocates the same ring for an hour of delay as for a tenth of a second", () => {
		const bytes = [Milli(100), HOUR].map((delay) => {
			const sent: Message[] = [];
			const port = Object.assign(new EventTarget(), {
				postMessage: (msg: Message) => sent.push(msg),
				start() {},
			});
			new AudioBuffer({ port } as unknown as AudioWorkletNode, {
				channels: 2,
				rate: 48_000,
				delay,
				buffered: false,
			}).close();
			const ring = new AudioRingBuffer(sent[0] as Init);
			return ring.capacity * ring.channels * Float32Array.BYTES_PER_ELEMENT;
		});
		expect(bytes[1]).toBe(bytes[0]);
		expect(bytes[1]).toBeLessThanOrEqual(2 ** 20);
	});

	it("holds a frame encoded until an hour of queued media is behind it", () => {
		const { buffer } = fake(HOUR);
		try {
			// The ring holds 500 ms decoded; the rest of the hour waits upstream.
			const hold = HOUR - 500;
			const wait = buffer.wait(ms(0), new AbortController().signal);
			buffer.received(ms(hold - 1));
			expect(Bun.peek.status(wait)).toBe("pending");
			buffer.received(ms(hold));
			expect(Bun.peek.status(wait)).toBe("fulfilled");
		} finally {
			buffer.close();
		}
	});

	// One second is a 500 ms floor and 500 ms held encoded. Playing 100 ms in, a rise to 1.5 s needs
	// queued media through 1600 ms to keep playing. A report of a stall may be stale, so it parks
	// regardless: parking a ring that is still stalled changes nothing.
	for (const stalled of [false, true]) {
		for (const [queued, parked] of [
			[1599, true],
			[1600, false],
		] as const) {
			it(`${parked ? "parks" : "keeps playing"} on a rise with ${queued} ms queued, reported ${stalled ? "stalled" : "playing"}`, () => {
				const { buffer, sent, state } = fake(Milli(1000));
				try {
					state(ms(100), stalled, 0);
					buffer.received(ms(queued));
					buffer.setLatency(Milli(1500));
					expect(sent.slice(1)).toEqual([
						{ type: "latency", latency: Milli(500), ...(parked ? { park: 1 } : {}) },
					]);
				} finally {
					buffer.close();
				}
			});
		}
	}

	// The ring resumes on any floor update without a park, so a fall that leaves the remaining hold
	// uncovered parks again, and one the queued media covers lets the ring resume.
	it("keeps a park across a fall until the queued media covers the hold", () => {
		const { buffer, sent, state } = fake(Milli(1000));
		try {
			state(ms(0), false, 0);
			buffer.received(ms(1000));
			buffer.setLatency(Milli(2000));
			buffer.setLatency(Milli(1800));
			buffer.received(ms(1800));
			buffer.setLatency(Milli(1600));
			expect(sent.slice(1)).toEqual([
				{ type: "latency", latency: Milli(500), park: 1 },
				{ type: "latency", latency: Milli(500), park: 2 },
				{ type: "latency", latency: Milli(500) },
			]);
		} finally {
			buffer.close();
		}
	});

	// A report the worklet posted before it applied the park still says the ring is playing.
	it("ignores state from before a park", () => {
		const { buffer, state } = fake(Milli(1000));
		try {
			state(ms(200), false, 0);
			buffer.received(ms(1599));
			buffer.setLatency(Milli(1500));
			const wait = buffer.wait(ms(700), new AbortController().signal);
			state(ms(200), false, 0);
			expect(Bun.peek.status(wait)).toBe("pending");
			state(ms(200), false, 1);
			expect(Bun.peek.status(wait)).toBe("fulfilled");
		} finally {
			buffer.close();
		}
	});
});

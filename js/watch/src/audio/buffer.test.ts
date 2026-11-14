import { describe, expect, it } from "bun:test";
import { Duration, Instant, Manual, now } from "@moq/clock";
import { type Getter, Signal } from "@moq/signals";
import { AudioBuffer, Backpressure } from "./buffer";
import type { Init, Message, State } from "./render";
import { AudioRingBuffer } from "./ring-buffer";

const HOUR = 60 * 60 * 1000;

// A manual clock moved past the page's origin, where a performance time of zero means none.
function started(): { close(): void } {
	const manual = new Manual();
	const installed = manual.install();
	manual.jump(Duration.secs(1));
	return installed;
}

// A context that reports its output position as sounding `sounding` now, or nothing until it is set.
function silent(): {
	currentTime: number;
	baseLatency: number;
	outputLatency: number;
	sounding?: number;
	getOutputTimestamp(): AudioTimestamp;
} {
	return {
		currentTime: 0,
		baseLatency: 0,
		outputLatency: 0,
		getOutputTimestamp() {
			if (this.sounding === undefined) return { contextTime: 0, performanceTime: 0 };
			return { contextTime: this.sounding, performanceTime: now().since(Instant.ZERO).asMillis() };
		},
	};
}
const ms = (value: number) => value * 1000;

// A buffered buffer over a real port, with the worklet's end of it left to the test.
function open(): {
	buffer: AudioBuffer;
	// An undefined timestamp reports a ring no frame has anchored yet, whose playhead reads 0.
	state: (timestamp: number | undefined, stalled: boolean, epoch: number) => Promise<void>;
	close: () => void;
} {
	const channel = new MessageChannel();
	const buffer = new AudioBuffer({ port: channel.port1, context: silent() } as unknown as AudioWorkletNode, {
		channels: 1,
		rate: 48_000,
		delay: Duration.millis(100),
		buffered: true,
	});
	return {
		buffer,
		// Posts a worklet state report and resolves once the buffer has handled it: this
		// listener is added after the buffer's, so it runs after it for the same message.
		state: (timestamp, stalled, epoch) => {
			const msg: State = {
				type: "state",
				timestamp: timestamp ?? 0,
				time: 0,
				stalled,
				anchored: timestamp !== undefined,
				underruns: 0,
				skips: 0,
				trimmed: 0,
				accelerated: 0,
				silent: false,
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
		const buffer = new AudioBuffer({ port: channel.port1, context: silent() } as unknown as AudioWorkletNode, {
			channels: 2,
			rate: 48_000,
			delay: Duration.millis(100),
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
			buffer.insert(500_000, [left, right]);
			expect(left.byteLength).toBe(0);
			expect(right.byteLength).toBe(0);
			buffer.setLatency(Duration.millis(200));
			buffer.truncate(502_000);
			buffer.reset();
			await received;
			expect(messages).toEqual([
				{ type: "init", channels: 2, rate: 48_000, latency: 100, target: 100, bound: 0 },
				{
					type: "data",
					timestamp: 500_000,
					data: [new Float32Array([1, 2, 3]), new Float32Array([4, 5, 6])],
				},
				{ type: "latency", latency: 200, target: 200, bound: 0 },
				{ type: "truncate", timestamp: 502_000 },
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
			expect(Bun.peek.status(buffer.wait(0, signal))).toBe("fulfilled");
			expect(Bun.peek.status(buffer.wait(100_000, signal))).toBe("fulfilled");
			expect(Bun.peek.status(buffer.wait(120_000, signal))).toBe("pending");
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
			buffer.insert(100_000, [new Float32Array(9600)]);
			const held = buffer.wait(10_000_000, new AbortController().signal);
			expect(Bun.peek.status(held)).toBe("pending");
			buffer.close();
			expect(Bun.peek.status(held)).toBe("fulfilled");
			await state(5_000_000, false, 0);
			expect(buffer.timestamp.peek()).toBe(100_000);
			expect(Bun.peek.status(buffer.wait(10_000_000, new AbortController().signal))).toBe("fulfilled");
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
			buffer.insert(0, [new Float32Array(9600)]);
			const controller = new AbortController();
			const wait = buffer.wait(10_000_000, controller.signal);
			await Promise.resolve();
			expect(Bun.peek.status(wait)).toBe("pending");

			controller.abort();
			expect(Bun.peek.status(wait)).toBe("fulfilled");
			expect(Bun.peek.status(buffer.wait(10_000_000, controller.signal))).toBe("fulfilled");
		} finally {
			close();
		}
	});

	// Media timestamps are absolute microseconds, so they pass 2^31 after ~36 minutes of broadcast
	// and 2^32 after ~72. The gate has to hold a playing ring to its floor and a window at any of them.
	it("holds a playing ring to its floor and a stretch window at any timestamp", async () => {
		for (const base of [0, 2 ** 31, 2 ** 32, 3_600_000_000]) {
			const { buffer, state, close } = open();
			try {
				await state(base, false, 0);
				buffer.insert(base, [new Float32Array(9600)]);
				const wait = buffer.wait(base + 200_000, new AbortController().signal);
				expect(Bun.peek.status(wait)).toBe("pending");

				// The ring still holds the 100 ms floor and a 30 ms window.
				await state(base + 70_000, false, 0);
				expect(Bun.peek.status(wait)).toBe("pending");

				await state(base + 80_000, false, 0);
				expect(Bun.peek.status(wait)).toBe("fulfilled");
			} finally {
				close();
			}
		}
	});

	// The decoder's output reaches the ring after the decode loop has moved on, so the frames still
	// decoding count toward what the ring holds. Counting only the PCM that landed would push the
	// whole lookahead into a floor-sized ring, which drops the oldest unplayed audio.
	it("counts the frames still decoding toward the floor and window", async () => {
		const { buffer, state, close } = open();
		const signal = new AbortController().signal;
		try {
			await state(0, false, 0);
			buffer.insert(0, [new Float32Array(960)]);
			for (let t = 20_000; t <= 120_000; t += 20_000)
				expect(Bun.peek.status(buffer.wait(t, signal))).toBe("fulfilled");
			const ahead = buffer.wait(140_000, signal);
			expect(Bun.peek.status(ahead)).toBe("pending");

			await state(20_000, false, 0);
			expect(Bun.peek.status(ahead)).toBe("fulfilled");
		} finally {
			close();
		}
	});

	// Decoded audio can land short of the timestamp its frame was released at (Opus pre-skip), so the
	// refill counts the PCM that reaches the ring: stopping at the released frames would leave the
	// ring short of un-stalling, with the gate waiting on it.
	it("refills until the ring holds the floor", () => {
		const { buffer, close } = open();
		const signal = new AbortController().signal;
		try {
			for (let t = 0; t <= 100_000; t += 20_000) {
				expect(Bun.peek.status(buffer.wait(t, signal))).toBe("fulfilled");
				// The first output lost 6 ms to pre-skip.
				const start = Math.max(t, 6_000);
				buffer.insert(start, [new Float32Array(((t + 20_000 - start) * 48) / 1000)]);
				buffer.decoded(t + 20_000);
			}
			expect(Bun.peek.status(buffer.wait(120_000, signal))).toBe("pending");
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
			expect(Bun.peek.status(buffer.wait(10_000_000, signal))).toBe("fulfilled");
			buffer.insert(10_000_000, [new Float32Array(960)]);
			buffer.decoded(10_020_000);
			const next = buffer.wait(10_020_000, signal);
			expect(Bun.peek.status(next)).toBe("pending");

			// The frame past the gap un-stalls the ring, which plays the gap out up to it and refills to
			// its floor and a window.
			await state(10_000_000, false, 0);
			expect(Bun.peek.status(next)).toBe("fulfilled");
			for (let t = 10_040_000; t <= 10_120_000; t += 20_000)
				expect(Bun.peek.status(buffer.wait(t, signal))).toBe("fulfilled");
			expect(Bun.peek.status(buffer.wait(10_140_000, signal))).toBe("pending");
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
			expect(Bun.peek.status(buffer.wait(500_000, signal))).toBe("fulfilled");
			expect(Bun.peek.status(buffer.wait(600_000, signal))).toBe("fulfilled");
			const ahead = buffer.wait(700_000, signal);
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
			expect(Bun.peek.status(buffer.wait(10_000_000, signal))).toBe("fulfilled");
			expect(Bun.peek.status(buffer.wait(10_050_000, signal))).toBe("fulfilled");
			expect(Bun.peek.status(buffer.wait(10_100_000, signal))).toBe("fulfilled");
			expect(Bun.peek.status(buffer.wait(10_120_000, signal))).toBe("pending");
		} finally {
			close();
		}
	});
});

describe("Backpressure", () => {
	// Two seconds is a 500 ms floor and 1.5 s held encoded. Once playing, whatever is queued, the ring
	// holds its floor and a 30 ms window: the rest waits upstream.
	for (const buffered of [false, true]) {
		it(`holds a playing ${buffered ? "buffered" : "live"} ring to its floor and a window`, () => {
			const gate = new Backpressure(1000, buffered, Duration.millis(2000));
			gate.queued(ms(60_000));
			gate.observe(ms(0), false);
			for (let t = 0; t < 1000; t += 20) gate.inserted(ms(t), ms(t + 20));
			const wait = gate.wait(ms(1000), new AbortController().signal);
			gate.observe(ms(470), false);
			expect(Bun.peek.status(wait)).toBe("pending");
			gate.observe(ms(471), false);
			expect(Bun.peek.status(wait)).toBe("fulfilled");
			expect(Bun.peek.status(gate.wait(ms(1020), new AbortController().signal))).toBe("pending");
		});
	}

	// The next frame past a hole lies far ahead of the playhead, which never nears it while the ring
	// drains, so it is released by what the ring holds and the hole plays as silence.
	it("releases the frame past a hole once the ring runs short", () => {
		const gate = new Backpressure(1000, false, Duration.millis(200));
		gate.observe(ms(0), false);
		gate.inserted(ms(0), ms(100));
		const signal = new AbortController().signal;
		expect(Bun.peek.status(gate.wait(ms(10_000), signal))).toBe("fulfilled");
		expect(Bun.peek.status(gate.wait(ms(10_100), signal))).toBe("pending");
	});

	it("starts a live ring once the queued media covers the hold behind its first frame", () => {
		const gate = new Backpressure(1000, false, Duration.millis(2000));
		const wait = gate.wait(ms(1000), new AbortController().signal);
		gate.queued(ms(2499));
		expect(Bun.peek.status(wait)).toBe("pending");
		gate.queued(ms(2500));
		expect(Bun.peek.status(wait)).toBe("fulfilled");
	});

	it("starts a stalled ring only once the queued media covers the hold", () => {
		const gate = new Backpressure(1000, true, Duration.millis(2000));
		const wait = gate.wait(ms(0), new AbortController().signal);
		gate.queued(ms(1499));
		expect(Bun.peek.status(wait)).toBe("pending");
		gate.queued(ms(1500));
		expect(Bun.peek.status(wait)).toBe("fulfilled");
	});

	// The ring un-stalls only once the decoder's output reaches it, so covered media released
	// outright would overflow the fixed ring before the worklet reports it playing.
	it("refills a covered buffered ring only to its floor", () => {
		const gate = new Backpressure(1000, true, Duration.millis(2000));
		gate.queued(ms(60_000));
		const signal = new AbortController().signal;
		expect(Bun.peek.status(gate.wait(ms(0), signal))).toBe("fulfilled");
		expect(Bun.peek.status(gate.wait(ms(500), signal))).toBe("fulfilled");
		expect(Bun.peek.status(gate.wait(ms(520), signal))).toBe("pending");
	});

	// A warmup longer than the floor discards everything the refill releases toward it, so the ring
	// would never anchor and report the playhead the gate waits on.
	it("refills past output discarded before the ring anchors", () => {
		const gate = new Backpressure(1000, true, Duration.millis(2000));
		gate.queued(ms(60_000));
		const signal = new AbortController().signal;
		for (let t = 0; t <= 500; t += 20) expect(Bun.peek.status(gate.wait(ms(t), signal))).toBe("fulfilled");
		const wait = gate.wait(ms(520), signal);
		expect(Bun.peek.status(wait)).toBe("pending");
		for (let t = 0; t <= 500; t += 20) gate.decoded(ms(t + 20));
		expect(Bun.peek.status(wait)).toBe("fulfilled");
		expect(Bun.peek.status(gate.wait(ms(1020), signal))).toBe("fulfilled");
		expect(Bun.peek.status(gate.wait(ms(1040), signal))).toBe("pending");
	});

	// A replacement rendition keeps the ring and its playhead, and its warmup can outlast the floor.
	it("refills a stalled ring past output discarded behind its playhead", () => {
		const gate = new Backpressure(1000, true, Duration.millis(2000));
		gate.queued(ms(60_000));
		gate.observe(ms(1000), true);
		const signal = new AbortController().signal;
		for (let t = 1000; t <= 1500; t += 20) expect(Bun.peek.status(gate.wait(ms(t), signal))).toBe("fulfilled");
		const wait = gate.wait(ms(1520), signal);
		expect(Bun.peek.status(wait)).toBe("pending");
		// The decoder may answer several frames with one output.
		gate.decoded(ms(1500));
		gate.decoded(ms(1520));
		expect(Bun.peek.status(wait)).toBe("fulfilled");
		expect(Bun.peek.status(gate.wait(ms(2020), signal))).toBe("fulfilled");
		expect(Bun.peek.status(gate.wait(ms(2040), signal))).toBe("pending");
	});

	// Opus pre-skip stamps the PCM behind the input it was decoded from, so a playing ring counts a
	// frame still decoding where its output lands, or a pre-skip past the floor would drain it.
	it("counts a frame still decoding where its output lands", () => {
		const gate = new Backpressure(1000, true, Duration.millis(2000));
		gate.observe(ms(0), false);
		gate.decoded(ms(520), ms(500));
		gate.inserted(ms(480), ms(500));
		const signal = new AbortController().signal;
		expect(Bun.peek.status(gate.wait(ms(520), signal))).toBe("fulfilled");
		// Its output lands at 520 ms, short of the 530 ms floor and window, not at 540 ms.
		expect(Bun.peek.status(gate.wait(ms(540), signal))).toBe("fulfilled");
		expect(Bun.peek.status(gate.wait(ms(560), signal))).toBe("pending");
	});

	// A replacement rendition keeps the ring, and the decoder it replaced never answers its frames.
	it("refills past frames a closed decoder dropped", () => {
		const gate = new Backpressure(1000, true, Duration.millis(2000));
		gate.queued(ms(60_000));
		const signal = new AbortController().signal;
		for (let t = 0; t <= 500; t += 20) expect(Bun.peek.status(gate.wait(ms(t), signal))).toBe("fulfilled");
		const wait = gate.wait(ms(520), signal);
		expect(Bun.peek.status(wait)).toBe("pending");
		gate.dropped();
		expect(Bun.peek.status(wait)).toBe("fulfilled");
	});

	// An utterance that arrived whole before the reset changes nothing queued, so nothing would
	// report it again: forgetting it would hold its first frame until the next utterance.
	it("keeps what is queued across a reset", () => {
		const gate = new Backpressure(1000, true, Duration.millis(2000));
		gate.queued(ms(5000));
		gate.observe(ms(0), false);
		gate.reset();
		expect(Bun.peek.status(gate.wait(ms(0), new AbortController().signal))).toBe("fulfilled");
	});
});

// A worklet whose port records what the buffer posts and delivers state reports synchronously.
function fake(
	delay: number,
	props: {
		bound?: number;
		clock?: { position(): number | undefined; out: { reference: Getter<number | undefined> } };
	} = {},
) {
	const sent: Message[] = [];
	const port = Object.assign(new EventTarget(), {
		postMessage: (msg: Message) => sent.push(msg),
		start() {},
	});
	// The context's time in seconds, which the test advances.
	const context = silent();
	const buffer = new AudioBuffer({ port, context } as unknown as AudioWorkletNode, {
		channels: 1,
		rate: 1000,
		delay: Duration.millis(delay),
		buffered: false,
		...(props.bound !== undefined && { bound: Duration.millis(props.bound) }),
		clock: props.clock,
	});
	const state = (timestamp: number, stalled: boolean, epoch: number, time = 0) => {
		const data: State = {
			type: "state",
			timestamp,
			time,
			stalled,
			anchored: true,
			underruns: 0,
			skips: 0,
			trimmed: 0,
			accelerated: 0,
			silent: false,
			epoch,
		};
		port.dispatchEvent(new MessageEvent("message", { data }));
	};
	return { buffer, sent, state, context };
}

describe("AudioBuffer past its decoded floor", () => {
	// A catalog advertising an hour of jitter once sized the ring to two hours of stereo PCM, 4 GiB.
	it("allocates the same ring for an hour of delay as for a tenth of a second", () => {
		const bytes = [100, HOUR].map((delay) => {
			const sent: Message[] = [];
			const port = Object.assign(new EventTarget(), {
				postMessage: (msg: Message) => sent.push(msg),
				start() {},
			});
			new AudioBuffer({ port, context: silent() } as unknown as AudioWorkletNode, {
				channels: 2,
				rate: 48_000,
				delay: Duration.millis(delay),
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
			buffer.queued(ms(hold - 1));
			expect(Bun.peek.status(wait)).toBe("pending");
			buffer.queued(ms(hold));
			expect(Bun.peek.status(wait)).toBe("fulfilled");
		} finally {
			buffer.close();
		}
	});
});

describe("AudioBuffer steering", () => {
	// A moved delay moves the clock, which the ring plays onto by stretching, so every move only sends
	// the new terms: nothing parks on a rise or skips on a fall.
	it("sends the terms on every move of the delay", () => {
		const { buffer, sent, state } = fake(1000, { bound: 1000 });
		try {
			state(ms(100), false, 0);
			buffer.setLatency(Duration.millis(1500));
			buffer.setLatency(Duration.millis(200), Duration.millis(1800));
			expect(sent).toEqual([
				{ type: "init", channels: 1, rate: 1000, latency: 500, target: 1000, bound: 1000 },
				{ type: "latency", latency: 500, target: 1500, bound: 1000 },
				{ type: "latency", latency: 200, target: 200, bound: 1800 },
			]);
		} finally {
			buffer.close();
		}
	});

	// Pre-skip lands output behind the frame it was decoded from, so what arrived is placed where it
	// will land in the ring.
	it("places received media on the ring's timeline", () => {
		const { buffer, sent } = fake(100);
		try {
			buffer.received(Duration.millis(1000));
			buffer.decoded(ms(1020), ms(1014));
			buffer.received(Duration.millis(1040));
			expect(sent.slice(1)).toEqual([
				{ type: "received", end: 1_000_000 },
				{ type: "received", end: 1_034_000 },
			]);
		} finally {
			buffer.close();
		}
	});

	// A browser that reads its output position once per render callback reports it up to a callback
	// early, so the offset keeps the greatest, falls only as slowly as drift needs, and moves whole when
	// the delay or the reference moves the clock for real.
	it("sends the clock against the context time sounding at its greatest offset", () => {
		let now: Duration | undefined = Duration.millis(1000);
		const reference = new Signal<number | undefined>(0);
		const clock = started();
		const { buffer, sent, state, context } = fake(100, {
			clock: { position: () => now?.asMillis(), out: { reference } },
		});
		const report = (time: number, sounding: number, clock: number | undefined) => {
			context.currentTime = time;
			context.sounding = sounding;
			now = clock === undefined ? undefined : Duration.millis(clock);
			state(ms(0), false, 0);
		};
		try {
			// The output sounds 100 ms behind the context's time.
			report(1, 0.9, 1000);
			// Read 2 ms early: the offset falls by only 100 us.
			report(1.25, 1.152, 1250);
			report(1.5, 1.4, 1500);
			// A delay change moved the clock 50 ms.
			report(1.75, 1.65, 1800);
			// The reference moved 5 ms earlier: a jump the offset takes whole, under the filter's.
			reference.set(-5);
			report(2, 1.9, 2055);
			// A later reference moves it back, though it is no jump.
			reference.set(0);
			report(2.25, 2.15, 2300);
			report(2.5, 2.4, undefined);
			expect(sent.filter((msg) => msg.type === "clock")).toEqual([
				{ type: "clock", timestamp: 1_100_000, time: 1 },
				{ type: "clock", timestamp: 1_349_900, time: 1.25 },
				{ type: "clock", timestamp: 1_600_000, time: 1.5 },
				{ type: "clock", timestamp: 1_900_000, time: 1.75 },
				{ type: "clock", timestamp: 2_155_000, time: 2, jumped: 5000 },
				{ type: "clock", timestamp: 2_400_000, time: 2.25 },
				{ type: "clock", time: 2.5 },
			]);
		} finally {
			buffer.close();
			clock.close();
		}
	});

	// Before media zero's turn, as while an opening waits for its output to sound it, the clock pairs
	// at its negative position rather than the zero it reads as.
	it("pairs a clock short of media zero at its negative position", () => {
		const clock = started();
		const { buffer, sent, state, context } = fake(100, {
			clock: { position: () => -120, out: { reference: new Signal<number | undefined>(0) } },
		});
		try {
			context.sounding = 1;
			state(ms(0), false, 0);
			expect(sent.filter((msg) => msg.type === "clock")).toEqual([
				{ type: "clock", timestamp: -1_120_000, time: 0 },
			]);
		} finally {
			buffer.close();
			clock.close();
		}
	});

	// Before the output has played anything there is no position to pair the clock with.
	it("sends no clock until the output reports a position", () => {
		const { buffer, sent, state } = fake(100, {
			clock: { position: () => 1000, out: { reference: new Signal<number | undefined>(0) } },
		});
		try {
			state(ms(0), false, 0);
			expect(sent.filter((msg) => msg.type === "clock")).toEqual([{ type: "clock", time: 0 }]);
		} finally {
			buffer.close();
		}
	});

	// The ring renders ahead of the speaker by the output latency, so it holds the delay less that,
	// decoded floor included, and the delay the clock holds counts the base latency and the output
	// latency's recent peak.
	it("holds the delay less the output latency it renders ahead by", () => {
		const { buffer, sent, state, context } = fake(300);
		try {
			context.baseLatency = 0.01;
			context.outputLatency = 0.12;
			state(ms(0), false, 0);
			context.outputLatency = 0.104;
			state(ms(0), false, 0);
			expect(buffer.output.peek()).toEqual(Duration.millis(130));
			buffer.setLatency(Duration.millis(300));
			// A delay short of the output latency still holds one callback.
			buffer.setLatency(Duration.millis(50));
			expect(sent.filter((msg) => msg.type === "latency")).toEqual([
				{ type: "latency", latency: 180, target: 180, bound: 0 },
				{ type: "latency", latency: 128, target: 10, bound: 0 },
			]);
		} finally {
			buffer.close();
		}
	});

	// Startup holds only what the ring must from the first message, before any state report.
	it("holds the delay less the output latency the context reports at construction", () => {
		const sent: Message[] = [];
		const port = Object.assign(new EventTarget(), { postMessage: (msg: Message) => sent.push(msg), start() {} });
		const context = silent();
		context.outputLatency = 0.12;
		const buffer = new AudioBuffer({ port, context } as unknown as AudioWorkletNode, {
			channels: 1,
			rate: 1000,
			delay: Duration.millis(300),
			buffered: false,
		});
		try {
			expect(buffer.held(Duration.millis(300))).toEqual(Duration.millis(180));
			expect(sent[0]).toMatchObject({ type: "init", target: 180 });
		} finally {
			buffer.close();
		}
	});

	// The speaker sounds what was rendered an output latency ago, so the playback timestamp is where
	// the rendered playhead stood then, advancing at real time, and holds through a stall.
	it("reports the playhead sounding now", () => {
		const clock = started();
		const { buffer, state, context } = fake(100);
		try {
			context.sounding = 1;
			state(ms(5000), false, 0, 1.1);
			// Nothing rendered at or before what is sounding: the first report, back by the difference.
			expect(buffer.timestamp.peek()).toBe(ms(4900));
			context.sounding = 1.12;
			state(ms(5050), false, 0, 1.15);
			expect(buffer.timestamp.peek()).toBe(ms(5020));
			// The ring stalled at 5050: what sounds holds there once it has caught up.
			context.sounding = 1.17;
			state(ms(5050), true, 0, 1.2);
			expect(buffer.timestamp.peek()).toBe(ms(5050));
		} finally {
			buffer.close();
			clock.close();
		}
	});
});

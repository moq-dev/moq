import { describe, expect, it } from "bun:test";
import type { Message, State } from "./render";

// Load the real processor with only the browser registration boundary replaced. Each instance
// gets its own port, and the globals are restored before any other test can use them.
class Processor {
	readonly sent: State[] = [];
	readonly port = {
		onmessage: null as ((event: MessageEvent<Message>) => void) | null,
		postMessage: (state: State) => this.sent.push(state),
	};
}
interface Renderer extends Processor {
	process(inputs: Float32Array[][], outputs: Float32Array[][], parameters: Record<string, Float32Array>): boolean;
}
let Render: new () => Renderer;
const globals = ["AudioWorkletProcessor", "registerProcessor"] as const;
const previous = globals.map((name) => Object.getOwnPropertyDescriptor(globalThis, name));
try {
	Object.defineProperties(globalThis, {
		AudioWorkletProcessor: { configurable: true, value: Processor },
		registerProcessor: {
			configurable: true,
			value: (name: string, processor: new () => Renderer) => {
				expect(name).toBe("render");
				Render = processor;
			},
		},
	});
	await import("./render-worklet");
} finally {
	for (const [i, name] of globals.entries()) {
		const descriptor = previous[i];
		if (descriptor) Object.defineProperty(globalThis, name, descriptor);
		else Reflect.deleteProperty(globalThis, name);
	}
}

// A 100 ms floor at 1 kHz, with no clock until a test sends one.
function renderer() {
	const render = new Render();
	const send = (data: Message) => render.port.onmessage?.({ data } as MessageEvent<Message>);
	send({ type: "init", channels: 1, rate: 1000, latency: 100, bound: 0 });
	return {
		send,
		sent: render.sent,
		read(samples: number) {
			const output = new Float32Array(samples);
			expect(render.process([], [[output]], {})).toBe(true);
			return output;
		},
	};
}

describe("render dispatch", () => {
	it("truncates the old track's tail without flushing the due prefix", () => {
		const { send, read } = renderer();
		send({ type: "data", timestamp: 0, data: [new Float32Array(200).fill(1)] });
		expect([...read(50)]).toEqual(Array(50).fill(1));
		send({ type: "truncate", timestamp: 100_000 });
		send({ type: "data", timestamp: 100_000, data: [new Float32Array(20).fill(2)] });
		const output = read(100);
		// A short quantum fades its final 64 samples. The full prefix remains untouched and the
		// retired tail is silent, so ignoring either message changes the output.
		expect([...output.slice(0, 6)]).toEqual(Array(6).fill(1));
		expect(output[50]).toBeGreaterThan(0);
		expect(output[50]).toBeLessThan(2);
		expect([...output.slice(70)]).toEqual(Array(30).fill(0));
	});

	it("echoes the applied reset epoch and anchors a replacement behind the previous timeline", () => {
		const { send, read, sent } = renderer();
		send({ type: "data", timestamp: 10_000_000, data: [new Float32Array(200).fill(1)] });
		read(20);
		send({ type: "reset", epoch: 7 });
		// Until it refills, no old samples may escape the reset.
		expect([...read(20)]).toEqual(Array(20).fill(0));
		send({ type: "data", timestamp: 500_000, data: [new Float32Array(100).fill(2)] });
		for (let i = 0; i < 3; i++) read(20);
		expect(sent).toEqual([
			{
				type: "state",
				epoch: 7,
				timestamp: 560_000,
				stalled: false,
				anchored: true,
				underruns: 0,
				skips: 0,
				trimmed: 0,
				accelerated: 0,
				silent: false,
			},
		]);
	});

	it("keeps playing through a latency rise, keeping unread PCM", () => {
		const { send, read } = renderer();
		send({ type: "data", timestamp: 0, data: [new Float32Array(100).fill(1)] });
		expect([...read(20)]).toEqual(Array(20).fill(1));
		send({ type: "latency", latency: 200, bound: 0 });
		expect([...read(20)]).toEqual(Array(20).fill(1));
		send({ type: "data", timestamp: 100_000, data: [new Float32Array(120).fill(2)] });
		const output = read(100);
		expect(output.slice(0, 60).every((value) => value === 1)).toBe(true);
		expect(output.slice(-20).every((value) => value === 2)).toBe(true);
	});

	it("plays onto the clock it is sent, skipping only past the bound", () => {
		const { send, read, sent } = renderer();
		const time = Object.getOwnPropertyDescriptor(globalThis, "currentTime");
		// The context's time, advanced a quantum per read as the clock is.
		let now = 1;
		const tick = () => {
			read(20);
			now += 0.02;
		};
		try {
			Object.defineProperty(globalThis, "currentTime", { configurable: true, get: () => now });
			send({ type: "data", timestamp: 0, data: [new Float32Array(1000).fill(1)] });
			// Half a second behind a clock that allows none, so the next quantum lands on the clock.
			send({ type: "clock", timestamp: 500_000, time: now });
			for (let i = 0; i < 5; i++) tick();
			expect(sent.at(-1)).toMatchObject({ timestamp: 600_000, skips: 1, trimmed: 500 });
			// A latency update moves the clock, so one without a new reading leaves none.
			send({ type: "latency", latency: 100, bound: 0 });
			now += 1;
			for (let i = 0; i < 5; i++) tick();
			expect(sent.at(-1)).toMatchObject({ timestamp: 700_000, skips: 1, trimmed: 500 });
			// One carrying the moved clock keeps playing onto it.
			send({ type: "latency", latency: 100, bound: 0, clock: { timestamp: 800_000, time: now } });
			for (let i = 0; i < 5; i++) tick();
			expect(sent.at(-1)).toMatchObject({ timestamp: 900_000, skips: 2, trimmed: 600 });
		} finally {
			if (time) Object.defineProperty(globalThis, "currentTime", time);
			else Reflect.deleteProperty(globalThis, "currentTime");
		}
	});
});

// A declared end plays comfort noise at the level of the quietest audio heard before it, through the
// refill of the next utterance, and none of it counts as an underrun.
it("fills a declared silence with comfort noise and counts no underrun", () => {
	const { send, read, sent } = renderer();
	const quiet = 0.0002;
	send({ type: "data", timestamp: 0, data: [new Float32Array(200).fill(quiet)] });
	send({ type: "end", timestamp: 200_000 });
	for (let i = 0; i < 10; i++) expect(read(20)[0]).toBeCloseTo(quiet, 9);

	const rms = (samples: Float32Array) => Math.sqrt(samples.reduce((sum, x) => sum + x * x, 0) / samples.length);
	const noise = read(2000);
	expect(Math.max(...noise.map(Math.abs))).toBeLessThanOrEqual(quiet * Math.sqrt(3));
	expect(rms(noise)).toBeCloseTo(quiet, 4);
	for (let i = 0; i < 4; i++) read(20);
	expect(sent.at(-1)).toMatchObject({ underruns: 0, silent: true });

	// The next utterance refills before it plays, and the noise carries on meanwhile.
	send({ type: "reset", epoch: 1, next: true });
	send({ type: "data", timestamp: 9_000_000, data: [new Float32Array(50).fill(1)] });
	expect(rms(read(20))).toBeGreaterThan(0);
	send({ type: "data", timestamp: 9_050_000, data: [new Float32Array(50).fill(1)] });
	read(20);
	for (let i = 0; i < 3; i++) read(20);
	expect(sent.at(-1)).toMatchObject({ underruns: 0, silent: false, stalled: false });
});

// Back-to-back presses leave a short declared silence buffered between two utterances, which plays as
// the same comfort noise as a longer one.
it("fills a declared silence between buffered utterances with comfort noise", () => {
	const { send, read, sent } = renderer();
	const quiet = 0.0002;
	send({ type: "data", timestamp: 0, data: [new Float32Array(200).fill(quiet)] });
	send({ type: "end", timestamp: 200_000 });
	for (let i = 0; i < 9; i++) read(20);
	send({ type: "reset", epoch: 1, next: true });
	send({ type: "data", timestamp: 400_000, data: [new Float32Array(200).fill(1)] });

	const output = read(20);
	expect(output[0]).toBeCloseTo(quiet, 9);
	const noise = read(200);
	expect(noise.some((value) => value !== 0)).toBe(true);
	expect(Math.max(...noise.map(Math.abs))).toBeLessThanOrEqual(quiet * Math.sqrt(3));
	// The next utterance fades in from the noise and then plays as sent.
	const resumed = read(150);
	expect(resumed[0]).toBeLessThan(1);
	expect(resumed[149]).toBe(1);
	for (let i = 0; i < 4; i++) read(10);
	expect(sent.at(-1)).toMatchObject({ underruns: 0, stalled: false });
});

it("caps comfort noise at -60 dBFS after loud audio", () => {
	const { send, read } = renderer();
	send({ type: "data", timestamp: 0, data: [new Float32Array(200).fill(0.5)] });
	send({ type: "end", timestamp: 200_000 });
	for (let i = 0; i < 10; i++) read(20);
	expect(Math.max(...read(2000).map(Math.abs))).toBeLessThanOrEqual(0.001 * Math.sqrt(3));
});

it("adds no noise to a source that was already silent", () => {
	const { send, read } = renderer();
	send({ type: "data", timestamp: 0, data: [new Float32Array(200)] });
	send({ type: "end", timestamp: 200_000 });
	for (let i = 0; i < 10; i++) read(20);
	expect([...read(100)]).toEqual(Array(100).fill(0));
});

it("holds PCM without consuming it until the shared startup gate opens", () => {
	const { send, read, sent } = renderer();
	send({ type: "playing", playing: false, epoch: 0 });
	send({ type: "data", timestamp: 500_000, data: [new Float32Array(200).fill(1)] });
	for (let i = 0; i < 5; i++) expect([...read(20)]).toEqual(Array(20).fill(0));
	expect(sent[0]).toMatchObject({ timestamp: 500_000, underruns: 0 });
	send({ type: "playing", playing: true, epoch: 0 });
	expect(read(100)[99]).toBe(1);
	send({ type: "reset", epoch: 1, playing: false });
	send({ type: "data", timestamp: 0, data: [new Float32Array(200).fill(2)] });
	send({ type: "playing", playing: true, epoch: 0 });
	expect([...read(20)]).toEqual(Array(20).fill(0));
	send({ type: "playing", playing: true, epoch: 1 });
	expect(read(100)[99]).toBe(2);
});

it("clears comfort noise when the timeline resets", () => {
	const { send, read } = renderer();
	send({ type: "data", timestamp: 0, data: [new Float32Array(200).fill(0.0002)] });
	send({ type: "end", timestamp: 200_000 });
	for (let i = 0; i < 10; i++) read(20);
	expect(read(20).some((value) => value !== 0)).toBe(true);
	send({ type: "reset", epoch: 1 });
	expect([...read(20)]).toEqual(Array(20).fill(0));
});

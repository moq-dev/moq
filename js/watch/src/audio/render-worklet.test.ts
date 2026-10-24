import { describe, expect, it } from "bun:test";
import { Micro, Milli } from "@moq/clock";
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

function renderer() {
	const render = new Render();
	const send = (data: Message) => render.port.onmessage?.({ data } as MessageEvent<Message>);
	send({ type: "init", channels: 1, rate: 1000, latency: Milli(100), buffered: true });
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
		send({ type: "data", timestamp: Micro(0), data: [new Float32Array(200).fill(1)] });
		expect([...read(50)]).toEqual(Array(50).fill(1));
		send({ type: "truncate", timestamp: Micro(100_000) });
		send({ type: "data", timestamp: Micro(100_000), data: [new Float32Array(20).fill(2)] });
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
		send({ type: "data", timestamp: Micro(10_000_000), data: [new Float32Array(200).fill(1)] });
		read(20);
		send({ type: "reset", epoch: 7 });
		// Until it refills, no old samples may escape the reset.
		expect([...read(20)]).toEqual(Array(20).fill(0));
		send({ type: "data", timestamp: Micro(500_000), data: [new Float32Array(100).fill(2)] });
		for (let i = 0; i < 3; i++) read(20);
		expect(sent).toEqual([
			{ type: "state", epoch: 7, timestamp: Micro(560_000), stalled: false, anchored: true, underruns: 0 },
		]);
	});

	// A covered ring parks and resumes only on the main thread's word: data decoded before the park
	// may still land. The echoed epoch is what tells the main thread that state reports no longer
	// predate the park.
	it("parks until the next floor update and echoes the park's epoch", () => {
		const { send, read, sent } = renderer();
		send({ type: "data", timestamp: Micro(0), data: [new Float32Array(200).fill(1)] });
		expect([...read(20)]).toEqual(Array(20).fill(1));
		send({ type: "latency", latency: Milli(100), park: 3 });
		expect([...read(20)]).toEqual(Array(20).fill(0));
		send({ type: "data", timestamp: Micro(200_000), data: [new Float32Array(20).fill(2)] });
		expect([...read(20)]).toEqual(Array(20).fill(0));
		send({ type: "latency", latency: Milli(100) });
		// It fades back in on the parked audio, not the new data.
		expect(read(20)[19]).toBe(1);
		read(20);
		expect(sent).toEqual([
			{ type: "state", epoch: 3, timestamp: Micro(60_000), stalled: false, anchored: true, underruns: 0 },
		]);
	});

	it("parks on a latency rise and preserves unread PCM", () => {
		const { send, read } = renderer();
		send({ type: "data", timestamp: Micro(0), data: [new Float32Array(100).fill(1)] });
		expect([...read(20)]).toEqual(Array(20).fill(1));
		send({ type: "latency", latency: Milli(200) });
		expect([...read(20)]).toEqual(Array(20).fill(0));
		send({ type: "data", timestamp: Micro(100_000), data: [new Float32Array(120).fill(2)] });
		const output = read(100);
		expect(output[79]).toBe(1);
		expect([...output.slice(80)]).toEqual(Array(20).fill(2));
	});
});

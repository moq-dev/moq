import { afterEach, beforeEach, expect, test } from "bun:test";
import { Signal } from "@moq/signals";
import type { Decoder } from "./decoder";
import { Emitter } from "./emitter";

// Records the automation a GainNode is scheduled with, as [method, value, time].
class FakeGainNode {
	readonly context: { currentTime: number };
	readonly calls: [string, number, number][] = [];
	readonly gain = {
		value: 0,
		setValueAtTime: (value: number, time: number) => this.calls.push(["set", value, time]),
		linearRampToValueAtTime: (value: number, time: number) => this.calls.push(["ramp", value, time]),
		cancelScheduledValues: (time: number) => this.calls.push(["cancel", 0, time]),
	};

	constructor(context: { currentTime: number }, options: GainOptions) {
		this.context = context;
		this.gain.value = options.gain ?? 1;
		nodes.push(this);
	}

	connect(): void {}
	disconnect(): void {}
}

let nodes: FakeGainNode[] = [];
const original = Object.getOwnPropertyDescriptor(globalThis, "GainNode");

beforeEach(() => {
	nodes = [];
	Object.defineProperty(globalThis, "GainNode", { configurable: true, writable: true, value: FakeGainNode });
});

afterEach(() => {
	if (original) Object.defineProperty(globalThis, "GainNode", original);
	else Reflect.deleteProperty(globalThis, "GainNode");
});

async function setup() {
	const context = { currentTime: 10, destination: {} };
	const source = { out: { root: new Signal({ context, connect: () => {} }) } } as unknown as Decoder;
	const volume = new Signal(0.5);
	const emitter = new Emitter({ source, volume });
	await settle();
	return { emitter, volume, node: nodes[0] };
}

async function settle(): Promise<void> {
	for (let i = 0; i < 5; i++) await new Promise<void>((resolve) => queueMicrotask(resolve));
}

test("ramps a volume change from wherever the last ramp reached", async () => {
	const { emitter, volume, node } = await setup();
	try {
		node.calls.length = 0;
		volume.set(0);
		await settle();
		expect(node.calls).toEqual([
			["cancel", 0, 10],
			["set", 0.5, 10],
			["ramp", 0, 10.2],
		]);
	} finally {
		emitter.close();
	}
});

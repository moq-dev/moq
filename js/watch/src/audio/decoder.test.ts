import { afterEach, beforeEach, describe, expect, it, mock, spyOn } from "bun:test";
import * as Catalog from "@moq/hang/catalog";
import * as Container from "@moq/hang/container";
import * as Moq from "@moq/net";
import { Time } from "@moq/net";
import { Signal } from "@moq/signals";
import type { Broadcast } from "../broadcast";
import { type Delay, Sync } from "../sync";
import { SharedRingBuffer } from "./shared-ring-buffer";
import { Source } from "./source";

// Bun cannot load the blob-URL worklet import.
mock.module("./render-worklet.ts?worklet", () => ({ default: async () => "blob:fake-render" }));
const { Decoder } = await import("./decoder");

// Drain reactive work without advancing playback time.
async function microtasks() {
	for (let i = 0; i < 100; i++) await Promise.resolve();
}

class FakeContext extends EventTarget {
	readonly state = "running";
	readonly sampleRate: number;
	readonly audioWorklet = { addModule: async () => {} };
	constructor(options: AudioContextOptions) {
		super();
		this.sampleRate = options.sampleRate ?? 48_000;
	}
	resume = async () => {};
	close = async () => {};
}

class FakeWorklet {
	readonly port = Object.assign(new EventTarget(), { postMessage() {}, start() {} });
	disconnect() {}
}

class FakeData {
	readonly sampleRate = 48_000;
	readonly numberOfChannels = 2;
	readonly numberOfFrames = 960;
	readonly timestamp: number;
	constructor(timestamp: number) {
		this.timestamp = timestamp;
	}
	copyTo() {}
	close() {}
}

type Read = NonNullable<Awaited<ReturnType<Container.Consumer["next"]>>>;

let frameTimestamp = 0;
function frame(): Read {
	frameTimestamp += 20_000;
	return {
		group: 0,
		discontinuity: 0,
		continuous: true,
		frame: { timestamp: Time.Micro(frameTimestamp), payload: new Uint8Array([1]), keyframe: true },
	};
}

const globals = ["AudioContext", "AudioWorkletNode", "AudioDecoder", "AudioEncoder", "EncodedAudioChunk"] as const;
const originals = new Map<string, PropertyDescriptor | undefined>();
let codecs = 0;

beforeEach(() => {
	frameTimestamp = 0;
	codecs = 0;
	for (const name of globals) originals.set(name, Object.getOwnPropertyDescriptor(globalThis, name));

	class Codec {
		state = "configured";
		readonly init: AudioDecoderInit;
		constructor(init: AudioDecoderInit) {
			this.init = init;
			codecs++;
		}
		configure() {}
		reset() {}
		decode(chunk: { timestamp: number }) {
			this.init.output(new FakeData(chunk.timestamp) as unknown as AudioData);
		}
		close() {
			this.state = "closed";
		}
	}
	const chunk = class {
		readonly timestamp: number;
		constructor(init: EncodedAudioChunkInit) {
			this.timestamp = init.timestamp;
		}
	};

	const values = {
		AudioContext: FakeContext,
		AudioWorkletNode: FakeWorklet,
		AudioDecoder: Codec,
		AudioEncoder: class {},
		EncodedAudioChunk: chunk,
	};
	for (const name of globals) Object.defineProperty(globalThis, name, { configurable: true, value: values[name] });
});

afterEach(() => {
	for (const name of globals) {
		const original = originals.get(name);
		if (original) Object.defineProperty(globalThis, name, original);
		else Reflect.deleteProperty(globalThis, name);
	}
	mock.restore();
});

// Hands frames to whichever container consumer is reading, and ends a consumer's reads on close.
function feed() {
	const closed = new WeakSet<Container.Consumer>();
	const queue: Read[] = [];
	let waiter: { consumer: Container.Consumer; resolve: (read: Read | undefined) => void } | undefined;

	spyOn(Container.Consumer.prototype, "next").mockImplementation(function (this: Container.Consumer) {
		if (closed.has(this)) return Promise.resolve(undefined);
		const read = queue.shift();
		if (read) return Promise.resolve(read);
		return new Promise<Read | undefined>((resolve) => {
			waiter = { consumer: this, resolve };
		});
	});

	const close = Container.Consumer.prototype.close;
	spyOn(Container.Consumer.prototype, "close").mockImplementation(function (this: Container.Consumer) {
		closed.add(this);
		if (waiter?.consumer === this) {
			waiter.resolve(undefined);
			waiter = undefined;
		}
		close.call(this);
	});

	return (read: Read) => {
		const current = waiter;
		waiter = undefined;
		if (current) current.resolve(read);
		else queue.push(read);
	};
}

async function play(initial: Delay) {
	const push = feed();
	const truncate = spyOn(SharedRingBuffer.prototype, "truncate");
	const reset = spyOn(SharedRingBuffer.prototype, "reset");

	const producer = new Moq.Broadcast.Producer();
	const consumer = producer.consume();
	const relativeBroadcast = mock(() => consumer);
	const audio = Catalog.AudioConfigSchema.parse({
		codec: "opus",
		container: { kind: "legacy" },
		sampleRate: 48_000,
		numberOfChannels: 2,
	});
	const catalog = new Signal<Catalog.Root>({ audio: { renditions: { audio } } });
	const broadcast = new Signal<Broadcast | undefined>({
		out: { catalog },
		relativeBroadcast,
	} as unknown as Broadcast);

	const delay = new Signal<Delay>(initial);
	const source = new Source({ broadcast, supported: async () => true });
	const sync = new Sync({ delay });
	const decoder = new Decoder({ source, sync });
	await microtasks();

	// Enough frames to get past the legacy decoder's warm-up, so a handover would truncate.
	const play = async () => {
		for (let i = 0; i < 6; i++) {
			push(frame());
			await microtasks();
		}
	};

	return {
		delay,
		play,
		// Each subscription resolves the rendition's broadcast once.
		subscriptions: () => relativeBroadcast.mock.calls.length,
		codecs: () => codecs,
		truncates: () => truncate.mock.calls.length,
		resets: () => reset.mock.calls.length,
		close() {
			decoder.close();
			sync.close();
			source.close();
			consumer.close();
			producer.close();
		},
	};
}

describe("Decoder across a delay change", () => {
	for (const initial of [Time.Milli(100), "auto"] as const) {
		it(`keeps the subscription and ring when ${initial} becomes a number`, async () => {
			const playback = await play(initial);
			try {
				await playback.play();
				expect(playback.subscriptions()).toBe(1);
				expect(playback.codecs()).toBe(1);

				playback.delay.set(Time.Milli(120));
				await microtasks();
				await playback.play();

				expect(playback.subscriptions()).toBe(1);
				expect(playback.codecs()).toBe(1);
				expect(playback.truncates()).toBe(0);
				expect(playback.resets()).toBe(0);
			} finally {
				playback.close();
			}
		});
	}

	it("rebuilds on a switch to and from instant", async () => {
		const playback = await play(Time.Milli(100));
		try {
			await playback.play();

			playback.delay.set("instant");
			await microtasks();
			expect(playback.resets()).toBe(1);

			playback.delay.set(Time.Milli(100));
			await microtasks();
			await playback.play();

			expect(playback.subscriptions()).toBe(2);
			expect(playback.codecs()).toBe(2);
			// The replacement subscription drops whatever the ring still held from the first.
			expect(playback.truncates()).toBe(1);
		} finally {
			playback.close();
		}
	});
});

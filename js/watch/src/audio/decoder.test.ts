import { afterEach, beforeEach, describe, expect, it, jest, mock, spyOn } from "bun:test";
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

// Every context built, and a gate the worklet module load waits on.
let contexts: FakeContext[] = [];
let moduleLoaded: Promise<void> = Promise.resolve();

class FakeContext extends EventTarget {
	state: AudioContextState = "running";
	readonly sampleRate: number;
	readonly audioWorklet = { addModule: () => moduleLoaded };
	readonly calls = { suspend: 0, resume: 0, close: 0 };
	constructor(options: AudioContextOptions) {
		super();
		this.sampleRate = options.sampleRate ?? 48_000;
		contexts.push(this);
	}
	#transition(state: AudioContextState) {
		this.state = state;
		this.dispatchEvent(new Event("statechange"));
	}
	suspend = async () => {
		this.calls.suspend++;
		this.#transition("suspended");
	};
	resume = async () => {
		this.calls.resume++;
		this.#transition("running");
	};
	close = async () => {
		this.calls.close++;
		this.#transition("closed");
	};
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

const globals = [
	"AudioContext",
	"AudioWorkletNode",
	"AudioDecoder",
	"AudioEncoder",
	"EncodedAudioChunk",
	"document",
] as const;
const originals = new Map<string, PropertyDescriptor | undefined>();
let codecs = 0;

beforeEach(() => {
	frameTimestamp = 0;
	codecs = 0;
	contexts = [];
	moduleLoaded = Promise.resolve();
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
		// Gesture.unlock listens here while a context is suspended.
		document: new EventTarget(),
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
	const insert = spyOn(SharedRingBuffer.prototype, "insert");

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
	const play = async (count = 6) => {
		const timestamps: number[] = [];
		for (let i = 0; i < count; i++) {
			const read = frame();
			timestamps.push(read.frame?.timestamp ?? 0);
			push(read);
			await microtasks();
		}
		return timestamps;
	};

	return {
		delay,
		play,
		// Remove the rendition from the catalog, then restore it.
		remove: () => catalog.set({}),
		restore: (patch: Record<string, unknown> = {}) =>
			catalog.set({ audio: { renditions: { audio: Catalog.AudioConfigSchema.parse({ ...audio, ...patch }) } } }),
		// The rings written to, and the timestamps of every frame that reached one.
		rings: () => [...new Set(insert.mock.contexts)] as SharedRingBuffer[],
		inserted: () => insert.mock.calls.map(([timestamp]) => timestamp as number),
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

// Play a ring out the way the worklet does, one render quantum at a time, until it runs dry.
function drain(ring: SharedRingBuffer): number {
	const output = [new Float32Array(128), new Float32Array(128)];
	let total = 0;
	for (;;) {
		const read = ring.read(output);
		if (read === 0) return total;
		total += read;
	}
}

describe("Decoder across a rendition's absence", () => {
	afterEach(() => jest.useRealTimers());

	it("plays the tail, suspends once drained, and resumes on return", async () => {
		// The ring's stall state is polled on an interval.
		jest.useFakeTimers();
		const playback = await play(Time.Milli(100));
		try {
			await playback.play(12);
			const [ring] = playback.rings();
			expect(ring.stalled).toBe(false);
			jest.advanceTimersByTime(50);
			await microtasks();

			playback.remove();
			await microtasks();

			// The ring still holds the tail, so the graph keeps playing it.
			expect(contexts).toHaveLength(1);
			const [context] = contexts;
			expect(context.calls).toEqual({ suspend: 0, resume: 0, close: 0 });

			expect(drain(ring)).toBeGreaterThan(0);
			expect(playback.truncates()).toBe(0);
			expect(playback.resets()).toBe(0);
			jest.advanceTimersByTime(50);
			await microtasks();
			expect(context.calls.suspend).toBe(1);
			expect(context.state).toBe("suspended");

			playback.restore();
			await microtasks();
			expect(context.state).toBe("running");

			const before = playback.inserted().length;
			const timestamps = await playback.play(6);

			// One graph throughout, and the return decodes into the same ring from its first frame past
			// the legacy warm-up.
			expect(contexts).toHaveLength(1);
			expect(context.calls.close).toBe(0);
			expect(playback.subscriptions()).toBe(2);
			expect(playback.rings()).toEqual([ring]);
			expect(playback.inserted().slice(before)).toEqual(timestamps.slice(3));
		} finally {
			playback.close();
		}
	});

	it("keeps the graph when only the container changes", async () => {
		const playback = await play(Time.Milli(100));
		try {
			await playback.play();
			playback.restore({ container: { kind: "loc" } });
			await microtasks();
			await playback.play();

			expect(playback.subscriptions()).toBe(2);
			expect(contexts).toHaveLength(1);
		} finally {
			playback.close();
		}
	});

	it("rebuilds the graph on a new channel count", async () => {
		const playback = await play(Time.Milli(100));
		try {
			await playback.play();
			playback.restore({ numberOfChannels: 1 });
			await microtasks();

			expect(contexts).toHaveLength(2);
			expect(contexts[0].calls.close).toBe(1);
		} finally {
			playback.close();
		}
	});

	it("decodes the frames that arrive before the worklet loads", async () => {
		let load = () => {};
		moduleLoaded = new Promise((resolve) => {
			load = resolve;
		});

		const playback = await play(Time.Milli(100));
		try {
			const timestamps = await playback.play(6);
			expect(playback.inserted()).toEqual([]);

			load();
			await microtasks();

			// Everything past the legacy warm-up reaches the ring, none of it dropped for want of one.
			expect(playback.inserted()).toEqual(timestamps.slice(3));
		} finally {
			playback.close();
		}
	});
});

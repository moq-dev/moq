import { afterEach, beforeEach, describe, expect, it, jest, mock, spyOn } from "bun:test";
import { Micro, Milli, Nano, now } from "@moq/clock";
import * as Catalog from "@moq/hang/catalog";
import * as Container from "@moq/hang/container";
import * as Moq from "@moq/net";
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

type Read = Container.Read;

let frameTimestamp = 0;
function frame(): Extract<Read, { kind: "frame" }> {
	frameTimestamp += 20_000;
	return {
		kind: "frame",
		group: 0,
		frame: {
			arrival: Nano.toMilli(Nano(now())),
			timestamp: Micro(frameTimestamp),
			payload: new Uint8Array([1]),
			keyframe: true,
		},
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
let codecErrors: AudioDecoderInit["error"][] = [];

beforeEach(() => {
	frameTimestamp = 0;
	codecs = 0;
	codecErrors = [];
	contexts = [];
	moduleLoaded = Promise.resolve();
	for (const name of globals) originals.set(name, Object.getOwnPropertyDescriptor(globalThis, name));

	class Codec {
		state = "configured";
		needsKey = true;
		readonly init: AudioDecoderInit;
		constructor(init: AudioDecoderInit) {
			this.init = init;
			codecErrors.push(init.error);
			codecs++;
		}
		configure() {
			this.needsKey = true;
		}
		reset() {
			this.needsKey = true;
		}
		decode(chunk: { timestamp: number; type: EncodedAudioChunkType }) {
			if (this.needsKey && chunk.type !== "key") throw new DOMException("key chunk required", "DataError");
			this.needsKey = false;
			this.init.output(new FakeData(chunk.timestamp) as unknown as AudioData);
		}
		close() {
			this.state = "closed";
		}
	}
	const chunk = class {
		readonly timestamp: number;
		readonly type: EncodedAudioChunkType;
		constructor(init: EncodedAudioChunkInit) {
			this.timestamp = init.timestamp;
			this.type = init.type;
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

async function play(initial: Delay, buffered = false, cmaf = false) {
	const push = feed();
	const truncate = spyOn(SharedRingBuffer.prototype, "truncate");
	const reset = spyOn(SharedRingBuffer.prototype, "reset");
	const insert = spyOn(SharedRingBuffer.prototype, "insert");

	let producer = new Moq.Broadcast.Producer();
	let consumer = producer.consume();
	const relativeBroadcast = mock(() => consumer);
	let audio = Catalog.AudioConfigSchema.parse({
		codec: "opus",
		container: { kind: "legacy" },
		sampleRate: 48_000,
		numberOfChannels: 2,
	});
	if (cmaf) {
		audio = Catalog.AudioConfigSchema.parse({
			...audio,
			container: {
				kind: "cmaf",
				init: Buffer.from(Container.Cmaf.createAudioInitSegment(audio)).toString("base64"),
			},
		});
	}
	const catalog = new Signal<Catalog.Root>({ audio: { renditions: { audio } } });
	const enabled = new Signal(true);
	// The catalog broadcast's instance, identified by its `closed` handle.
	const active = new Signal({ closed: {} });
	const broadcast = new Signal<Broadcast | undefined>({
		in: { enabled },
		out: { catalog, active },
		relativeBroadcast,
	} as unknown as Broadcast);

	const delay = new Signal<Delay>(initial);
	const source = new Source({ broadcast, supported: async () => true });
	const sync = new Sync({ delay, buffer: Milli(buffered ? 1000 : 0) });
	// The decoder's own switch, which a mute or pause turns off.
	const decoding = new Signal(true);
	const decoder = new Decoder({ source, sync, enabled: decoding });
	await microtasks();

	// Enough frames to get past the legacy decoder's warm-up, so a handover would truncate.
	const play = async (count = 6) => {
		const timestamps: number[] = [];
		for (let i = 0; i < count; i++) {
			const read = frame();
			timestamps.push(read.frame.timestamp);
			push(read);
			await microtasks();
		}
		return timestamps;
	};

	return {
		delay,
		push,
		enabled,
		decoding,
		context: decoder.out.context,
		arrivals: () => decoder.arrivals(),
		error: decoder.out.error,
		play,
		// Remove the rendition from the catalog, then restore it.
		remove: () => catalog.set({}),
		// Point the player at another broadcast, one without audio.
		leave: () => {
			active.set({ closed: {} });
			catalog.set({});
		},
		// Keep the rendition in the catalog with `enabled: false`, as a muted publisher does.
		disable: () =>
			catalog.set({
				audio: { renditions: { audio: Catalog.AudioConfigSchema.parse({ ...audio, enabled: false }) } },
			}),
		// The rendition the source selected.
		track: () => source.out.track.peek(),
		restore: (patch: Record<string, unknown> = {}) =>
			catalog.set({ audio: { renditions: { audio: Catalog.AudioConfigSchema.parse({ ...audio, ...patch }) } } }),
		// Serve another broadcast from the next subscription, its timeline starting over.
		replace: () => {
			consumer.close();
			producer.close();
			producer = new Moq.Broadcast.Producer();
			consumer = producer.consume();
			frameTimestamp = 0;
		},
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

it("decodes an audio packet inside a group after a gap", async () => {
	const playback = await play(Milli(100));
	try {
		await playback.play();
		// Opened late, so it observes only the gap and frame below.
		const arrivals = playback.arrivals();
		playback.push({ kind: "gap", timestamp: Micro(20_000), duration: Micro(40_000) });
		const next = frame();
		next.frame.keyframe = false;
		playback.push(next);
		await microtasks();
		expect(playback.inserted()).toContain(next.frame.timestamp);
		expect(await arrivals.next()).toEqual({ kind: "gap", timestamp: Milli(20), duration: Milli(40) });
		expect(await arrivals.next()).toMatchObject({
			kind: "frame",
			timestamp: Milli.fromMicro(next.frame.timestamp),
			arrival: next.frame.arrival,
		});
		arrivals.close();
		expect(await arrivals.next()).toBeUndefined();
	} finally {
		playback.close();
		await microtasks();
	}
});

describe("Decoder across a broadcast disable", () => {
	it("releases the graph when its broadcast is disabled and rebuilds on return", async () => {
		const playback = await play(Milli(100));
		try {
			await playback.play();
			const [context] = contexts;

			playback.enabled.set(false);
			await microtasks();
			expect(context.calls.close).toBe(1);
			expect(playback.context.peek()).toBeUndefined();

			playback.enabled.set(true);
			await microtasks();
			expect(contexts).toHaveLength(2);
			expect(contexts[1].state).toBe("running");
			expect(playback.context.peek()).toBe(contexts[1] as unknown as AudioContext);
		} finally {
			playback.close();
		}
	});
});

describe("Decoder across a delay change", () => {
	for (const initial of [Milli(100), "auto"] as const) {
		it(`keeps the subscription and ring when ${initial} becomes a number`, async () => {
			const playback = await play(initial);
			try {
				await playback.play();
				expect(playback.subscriptions()).toBe(1);
				expect(playback.codecs()).toBe(1);

				playback.delay.set(Milli(120));
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
		const playback = await play(Milli(100));
		try {
			await playback.play();

			playback.delay.set("instant");
			await microtasks();
			expect(playback.resets()).toBe(1);

			playback.delay.set(Milli(100));
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
		const playback = await play(Milli(100));
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

			// One graph throughout; a rendition without warmup keeps its entire return.
			expect(contexts).toHaveLength(1);
			expect(context.calls.close).toBe(0);
			expect(playback.subscriptions()).toBe(2);
			expect(playback.rings()).toEqual([ring]);
			expect(playback.inserted().slice(before)).toEqual(timestamps);
		} finally {
			playback.close();
		}
	});

	it("deselects a disabled rendition and keeps one graph across disable and enable", async () => {
		const playback = await play(Milli(100));
		try {
			await playback.play(12);
			expect(playback.track()).toBe("audio");

			playback.disable();
			await microtasks();
			expect(playback.track()).toBeUndefined();
			expect(contexts).toHaveLength(1);

			playback.restore();
			await microtasks();
			expect(playback.track()).toBe("audio");
			await playback.play(6);

			expect(contexts).toHaveLength(1);
			expect(contexts[0].calls.close).toBe(0);
			expect(playback.subscriptions()).toBe(2);
		} finally {
			playback.close();
		}
	});

	it("plays a different broadcast whose timeline starts behind the playhead", async () => {
		const playback = await play(Milli(100));
		try {
			await playback.play(12);
			const [ring] = playback.rings();
			expect(drain(ring)).toBeGreaterThan(0);

			playback.remove();
			await microtasks();
			const reanchor = spyOn(Sync.prototype, "reset");
			playback.replace();
			playback.restore();
			await microtasks();
			await playback.play(12);

			// The ring and the shared clock re-anchor on the new broadcast rather than discarding it as
			// already played.
			expect(reanchor).toHaveBeenCalledTimes(1);
			reanchor.mockRestore();
			expect(drain(ring)).toBeGreaterThan(0);
			expect(contexts).toHaveLength(1);
			expect(playback.rings()).toEqual([ring]);
		} finally {
			playback.close();
		}
	});

	it("keeps the graph when only the container changes", async () => {
		const playback = await play(Milli(100));
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
		const playback = await play(Milli(100));
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

	it("keeps the graph for a rendition at the rate the decoder already outputs", async () => {
		const playback = await play(Milli(100));
		try {
			// Advertised at 24 kHz but decoded at 48 kHz (Opus on Chrome), so the graph settles at 48 kHz.
			playback.restore({ sampleRate: 24_000 });
			await microtasks();
			await playback.play();
			const built = contexts.length;
			expect(contexts.at(-1)?.sampleRate).toBe(48_000);

			playback.restore({ sampleRate: 48_000 });
			await microtasks();
			expect(contexts).toHaveLength(built);
		} finally {
			playback.close();
		}
	});

	it("decodes the frames that arrive before the worklet loads", async () => {
		let load = () => {};
		moduleLoaded = new Promise((resolve) => {
			load = resolve;
		});

		const playback = await play(Milli(100));
		try {
			const timestamps = await playback.play(6);
			expect(playback.inserted()).toEqual([]);

			load();
			await microtasks();

			// Every frame reaches the ring once the worklet loads.
			expect(playback.inserted()).toEqual(timestamps);
		} finally {
			playback.close();
		}
	});
});

for (const cmaf of [false, true]) {
	it(`settles a buffered ${cmaf ? "CMAF" : "legacy"} rendition change with a stationary playhead`, async () => {
		const playback = await play(Milli(100), true, cmaf);
		try {
			const timestamps = await playback.play(20);
			const before = playback.codecs();
			expect(before).toBe(1);
			// Nothing reads the ring, as with a muted AudioContext. Decode is held upstream.
			// The first frame past the last decoded one is the one blocked on backpressure. Earlier gaps
			// are the legacy decoder's dropped warm-up output.
			const last = Math.max(...playback.inserted());
			const held = timestamps.find((timestamp) => timestamp > last);
			expect(held).toBeDefined();
			playback.restore({ description: "4f707573486561640102000080bb0000000000" });
			await microtasks();
			expect(playback.codecs()).toBe(before + 1);
			// Releasing the retired loop's wait must not decode the frame it held.
			expect(playback.inserted()).not.toContain(held);
		} finally {
			playback.close();
			await microtasks();
		}
	});
}

describe("fatal decoder errors", () => {
	it("releases audio subscriptions and retains the failure until the source changes", async () => {
		const close = spyOn(Moq.Track.Subscriber.prototype, "close");
		const playback = await play(Milli(100));
		try {
			await playback.play();
			const before = close.mock.calls.length;
			const failure = new DOMException("invalid encoded audio", "EncodingError");
			codecErrors.at(-1)?.(failure);
			await microtasks();
			expect(playback.error.peek()).toBe(failure);
			expect(close.mock.calls.length).toBeGreaterThan(before);
			playback.decoding.set(false);
			await microtasks();
			playback.decoding.set(true);
			await microtasks();
			const attempts = playback.subscriptions();
			expect(playback.error.peek()).toBe(failure);
			expect(playback.codecs()).toBe(1);
			playback.remove();
			await microtasks();
			playback.replace();
			playback.restore();
			await microtasks();
			expect(playback.error.peek()).toBeUndefined();
			expect(playback.subscriptions()).toBeGreaterThan(attempts);
		} finally {
			playback.close();
			close.mockRestore();
		}
	});

	it("clears the failure when a source without audio is selected while muted", async () => {
		const playback = await play(Milli(100));
		try {
			await playback.play();
			const failure = new DOMException("invalid encoded audio", "EncodingError");
			codecErrors.at(-1)?.(failure);
			await microtasks();
			playback.decoding.set(false);
			await microtasks();
			expect(playback.error.peek()).toBe(failure);
			playback.leave();
			await microtasks();
			expect(playback.error.peek()).toBeUndefined();
		} finally {
			playback.close();
		}
	});

	it("keeps the failure while the same rendition is muted by its publisher", async () => {
		const playback = await play(Milli(100));
		try {
			await playback.play();
			const failure = new DOMException("invalid encoded audio", "EncodingError");
			codecErrors.at(-1)?.(failure);
			await microtasks();
			playback.disable();
			await microtasks();
			playback.restore();
			await microtasks();
			expect(playback.error.peek()).toBe(failure);
			expect(playback.codecs()).toBe(1);
		} finally {
			playback.close();
		}
	});
});

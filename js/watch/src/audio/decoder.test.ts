import { afterEach, beforeEach, describe, expect, it, mock, spyOn } from "bun:test";
import { readFileSync } from "node:fs";
import { Manual, Micro, Milli, Nano, now } from "@moq/clock";
import * as Catalog from "@moq/hang/catalog";
import * as Container from "@moq/hang/container";
import * as Moq from "@moq/net";
import { Effect, Signal } from "@moq/signals";
import type { Broadcast } from "../broadcast";
import { type Delay, Sync } from "../sync";
import { Decoder as VideoDecoder } from "../video/decoder";
import { Source as VideoSource } from "../video/source";
import type { Message, State } from "./render";
import { AudioRingBuffer } from "./ring-buffer";
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

// Every worklet built.
let worklets: FakeWorklet[] = [];

// The render worklet's side of the port, synchronous: each message applies to a real ring, and
// every quantum read reports the ring's state back, the way `render-worklet.ts` does.
class FakeWorklet {
	ring?: AudioRingBuffer;
	#epoch = 0;
	#playing = true;
	readonly port = Object.assign(new EventTarget(), {
		postMessage: (msg: Message) => {
			if (msg.type === "init") {
				this.ring = new AudioRingBuffer(msg);
				this.#playing = msg.playing ?? true;
			} else if (msg.type === "data") this.ring?.write(msg.timestamp, msg.data);
			else if (msg.type === "latency") {
				this.ring?.setLatency(msg.latency);
				if (msg.park !== undefined) {
					this.ring?.park();
					this.#epoch = msg.park;
				}
			} else if (msg.type === "truncate") this.ring?.truncate(msg.timestamp);
			else if (msg.type === "reset") {
				this.ring?.reset();
				this.#epoch = msg.epoch;
				this.#playing = msg.playing ?? true;
			} else if (msg.type === "playing" && msg.epoch === this.#epoch) this.#playing = msg.playing;
		},
		start() {},
	});
	constructor() {
		worklets.push(this);
	}
	get stalled(): boolean {
		return this.ring?.stalled ?? true;
	}
	get length(): number {
		return this.ring?.length ?? 0;
	}
	// Render one quantum and report the ring's state back to the main thread.
	read(output: Float32Array[]): number {
		const ring = this.ring;
		if (!ring) return 0;
		const read = this.#playing ? ring.read(output) : 0;
		const state: State = {
			type: "state",
			timestamp: ring.timestamp,
			stalled: ring.stalled,
			anchored: ring.anchored,
			underruns: ring.underruns,
			epoch: this.#epoch,
		};
		this.port.dispatchEvent(new MessageEvent("message", { data: state }));
		return read;
	}
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
// Thrown by the next `decode`, as WebCodecs does synchronously for a chunk it refuses.
let decodeThrows: Error | undefined;
let deferAudioOutput = false;
let pendingAudio: (() => void)[] = [];
let audioInputs: number[] = [];

beforeEach(() => {
	frameTimestamp = 0;
	codecs = 0;
	codecErrors = [];
	decodeThrows = undefined;
	deferAudioOutput = false;
	pendingAudio = [];
	audioInputs = [];
	contexts = [];
	worklets = [];
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
			pendingAudio = [];
		}
		decode(chunk: { timestamp: number; type: EncodedAudioChunkType }) {
			if (decodeThrows) throw decodeThrows;
			if (this.needsKey && chunk.type !== "key") throw new DOMException("key chunk required", "DataError");
			this.needsKey = false;
			audioInputs.push(chunk.timestamp);
			const emit = () => this.init.output(new FakeData(chunk.timestamp) as unknown as AudioData);
			if (deferAudioOutput) pendingAudio.push(emit);
			else emit();
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

// Hands frames to whichever container consumer is reading, and ends a consumer's reads on close. An
// Error stands in for a malformed read.
function feed() {
	let currentConsumer: Container.Consumer | undefined;
	const closed = new WeakSet<Container.Consumer>();
	const queue: (Read | Error)[] = [];
	let waiter:
		| { consumer: Container.Consumer; resolve: (read: Read | undefined) => void; reject: (err: Error) => void }
		| undefined;

	spyOn(Container.Consumer.prototype, "next").mockImplementation(function (this: Container.Consumer) {
		currentConsumer = this;
		if (closed.has(this)) return Promise.resolve(undefined);
		const read = queue.shift();
		if (read instanceof Error) return Promise.reject(read);
		if (read) return Promise.resolve(read);
		return new Promise<Read | undefined>((resolve, reject) => {
			waiter = { consumer: this, resolve, reject };
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

	const push = (read: Read | Error) => {
		const current = waiter;
		waiter = undefined;
		if (!current) queue.push(read);
		else if (read instanceof Error) current.reject(read);
		else current.resolve(read);
	};
	return {
		push,
		// Network arrivals can advance while next() is held behind decoder backpressure.
		buffered: (ranges: Container.BufferedRanges) => {
			if (!currentConsumer) throw new Error("consumer has not started");
			(currentConsumer.buffered as Signal<Container.BufferedRanges>).set(ranges);
		},
	};
}

async function play(initial: Delay, buffered = false, cmaf = false) {
	// These cases test timeline handover after the optional decoder has loaded.
	if (cmaf) await import("@moq/hang/cmaf");
	const { push, buffered: queued } = feed();
	const truncate = spyOn(AudioRingBuffer.prototype, "truncate");
	const reset = spyOn(AudioRingBuffer.prototype, "reset");
	const insert = spyOn(AudioRingBuffer.prototype, "write");

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
				init: JSON.parse(readFileSync(new URL("../../../hang/src/cmaf/fixtures.json", import.meta.url), "utf8"))
					.audioInit,
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
	const buffer = new Signal(Milli(buffered ? 1000 : 0));
	const sync = new Sync({ delay, buffer });
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
		buffer,
		sync,
		push,
		queued,
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
		// The resolved playout target.
		target: () => sync.out.delay.peek(),
		// The rings written to, and the timestamps of every frame that reached one.
		rings: () => worklets.filter((worklet) => worklet.ring && insert.mock.contexts.includes(worklet.ring)),
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

for (const cmaf of [false, true]) {
	it(`a ${cmaf ? "CMAF" : "legacy"} gap releases accounting for codec outputs it discarded`, async () => {
		const clock = new Manual();
		const installed = clock.install();
		deferAudioOutput = true;
		const playback = await play(Milli(100), true, cmaf);
		try {
			for (let n = 0; n < 6; n++) {
				playback.push({ ...frame(), group: n === 0 ? 0 : 1 });
				await microtasks();
			}
			expect(audioInputs).toHaveLength(6);
			expect(pendingAudio).toHaveLength(6);
			expect(playback.inserted()).toHaveLength(0);
			playback.push({ kind: "gap", timestamp: Micro(140_000), duration: Micro(20_000) });
			await microtasks();
			expect(pendingAudio).toHaveLength(0);
			deferAudioOutput = false;
			frameTimestamp = 160_000;
			const resumed = frame();
			playback.push({ ...resumed, group: 2 });
			await microtasks();
			expect(audioInputs).toHaveLength(7);
			expect(playback.inserted()).toContain(resumed.frame.timestamp);
			await playback.play(5);
			expect(playback.sync.playing.peek()).toBe(true);
		} finally {
			playback.close();
			await microtasks();
			installed.close();
		}
	});
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

// A subscription's head arrives newest-first, one group per frame. Chrome's AudioDecoder stamps every
// output from its first input, so decoding the older frames after it shifts the rest of the track.
for (const cmaf of [false, true]) {
	it(`decodes a ${cmaf ? "CMAF" : "legacy"} buffered subscription head in timestamp order`, async () => {
		const playback = await play(Milli(100), true, cmaf);
		try {
			for (const ms of [140, 120, 100, 80, 60, 40, 20, 0]) {
				frameTimestamp = (ms - 20) * 1000;
				playback.push({ ...frame(), group: ms / 20 });
				await microtasks();
			}
			expect(playback.inserted()).toEqual([]);

			// The live edge moves on, so the head is complete and plays from its oldest frame, refilling
			// the 100 ms floor.
			frameTimestamp = 140_000;
			playback.push({ ...frame(), group: 8 });
			await microtasks();
			expect(playback.inserted()).toEqual([0, 20_000, 40_000, 60_000, 80_000]);
		} finally {
			playback.close();
			await microtasks();
		}
	});

	// A buffered head above the decoded floor waits in the reorder buffer, not the consumer, until it
	// runs past the target. Those frames are what cover the part of the target held encoded.
	it(`starts a ${cmaf ? "CMAF" : "legacy"} buffered head held past a target above the floor`, async () => {
		const playback = await play(Milli(1000), true, cmaf);
		try {
			const target = playback.target();
			expect(target).toBeGreaterThan(500);
			while (frameTimestamp <= target * 1000 + 20_000) {
				playback.push(frame());
				await microtasks();
			}
			expect(playback.inserted()[0]).toBe(20_000);
		} finally {
			playback.close();
			await microtasks();
		}
	});

	// Pre-skip longer than the decoded floor discards every output the refill released toward it, so
	// the refill has to count what reaches the ring, not what it released.
	if (!cmaf) {
		it("starts a buffered Opus head whose pre-skip outlasts the floor", async () => {
			const playback = await play(Milli(1000), true);
			try {
				// OpusHead, version 1, stereo, a 520 ms pre-skip of 24,960 samples: past the 500 ms floor, yet
				// within the head the harness covers, since it queues nothing behind a released batch.
				playback.restore({ description: "4f707573486561640102" + "8061" + "80bb0000" + "0000" + "00" });
				await microtasks();
				const target = playback.target();
				while (frameTimestamp <= (target + 2000) * 1000) {
					playback.push(frame());
					await microtasks();
				}
				// The first output to outlast the pre-skip lands at the start of the timeline.
				expect(playback.inserted()[0]).toBe(20_000);
			} finally {
				playback.close();
				await microtasks();
			}
		});
	}

	// A fresh encoder after a declared end may restart its timeline below the old one, and the decoder
	// resets for it before anything is judged out of order.
	it(`decodes a ${cmaf ? "CMAF" : "legacy"} fresh encoder below the timeline it replaced`, async () => {
		const playback = await play(Milli(100), false, cmaf);
		try {
			frameTimestamp = 480_000;
			playback.push({ ...frame(), group: 0 });
			await microtasks();
			playback.push({ kind: "end", group: 0, end: Micro(520_000) });
			frameTimestamp = 0;
			playback.push({ ...frame(), group: 1 });
			await microtasks();
			expect(playback.inserted()).toEqual([500_000, 20_000]);
		} finally {
			playback.close();
			await microtasks();
		}
	});

	it(`skips a ${cmaf ? "CMAF" : "legacy"} live subscription head older than its live edge`, async () => {
		const playback = await play(Milli(100), false, cmaf);
		try {
			for (const ms of [140, 120, 100, 80, 60, 40, 20, 0, 160]) {
				frameTimestamp = (ms - 20) * 1000;
				playback.push({ ...frame(), group: ms / 20 });
				await microtasks();
			}
			expect(playback.inserted()).toEqual([140_000, 160_000]);
		} finally {
			playback.close();
			await microtasks();
		}
	});
}

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
function drain(ring: FakeWorklet): number {
	const output = [new Float32Array(128), new Float32Array(128)];
	let total = 0;
	for (;;) {
		const read = ring.read(output);
		if (read === 0) return total;
		total += read;
	}
}

describe("Decoder across a rendition's absence", () => {
	it("plays the tail, suspends once drained, and resumes on return", async () => {
		const playback = await play(Milli(100));
		try {
			await playback.play(12);
			const [ring] = playback.rings();
			// The worklet reports the ring playing once it renders a quantum.
			expect(ring.read([new Float32Array(128), new Float32Array(128)])).toBe(128);
			expect(ring.stalled).toBe(false);
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

	it("holds a disabled rendition and keeps one subscription across mute and unmute", async () => {
		const playback = await play(Milli(100));
		try {
			await playback.play(12);
			expect(playback.track()).toBe("audio");

			playback.disable();
			await microtasks();
			expect(playback.track()).toBe("audio");
			expect(contexts).toHaveLength(1);

			playback.restore();
			await microtasks();
			expect(playback.track()).toBe("audio");
			const before = playback.inserted().length;
			const timestamps = await playback.play(6);

			expect(contexts).toHaveLength(1);
			expect(contexts[0].calls.close).toBe(0);
			expect(playback.subscriptions()).toBe(1);
			expect(playback.inserted().slice(before)).toEqual(timestamps);
		} finally {
			playback.close();
		}
	});

	it("suspends a held muted tile once drained, and resumes on unmute", async () => {
		const playback = await play(Milli(100));
		try {
			await playback.play(12);
			const [ring] = playback.rings();
			expect(ring.read([new Float32Array(128), new Float32Array(128)])).toBe(128);
			await microtasks();

			playback.disable();
			await microtasks();
			const [context] = contexts;
			expect(context.calls.suspend).toBe(0);

			expect(drain(ring)).toBeGreaterThan(0);
			await microtasks();
			expect(context.calls.suspend).toBe(1);
			expect(context.state).toBe("suspended");
			expect(playback.track()).toBe("audio");

			playback.restore();
			await microtasks();
			expect(context.state).toBe("running");
			expect(playback.subscriptions()).toBe(1);
		} finally {
			playback.close();
		}
	});

	it("drops a held rendition once it is removed", async () => {
		const playback = await play(Milli(100));
		try {
			await playback.play(12);
			playback.disable();
			await microtasks();
			expect(playback.track()).toBe("audio");

			playback.remove();
			await microtasks();
			expect(playback.track()).toBeUndefined();

			// Its return is a fresh selection, with a fresh subscription.
			playback.restore();
			await microtasks();
			expect(playback.track()).toBe("audio");
			await playback.play(6);
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

	for (const [name, patch] of Object.entries({
		container: { container: { kind: "loc" } },
		codec: { codec: "mp4a.40.2" },
	})) {
		it(`keeps compatible PCM and Sync when the ${name} changes on the same instance`, async () => {
			const clock = new Manual();
			const installed = clock.install();
			const playback = await play(Milli(100), true);
			const reset = spyOn(playback.sync, "reset");
			try {
				await playback.play(7);
				const ring = playback.rings().at(-1);
				if (!ring) throw new Error("missing audio ring");
				const held = ring.length;
				const reference = playback.sync.out.reference.peek();
				expect(held).toBeGreaterThan(0);
				playback.restore(patch);
				await microtasks();
				expect(playback.subscriptions()).toBe(2);
				expect(playback.codecs()).toBe(2);
				expect(contexts).toHaveLength(1);
				expect(playback.rings()).toEqual([ring]);
				expect(ring.length).toBe(held);
				expect(playback.resets()).toBe(0);
				expect(reset).not.toHaveBeenCalled();
				expect(playback.sync.out.reference.peek()).toBe(reference);
				expect(ring.read([new Float32Array(960), new Float32Array(960)])).toBe(960);
				await playback.play();
				expect(reset).not.toHaveBeenCalled();
			} finally {
				playback.close();
				await microtasks();
				installed.close();
			}
		});
	}

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
	// A failure in the read loop, not a WebCodecs callback, must still reach `error` and release the
	// subscription, instead of leaving the player apparently live.
	for (const cmaf of [false, true]) {
		for (const failure of ["a malformed read", "a decode that throws"] as const) {
			it(`reports ${failure} on ${cmaf ? "CMAF" : "legacy"} and releases its subscription`, async () => {
				const close = spyOn(Moq.Track.Subscriber.prototype, "close");
				const playback = await play(Milli(100), false, cmaf);
				try {
					await playback.play(2);
					const before = close.mock.calls.length;
					const error = new Error(failure);
					if (failure === "a malformed read") {
						playback.push(error);
					} else {
						decodeThrows = error;
						await playback.play(1);
					}
					await microtasks();
					expect(playback.error.peek()).toBe(error);
					expect(close.mock.calls.length).toBeGreaterThan(before);
				} finally {
					playback.close();
					close.mockRestore();
				}
			});
		}
	}

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

// A manual clock installed at `ms`, so arrivals are stamped only as fast as a test advances it.
function manualAt(ms: number) {
	const clock = new Manual();
	clock.jump(Nano.fromMilli(Milli(ms)));
	return { advance: (by: number) => clock.advance(Nano.fromMilli(Milli(by))), installed: clock.install() };
}

// A legacy container frame: the media timestamp, then one payload byte.
function legacyFrame(media: Micro): Moq.Group.Frame {
	const timestamp = Moq.Varint.encode(media);
	const payload = new Uint8Array(timestamp.byteLength + 1);
	payload.set(timestamp, 0);
	return { payload, timestamp: Moq.Time.Timestamp.fromMicros(media) };
}

// A live "auto" playback reading real tracks through real container consumers on a manual clock.
// One 20ms Opus frame per group, flushed in runs of `burst`, so each run's first frame lands that
// much late. Each tick renders 20ms from the decoder's ring the way the worklet does.
async function livePlayback(burst: number) {
	const FRAME = 20;
	const clock = manualAt(0);
	const insert = spyOn(AudioRingBuffer.prototype, "write");

	const audio = Catalog.AudioConfigSchema.parse({
		codec: "opus",
		container: { kind: "legacy" },
		sampleRate: 48_000,
		numberOfChannels: 2,
	});
	const catalog = new Signal<Catalog.Root>({ audio: { renditions: { audio } } });
	// The broadcast the rendition reads, which a `broadcast` override can move to another path.
	let producer = new Moq.Broadcast.Producer();
	const media = new Signal<Moq.Broadcast.Consumer>(producer.consume());
	const broadcast = new Signal<Broadcast | undefined>({
		in: { enabled: new Signal(true) },
		out: { catalog, active: new Signal({ closed: {} }) },
		relativeBroadcast: (effect: Effect) => effect.get(media),
	} as unknown as Broadcast);
	const source = new Source({ broadcast, supported: async () => true });
	const sync = new Sync({ delay: new Signal<Delay>("auto") });
	// Published before the decoder subscribes, since a broadcast with no request handler refuses
	// an unknown track.
	let track = producer.createTrack("audio", { timescale: Moq.Time.Timescale.MILLI });
	const decoder = new Decoder({ source, sync });

	const samples = (48_000 * FRAME) / 1000;
	const output = [new Float32Array(samples), new Float32Array(samples)];
	const counts = { inserted: 0, played: 0, silent: 0 };
	let frame = 0;
	let sequence = 0;
	let held: number[] = [];

	const ring = () => worklets.findLast((worklet) => worklet.ring && insert.mock.contexts.includes(worklet.ring));
	return {
		sync,
		error: decoder.out.error,
		arrivals: () => decoder.arrivals(),
		// Lose one whole group without changing the following media timestamps or arrival cadence.
		async hole() {
			if (held.length) throw new Error("a hole needs an empty delivery burst");
			sequence++;
			frame++;
			await clock.advance(FRAME);
		},
		counts,
		ring,
		get burst() {
			return burst;
		},
		set burst(value: number) {
			burst = value;
		},
		// Rename the rendition, publishing the rest of the timeline on the new track.
		rename(name: string, patch: Record<string, unknown> = {}) {
			track.finish();
			track.close();
			track = producer.createTrack(name, { timescale: Moq.Time.Timescale.MILLI });
			sequence = 0;
			catalog.set({ audio: { renditions: { [name]: Catalog.AudioConfigSchema.parse({ ...audio, ...patch }) } } });
		},
		// Serve the same rendition from another path, as a `broadcast` override does.
		move(path: string) {
			media.peek().close();
			producer = new Moq.Origin.Producer().createBroadcast(Moq.Path.from(path)).broadcast;
			track = producer.createTrack("audio", { timescale: Moq.Time.Timescale.MILLI });
			sequence = 0;
			media.set(producer.consume());
		},
		// Remove the rendition from the catalog.
		remove: () => catalog.set({}),
		// Publish every frame due through `until`, rendering 20ms per tick.
		async run(until: number) {
			for (let t = frame * FRAME; t <= until; t += FRAME) {
				await clock.advance(FRAME);
				for (; frame * FRAME <= t; frame++) {
					held.push(frame * FRAME);
					if (held.length < burst) continue;
					for (const timestamp of held) {
						const group = track.createGroup(sequence++);
						group.writeFrame(legacyFrame(Micro.fromMilli(Milli(timestamp))));
						group.finish();
					}
					held = [];
				}
				await new Promise((resolve) => setImmediate(resolve));
				await microtasks();

				const current = ring();
				if (!current || current.stalled) continue;
				const read = current.read(output);
				counts.played += read;
				if (read < samples) counts.silent++;
			}
			counts.inserted = insert.mock.calls.reduce((sum, [, data]) => sum + (data as Float32Array[])[0].length, 0);
		},
		async close() {
			decoder.close();
			source.close();
			sync.close();
			media.peek().close();
			producer.close();
			await microtasks();
			clock.installed.close();
		},
	};
}

it("a missing live group re-primes without clearing decoded PCM or resetting Sync", async () => {
	const playback = await livePlayback(1);
	const reset = spyOn(playback.sync, "reset");
	const prime = spyOn(AudioDecoder.prototype, "reset");
	const arrivals = playback.arrivals();
	const received: { kind: string }[] = [];
	const reading = (async () => {
		for await (const arrival of arrivals) received.push(arrival);
	})();
	try {
		await playback.run(1000);
		const ring = playback.ring();
		expect(ring?.stalled).toBe(false);
		const buffered = ring?.length;
		await playback.hole();
		expect(ring?.length).toBe(buffered);
		await playback.run(2000);
		expect(received.some((arrival) => arrival.kind === "gap")).toBe(true);
		expect(prime).toHaveBeenCalledTimes(1);
		expect(reset).not.toHaveBeenCalled();
		expect(playback.ring()).toBe(ring);
		expect(ring?.stalled).toBe(false);
		expect(playback.error.peek()).toBeUndefined();
	} finally {
		arrivals.close();
		await reading;
		await playback.close();
	}
});

describe("Decoder across a rename", () => {
	// A codec change renames the audio track, and the replacement's consumer reads the same path.
	// Clearing the estimate at teardown and restarting it from the cold-start prior dropped the
	// shared "auto" delay and climbed back as the new track relearned the lateness, so the ring
	// skipped ahead on the drop and went silent refilling on each rise.
	it("keeps the delay, so the ring neither skips nor goes silent", async () => {
		// Flushed in runs of 12, so each run's first frame lands ~220ms late.
		const playback = await livePlayback(12);
		try {
			// Long enough for the estimate to settle on the lateness this path shows.
			await playback.run(20_000);
			const delay = playback.sync.out.delay.peek();
			expect(delay).toBeGreaterThan(Milli(200));
			const ring = playback.ring();
			const before = { ...playback.counts, buffered: ring?.length ?? 0 };
			let lowest = delay;
			const watch = new Effect();
			watch.subscribe(playback.sync.out.delay, (value) => {
				lowest = Milli(Math.min(lowest, value));
			});

			playback.rename("audio/1");
			await playback.run(26_000);
			watch.close();

			const { inserted, played, silent } = playback.counts;
			const buffered = playback.ring()?.length ?? 0;
			const skipped = inserted - before.inserted - (played - before.played) - (buffered - before.buffered);
			expect({ skipped, silent: silent - before.silent }).toEqual({ skipped: 0, silent: 0 });
			expect(playback.ring()).toBe(ring);
			expect(lowest).toBeGreaterThanOrEqual(delay);
		} finally {
			await playback.close();
		}
	});

	// Another path's arrivals say nothing about this one's, so a rendition read from elsewhere
	// learns its own estimate instead of inheriting the outgoing one.
	it("does not carry the delay to another path", async () => {
		const playback = await livePlayback(12);
		try {
			await playback.run(20_000);
			const warm = playback.sync.out.delay.peek();
			expect(warm).toBeGreaterThan(Milli(200));

			// The same media from another path, where every frame lands on time.
			playback.move("elsewhere");
			playback.burst = 1;
			await playback.run(22_000);

			expect(playback.sync.out.delay.peek()).toBeLessThan(warm);
		} finally {
			await playback.close();
		}
	});

	// The probe accepts a CMAF init segment it cannot parse for Opus, so setup throws before the
	// replacement's consumer exists. Nothing replaces the outgoing estimate, so it must not linger.
	it("reports a replacement that fails to set up and stops holding the delay", async () => {
		const playback = await livePlayback(12);
		try {
			await playback.run(20_000);
			const warm = playback.sync.out.delay.peek();
			expect(warm).toBeGreaterThan(Milli(200));

			playback.rename("audio/1", { container: { kind: "cmaf", init: "AAAA" } });
			await microtasks();

			expect(playback.error.peek()).toBeInstanceOf(Error);
			expect(playback.sync.out.delay.peek()).toBeLessThan(warm);
		} finally {
			await playback.close();
		}
	});

	it("stops holding the delay once the rendition is removed", async () => {
		const playback = await livePlayback(12);
		try {
			await playback.run(20_000);
			const warm = playback.sync.out.delay.peek();
			expect(warm).toBeGreaterThan(Milli(200));

			playback.remove();
			await microtasks();

			expect(playback.sync.out.delay.peek()).toBeLessThan(warm);
		} finally {
			await playback.close();
		}
	});
});

it("keeps audio's opening buffered while an enabled video track fills", async () => {
	const clock = new Manual();
	const installed = clock.install();
	const playback = await play(Milli(100), true);
	const video = new Signal<{ start: Milli; end: Milli } | null>(null);
	const remove = playback.sync.register(new Signal(Milli(100)), video);
	try {
		await playback.play(8);
		await clock.advance(Nano.zero);
		const ring = playback.rings()[0];
		expect(ring).toBeDefined();
		const opening = ring.ring?.timestamp;
		for (let i = 0; i < 5; i++) expect(ring.read([new Float32Array(960), new Float32Array(960)])).toBe(0);
		await clock.advance(Nano.fromMilli(Milli(500)));
		expect(ring.ring?.timestamp).toBe(opening);
		video.set({ start: Milli(20), end: Milli(120) });
		await clock.advance(Nano.zero);
		expect(ring.read([new Float32Array(960), new Float32Array(960)])).toBe(960);
		expect(ring.ring?.timestamp).toBe(Micro((opening ?? 0) + 20_000));
	} finally {
		remove();
		playback.close();
		await microtasks();
		installed.close();
	}
});

it("starts above the decoded audio floor without expanding PCM storage", async () => {
	const clock = new Manual();
	const installed = clock.install();
	const playback = await play(Milli(1000), true);
	const video = new Signal<{ start: Milli; end: Milli } | null>(null);
	const remove = playback.sync.register(new Signal(Milli(1000)), video);
	try {
		await playback.play(80);
		await clock.advance(Nano.zero);
		const ring = playback.rings()[0];
		expect(ring).toBeDefined();
		expect(ring.ring?.length).toBeLessThanOrEqual(24_000);
		expect(playback.sync.playing.peek()).toBe(false);
		const opening = ring.ring?.timestamp;
		for (let i = 0; i < 5; i++) expect(ring.read([new Float32Array(960), new Float32Array(960)])).toBe(0);
		await clock.advance(Nano.fromMilli(Milli(500)));
		expect(ring.ring?.timestamp).toBe(opening);
		video.set({ start: Milli(20), end: Milli(1020) });
		await clock.advance(Nano.zero);
		expect(ring.read([new Float32Array(960), new Float32Array(960)])).toBe(960);
		expect(ring.ring?.timestamp).toBe(Micro((opening ?? 0) + 20_000));
	} finally {
		remove();
		playback.close();
		await microtasks();
		installed.close();
	}
});

it("waits for video support before allowing audio-only startup", async () => {
	const playback = await play(Milli(100), true);
	let resolve!: (supported: boolean) => void;
	const source = new VideoSource({
		broadcast: {
			out: {
				catalog: new Signal({
					video: {
						renditions: {
							video: { codec: "vp8", codedWidth: 16, codedHeight: 16, container: { kind: "legacy" } },
						},
					},
				}),
			},
		} as unknown as Broadcast,
		supported: () =>
			new Promise<boolean>((done) => {
				resolve = done;
			}),
	});
	const video = new VideoDecoder({ source, sync: playback.sync });
	try {
		await microtasks();
		await playback.play(8);
		const ring = playback.rings()[0];
		expect(ring).toBeDefined();
		expect(ring.read([new Float32Array(960), new Float32Array(960)])).toBe(0);
		resolve(false);
		await microtasks();
		expect(ring.read([new Float32Array(960), new Float32Array(960)])).toBe(960);
	} finally {
		video.close();
		source.close();
		playback.close();
		await microtasks();
	}
});

for (const cmaf of [false, true]) {
	it(`a ${cmaf ? "CMAF" : "legacy"} hole re-primes the codec while keeping PCM and the clock`, async () => {
		const clock = new Manual();
		const installed = clock.install();
		const playback = await play(Milli(100), true, cmaf);
		const reset = spyOn(playback.sync, "reset");
		const prime = spyOn(AudioDecoder.prototype, "reset");
		try {
			await playback.play(7);
			expect(playback.sync.playing.peek()).toBe(true);
			const ring = playback.rings().at(-1);
			if (!ring) throw new Error("missing audio ring");
			ring.read([new Float32Array(960), new Float32Array(960)]);
			await microtasks();
			const held = ring.length;
			const reference = playback.sync.out.reference.peek();
			const resets = playback.resets();
			const parked = playback.sync.wait(Milli(1000));
			expect(held).toBeGreaterThan(0);
			playback.push({ kind: "gap", timestamp: Micro(160_000), duration: Micro(20_000) });
			await microtasks();
			expect(prime).toHaveBeenCalledTimes(1);
			expect(reset).not.toHaveBeenCalled();
			expect(playback.resets()).toBe(resets);
			expect(ring.length).toBe(held);
			expect(playback.sync.out.reference.peek()).toBe(reference);
			expect(playback.sync.playing.peek()).toBe(true);
			expect(Bun.peek.status(parked)).toBe("pending");
			frameTimestamp = 160_000;
			await playback.play(1);
			expect(playback.sync.playing.peek()).toBe(true);
			expect(ring.read([new Float32Array(960), new Float32Array(960)])).toBe(960);
			await clock.advance(Nano.fromMilli(Milli(1200)));
			expect(await parked).toBe(true);
		} finally {
			playback.close();
			await microtasks();
			installed.close();
		}
	});
}

it.each([100, 1000])(
	"closing during shared startup at %dms releases the audio graph and backpressured reader",
	async (delay) => {
		const playback = await play(Milli(delay), true);
		const video = new Signal<{ start: Milli; end: Milli } | null>(null);
		const remove = playback.sync.register(new Signal(Milli(delay)), video);
		const disconnect = spyOn(FakeWorklet.prototype, "disconnect");
		const closeTrack = spyOn(Moq.Track.Subscriber.prototype, "close");
		try {
			await playback.play(delay / 20 + 30);
			expect(playback.sync.playing.peek()).toBe(false);
			const context = playback.context.peek() as unknown as FakeContext;
			expect(context).toBeDefined();
			const decoded = playback.inserted().length;
			playback.close();
			await microtasks();
			expect(context.calls.close).toBe(1);
			expect(disconnect).toHaveBeenCalled();
			expect(closeTrack).toHaveBeenCalled();
			// The other track becoming ready cannot revive the cancelled audio reader.
			video.set({ start: Milli(0), end: Milli(delay) });
			await microtasks();
			expect(playback.inserted()).toHaveLength(decoded);
		} finally {
			remove();
			playback.close();
			await microtasks();
			disconnect.mockRestore();
			closeTrack.mockRestore();
		}
	},
);

it("a rejected audio support probe does not leave buffered video waiting on a pending track", async () => {
	const sync = new Sync({ delay: Milli(100), buffer: Milli(1000) });
	const remove = sync.register(new Signal(Milli(100)), new Signal({ start: Milli(0), end: Milli(100) }));
	let reject!: (error: Error) => void;
	const failure = new Error("audio support probe failed");
	const logged = spyOn(console, "warn").mockImplementation(() => {});
	const source = new Source({
		broadcast: {
			in: { enabled: new Signal(true) },
			out: {
				catalog: new Signal({
					audio: {
						renditions: {
							audio: Catalog.AudioConfigSchema.parse({
								codec: "opus",
								container: { kind: "legacy" },
								sampleRate: 48000,
								numberOfChannels: 2,
							}),
						},
					},
				}),
			},
		} as unknown as Broadcast,
		supported: () =>
			new Promise<boolean>((_resolve, fail) => {
				reject = fail;
			}),
	});
	const decoder = new Decoder({ source, sync });
	try {
		await microtasks();
		sync.received(Milli(0));
		await microtasks();
		expect(sync.playing.peek()).toBe(false);
		reject(failure);
		await microtasks();
		expect(source.out.available.peek()).toEqual({});
		expect(source.pending.peek()).toBe(false);
		expect(source.out.config.peek()).toBeUndefined();
		expect(sync.playing.peek()).toBe(true);
	} finally {
		decoder.close();
		source.close();
		remove();
		sync.close();
		logged.mockRestore();
	}
});

it.each([false, true])("releases shared startup across a hole in queued audio (CMAF=%s)", async (cmaf) => {
	const clock = new Manual();
	const installed = clock.install();
	const playback = await play(Milli(1500), true, cmaf);
	const video = new Signal<{ start: Milli; end: Milli } | null>({ start: Milli(20), end: Milli(1520) });
	const remove = playback.sync.register(new Signal(Milli(1500)), video);
	try {
		await playback.play(25);
		// The audio after the hole counts, the hole itself does not.
		playback.queued([
			{ start: Milli(520), end: Milli(620) },
			{ start: Milli(1500), end: Milli(1600) },
		]);
		const next = frame();
		next.group = 1;
		playback.push(next);
		await microtasks();
		await clock.advance(Nano.zero);
		const ring = playback.rings()[0];
		expect(ring.ring?.length).toBe(24000);
		expect(playback.sync.playing.peek()).toBe(false);
		expect(ring.read([new Float32Array(960), new Float32Array(960)])).toBe(0);
		// Enough arrives past the hole while it stays open.
		playback.queued([
			{ start: Milli(520), end: Milli(620) },
			{ start: Milli(1500), end: Milli(2500) },
		]);
		await clock.advance(Nano.zero);
		expect(playback.sync.playing.peek()).toBe(true);
		expect(ring.read([new Float32Array(960), new Float32Array(960)])).toBe(960);
	} finally {
		remove();
		playback.close();
		await microtasks();
		installed.close();
	}
});

for (const cmaf of [false, true]) {
	for (const buffered of [false, true]) {
		it(`switches a running ${cmaf ? "CMAF" : "legacy"} player to ${buffered ? "buffered" : "live"} audio`, async () => {
			const playback = await play(Milli(100), !buffered, cmaf);
			try {
				await playback.play(12);
				playback.buffer.set(Milli(buffered ? 1000 : 0));
				await microtasks();
				const inserted = playback.inserted().length;
				const start = frameTimestamp;
				for (const ms of [140, 120, 100, 80, 60, 40, 20, 0, 160]) {
					frameTimestamp = start + (ms - 20) * 1000;
					playback.push({ ...frame(), group: 1 + ms / 20 });
					await microtasks();
				}
				expect(playback.inserted().slice(inserted)).toEqual(
					(buffered ? [0, 20, 40, 60, 80] : [140, 160]).map((ms) => start + ms * 1000),
				);

				// A running live ring sheds old audio when it gets ahead; a buffered ring plays it all.
				const ring = worklets.at(-1)?.ring;
				if (!ring) throw new Error("no running audio ring");
				ring.reset();
				const samples = [new Float32Array(4800), new Float32Array(4800)];
				for (const timestamp of [0, 100_000, 200_000]) ring.write(Micro(timestamp), samples);
				expect(ring.length).toBe(samples[0].length * (buffered ? 3 : 1));
			} finally {
				playback.close();
				await microtasks();
			}
		});
	}
}

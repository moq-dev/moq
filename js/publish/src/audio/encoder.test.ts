import { describe, expect, mock, test } from "bun:test";
import * as Moq from "@moq/net";
import { Time } from "@moq/net";
import { Signal } from "@moq/signals";
import { Baseline } from "../jitter";
import type { AudioFrame, Format } from "./capture";
import { Encoder, resolve } from "./encoder";

// Bun does not load Vite's worklet URL imports from the public audio entrypoint.
mock.module("./capture-worklet.ts?worklet", () => ({ default: "blob:fake-capture" }));

const Audio = await import("./index");

const captured: Format = { sampleRate: 48_000, channelCount: 2 };

describe("resolve", () => {
	test("keeps resolution out of the public audio namespace", () => {
		// @ts-expect-error Resolution is internal to the encoder.
		expect(Audio.resolve).toBeUndefined();
	});

	test("defaults Opus to 20ms", () => {
		const resolved = resolve(captured, "opus");
		expect(resolved.frameDuration).toBe(Time.Micro(20_000));
		expect(resolved.catalog.jitter).toBeUndefined();
	});

	// The exact frame duration does not imply encoder flush lateness.
	test("keeps a 2.5ms Opus frame exact without a catalog hint", () => {
		const resolved = resolve(captured, { mime: "opus", frameDuration: Time.Milli(2.5) });
		expect(resolved.frameDuration).toBe(Time.Micro(2_500));
		expect(resolved.catalog.jitter).toBeUndefined();
	});

	test("carries every Opus frame duration", () => {
		for (const millis of [2.5, 5, 10, 20, 40, 60]) {
			const resolved = resolve(captured, { mime: "opus", frameDuration: Time.Milli(millis) });
			expect(resolved.frameDuration).toBe(Time.Micro(millis * 1000));
		}
	});

	// Otherwise AudioEncoder.configure throws instead, by which point the rendition has already
	// been advertised and the failure lands on a subscriber rather than the caller.
	test("rejects a duration Opus cannot encode", () => {
		for (const millis of [2.5005, 15, 0, -20]) {
			expect(() => resolve(captured, { mime: "opus", frameDuration: Time.Milli(millis) })).toThrow();
		}
	});

	// AAC-LC has a fixed 1024-sample frame, so there is no duration to configure.
	test("leaves AAC without a frame duration", () => {
		const resolved = resolve(captured, "aac");
		expect(resolved.frameDuration).toBeUndefined();
		expect(resolved.catalog.jitter).toBeUndefined();
	});
});

// Like Chrome's Opus encoder, it holds the newest chunks until later input pushes them out, and
// stamps each chunk from the first input's timestamp plus the audio encoded since, so a jump in
// input timestamps never reaches the output.
class LaggingAudioEncoder {
	static readonly LAG = 2;

	// Called on configure; the encoder publishes its pipeline synchronously right after.
	static onConfigure: (() => void) | undefined;

	state: CodecState = "unconfigured";
	#output: EncodedAudioChunkOutputCallback;
	#held: { timestamp: number; duration: number }[] = [];
	#base: number | undefined;
	#encoded = 0;

	constructor(init: AudioEncoderInit) {
		this.#output = init.output;
	}

	configure(): void {
		this.state = "configured";
		LaggingAudioEncoder.onConfigure?.();
	}

	encode(data: AudioData): void {
		const duration = Math.round((data.numberOfFrames / data.sampleRate) * 1_000_000);
		this.#base ??= data.timestamp;
		this.#held.push({ timestamp: this.#base + this.#encoded, duration });
		this.#encoded += duration;
		while (this.#held.length > LaggingAudioEncoder.LAG) {
			const { timestamp, duration } = this.#held.shift() as { timestamp: number; duration: number };
			const chunk = {
				type: "key",
				timestamp,
				duration,
				byteLength: 1,
				copyTo: (dest: Uint8Array) => dest.set([1]),
			};
			this.#output(chunk as unknown as EncodedAudioChunk);
		}
	}

	reset(): void {
		this.state = "unconfigured";
		this.#held = [];
		this.#base = undefined;
		this.#encoded = 0;
	}

	close(): void {
		this.state = "closed";
	}
}

class FakeAudioData {
	readonly timestamp: number;
	readonly numberOfFrames: number;
	readonly sampleRate: number;

	constructor(init: AudioDataInit) {
		// WebIDL's `long long` conversion truncates a fractional timestamp.
		this.timestamp = Math.trunc(init.timestamp);
		this.numberOfFrames = init.numberOfFrames;
		this.sampleRate = init.sampleRate;
	}

	close(): void {}
}

function installFakeWebCodecs() {
	const names = ["AudioEncoder", "AudioDecoder", "AudioData"] as const;
	const originals = names.map((name) => Object.getOwnPropertyDescriptor(globalThis, name));
	const fakes = [LaggingAudioEncoder, class {}, FakeAudioData];
	names.forEach((name, i) => {
		Object.defineProperty(globalThis, name, { configurable: true, writable: true, value: fakes[i] });
	});

	return {
		[Symbol.dispose]() {
			names.forEach((name, i) => {
				const original = originals[i];
				if (original) Object.defineProperty(globalThis, name, original);
				else Reflect.deleteProperty(globalThis, name);
			});
		},
	};
}

// A capture stream that hands over one frame per read. The reader pushes each frame through the
// pipeline before reading again, so a pending read proves the previous frame was fully processed.
class Feed {
	readonly stream: ReadableStream<AudioFrame>;
	#deliver: ((frame: AudioFrame) => void) | undefined;
	#requested!: () => void;
	#request = this.#next();

	constructor() {
		this.stream = new ReadableStream<AudioFrame>(
			{
				pull: (controller) =>
					new Promise<void>((resolve) => {
						this.#deliver = (frame) => {
							controller.enqueue(frame);
							resolve();
						};
						this.#requested();
					}),
			},
			{ highWaterMark: 0 },
		);
	}

	#next(): Promise<void> {
		return new Promise((resolve) => {
			this.#requested = resolve;
		});
	}

	// Resolves once every frame pushed so far has been processed.
	async drain(): Promise<void> {
		await this.#request;
	}

	async push(frame: AudioFrame): Promise<void> {
		await this.drain();
		this.#request = this.#next();
		this.#deliver?.(frame);
	}
}

// An Encoder wired to a fake capture feed, recording each written frame as [timestamp, payload bytes].
async function setup(baseline = new Baseline()) {
	const configured = new Promise<void>((resolve) => {
		LaggingAudioEncoder.onConfigure = resolve;
	});

	const track = new Moq.Track.Producer("audio").accept();
	const written: [number, number][] = [];
	const writes = { onWrite: undefined as (() => void) | undefined };
	const writeFrame = track.writeFrame.bind(track);
	track.writeFrame = (frame) => {
		const [timestamp, payload] = Moq.Varint.decode(frame.payload);
		written.push([timestamp, payload.byteLength]);
		writeFrame(frame);
		writes.onWrite?.();
	};

	const rendition = {
		config: new Signal(undefined),
		track: new Signal<Moq.Track.Producer | undefined>(track),
		close: () => track.close(),
	};

	const feed = new Feed();
	const capture = {
		in: { source: new Signal(undefined) },
		out: {
			root: new Signal(undefined),
			format: new Signal<Format>({ sampleRate: 48_000, channelCount: 1 }),
			frames: new Signal({ subscribe: () => feed.stream }),
		},
	};

	const encoder = new Encoder("audio", {
		broadcast: { audio: () => rendition, baseline } as never,
		capture: capture as never,
	});

	await configured;
	LaggingAudioEncoder.onConfigure = undefined;

	return {
		encoder,
		track,
		rendition,
		feed,
		written,
		writes,
		[Symbol.dispose]() {
			encoder.close();
		},
	};
}

// The encoder outlives a demand gap, so chunks it held when demand disappeared surface after the
// resume. Written after the marker, they would put pre-gap media on the live edge, and a rounding
// step below the marker aborts every subscriber. The resumed chunks have to carry the capture clock,
// not the encoder's gap-blind one, or they trail the next gap's marker and are dropped as pre-gap.
test("a demand gap marks where submitted audio ends and drops the chunks held across it", async () => {
	using _webcodecs = installFakeWebCodecs();
	using env = await setup();
	const { track, rendition, feed, written, writes } = env;

	// One 20ms Opus frame per push, on a clock with a fractional microsecond origin.
	let index = 0;
	const push = async (count: number) => {
		for (let i = 0; i < count; i++, index++) {
			await feed.push({ timestamp: Time.Micro(18_699.6 + index * 20_000), channels: [new Float32Array(960)] });
		}
		await feed.drain();
	};

	await push(4); // two written, two held

	const marked = new Promise<void>((resolve) => {
		writes.onWrite = resolve;
	});
	rendition.track.set(undefined);
	await marked;
	writes.onWrite = undefined;

	await push(2); // gated
	rendition.track.set(track);
	await push(4); // releases the two held pre-gap chunks, then two resumed ones

	expect(written).toEqual([
		[18_700, 1],
		[38_700, 1],
		[98_700, 0],
		[138_700, 1],
		[158_700, 1],
	]);
});

// A push that completes several frames is still one continuous stream, so it must not restart the
// encoder and drop the chunks it holds.
test("a push completing several frames keeps the encoder running", async () => {
	using _webcodecs = installFakeWebCodecs();
	using env = await setup();
	const { feed, written } = env;

	// Two 20ms Opus frames per push.
	for (let index = 0; index < 3; index++) {
		await feed.push({ timestamp: Time.Micro(18_699.6 + index * 40_000), channels: [new Float32Array(1920)] });
	}
	await feed.drain();

	expect(written).toEqual([
		[18_700, 1],
		[38_700, 1],
		[58_700, 1],
		[78_700, 1],
	]);
});

// Another rendition on the same broadcast flushing with far less lateness leaves this one trailing
// it, which the catalog advertises as `delay`.
test("a rendition trailing the broadcast's earliest advertises delay", async () => {
	using _webcodecs = installFakeWebCodecs();
	const baseline = new Baseline();
	using env = await setup(baseline);
	const { encoder, feed } = env;

	expect(encoder.out.catalog.peek()?.delay).toBeUndefined();

	// A sibling that flushes each frame the instant it is captured.
	baseline.observe(0, performance.now() * 1000);

	// Captured 100ms ago, so this rendition flushes at least that late.
	const start = performance.now() * 1000 - 100_000;
	for (let index = 0; index < 4; index++) {
		await feed.push({ timestamp: Time.Micro(start + index * 20_000), channels: [new Float32Array(960)] });
	}
	await feed.drain();

	expect(env.written.length).toBe(2);
	expect(encoder.out.catalog.peek()?.delay).toBeGreaterThanOrEqual(100);
});

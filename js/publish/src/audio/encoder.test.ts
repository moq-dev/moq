import { describe, expect, mock, spyOn, test } from "bun:test";
import { Duration, Instant, now } from "@moq/clock";
import { Manual } from "@moq/clock/test";
import * as Catalog from "@moq/hang/catalog";
import * as Container from "@moq/hang/container";
import * as Json from "@moq/json";
import * as Moq from "@moq/net";
import { Signal } from "@moq/signals";
import { Broadcast } from "../broadcast";
import { Estimator, Order } from "../order";
import type { AudioFrame, Format } from "./capture";
import { type Codec, Encoder, groupSpacing, resolve, toEncoderConfig } from "./encoder";

/** Install a Manual clock reading `ms`, set forward the way a mocked `performance.now()` was. */
function manualAt(ms: number) {
	const clock = new Manual();
	const installed = clock.install();
	const set = (to: number) => clock.jump(Duration.millis(to).sub(clock.now().since(Instant.ZERO)));
	set(ms);
	return { set, close: () => installed.close(), [Symbol.dispose]: () => installed.close() };
}

// Bun does not load Vite's worklet URL imports from the public audio entrypoint.
mock.module("./capture-worklet.ts?worklet", () => ({ default: async () => "blob:fake-capture" }));

const Audio = await import("./index");

const captured: Format = { sampleRate: 48_000, channelCount: 2 };

describe("resolve", () => {
	test("keeps resolution out of the public audio namespace", () => {
		// @ts-expect-error Resolution is internal to the encoder.
		expect(Audio.resolve).toBeUndefined();
	});

	test("the default Opus catalog declares the RFC 7845 preroll", () => {
		// 80ms per RFC 7845 section 4.6, matching WARMUP in rs/moq-mux/src/codec/opus/mod.rs.
		expect(Number(resolve(captured, "opus").catalog.warmup)).toBe(80);
	});

	// The exact frame duration is not a jitter hint.
	test("keeps a 2.5ms Opus frame exact without a catalog hint", () => {
		const resolved = resolve(captured, { mime: "opus", frameDuration: Duration.millis(2.5) });
		expect(resolved.frameDuration).toEqual(Duration.micros(2_500));
		expect(resolved.catalog.jitter).toBeUndefined();
	});

	test("carries every Opus frame duration", () => {
		for (const millis of [2.5, 5, 10, 20, 40, 60]) {
			const resolved = resolve(captured, { mime: "opus", frameDuration: Duration.millis(millis) });
			expect(resolved.frameDuration).toEqual(Duration.micros(millis * 1000));
		}
	});

	// Otherwise AudioEncoder.configure throws instead, by which point the rendition has already
	// been advertised and the failure lands on a subscriber rather than the caller.
	test("rejects a duration Opus cannot encode", () => {
		for (const millis of [2.5005, 15, 0, -20]) {
			expect(() => resolve(captured, { mime: "opus", frameDuration: Duration.millis(millis) })).toThrow();
		}
	});

	// A frame opens a group once it is at least `join` past the open one's start, so groups start a
	// whole number of frames apart: that, not the requested join, is the effective one.
	test("groups start a whole number of frames apart", () => {
		const opus = (frame: number) => resolve(captured, { mime: "opus", frameDuration: Duration.millis(frame) });
		for (const [frame, duration, spacing] of [
			[20, 20, 20],
			[20, 30, 40],
			[60, 20, 60],
			[2.5, 20, 20],
			// A join shorter than a frame opens a group per frame.
			[20, 1, 20],
		] as const) {
			expect(groupSpacing(Duration.millis(duration), opus(frame))).toEqual(Duration.millis(spacing));
		}
		// AAC-LC's 1024 samples at the captured rate.
		const aac = resolve(captured, "aac");
		const frame = (1024 * 1000) / Number(aac.catalog.sampleRate);
		// Durations hold whole nanoseconds, so the spacing matches to the nanosecond.
		expect(groupSpacing(Duration.millis(20), aac).asMillis()).toBeCloseTo(frame, 6);
		expect(groupSpacing(Duration.millis(frame * 3), aac).asMillis()).toBeCloseTo(frame * 3, 6);
	});

	// AAC-LC has a fixed 1024-sample frame, so there is no duration to configure.
	test("leaves AAC without a frame duration", () => {
		const resolved = resolve(captured, "aac");
		expect(resolved.frameDuration).toBeUndefined();
		expect(resolved.catalog.warmup).toBeUndefined();
		expect(resolved.catalog.jitter).toBeUndefined();
	});
});

describe("toEncoderConfig", () => {
	test("configures voice without DTX", () => {
		const config = toEncoderConfig(resolve(captured, "opus"), "voice", {});
		expect(config.opus).toEqual({ application: "voip", signal: "voice", frameDuration: 20_000 } as never);
	});
});

// Like Chrome's Opus encoder, it emits one chunk per input and stamps each from the first input's
// timestamp plus the audio encoded since, so a jump in input timestamps never reaches the output.
// `LAG` holds that many of the newest chunks until later input pushes them out, a slower codec than
// any browser measured, for the tests that need one.
class FakeAudioEncoder {
	static instances: FakeAudioEncoder[] = [];
	static probes: AudioEncoderConfig[] = [];
	static description: ((config: AudioEncoderConfig) => Uint8Array) | undefined;
	static LAG = 0;

	// Called on configure; the encoder publishes its pipeline synchronously right after.
	static onConfigure: ((config: AudioEncoderConfig) => void) | undefined;

	state: CodecState = "unconfigured";
	#output: EncodedAudioChunkOutputCallback;
	#held: { timestamp: number; duration: number }[] = [];
	#base: number | undefined;
	#encoded = 0;
	#probe = false;
	// Configured this tick: the probe flushes at once, while the live pipeline flushes only before a break.
	#fresh = false;
	#config?: AudioEncoderConfig;
	#error: AudioEncoderInit["error"];

	constructor(init: AudioEncoderInit) {
		this.#output = init.output;
		this.#error = init.error;
		FakeAudioEncoder.instances.push(this);
	}

	fail(error: Error): void {
		this.state = "closed";
		this.#error(error as DOMException);
	}

	configure(config: AudioEncoderConfig): void {
		this.state = "configured";
		this.#config = config;
		this.#fresh = true;
		queueMicrotask(() => {
			this.#fresh = false;
			if (!this.#probe) FakeAudioEncoder.onConfigure?.(config);
		});
	}

	encode(data: AudioData): void {
		const duration = Math.round((data.numberOfFrames / data.sampleRate) * 1_000_000);
		this.#base ??= data.timestamp;
		this.#held.push({ timestamp: this.#base + this.#encoded, duration });
		this.#encoded += duration;
		while (this.#held.length > FakeAudioEncoder.LAG) {
			this.#emit(this.#held.shift() as { timestamp: number; duration: number });
		}
	}

	#emit({ timestamp, duration }: { timestamp: number; duration: number }): void {
		const chunk = { type: "key", timestamp, duration, byteLength: 1, copyTo: (dest: Uint8Array) => dest.set([1]) };
		const config = this.#config;
		const description = config && FakeAudioEncoder.description?.(config);
		const metadata =
			config && description
				? {
						decoderConfig: {
							codec: config.codec,
							sampleRate: config.sampleRate,
							numberOfChannels: config.numberOfChannels,
							description,
						},
					}
				: undefined;
		this.#output(chunk as unknown as EncodedAudioChunk, metadata);
	}

	async flush(): Promise<void> {
		if (this.#fresh) {
			this.#probe = true;
			if (this.#config) FakeAudioEncoder.probes.push(structuredClone(this.#config));
			const index = FakeAudioEncoder.instances.indexOf(this);
			if (index !== -1) FakeAudioEncoder.instances.splice(index, 1);
		}
		for (const chunk of this.#held.splice(0)) this.#emit(chunk);
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
	FakeAudioEncoder.instances = [];
	FakeAudioEncoder.probes = [];
	FakeAudioEncoder.LAG = 0;
	FakeAudioEncoder.description = undefined;
	const names = ["AudioEncoder", "AudioDecoder", "AudioData"] as const;
	const originals = names.map((name) => Object.getOwnPropertyDescriptor(globalThis, name));
	const fakes = [FakeAudioEncoder, class {}, FakeAudioData];
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

function fakeCapture(feed: Feed) {
	return {
		in: { source: new Signal(undefined) },
		out: {
			root: new Signal(undefined),
			format: new Signal<Format>({ sampleRate: 48_000, channelCount: 1 }),
			frames: new Signal({ subscribe: () => feed.stream }),
		},
		blocked: new Signal(false),
	};
}

// Resolves on the next AudioEncoder configure; the encoder publishes its pipeline synchronously after.
function configured(): Promise<AudioEncoderConfig> {
	return new Promise((resolve) => {
		FakeAudioEncoder.onConfigure = (config) => {
			FakeAudioEncoder.onConfigure = undefined;
			resolve(config);
		};
	});
}

// An Encoder wired to a fake capture feed, recording each written frame as [timestamp, payload bytes]
// and how many frames each group carries.
async function setup(order = new Order(), codec?: Codec, join?: Duration) {
	const configuring = configured();

	const track = new Moq.Track.Producer("audio").accept({ timescale: Moq.Time.Timescale.MILLI });
	const written: [number, number][] = [];
	const groups: number[] = [];
	const appended: ReturnType<typeof track.appendGroup>[] = [];
	const writes = { onWrite: undefined as (() => void) | undefined };
	const appendGroup = track.appendGroup.bind(track);
	track.appendGroup = () => {
		const group = appendGroup();
		appended.push(group);
		const index = groups.push(0) - 1;
		const writeFrame = group.writeFrame.bind(group);
		group.writeFrame = (frame) => {
			const [timestamp, payload] = Moq.Varint.decode(frame.payload);
			written.push([timestamp, payload.byteLength]);
			groups[index]++;
			writeFrame(frame);
			writes.onWrite?.();
		};
		return group;
	};

	const rendition = {
		slot: new Signal(undefined),
		config: new Signal(undefined),
		stats: new Signal(undefined),
		track: new Signal<Moq.Track.Producer | undefined>(track),
		join: new Signal(undefined),
		close: () => {
			track.finish();
			track.close();
		},
	};

	const feed = new Feed();
	const capture = fakeCapture(feed);

	const enabled = new Signal(true);
	const encoder = new Encoder("audio", {
		broadcast: {
			audio: () => rendition,
			order,
			in: { maxAge: new Signal(undefined), onDemand: new Signal("off") },
		} as never,
		capture: capture as never,
		enabled,
		codec,
		join,
	});

	const config = await configuring;

	return {
		config,
		encoder,
		enabled,
		capture,
		track,
		rendition,
		feed,
		written,
		groups,
		appended,
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
test("an on-demand gap marks where submitted audio ends and drops the chunks held across it", async () => {
	using _webcodecs = installFakeWebCodecs();
	FakeAudioEncoder.LAG = 2;
	using env = await setup();
	const { track, rendition, feed, written, writes } = env;

	// One 20ms Opus frame per push, on a clock with a fractional microsecond origin.
	let index = 0;
	const push = async (count: number) => {
		for (let i = 0; i < count; i++, index++) {
			await feed.push({
				timestamp: Duration.micros(18_699.6 + index * 20_000),
				channels: [new Float32Array(960)],
			});
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

// A pause with a subscriber attached breaks the timeline just as losing demand does: a subscriber
// that stays across it, or joins during it, must not read the audio before it as live. The codec
// keeps its configuration and is not flushed.
test("disabling with a subscriber attached marks where the submitted audio ends", async () => {
	using _webcodecs = installFakeWebCodecs();
	using env = await setup();
	const { enabled, feed, written } = env;

	let index = 0;
	const push = async (count: number) => {
		for (let i = 0; i < count; i++, index++) {
			await feed.push({ timestamp: Duration.micros(20_000 + index * 20_000), channels: [new Float32Array(960)] });
		}
		await feed.drain();
	};

	await push(4);

	enabled.set(false);
	await settle();

	await push(2); // nothing publishing
	enabled.set(true);
	await push(3);

	expect(written).toEqual([
		[20_000, 1],
		[40_000, 1],
		[60_000, 1],
		[80_000, 1],
		[100_000, 0],
		[140_000, 1],
		[160_000, 1],
		[180_000, 1],
	]);
	expect(FakeAudioEncoder.instances).toHaveLength(1);
});

// A codec that still holds chunks at a capture gap stamps them, and the input after the gap, as one
// contiguous run. Stamping from what was submitted keeps each on its capture clock, and the marker
// waits behind the held chunks rather than dropping them as pre-gap.
test("a capture gap marks where the audio before it ends, after the chunks the codec holds", async () => {
	using _webcodecs = installFakeWebCodecs();
	FakeAudioEncoder.LAG = 2;
	using _now = manualAt(0);
	using env = await setup();
	const { feed, written } = env;

	const push = async (start: number, count: number) => {
		for (let i = 0; i < count; i++) {
			await feed.push({ timestamp: Duration.micros(start + i * 20_000), channels: [new Float32Array(960)] });
		}
		await feed.drain();
	};

	await push(20_000, 4); // two written, two held
	await push(200_000, 4); // the held two, the marker, then two more written and two held

	expect(written).toEqual([
		[20_000, 1],
		[40_000, 1],
		[60_000, 1],
		[80_000, 1],
		[100_000, 0],
		[200_000, 1],
		[220_000, 1],
	]);
	expect(FakeAudioEncoder.instances).toHaveLength(1);
});

// A lost track already marked where its audio ends, so what the codec held for it must not open the
// track that replaces it.
test("a closed track's held audio stays off its replacement", async () => {
	using _webcodecs = installFakeWebCodecs();
	FakeAudioEncoder.LAG = 2;
	using _now = manualAt(0);
	using env = await setup();
	const { track, rendition, feed } = env;
	const encode = spyOn(Container.Legacy.Producer.prototype, "encode");

	try {
		const push = async (start: number, count: number) => {
			for (let i = 0; i < count; i++) {
				await feed.push({ timestamp: Duration.micros(start + i * 20_000), channels: [new Float32Array(960)] });
			}
			await feed.drain();
		};

		await push(20_000, 4); // two written, two held
		track.close();
		rendition.track.set(undefined);
		await settle();

		const replacement = new Moq.Track.Producer("audio").accept({ timescale: Moq.Time.Timescale.MILLI });
		encode.mockClear();
		rendition.track.set(replacement);
		await push(200_000, 4);
		await settle();

		expect(encode.mock.calls.map(([, timestamp]) => timestamp)).toEqual([
			Duration.micros(200_000),
			Duration.micros(220_000),
		]);
	} finally {
		encode.mockRestore();
	}
});

// A muted rendition stays in the catalog, so a viewer deselects it with a one-field delta instead of
// seeing it removed and re-added.
test("disabling keeps the rendition in the catalog with enabled: false", async () => {
	using _webcodecs = installFakeWebCodecs();
	using env = await setup();
	const { encoder, enabled, capture, rendition } = env;

	const before = encoder.out.catalog.peek();
	expect(before).toBeDefined();
	expect(before?.enabled).toBeUndefined();

	enabled.set(false);
	await settle();
	expect(encoder.out.catalog.peek()).toEqual({ ...before, enabled: false } as never);
	expect(rendition.config.peek()).toEqual({ ...before, enabled: false } as never);
	expect(encoder.out.active.peek()).toBe(false);

	// Muting released the microphone, so re-enabling waits on its format without dropping the rendition.
	const format = capture.out.format.peek();
	capture.out.format.set(undefined as never);
	enabled.set(true);
	await settle();
	expect(encoder.out.catalog.peek()).toEqual({ ...before, enabled: false } as never);

	capture.out.format.set(format);
	await settle();
	expect(encoder.out.catalog.peek()).toEqual(before);
});

// Push-to-talk: an unmute with the capture left open must not wait on a fresh Opus probe before the
// catalog advertises the rendition again.
test("re-enabling with an unchanged capture reuses the probe", async () => {
	using _webcodecs = installFakeWebCodecs();
	using env = await setup();
	const { encoder, enabled } = env;

	const before = encoder.out.catalog.peek();
	expect(FakeAudioEncoder.probes).toHaveLength(1);

	enabled.set(false);
	await settle();
	expect(encoder.out.catalog.peek()?.enabled).toBe(false);

	enabled.set(true);
	await settle(2);
	expect(encoder.out.catalog.peek()).toEqual(before);
	expect(FakeAudioEncoder.probes).toHaveLength(1);
});

test("re-enabling with a changed sample rate probes again", async () => {
	using _webcodecs = installFakeWebCodecs();
	using env = await setup();
	const { encoder, enabled, capture } = env;

	enabled.set(false);
	await settle();
	// A disabled rendition keeps its codec ready, so the new format probes at once.
	let configuring = configured();
	capture.out.format.set({ sampleRate: 24_000, channelCount: 1 });
	// The old config keeps the codec running, resampling, until the probe resolves the new one.
	for (let config = await configuring; config.sampleRate !== 24_000; config = await configuring) {
		configuring = configured();
	}
	expect(FakeAudioEncoder.probes.map((probe) => probe.sampleRate)).toEqual([48_000, 24_000]);
	expect(encoder.out.catalog.peek()?.enabled).toBe(false);

	enabled.set(true);
	await settle();
	expect(encoder.out.catalog.peek()).toMatchObject({ sampleRate: 24_000 });
	expect(encoder.out.catalog.peek()?.enabled).toBeUndefined();
});

// Push-to-talk: a mute sends nothing and ends the utterance with a marker, while the rendition stays
// enabled and its track stays the same, so a listener hears the next press with no catalog update.
test("muting sends nothing and marks where the audio ends, keeping the rendition enabled", async () => {
	using _webcodecs = installFakeWebCodecs();
	using env = await setup();
	const { encoder, feed, written, rendition } = env;

	const push = async (start: number, count: number) => {
		for (let i = 0; i < count; i++) {
			await feed.push({ timestamp: Duration.micros(start + i * 20_000), channels: [new Float32Array(960)] });
		}
		await feed.drain();
	};

	const before = encoder.out.catalog.peek();
	expect(before?.enabled).toBeUndefined();
	await push(20_000, 2);

	encoder.muted.set(true);
	await settle();
	expect(encoder.out.active.peek()).toBe(false);
	await push(60_000, 3); // sent nowhere
	expect(rendition.config.peek()).toEqual(before as never);

	encoder.muted.set(false);
	await settle();
	expect(encoder.out.active.peek()).toBe(true);
	await push(120_000, 2);

	expect(written).toEqual([
		[20_000, 1],
		[40_000, 1],
		[60_000, 0],
		[120_000, 1],
		[140_000, 1],
	]);
	expect(rendition.config.peek()).toEqual(before as never);
	expect(FakeAudioEncoder.instances).toHaveLength(1);
	expect(FakeAudioEncoder.probes).toHaveLength(1);
});

// With `capture="release"` a mute closes the microphone, taking its format away. The rendition stays
// advertised, still enabled, through the mute. Once unmuted it is disabled until the microphone
// reopens, so one that never does isn't advertised as sending.
test("a muted rendition keeps its config enabled while its capture is released", async () => {
	using _webcodecs = installFakeWebCodecs();
	using env = await setup();
	const { encoder, capture } = env;

	const before = encoder.out.catalog.peek();
	encoder.muted.set(true);
	const format = capture.out.format.peek();
	capture.out.format.set(undefined as never);
	await settle();
	expect(encoder.out.catalog.peek()).toEqual(before);

	encoder.muted.set(false);
	await settle();
	expect(encoder.out.catalog.peek()).toEqual({ ...before, enabled: false } as never);

	// Muting again while the reopen is pending is muted, not disabled.
	encoder.muted.set(true);
	await settle();
	expect(encoder.out.catalog.peek()).toEqual(before);
	encoder.muted.set(false);
	await settle();
	expect(encoder.out.catalog.peek()).toEqual({ ...before, enabled: false } as never);

	capture.out.format.set(format);
	await settle(10);
	expect(encoder.out.catalog.peek()).toEqual(before);
	expect(FakeAudioEncoder.probes).toHaveLength(1);
});

// Chrome stamps encoder output from its first input, ignoring later jumps, so audio resumed after a
// long mute must still carry its capture timestamps rather than trail them by the mute.
test("audio resumed after a multi-second mute carries its capture timestamps", async () => {
	using _webcodecs = installFakeWebCodecs();
	using env = await setup();
	const { enabled, feed, written } = env;

	const push = async (start: number, count: number) => {
		for (let i = 0; i < count; i++) {
			await feed.push({ timestamp: Duration.micros(start + i * 20_000), channels: [new Float32Array(960)] });
		}
		await feed.drain();
	};

	await push(20_000, 4);

	enabled.set(false);
	await settle();

	// Capture keeps running through a five-second mute.
	await push(100_000, 250);
	enabled.set(true);
	await push(5_100_000, 4);

	expect(written).toEqual([
		[20_000, 1],
		[40_000, 1],
		[60_000, 1],
		[80_000, 1],
		[100_000, 0],
		[5_100_000, 1],
		[5_120_000, 1],
		[5_140_000, 1],
		[5_160_000, 1],
	]);
	expect(FakeAudioEncoder.probes).toHaveLength(1);
});

// Closing tears down the subscription and the pipeline, which both end the epoch; cleanups run
// last-in, first-out, so the marker lands once and before the rendition closes the track.
test("closing with a subscriber attached marks the end once before the track closes", async () => {
	using _webcodecs = installFakeWebCodecs();
	using env = await setup();
	const { encoder, track, feed, written } = env;

	for (let i = 0; i < 4; i++) {
		await feed.push({ timestamp: Duration.micros(20_000 + i * 20_000), channels: [new Float32Array(960)] });
	}
	await feed.drain();

	encoder.close();

	expect(written).toEqual([
		[20_000, 1],
		[40_000, 1],
		[60_000, 1],
		[80_000, 1],
		[100_000, 0],
	]);
	expect(track.closed.peek()).toBeDefined();
});

// A push that completes several frames is still one continuous stream, so it must not break the
// timeline between them.
test("a push completing several frames stays one stream", async () => {
	using _webcodecs = installFakeWebCodecs();
	using env = await setup();
	const { feed, written } = env;

	// Two 20ms Opus frames per push.
	for (let index = 0; index < 3; index++) {
		await feed.push({ timestamp: Duration.micros(18_699.6 + index * 40_000), channels: [new Float32Array(1920)] });
	}
	await feed.drain();

	expect(written).toEqual([
		[18_700, 1],
		[38_700, 1],
		[58_700, 1],
		[78_700, 1],
		[98_700, 1],
		[118_700, 1],
	]);
});

// An AAC frame lasts a fractional number of microseconds, so each frame's rounded start must come
// from the framer's own cumulative timeline: adding an unrounded duration to an already rounded
// start lands a microsecond off at the third frame and breaks continuous audio.
test("AAC frames with fractional durations stay one stream", async () => {
	using _webcodecs = installFakeWebCodecs();
	using env = await setup(new Order(), "aac");
	const { feed, written } = env;
	const discontinuity = spyOn(Container.Legacy.Producer.prototype, "discontinuity");
	try {
		await feed.push({ timestamp: Duration.micros(1000.833), channels: [new Float32Array(1024 * 4)] });
		await feed.drain();

		expect(discontinuity).not.toHaveBeenCalled();
		expect(written.map(([timestamp]) => timestamp)).toEqual([1_001, 22_334, 43_668, 65_001]);
	} finally {
		discontinuity.mockRestore();
	}
});

// The first frame at the minimum opens the next group. A timeline break closes the group early,
// so the first frame after it opens a fresh one.
test("a join packs frames until the minimum and restarts after a break", async () => {
	using _webcodecs = installFakeWebCodecs();
	using env = await setup(new Order(), undefined, Duration.millis(100));
	const { enabled, feed, written, groups, appended } = env;

	let index = 0;
	const push = async (count: number) => {
		for (let i = 0; i < count; i++, index++) {
			await feed.push({ timestamp: Duration.micros(20_000 + index * 20_000), channels: [new Float32Array(960)] });
		}
		await feed.drain();
	};

	await push(5);
	expect(groups).toEqual([5]);
	expect(appended[0].closed.peek()).toBeUndefined();
	await push(2); // the next frame opens a new group and closes the first
	expect(groups).toEqual([5, 2]);
	expect(appended[0].closed.peek()).toBeNull();

	enabled.set(false);
	await settle();

	await push(2); // nothing publishing
	enabled.set(true);
	await push(2);

	expect(written.map(([timestamp]) => timestamp)).toEqual([
		20_000, 40_000, 60_000, 80_000, 100_000, 120_000, 140_000, 160_000, 200_000, 220_000,
	]);
	// 120ms opens the next group, closing the five frames at 20-100ms. The break ends that group
	// early, so resumed frames start a fresh group after the marker.
	expect(groups).toEqual([5, 2, 1, 2]);
});

test("default audio groups carry twenty milliseconds", async () => {
	using _webcodecs = installFakeWebCodecs();
	for (const [frameMs, packets, groupMs] of [
		[2.5, 8, undefined],
		[10, 2, undefined],
		[20, 1, undefined],
		[2.5, 1, 1],
	] as const) {
		using env = await setup(
			new Order(),
			{ mime: "opus", frameDuration: Duration.millis(frameMs) },
			groupMs === undefined ? undefined : Duration.millis(groupMs),
		);
		for (let index = 0; index < packets * 2 + FakeAudioEncoder.LAG; index++) {
			await env.feed.push({
				timestamp: Duration.micros(20_000 + index * frameMs * 1000),
				channels: [new Float32Array(frameMs * 48)],
			});
		}
		await env.feed.drain();
		expect(env.groups).toEqual([packets, packets]);
		expect(env.written).toHaveLength(packets * 2);
	}
});

// Chromium stamps Opus output by counting the samples emitted, so every frame DTX suppresses pulls
// later audio earlier. A plain-JS caller passing the old knob must not reach the encoder.
test("never enables Opus DTX", async () => {
	using _webcodecs = installFakeWebCodecs();
	using env = await setup(new Order(), { mime: "opus", usedtx: true } as Codec);
	expect(env.config.opus?.usedtx).toBeUndefined();
});

// The catalog bitrate is a maximum: lowering the knob retunes the encoder, raising it raises both.
test("the catalog bitrate only rises", async () => {
	using _webcodecs = installFakeWebCodecs();
	using env = await setup(new Order(), { mime: "opus", bitrate: 64_000 });
	const { encoder } = env;
	expect(env.config.bitrate).toBe(64_000);
	expect(encoder.out.catalog.peek()?.bitrate as number | undefined).toBe(64_000);

	let reconfigured = configured();
	encoder.codec.set({ mime: "opus", bitrate: 32_000 });
	expect((await reconfigured).bitrate).toBe(32_000);
	await settle();
	expect(encoder.out.catalog.peek()?.bitrate as number | undefined).toBe(64_000);

	reconfigured = configured();
	encoder.codec.set({ mime: "opus", bitrate: 96_000 });
	expect((await reconfigured).bitrate).toBe(96_000);
	await settle();
	expect(encoder.out.catalog.peek()?.bitrate as number | undefined).toBe(96_000);
});

// Another rendition on the same broadcast that already wrote media ahead of this one's frames leaves
// this one trailing it, which the catalog advertises as `delay` once a second of it has been measured.
test("a rendition written behind another advertises delay", async () => {
	using _webcodecs = installFakeWebCodecs();
	// Each push writes the frame captured two before it, so the sibling stays ahead.
	FakeAudioEncoder.LAG = 2;
	const clock = manualAt(200);

	try {
		const order = new Order();
		using env = await setup(order);
		const { encoder, feed } = env;

		expect(encoder.out.catalog.peek()?.delay).toBeUndefined();

		// A sibling that stays 100ms ahead of the frames written, with a clock origin that keeps
		// timestamps nonnegative.
		const sibling = new Estimator();
		const start = now().since(Instant.ZERO).asMicros() - 100_000;
		const push = async (index: number) => {
			sibling.write(start + 100_000 + (index - 2) * 20_000, order, true);
			await feed.push({ timestamp: Duration.micros(start + index * 20_000), channels: [new Float32Array(960)] });
		};
		for (let index = 0; index < 4; index++) await push(index);
		await feed.drain();

		expect(env.written).toEqual([
			[100_000, 1],
			[120_000, 1],
		]);
		expect(encoder.out.catalog.peek()?.delay).toBeUndefined();

		for (let index = 4; index < 60; index++) await push(index);
		await feed.drain();
		expect(encoder.out.catalog.peek()).toMatchObject({ delay: 100 });
	} finally {
		clock.close();
	}
});

// Regression: codec settings that can't resolve left the encoder unsettled, so `<moq-publish>` never
// announced and withheld every other rendition with it.
test("settles when the codec settings can't resolve", async () => {
	using _webcodecs = installFakeWebCodecs();
	const error = spyOn(console, "error").mockImplementation(() => {});
	const capture = {
		in: { source: new Signal(undefined) },
		out: {
			root: new Signal(undefined),
			format: new Signal<Format>({ sampleRate: 48_000, channelCount: 1 }),
			frames: new Signal(undefined),
		},
		blocked: new Signal(false),
	};
	const encoder = new Encoder("audio", {
		capture: capture as never,
		codec: { mime: "opus", frameDuration: Duration.millis(15) },
	});

	try {
		await settle();
		expect(encoder.out.catalog.peek()).toBeUndefined();
		expect(encoder.settled.peek()).toBe(true);
		expect(error).toHaveBeenCalled();

		// A valid duration resolves, and the config keeps it settled.
		const ready = encoder.out.catalog.changed();
		encoder.codec.set({ mime: "opus", frameDuration: Duration.millis(20) });
		await ready;
		await settle();
		expect(encoder.out.catalog.peek()).toBeDefined();
		expect(encoder.settled.peek()).toBe(true);
	} finally {
		encoder.close();
		error.mockRestore();
	}
});

// A join of zero would open a group per frame, so it refuses the rendition like any invalid knob,
// settling the gate without a config until a valid join arrives.
test("refuses a join that is not a positive duration", async () => {
	using _webcodecs = installFakeWebCodecs();
	const error = spyOn(console, "error").mockImplementation(() => {});
	for (const join of [Duration.ZERO]) {
		const capture = {
			in: { source: new Signal(undefined) },
			out: {
				root: new Signal(undefined),
				format: new Signal<Format>({ sampleRate: 48_000, channelCount: 1 }),
				frames: new Signal(undefined),
			},
			blocked: new Signal(false),
		};
		const encoder = new Encoder("audio", { capture: capture as never, join });
		try {
			await settle();
			expect(encoder.out.catalog.peek()).toBeUndefined();
			expect(encoder.settled.peek()).toBe(true);

			const ready = encoder.out.catalog.changed();
			encoder.join.set(Duration.millis(20));
			await ready;
			expect(encoder.out.catalog.peek()).toBeDefined();
		} finally {
			encoder.close();
		}
	}
	expect(error).toHaveBeenCalled();
	error.mockRestore();
});

// The broadcast reads every registered rendition's spacing, listed or not, so a refused join must not
// leave the old one for a new network broadcast to declare.
test("a refused join withdraws its group spacing", async () => {
	using _webcodecs = installFakeWebCodecs();
	const error = spyOn(console, "error").mockImplementation(() => {});
	const name = new Signal(Moq.Path.from("refused.hang"));
	const broadcast = new Broadcast({ origin: new Moq.Origin.Producer(), name });
	const video = broadcast.video("video");
	video.join.set(Duration.millis(20));
	video.config.set({ codec: "avc1.640028", container: { kind: "legacy" } });
	// 20 ms Opus frames start a group every two frames for a 30 ms join.
	const encoder = new Encoder("audio", {
		broadcast,
		capture: fakeCapture(new Feed()) as never,
		join: Duration.millis(30),
	});
	try {
		await settle();
		expect(broadcast.join.peek()).toEqual(Duration.millis(40));

		encoder.join.set(Duration.millis(0));
		name.set(Moq.Path.from("renamed.hang"));
		await settle();
		expect(broadcast.join.peek()).toEqual(Duration.millis(20));
	} finally {
		encoder.close();
		broadcast.close();
		error.mockRestore();
	}
});

test("settles while the capture waits on a gesture", async () => {
	const capture = {
		in: { source: new Signal(undefined) },
		out: { root: new Signal(undefined), format: new Signal(undefined), frames: new Signal(undefined) },
		blocked: new Signal(true),
	};
	const encoder = new Encoder("audio", { capture: capture as never });

	try {
		await settle();
		expect(encoder.settled.peek()).toBe(true);

		// The gesture arrives, so a format is on its way: unsettled until it resolves.
		capture.blocked.set(false);
		await settle();
		expect(encoder.settled.peek()).toBe(false);
	} finally {
		encoder.close();
	}
});

async function settle(times = 5): Promise<void> {
	for (let i = 0; i < times; i++) await new Promise<void>((resolve) => queueMicrotask(resolve));
}

test("a failed audio encoder keeps its last catalog config disabled", async () => {
	using _webcodecs = installFakeWebCodecs();
	using _now = manualAt(0);
	using _error = spyOn(console, "error").mockImplementation(() => {});
	using env = await setup();
	const live = FakeAudioEncoder.instances.at(-1);
	if (!live) throw new Error("expected live audio encoder");
	const failure = new Error("audio encoder failed");
	live.fail(failure);
	for (let i = 0; i < 10; i++) await Promise.resolve();
	expect(env.track.closed.peek()).toBe(failure);
	expect(env.encoder.out.error.peek()).toBe(failure);
	expect(env.encoder.out.catalog.peek()).toMatchObject({ codec: "opus", enabled: false });
	expect(env.encoder.out.active.peek()).toBe(false);
	expect(FakeAudioEncoder.instances).toHaveLength(1);
});

test("audio failure stays local through demand changes and capture format re-arms a fresh track", async () => {
	using _webcodecs = installFakeWebCodecs();
	using _now = manualAt(0);
	const broadcast = new Broadcast({
		origin: new Moq.Origin.Producer(),
		name: Moq.Path.from("failure.hang"),
		onDemand: "off",
	});
	const feed = new Feed();
	const capture = fakeCapture(feed);
	const estimate = new Signal<number | undefined>(10_000_000);
	const bandwidth = new Moq.Bandwidth.Allocator(estimate);
	const enabled = new Signal(true);
	const configuring = configured();
	const encoder = new Encoder("audio", { broadcast, capture: capture as never, bandwidth, enabled });
	let subscriber: Moq.Track.Subscriber | undefined;
	let siblingSubscriber: Moq.Track.Subscriber | undefined;
	let catalog: Json.Snapshot.Consumer<Catalog.Root> | undefined;
	let recovered: Moq.Track.Subscriber | undefined;
	const push = async (first: number, channels: number) => {
		for (let i = 0; i < 3; i++) {
			await feed.push({
				timestamp: Duration.micros((first + i) * 20_000),
				channels: Array.from({ length: channels }, () => new Float32Array(960)),
			});
		}
		await feed.drain();
	};
	let announced: Moq.Origin.Following | undefined;
	try {
		await configuring;
		await settle();
		const net = broadcast.net.peek();
		if (!net) throw new Error("expected network broadcast");
		announced = broadcast.in.origin.peek()?.follow(broadcast.in.name.peek());
		const epoch = announced?.active.peek()?.instance;
		const sibling = broadcast.video("video");
		sibling.config.set({ codec: "avc1.640028", container: { kind: "legacy" } });
		await settle();
		siblingSubscriber = net.track("video").subscribe();
		subscriber = net.track("audio").subscribe();
		await settle();
		const siblingTrack = sibling.track.peek();
		if (!siblingTrack) throw new Error("expected demanded sibling track");
		expect(siblingTrack.appendGroup().sequence).toBe(0n);
		await push(0, 1); // a group per frame
		expect((await subscriber.largest())?.group).toBe(2n);
		catalog = new Json.Snapshot.Consumer<Catalog.Root>({ track: net.track(Broadcast.CATALOG_TRACK) });
		expect((await catalog.latest())?.value.audio?.renditions.audio?.enabled).not.toBe(false);
		const live = FakeAudioEncoder.instances.at(-1);
		if (!live) throw new Error("expected live audio encoder");
		const failure = new Error("audio codec failed");
		live.fail(failure);
		await settle();
		expect(subscriber.closed.peek()).toBe(failure);
		expect(encoder.out.error.peek()).toBe(failure);
		expect((await catalog.latest())?.value.audio?.renditions.audio?.enabled).toBe(false);
		expect(encoder.out.active.peek()).toBe(false);
		const instances = FakeAudioEncoder.instances.length;
		subscriber.close();
		subscriber = net.track("audio").subscribe();
		broadcast.audio("audio/new");
		estimate.set(20_000_000);
		enabled.set(false);
		await settle();
		enabled.set(true);
		await settle();
		expect(subscriber.closed.peek()).toBeDefined();
		expect(encoder.out.error.peek()).toBe(failure);
		expect(encoder.out.catalog.peek()?.enabled).toBe(false);
		expect(FakeAudioEncoder.instances).toHaveLength(instances);
		expect(broadcast.net.peek()).toBe(net);
		expect(announced?.active.peek()?.instance).toBe(epoch);
		expect(siblingTrack.appendGroup().sequence).toBe(1n);

		const reconfiguring = configured();
		capture.out.format.set({ sampleRate: 48_000, channelCount: 2 });
		await reconfiguring;
		await settle();
		expect(encoder.out.error.peek()).toBeUndefined();
		const config = (await catalog.latest())?.value.audio?.renditions.audio;
		if (!config?.track) throw new Error("expected recovered track name");
		expect(config.track).not.toBe("audio");
		expect(config.numberOfChannels).toBe(Catalog.u53(2));
		expect(config.enabled).not.toBe(false);
		recovered = net.track(config.track).subscribe();
		await settle();
		await push(3, 2);
		expect((await recovered.largest())?.group).toBe(2n);
		expect(encoder.out.active.peek()).toBe(true);
		expect(recovered.closed.peek()).toBeUndefined();
		expect(broadcast.net.peek()).toBe(net);
		expect(announced?.active.peek()?.instance).toBe(epoch);
		expect(siblingTrack.appendGroup().sequence).toBe(2n);
	} finally {
		subscriber?.close();
		siblingSubscriber?.close();
		catalog?.close();
		recovered?.close();
		encoder.close();
		bandwidth.close();
		announced?.close();
		broadcast.close();
	}
});

test("an audio failure without demand still recovers under a fresh track name", async () => {
	using _webcodecs = installFakeWebCodecs();
	using _now = manualAt(0);
	const broadcast = new Broadcast({ origin: new Moq.Origin.Producer(), name: Moq.Path.from("idle.hang") });
	const configuring = configured();
	const encoder = new Encoder("audio", { broadcast, capture: fakeCapture(new Feed()) as never });
	let catalog: Json.Snapshot.Consumer<Catalog.Root> | undefined;
	try {
		await configuring;
		await settle();
		const net = broadcast.net.peek();
		if (!net) throw new Error("expected network broadcast");
		const live = FakeAudioEncoder.instances.at(-1);
		if (!live) throw new Error("expected audio encoder without demand");
		const failure = new Error("idle audio encoder failed");
		live.fail(failure);
		await settle();
		expect(encoder.out.error.peek()).toBe(failure);
		expect(encoder.out.catalog.peek()?.enabled).toBe(false);
		const failed = net.track("audio").subscribe();
		expect(failed.closed.peek()).toBeDefined();
		failed.close();
		const reconfiguring = configured();
		encoder.codec.set({ mime: "opus", bitrate: 64_000 });
		await reconfiguring;
		await settle();
		catalog = new Json.Snapshot.Consumer<Catalog.Root>({ track: net.track(Broadcast.CATALOG_TRACK) });
		const config = (await catalog.latest())?.value.audio?.renditions.audio;
		expect(config?.track).toBeDefined();
		expect(config?.track).not.toBe("audio");
		expect(encoder.out.error.peek()).toBeUndefined();
		expect(broadcast.net.peek()).toBe(net);
	} finally {
		catalog?.close();
		encoder.close();
		broadcast.close();
	}
});

test.each(["failure first", "config first"])(
	"same-turn audio failure and config recovery keep a fresh track: %s",
	async (order) => {
		using _webcodecs = installFakeWebCodecs();
		using _now = manualAt(0);
		const broadcast = new Broadcast({
			origin: new Moq.Origin.Producer(),
			onDemand: "off",
			name: Moq.Path.from("coalesced.hang"),
		});
		const configuring = configured();
		const encoder = new Encoder("audio", { broadcast, capture: fakeCapture(new Feed()) as never });
		let catalog: Json.Snapshot.Consumer<Catalog.Root> | undefined;
		try {
			await configuring;
			await settle();
			const net = broadcast.net.peek();
			if (!net) throw new Error("expected network broadcast");
			const live = FakeAudioEncoder.instances.at(-1);
			if (!live) throw new Error("expected idle audio encoder");
			const reconfiguring = configured();
			if (order === "config first") encoder.codec.set({ mime: "opus", bitrate: 64_000 });
			live.fail(new Error("idle encoder failed"));
			if (order === "failure first") encoder.codec.set({ mime: "opus", bitrate: 64_000 });
			await reconfiguring;
			await settle();
			catalog = new Json.Snapshot.Consumer<Catalog.Root>({ track: net.track(Broadcast.CATALOG_TRACK) });
			const config = (await catalog.latest())?.value.audio?.renditions.audio;
			expect(config?.track).toBeDefined();
			expect(config?.track).not.toBe("audio");
			expect(encoder.out.error.peek()).toBeUndefined();
			// Encoding resumes on the fresh track without waiting for a subscriber.
			expect(encoder.out.active.peek()).toBe(true);
			expect(broadcast.net.peek()).toBe(net);
		} finally {
			catalog?.close();
			encoder.close();
			broadcast.close();
		}
	},
);

// Both policies preserve the past; only always-on spends encode work while idle.
test.each([undefined, "off", "all"] as const)(
	"a late subscriber reaches captured audio with either encoding policy (%p)",
	async (onDemand) => {
		using _webcodecs = installFakeWebCodecs();
		const broadcast = new Broadcast({
			origin: new Moq.Origin.Producer(),
			name: Moq.Path.from("eager.hang"),
			onDemand,
		});
		const feed = new Feed();
		const configuring = configured();
		const encoder = new Encoder("audio", { broadcast, capture: fakeCapture(feed) as never });
		const discontinuity = spyOn(Container.Legacy.Producer.prototype, "discontinuity");
		let late: Moq.Track.Subscriber | undefined;
		try {
			await configuring;
			await settle();
			const net = broadcast.net.peek();
			if (!net) throw new Error("expected network broadcast");
			expect(encoder.out.active.peek()).toBe(onDemand === "off");

			// One 20ms Opus frame per push, each its own group.
			let index = 0;
			const push = async (count: number) => {
				for (let i = 0; i < count; i++, index++) {
					await feed.push({ timestamp: Duration.micros(index * 20_000), channels: [new Float32Array(960)] });
				}
				await feed.drain();
			};

			await push(4);
			expect(encoder.out.stats.peek().frames).toBe(onDemand === "off" ? 4 : 0);
			late = net.track("audio").subscribe({ delay: { max: Duration.millis(10_000) } });
			await settle();
			expect(encoder.out.active.peek()).toBe(true);
			await push(3);

			const group = await late.ordered().nextGroup();
			if (!group) throw new Error("expected a group");
			const frame = await group.readFrame();
			if (!frame) throw new Error("expected a frame");
			const [timestamp] = Moq.Varint.decode(frame.payload);
			// Buffered audio is encoded ahead of the live input on the capture clock.
			expect(group.sequence).toBe(0n);
			expect(timestamp).toBe(0);

			// Losing demand preserves the epoch and resumes raw retention.
			late.close();
			late = undefined;
			await settle();
			expect(discontinuity).not.toHaveBeenCalled();
			expect(encoder.out.active.peek()).toBe(onDemand === "off");
		} finally {
			discontinuity.mockRestore();
			late?.close();
			encoder.close();
			broadcast.close();
		}
	},
);

// An HLS export of an audio-only broadcast cuts segments on the audio group spacing and reads the
// media directly, so the catalog declares it before anything is encoded and advertises no records.
test("an audio-only broadcast declares its group spacing as the join interval", async () => {
	using _webcodecs = installFakeWebCodecs();
	const broadcast = new Broadcast({ origin: new Moq.Origin.Producer(), name: Moq.Path.from("join.hang") });
	const feed = new Feed();
	const configuring = configured();
	// 20 ms Opus frames start a group every two frames for a 30 ms join.
	const encoder = new Encoder("audio", { broadcast, capture: fakeCapture(feed) as never, join: Duration.millis(30) });
	let catalog: Json.Snapshot.Consumer<Catalog.Root> | undefined;
	try {
		await configuring;
		await settle();
		const net = broadcast.net.peek();
		if (!net) throw new Error("expected network broadcast");
		expect(encoder.out.active.peek()).toBe(false);

		catalog = new Json.Snapshot.Consumer<Catalog.Root>({ track: net.track(Broadcast.CATALOG_TRACK) });
		const root = (await catalog.latest())?.value;
		expect(root?.media?.join).toEqual({ interval: Catalog.u53(40) });
		expect(root?.audio?.renditions.audio).toBeDefined();
		expect(root?.audio?.renditions.audio).not.toHaveProperty("hls");
	} finally {
		catalog?.close();
		encoder.close();
		broadcast.close();
	}
});

test("a backlog encode refusal aborts its rendition when demand arrives", async () => {
	using _webcodecs = installFakeWebCodecs();
	const broadcast = new Broadcast({ origin: new Moq.Origin.Producer(), name: Moq.Path.from("refused.hang") });
	const feed = new Feed();
	const configuring = configured();
	const encoder = new Encoder("audio", { broadcast, capture: fakeCapture(feed) as never });
	let subscriber: Moq.Track.Subscriber | undefined;
	try {
		await configuring;
		await settle();
		await feed.push({ timestamp: Duration.micros(0), channels: [new Float32Array(960)] });
		await feed.drain();
		const live = FakeAudioEncoder.instances.at(-1);
		if (!live) throw new Error("expected configured encoder");
		const failure = new Error("codec refused buffered input");
		using _encode = spyOn(live, "encode").mockImplementation(() => {
			throw failure;
		});
		const net = broadcast.net.peek();
		if (!net) throw new Error("expected network broadcast");
		// A live-edge budget would leave the retained frame unencoded.
		subscriber = net.track("audio").subscribe({ delay: { max: null } });
		await settle();
		expect(encoder.out.error.peek()).toBe(failure);
		expect(subscriber.closed.peek()).toBe(failure);
	} finally {
		subscriber?.close();
		encoder.close();
		broadcast.close();
	}
});

// Advancing capture and time independently catches a buffer bounded only by incoming timestamps,
// and forces the on-demand transition to replay history even before another live quantum arrives.
test("default audio replays the last second after three seconds without a subscriber", async () => {
	using _webcodecs = installFakeWebCodecs();
	const clock = new Manual();
	const installed = clock.install();
	const broadcast = new Broadcast({
		origin: new Moq.Origin.Producer(),
		name: Moq.Path.from("raw.hang"),
		maxAge: Duration.millis(1000),
	});
	const feed = new Feed();
	const configuring = configured();
	const encoder = new Encoder("audio", { broadcast, capture: fakeCapture(feed) as never });
	let late: Moq.Track.Subscriber | undefined;
	try {
		await configuring;
		await settle();
		for (let index = 0; index < 150; index++) {
			await feed.push({ timestamp: Duration.micros(index * 20_000), channels: [new Float32Array(960)] });
			await feed.drain();
			await clock.advance(Duration.millis(20));
		}
		expect(encoder.out.stats.peek().frames).toBe(0);
		expect(encoder.out.active.peek()).toBe(false);
		const net = broadcast.net.peek();
		if (!net) throw new Error("expected network broadcast");
		late = net.track("audio").subscribe({ delay: { max: Duration.millis(1000) } });
		await settle();
		expect(encoder.out.stats.peek().frames).toBe(50);
		for (let index = 150; index < 153; index++) {
			await feed.push({ timestamp: Duration.micros(index * 20_000), channels: [new Float32Array(960)] });
		}
		await feed.drain();
		// The subscription's one second reaches back from the newest frame, 152, to 102.
		const reader = late.ordered();
		for (let index = 102; index < 153; index++) {
			const group = await reader.nextGroup();
			if (!group) throw new Error("expected buffered group");
			const frame = await group.readFrame();
			if (!frame) throw new Error("expected buffered frame");
			expect(Moq.Varint.decode(frame.payload)[0]).toBe(index * 20_000);
			group.close();
		}
	} finally {
		late?.close();
		encoder.close();
		broadcast.close();
		installed.close();
	}
});

// Replayed groups reach the track as fresh arrivals, so only the encoder can tell they are old: a
// live viewer rejoining after seconds idle would otherwise wade through all of them first.
test("demand replays only the retained audio its max delay still wants", async () => {
	using _webcodecs = installFakeWebCodecs();
	const clock = new Manual();
	const installed = clock.install();
	const broadcast = new Broadcast({ origin: new Moq.Origin.Producer(), name: Moq.Path.from("rejoin.hang") });
	const feed = new Feed();
	const configuring = configured();
	const encoder = new Encoder("audio", { broadcast, capture: fakeCapture(feed) as never });
	let late: Moq.Track.Subscriber | undefined;
	try {
		await configuring;
		await settle();
		for (let index = 0; index < 150; index++) {
			await feed.push({ timestamp: Duration.micros(index * 20_000), channels: [new Float32Array(960)] });
			await feed.drain();
			await clock.advance(Duration.millis(20));
		}
		const net = broadcast.net.peek();
		if (!net) throw new Error("expected network broadcast");
		// At 3000 ms, a 130 ms budget still wants the frames ending after 2870 ms: 143 onward.
		late = net.track("audio").subscribe({ delay: { max: Duration.millis(130) } });
		await settle();
		expect(encoder.out.stats.peek().frames).toBe(7 - FakeAudioEncoder.LAG);
		await feed.push({ timestamp: Duration.micros(150 * 20_000), channels: [new Float32Array(960)] });
		await feed.drain();
		const group = await late.ordered().nextGroup();
		const frame = await group?.readFrame();
		if (!frame) throw new Error("expected a replayed frame");
		expect(Moq.Varint.decode(frame.payload)[0]).toBe(143 * 20_000);
	} finally {
		late?.close();
		encoder.close();
		broadcast.close();
		installed.close();
	}
});

// A fetch's own budget judges only the group it asks for, so it must not narrow what the encoder
// replays for the live subscribers that come later.
test("a fetch alone replays the whole retention", async () => {
	using _webcodecs = installFakeWebCodecs();
	const clock = new Manual();
	const installed = clock.install();
	const broadcast = new Broadcast({ origin: new Moq.Origin.Producer(), name: Moq.Path.from("fetch.hang") });
	const feed = new Feed();
	const configuring = configured();
	const encoder = new Encoder("audio", { broadcast, capture: fakeCapture(feed) as never });
	let fetched: Moq.Group.Consumer | undefined;
	try {
		await configuring;
		await settle();
		const net = broadcast.net.peek();
		if (!net) throw new Error("expected network broadcast");
		// A first viewer leaves group 0 in the cache for the fetch to find.
		const first = net.track("audio").subscribe({ delay: { max: null } });
		await settle();
		await feed.push({ timestamp: Duration.micros(0), channels: [new Float32Array(960)] });
		await feed.drain();
		first.close();
		await settle();
		const encoded = encoder.out.stats.peek().frames;
		for (let index = 1; index <= 150; index++) {
			await clock.advance(Duration.millis(20));
			await feed.push({ timestamp: Duration.micros(index * 20_000), channels: [new Float32Array(960)] });
			await feed.drain();
		}
		expect(encoder.out.stats.peek().frames).toBe(encoded);
		// Held open, the fetched group keeps the fetch's demand alive while the encoder reacts.
		fetched = await net.track("audio").fetchGroup(0n);
		await settle();
		expect(encoder.out.stats.peek().frames).toBe(encoded + 150);
	} finally {
		fetched?.close();
		encoder.close();
		broadcast.close();
		installed.close();
	}
});

// Replay catches up behind the live edge another rendition already reached; advertising that as
// `delay` would buffer playback by the retention for the rest of the broadcast.
test("replayed raw audio raises no catalog delay", async () => {
	using _webcodecs = installFakeWebCodecs();
	const clock = new Manual();
	const installed = clock.install();
	const broadcast = new Broadcast({
		origin: new Moq.Origin.Producer(),
		name: Moq.Path.from("history.hang"),
		maxAge: Duration.millis(1000),
	});
	const feed = new Feed();
	const configuring = configured();
	const encoder = new Encoder("audio", { broadcast, capture: fakeCapture(feed) as never });
	let late: Moq.Track.Subscriber | undefined;
	try {
		await configuring;
		await settle();
		for (let index = 0; index < 150; index++) {
			await feed.push({ timestamp: Duration.micros(index * 20_000), channels: [new Float32Array(960)] });
			await feed.drain();
			await clock.advance(Duration.millis(20));
		}
		// Video already wrote up to the live edge.
		new Estimator().write(150 * 20_000, broadcast.order, true);
		const net = broadcast.net.peek();
		if (!net) throw new Error("expected network broadcast");
		late = net.track("audio").subscribe({ delay: { max: Duration.millis(1000) } });
		await settle();
		for (let index = 150; index < 153; index++) {
			await feed.push({ timestamp: Duration.micros(index * 20_000), channels: [new Float32Array(960)] });
		}
		await feed.drain();
		await settle();
		expect(encoder.out.stats.peek().frames).toBeGreaterThan(50);
		expect(encoder.out.catalog.peek()?.delay).toBeUndefined();
	} finally {
		late?.close();
		encoder.close();
		broadcast.close();
		installed.close();
	}
});

// Losing demand keeps the epoch, but history that expires unencoded is a real gap: the frame
// before it must not read as lasting until the replay resumes.
test("expired raw audio ends the epoch before the replay resumes", async () => {
	using _webcodecs = installFakeWebCodecs();
	const clock = new Manual();
	const installed = clock.install();
	const broadcast = new Broadcast({
		origin: new Moq.Origin.Producer(),
		name: Moq.Path.from("evicted.hang"),
		maxAge: Duration.millis(1000),
	});
	const feed = new Feed();
	const configuring = configured();
	const encoder = new Encoder("audio", { broadcast, capture: fakeCapture(feed) as never });
	const discontinuity = spyOn(Container.Legacy.Producer.prototype, "discontinuity");
	let subscriber: Moq.Track.Subscriber | undefined;
	try {
		await configuring;
		await settle();
		const net = broadcast.net.peek();
		if (!net) throw new Error("expected network broadcast");
		let index = 0;
		const push = async (count: number) => {
			for (const end = index + count; index < end; index++) {
				await feed.push({ timestamp: Duration.micros(index * 20_000), channels: [new Float32Array(960)] });
				await feed.drain();
				await clock.advance(Duration.millis(20));
			}
		};

		subscriber = net.track("audio").subscribe();
		await settle();
		await push(4);
		subscriber.close();
		subscriber = undefined;
		await settle();
		expect(discontinuity).not.toHaveBeenCalled();

		// Twice the retention unwatched: the first half expires before anyone returns.
		await push(100);
		subscriber = net.track("audio").subscribe();
		await settle();
		expect(discontinuity).toHaveBeenCalledTimes(1);
		expect(discontinuity.mock.calls[0][0]).toEqual(Duration.micros(4 * 20_000));
	} finally {
		discontinuity.mockRestore();
		subscriber?.close();
		encoder.close();
		broadcast.close();
		installed.close();
	}
});

test("Opus description is probed with the exact live config before advertising", async () => {
	using _codecs = installFakeWebCodecs();
	FakeAudioEncoder.description = (config) => new TextEncoder().encode(JSON.stringify(config));
	using env = await setup(new Order(), {
		mime: "opus",
		frameDuration: Duration.millis(2.5),
		complexity: 3,
		packetlossperc: 7,
		useinbandfec: true,
		bitrate: 64000,
	});
	expect(env.encoder.out.catalog.peek()?.description).toBe(Buffer.from(JSON.stringify(env.config)).toString("hex"));
	expect(FakeAudioEncoder.probes).toEqual([env.config]);
	expect(FakeAudioEncoder.instances).toHaveLength(1);
	expect(env.written).toHaveLength(0);
	const configuring = configured();
	env.capture.in.source.set({ track: {}, kind: "voice" } as never);
	const voice = await configuring;
	expect(voice.opus).toMatchObject({ application: "voip", signal: "voice" });
	expect(FakeAudioEncoder.probes.at(-1)).toEqual(voice);
	expect(env.encoder.out.catalog.peek()?.description).toBe(Buffer.from(JSON.stringify(voice)).toString("hex"));
});

// A disable stops taking capture, but the audio retained before it stays reachable: a subscriber
// arriving while disabled still gets it encoded, and otherwise it expires on its own.
test.each([false, true])("audio retained before a disable is encoded on demand (expired: %p)", async (expire) => {
	using _webcodecs = installFakeWebCodecs();
	const clock = new Manual();
	const installed = clock.install();
	const enabled = new Signal(true);
	const broadcast = new Broadcast({
		origin: new Moq.Origin.Producer(),
		name: Moq.Path.from("disabled.hang"),
		maxAge: Duration.millis(1000),
	});
	const feed = new Feed();
	const configuring = configured();
	const encoder = new Encoder("audio", { broadcast, enabled, capture: fakeCapture(feed) as never });
	let late: Moq.Track.Subscriber | undefined;
	try {
		await configuring;
		await settle();
		for (let index = 0; index < 4; index++) {
			await feed.push({ timestamp: Duration.micros(index * 20_000), channels: [new Float32Array(960)] });
			await feed.drain();
			await clock.advance(Duration.millis(20));
		}
		enabled.set(false);
		await settle();
		// Disabling encodes nothing eagerly.
		expect(encoder.out.stats.peek().frames).toBe(0);
		if (expire) await clock.advance(Duration.millis(1100));

		const net = broadcast.net.peek();
		if (!net) throw new Error("expected network broadcast");
		late = net.track("audio").subscribe({ delay: { max: Duration.millis(1000) } });
		await settle();
		expect(encoder.out.catalog.peek()?.enabled).toBe(false);
		expect(encoder.out.active.peek()).toBe(false);
		if (expire) {
			expect(encoder.out.stats.peek().frames).toBe(0);
			return;
		}
		// The history is encoded and its marker follows at once, since nothing else is owed while disabled.
		expect(encoder.out.stats.peek().frames).toBe(4);
		const reader = late.ordered();
		for (let index = 0; index < 4; index++) {
			const group = await reader.nextGroup();
			const frame = await group?.readFrame();
			if (!frame) throw new Error("expected retained frame");
			expect(Moq.Varint.decode(frame.payload)[0]).toBe(index * 20_000);
			group?.close();
		}
	} finally {
		late?.close();
		encoder.close();
		broadcast.close();
		installed.close();
	}
});

// Re-enabled before anyone wanted it, the retained audio stays ahead of the break: all of it is
// encoded before the marker, then the resumed audio starts a fresh epoch.
test("audio retained across a disable and re-enable is served before the break", async () => {
	using _webcodecs = installFakeWebCodecs();
	const clock = new Manual();
	const installed = clock.install();
	const enabled = new Signal(true);
	const broadcast = new Broadcast({
		origin: new Moq.Origin.Producer(),
		name: Moq.Path.from("resumed.hang"),
		maxAge: Duration.millis(1000),
	});
	const feed = new Feed();
	const configuring = configured();
	const encoder = new Encoder("audio", { broadcast, enabled, capture: fakeCapture(feed) as never });
	const discontinuity = spyOn(Container.Legacy.Producer.prototype, "discontinuity");
	let late: Moq.Track.Subscriber | undefined;
	try {
		await configuring;
		await settle();
		const push = async (start: number, count: number) => {
			for (let index = 0; index < count; index++) {
				await feed.push({
					timestamp: Duration.micros(start + index * 20_000),
					channels: [new Float32Array(960)],
				});
				await feed.drain();
				await clock.advance(Duration.millis(20));
			}
		};
		await push(0, 4);
		enabled.set(false);
		await settle();
		await push(80_000, 2); // dropped while disabled
		enabled.set(true);
		await settle();
		await push(200_000, 3);
		expect(encoder.out.stats.peek().frames).toBe(0);

		const net = broadcast.net.peek();
		if (!net) throw new Error("expected network broadcast");
		late = net.track("audio").subscribe({ delay: { max: Duration.millis(1000) } });
		await settle(20);
		// The four retained frames, then the marker, then the three resumed.
		expect(encoder.out.stats.peek().frames).toBe(7);
		expect(discontinuity.mock.calls.map(([end]) => end)).toEqual([Duration.micros(80_000)]);
		const reader = late.ordered();
		const read: [number, number][] = [];
		while (read.length < 6) {
			const group = await reader.nextGroup();
			const frame = await group?.readFrame();
			if (!frame) throw new Error("expected a frame");
			const [timestamp, payload] = Moq.Varint.decode(frame.payload);
			read.push([timestamp, payload.byteLength]);
			group?.close();
		}
		expect(read).toEqual([
			[0, 1],
			[20_000, 1],
			[40_000, 1],
			[60_000, 1],
			[80_000, 0],
			[200_000, 1],
		]);
	} finally {
		discontinuity.mockRestore();
		late?.close();
		encoder.close();
		broadcast.close();
		installed.close();
	}
});

// A replay submits the whole backlog at once, so a gap inside it must break the timeline without
// dropping any frame of that drain still owed by the codec.
test("a capture gap inside replayed audio keeps every frame on both sides of it", async () => {
	using _webcodecs = installFakeWebCodecs();
	const clock = new Manual();
	const installed = clock.install();
	const broadcast = new Broadcast({
		origin: new Moq.Origin.Producer(),
		name: Moq.Path.from("gap.hang"),
		maxAge: Duration.millis(1000),
	});
	const feed = new Feed();
	const configuring = configured();
	const encoder = new Encoder("audio", { broadcast, capture: fakeCapture(feed) as never });
	const discontinuity = spyOn(Container.Legacy.Producer.prototype, "discontinuity");
	let late: Moq.Track.Subscriber | undefined;
	try {
		await configuring;
		await settle();
		const push = async (start: number, count: number) => {
			for (let index = 0; index < count; index++) {
				await feed.push({
					timestamp: Duration.micros(start + index * 20_000),
					channels: [new Float32Array(960)],
				});
				await feed.drain();
				await clock.advance(Duration.millis(20));
			}
		};
		await push(0, 4);
		await push(200_000, 4);
		expect(encoder.out.stats.peek().frames).toBe(0);

		const net = broadcast.net.peek();
		if (!net) throw new Error("expected network broadcast");
		late = net.track("audio").subscribe({ delay: { max: Duration.millis(1000) } });
		await settle(20);
		// Every frame on both sides of the gap.
		expect(encoder.out.stats.peek().frames).toBe(8);
		expect(discontinuity.mock.calls.map(([end]) => end)).toEqual([Duration.micros(80_000)]);
		const reader = late.ordered();
		const read: [number, number][] = [];
		while (read.length < 7) {
			const group = await reader.nextGroup();
			const frame = await group?.readFrame();
			if (!frame) throw new Error("expected a frame");
			const [timestamp, payload] = Moq.Varint.decode(frame.payload);
			read.push([timestamp, payload.byteLength]);
			group?.close();
		}
		expect(read).toEqual([
			[0, 1],
			[20_000, 1],
			[40_000, 1],
			[60_000, 1],
			[80_000, 0],
			[200_000, 1],
			[220_000, 1],
		]);
	} finally {
		discontinuity.mockRestore();
		late?.close();
		encoder.close();
		broadcast.close();
		installed.close();
	}
});

// A zero max age retains nothing, so a gap must not depend on the backlog to be noticed, or every
// frame after it would wait forever.
test("a capture gap without retention resumes the audio after it", async () => {
	using _webcodecs = installFakeWebCodecs();
	const clock = new Manual();
	const installed = clock.install();
	const broadcast = new Broadcast({
		origin: new Moq.Origin.Producer(),
		name: Moq.Path.from("unretained.hang"),
		maxAge: Duration.millis(0),
	});
	const feed = new Feed();
	const configuring = configured();
	const encoder = new Encoder("audio", { broadcast, capture: fakeCapture(feed) as never });
	const encode = spyOn(Container.Legacy.Producer.prototype, "encode");
	const discontinuity = spyOn(Container.Legacy.Producer.prototype, "discontinuity");
	let subscriber: Moq.Track.Subscriber | undefined;
	try {
		await configuring;
		await settle();
		const push = async (start: number, count: number) => {
			for (let index = 0; index < count; index++) {
				await feed.push({
					timestamp: Duration.micros(start + index * 20_000),
					channels: [new Float32Array(960)],
				});
				await feed.drain();
				await clock.advance(Duration.millis(20));
			}
		};
		const net = broadcast.net.peek();
		if (!net) throw new Error("expected network broadcast");
		subscriber = net.track("audio").subscribe();
		await settle();

		await push(0, 4);
		await push(200_000, 4);

		expect(encode.mock.calls.map(([, timestamp]) => timestamp)).toEqual(
			[0, 20_000, 40_000, 60_000, 200_000, 220_000, 240_000, 260_000].map((t) => Duration.micros(t)),
		);
		expect(discontinuity.mock.calls.map(([end]) => end)).toEqual([Duration.micros(80_000)]);
	} finally {
		encode.mockRestore();
		discontinuity.mockRestore();
		subscriber?.close();
		encoder.close();
		broadcast.close();
		installed.close();
	}
});

// Two disables before anyone listens leave two breaks in the backlog: each follows the audio before it.
test("every disable retained before demand gets its own break", async () => {
	using _webcodecs = installFakeWebCodecs();
	const clock = new Manual();
	const installed = clock.install();
	const enabled = new Signal(true);
	const broadcast = new Broadcast({
		origin: new Moq.Origin.Producer(),
		name: Moq.Path.from("twice.hang"),
		maxAge: Duration.millis(1000),
	});
	const feed = new Feed();
	const configuring = configured();
	const encoder = new Encoder("audio", { broadcast, enabled, capture: fakeCapture(feed) as never });
	const discontinuity = spyOn(Container.Legacy.Producer.prototype, "discontinuity");
	let late: Moq.Track.Subscriber | undefined;
	try {
		await configuring;
		await settle();
		const push = async (start: number, count: number) => {
			for (let index = 0; index < count; index++) {
				await feed.push({
					timestamp: Duration.micros(start + index * 20_000),
					channels: [new Float32Array(960)],
				});
				await feed.drain();
				await clock.advance(Duration.millis(20));
			}
		};
		await push(0, 4);
		enabled.set(false);
		await settle();
		enabled.set(true);
		await settle();
		await push(200_000, 3);
		enabled.set(false);
		await settle();

		const net = broadcast.net.peek();
		if (!net) throw new Error("expected network broadcast");
		late = net.track("audio").subscribe({ delay: { max: Duration.millis(1000) } });
		await settle(20);
		expect(encoder.out.stats.peek().frames).toBe(7);
		expect(discontinuity.mock.calls.map(([end]) => end)).toEqual([
			Duration.micros(80_000),
			Duration.micros(260_000),
		]);
	} finally {
		discontinuity.mockRestore();
		late?.close();
		encoder.close();
		broadcast.close();
		installed.close();
	}
});

// History that expired while disabled leaves a break with nothing in flight, which must not stall
// the audio resumed after it.
test("a break after expired history keeps the resumed audio flowing", async () => {
	using _webcodecs = installFakeWebCodecs();
	const clock = new Manual();
	const installed = clock.install();
	const enabled = new Signal(true);
	const broadcast = new Broadcast({
		origin: new Moq.Origin.Producer(),
		name: Moq.Path.from("expired.hang"),
		maxAge: Duration.millis(1000),
	});
	const feed = new Feed();
	const configuring = configured();
	const encoder = new Encoder("audio", { broadcast, enabled, capture: fakeCapture(feed) as never });
	let late: Moq.Track.Subscriber | undefined;
	try {
		await configuring;
		await settle();
		const push = async (start: number, count: number) => {
			for (let index = 0; index < count; index++) {
				await feed.push({
					timestamp: Duration.micros(start + index * 20_000),
					channels: [new Float32Array(960)],
				});
				await feed.drain();
				await clock.advance(Duration.millis(20));
			}
		};
		await push(0, 4);
		enabled.set(false);
		await settle();
		await clock.advance(Duration.millis(1100));
		enabled.set(true);
		await settle();
		await push(2_000_000, 3);

		const net = broadcast.net.peek();
		if (!net) throw new Error("expected network broadcast");
		late = net.track("audio").subscribe({ delay: { max: Duration.millis(1000) } });
		await settle(20);
		expect(encoder.out.stats.peek().frames).toBe(3);
		await push(2_060_000, 2);
		expect(encoder.out.stats.peek().frames).toBe(5);
	} finally {
		late?.close();
		encoder.close();
		broadcast.close();
		installed.close();
	}
});

// Demand that arrives after a re-enable but before any resumed frame drains the retained audio first,
// so the break follows it rather than cutting it off with the codec mid-flight.
test("demand between a re-enable and the resumed audio still breaks after the retained audio", async () => {
	using _webcodecs = installFakeWebCodecs();
	const clock = new Manual();
	const installed = clock.install();
	const enabled = new Signal(true);
	const broadcast = new Broadcast({
		origin: new Moq.Origin.Producer(),
		name: Moq.Path.from("between.hang"),
		maxAge: Duration.millis(1000),
	});
	const feed = new Feed();
	const configuring = configured();
	const encoder = new Encoder("audio", { broadcast, enabled, capture: fakeCapture(feed) as never });
	const discontinuity = spyOn(Container.Legacy.Producer.prototype, "discontinuity");
	let late: Moq.Track.Subscriber | undefined;
	try {
		await configuring;
		await settle();
		const push = async (start: number, count: number) => {
			for (let index = 0; index < count; index++) {
				await feed.push({
					timestamp: Duration.micros(start + index * 20_000),
					channels: [new Float32Array(960)],
				});
				await feed.drain();
				await clock.advance(Duration.millis(20));
			}
		};
		await push(0, 4);
		enabled.set(false);
		await settle();
		enabled.set(true);
		await settle();

		const net = broadcast.net.peek();
		if (!net) throw new Error("expected network broadcast");
		late = net.track("audio").subscribe({ delay: { max: Duration.millis(1000) } });
		await settle(20);
		expect(encoder.out.stats.peek().frames).toBe(4);
		expect(discontinuity.mock.calls.map(([end]) => end)).toEqual([Duration.micros(80_000)]);

		await push(200_000, 3);
		expect(encoder.out.stats.peek().frames).toBe(7);
		expect(discontinuity).toHaveBeenCalledTimes(1);
	} finally {
		discontinuity.mockRestore();
		late?.close();
		encoder.close();
		broadcast.close();
		installed.close();
	}
});

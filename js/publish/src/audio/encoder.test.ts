import { describe, expect, mock, spyOn, test } from "bun:test";
import { Manual, Micro, Milli, Nano, now } from "@moq/clock";
import * as Catalog from "@moq/hang/catalog";
import * as Container from "@moq/hang/container";
import * as Json from "@moq/json";
import * as Moq from "@moq/net";
import { Signal } from "@moq/signals";
import { Broadcast } from "../broadcast";
import { Estimator, Order } from "../order";
import type { AudioFrame, Format } from "./capture";
import { type Codec, Encoder, resolve, toEncoderConfig } from "./encoder";

/** Install a Manual clock reading `ms`, set forward the way a mocked `performance.now()` was. */
function manualAt(ms: number) {
	const clock = new Manual();
	const installed = clock.install();
	const set = (to: number) => clock.jump(Nano(Nano.fromMilli(Milli(to)) - clock.now()));
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
		const resolved = resolve(captured, { mime: "opus", frameDuration: Milli(2.5) });
		expect(resolved.frameDuration).toBe(Micro(2_500));
		expect(resolved.catalog.jitter).toBeUndefined();
	});

	test("carries every Opus frame duration", () => {
		for (const millis of [2.5, 5, 10, 20, 40, 60]) {
			const resolved = resolve(captured, { mime: "opus", frameDuration: Milli(millis) });
			expect(resolved.frameDuration).toBe(Micro(millis * 1000));
		}
	});

	// Otherwise AudioEncoder.configure throws instead, by which point the rendition has already
	// been advertised and the failure lands on a subscriber rather than the caller.
	test("rejects a duration Opus cannot encode", () => {
		for (const millis of [2.5005, 15, 0, -20]) {
			expect(() => resolve(captured, { mime: "opus", frameDuration: Milli(millis) })).toThrow();
		}
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

// Like Chrome's Opus encoder, it holds the newest chunks until later input pushes them out, and
// stamps each chunk from the first input's timestamp plus the audio encoded since, so a jump in
// input timestamps never reaches the output.
class LaggingAudioEncoder {
	static instances: LaggingAudioEncoder[] = [];
	static probes: AudioEncoderConfig[] = [];
	static description: ((config: AudioEncoderConfig) => Uint8Array) | undefined;
	static readonly LAG = 2;

	// Called on configure; the encoder publishes its pipeline synchronously right after.
	static onConfigure: ((config: AudioEncoderConfig) => void) | undefined;

	state: CodecState = "unconfigured";
	#output: EncodedAudioChunkOutputCallback;
	#held: { timestamp: number; duration: number }[] = [];
	#base: number | undefined;
	#encoded = 0;
	#probe = false;
	#config?: AudioEncoderConfig;
	#error: AudioEncoderInit["error"];

	constructor(init: AudioEncoderInit) {
		this.#output = init.output;
		this.#error = init.error;
		LaggingAudioEncoder.instances.push(this);
	}

	fail(error: Error): void {
		this.state = "closed";
		this.#error(error as DOMException);
	}

	configure(config: AudioEncoderConfig): void {
		this.state = "configured";
		this.#config = config;
		queueMicrotask(() => {
			if (!this.#probe) LaggingAudioEncoder.onConfigure?.(config);
		});
	}

	encode(data: AudioData): void {
		const duration = Math.round((data.numberOfFrames / data.sampleRate) * 1_000_000);
		this.#base ??= data.timestamp;
		this.#held.push({ timestamp: this.#base + this.#encoded, duration });
		this.#encoded += duration;
		while (this.#held.length > LaggingAudioEncoder.LAG) {
			this.#emit(this.#held.shift() as { timestamp: number; duration: number });
		}
	}

	#emit({ timestamp, duration }: { timestamp: number; duration: number }): void {
		const chunk = { type: "key", timestamp, duration, byteLength: 1, copyTo: (dest: Uint8Array) => dest.set([1]) };
		const config = this.#config;
		const description = config && LaggingAudioEncoder.description?.(config);
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
		this.#probe = true;
		if (this.#config) LaggingAudioEncoder.probes.push(structuredClone(this.#config));
		const index = LaggingAudioEncoder.instances.indexOf(this);
		if (index !== -1) LaggingAudioEncoder.instances.splice(index, 1);
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
	LaggingAudioEncoder.instances = [];
	LaggingAudioEncoder.probes = [];
	LaggingAudioEncoder.description = undefined;
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
		LaggingAudioEncoder.onConfigure = (config) => {
			LaggingAudioEncoder.onConfigure = undefined;
			resolve(config);
		};
	});
}

// An Encoder wired to a fake capture feed, recording each written frame as [timestamp, payload bytes]
// and how many frames each group carries.
async function setup(order = new Order(), codec?: Codec, groupDuration?: Milli) {
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
		config: new Signal(undefined),
		track: new Signal<Moq.Track.Producer | undefined>(track),
		close: () => {
			track.finish();
			track.close();
		},
	};

	const feed = new Feed();
	const capture = fakeCapture(feed);

	const enabled = new Signal(true);
	const encoder = new Encoder("audio", {
		broadcast: { audio: () => rendition, order } as never,
		capture: capture as never,
		enabled,
		codec,
		groupDuration,
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
	using env = await setup();
	const { track, rendition, feed, written, writes } = env;

	// One 20ms Opus frame per push, on a clock with a fractional microsecond origin.
	let index = 0;
	const push = async (count: number) => {
		for (let i = 0; i < count; i++, index++) {
			await feed.push({ timestamp: Micro(18_699.6 + index * 20_000), channels: [new Float32Array(960)] });
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
// that stays across it, or joins during it, must not read the audio before it as live.
test("disabling with a subscriber attached marks where submitted audio ends", async () => {
	using _webcodecs = installFakeWebCodecs();
	using env = await setup();
	const { enabled, feed, written, writes } = env;

	let index = 0;
	const push = async (count: number) => {
		for (let i = 0; i < count; i++, index++) {
			await feed.push({ timestamp: Micro(20_000 + index * 20_000), channels: [new Float32Array(960)] });
		}
		await feed.drain();
	};

	await push(4); // two written, two held

	const marked = new Promise<void>((resolve) => {
		writes.onWrite = resolve;
	});
	enabled.set(false);
	await marked;
	writes.onWrite = undefined;

	await push(2); // nothing publishing
	const resumed = configured();
	enabled.set(true);
	await resumed;
	await push(3); // a fresh AudioEncoder, so one written and two held

	expect(written).toEqual([
		[20_000, 1],
		[40_000, 1],
		[100_000, 0],
		[140_000, 1],
	]);
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
	expect(LaggingAudioEncoder.probes).toHaveLength(1);

	enabled.set(false);
	await settle();
	expect(encoder.out.catalog.peek()?.enabled).toBe(false);

	enabled.set(true);
	await settle(2);
	expect(encoder.out.catalog.peek()).toEqual(before);
	expect(LaggingAudioEncoder.probes).toHaveLength(1);
});

test("re-enabling with a changed sample rate probes again", async () => {
	using _webcodecs = installFakeWebCodecs();
	using env = await setup();
	const { encoder, enabled, capture } = env;

	enabled.set(false);
	await settle();
	capture.out.format.set({ sampleRate: 24_000, channelCount: 1 });

	const configuring = configured();
	enabled.set(true);
	const config = await configuring;
	expect(config.sampleRate).toBe(24_000);
	expect(LaggingAudioEncoder.probes.map((probe) => probe.sampleRate)).toEqual([48_000, 24_000]);
	expect(encoder.out.catalog.peek()).toMatchObject({ sampleRate: 24_000 });
	expect(encoder.out.catalog.peek()?.enabled).toBeUndefined();
});

// Chrome stamps encoder output from its first input, ignoring later jumps, so audio resumed after a
// long mute must still carry its capture timestamps rather than trail them by the mute.
test("audio resumed after a multi-second mute carries its capture timestamps", async () => {
	using _webcodecs = installFakeWebCodecs();
	using env = await setup();
	const { enabled, feed, written, writes } = env;

	const push = async (start: number, count: number) => {
		for (let i = 0; i < count; i++) {
			await feed.push({ timestamp: Micro(start + i * 20_000), channels: [new Float32Array(960)] });
		}
		await feed.drain();
	};

	await push(20_000, 4); // two written, two held

	const marked = new Promise<void>((resolve) => {
		writes.onWrite = resolve;
	});
	enabled.set(false);
	await marked;
	writes.onWrite = undefined;

	// Capture keeps running through a five-second mute.
	await push(100_000, 250);
	const resumed = configured();
	enabled.set(true);
	await resumed;
	await push(5_100_000, 4); // two written, two held

	expect(written).toEqual([
		[20_000, 1],
		[40_000, 1],
		[100_000, 0],
		[5_100_000, 1],
		[5_120_000, 1],
	]);
	expect(LaggingAudioEncoder.probes).toHaveLength(1);
});

// Closing tears down the subscription and the pipeline, which both end the epoch; cleanups run
// last-in, first-out, so the marker lands once and before the rendition closes the track.
test("closing with a subscriber attached marks the end once before the track closes", async () => {
	using _webcodecs = installFakeWebCodecs();
	using env = await setup();
	const { encoder, track, feed, written } = env;

	for (let i = 0; i < 4; i++) {
		await feed.push({ timestamp: Micro(20_000 + i * 20_000), channels: [new Float32Array(960)] });
	}
	await feed.drain(); // two written, two held

	encoder.close();

	expect(written).toEqual([
		[20_000, 1],
		[40_000, 1],
		[100_000, 0],
	]);
	expect(track.closed.peek()).toBeDefined();
});

// A push that completes several frames is still one continuous stream, so it must not restart the
// encoder and drop the chunks it holds.
test("a push completing several frames keeps the encoder running", async () => {
	using _webcodecs = installFakeWebCodecs();
	using env = await setup();
	const { feed, written } = env;

	// Two 20ms Opus frames per push.
	for (let index = 0; index < 3; index++) {
		await feed.push({ timestamp: Micro(18_699.6 + index * 40_000), channels: [new Float32Array(1920)] });
	}
	await feed.drain();

	expect(written).toEqual([
		[18_700, 1],
		[38_700, 1],
		[58_700, 1],
		[78_700, 1],
	]);
});

// The first frame at the minimum opens the next group. A timeline break closes the group early,
// so the first frame after it opens a fresh one.
test("a group duration packs frames until the minimum and restarts after a break", async () => {
	using _webcodecs = installFakeWebCodecs();
	using env = await setup(new Order(), undefined, Milli(100));
	const { enabled, feed, written, groups, appended, writes } = env;

	let index = 0;
	const push = async (count: number) => {
		for (let i = 0; i < count; i++, index++) {
			await feed.push({ timestamp: Micro(20_000 + index * 20_000), channels: [new Float32Array(960)] });
		}
		await feed.drain();
	};

	await push(7); // five written, two held
	expect(groups).toEqual([5]);
	expect(appended[0].closed.peek()).toBeUndefined();
	await push(2); // the next frame opens a new group and closes the first
	expect(groups).toEqual([5, 2]);
	expect(appended[0].closed.peek()).toBeNull();

	const marked = new Promise<void>((resolve) => {
		writes.onWrite = resolve;
	});
	enabled.set(false);
	await marked;
	writes.onWrite = undefined;

	const resumed = configured();
	enabled.set(true);
	await resumed;
	await push(4); // a fresh AudioEncoder, so two written and two held

	expect(written.map(([timestamp]) => timestamp)).toEqual([
		20_000, 40_000, 60_000, 80_000, 100_000, 120_000, 140_000, 200_000, 200_000, 220_000,
	]);
	// 120ms opens the next group, closing the five frames at 20-100ms. The break ends the
	// 120-140ms group early, so resumed frames start a fresh group after the marker.
	expect(groups).toEqual([5, 2, 1, 2]);
});

test("default audio groups carry twenty milliseconds", async () => {
	using _webcodecs = installFakeWebCodecs();
	for (const [frameMs, packets, groupMs] of [
		[2.5, 8, undefined],
		[10, 2, undefined],
		[20, 1, undefined],
		[2.5, 1, 0],
	] as const) {
		using env = await setup(
			new Order(),
			{ mime: "opus", frameDuration: Milli(frameMs) },
			groupMs === undefined ? undefined : Milli(groupMs),
		);
		for (let index = 0; index < packets * 2 + LaggingAudioEncoder.LAG; index++) {
			await env.feed.push({
				timestamp: Micro(20_000 + index * frameMs * 1000),
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
// this one trailing it, which the catalog advertises as `delay`.
test("a rendition written behind another advertises delay", async () => {
	using _webcodecs = installFakeWebCodecs();
	const clock = manualAt(200);

	try {
		const order = new Order();
		using env = await setup(order);
		const { encoder, feed } = env;

		expect(encoder.out.catalog.peek()?.delay).toBeUndefined();

		// A sibling that already wrote media up to now.
		new Estimator().write(Nano.toMicro(Nano(now())), order);

		// Captured 100ms ago, with a clock origin that keeps timestamps nonnegative.
		const start = Nano.toMicro(Nano(now())) - 100_000;
		for (let index = 0; index < 4; index++) {
			await feed.push({ timestamp: Micro(start + index * 20_000), channels: [new Float32Array(960)] });
		}
		await feed.drain();

		expect(env.written).toEqual([
			[100_000, 1],
			[120_000, 1],
		]);
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
		codec: { mime: "opus", frameDuration: Milli(15) },
	});

	try {
		await settle();
		expect(encoder.out.catalog.peek()).toBeUndefined();
		expect(encoder.settled.peek()).toBe(true);
		expect(error).toHaveBeenCalled();

		// A valid duration resolves, and the config keeps it settled.
		const ready = encoder.out.catalog.changed();
		encoder.codec.set({ mime: "opus", frameDuration: Milli(20) });
		await ready;
		await settle();
		expect(encoder.out.catalog.peek()).toBeDefined();
		expect(encoder.settled.peek()).toBe(true);
	} finally {
		encoder.close();
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
	const live = LaggingAudioEncoder.instances.at(-1);
	if (!live) throw new Error("expected live audio encoder");
	const failure = new Error("audio encoder failed");
	live.fail(failure);
	for (let i = 0; i < 10; i++) await Promise.resolve();
	expect(env.track.closed.peek()).toBe(failure);
	expect(env.encoder.out.error.peek()).toBe(failure);
	expect(env.encoder.out.catalog.peek()).toMatchObject({ codec: "opus", enabled: false });
	expect(env.encoder.out.active.peek()).toBe(false);
	expect(LaggingAudioEncoder.instances).toHaveLength(1);
});

test("audio failure stays local through demand changes and capture format re-arms a fresh track", async () => {
	using _webcodecs = installFakeWebCodecs();
	using _now = manualAt(0);
	const broadcast = new Broadcast({ origin: new Moq.Origin.Producer(), name: Moq.Path.from("failure.hang") });
	const feed = new Feed();
	const capture = fakeCapture(feed);
	const estimate = new Signal<number | undefined>(10_000_000);
	const bandwidth = new Moq.Bandwidth.Allocator(estimate);
	const enabled = new Signal(true);
	const configuring = configured();
	const encoder = new Encoder("audio", { broadcast, capture: capture as never, bandwidth, enabled });
	let subscriber: Moq.Track.Subscriber | undefined;
	let siblingSubscriber: Moq.Track.Subscriber | undefined;
	let catalogSubscriber: Moq.Track.Subscriber | undefined;
	let recovered: Moq.Track.Subscriber | undefined;
	const push = async (first: number, channels: number) => {
		for (let i = 0; i < 3; i++) {
			await feed.push({
				timestamp: Micro((first + i) * 20_000),
				channels: Array.from({ length: channels }, () => new Float32Array(960)),
			});
		}
		await feed.drain();
	};
	let announced: Moq.Origin.Requesting | undefined;
	try {
		await configuring;
		await settle();
		const net = broadcast.net.peek();
		if (!net) throw new Error("expected network broadcast");
		announced = broadcast.in.origin.peek()?.follow(broadcast.in.name.peek());
		const epoch = announced?.active.peek()?.epoch;
		const sibling = broadcast.video("video");
		sibling.config.set({ codec: "avc1.640028", container: { kind: "legacy" } });
		await settle();
		siblingSubscriber = net.track("video").subscribe();
		subscriber = net.track("audio").subscribe();
		await settle();
		const siblingTrack = sibling.track.peek();
		if (!siblingTrack) throw new Error("expected demanded sibling track");
		expect(siblingTrack.appendGroup().sequence).toBe(0);
		await push(0, 1);
		expect((await subscriber.largest())?.group).toBe(0);
		catalogSubscriber = net.track(Broadcast.CATALOG_TRACK).subscribe();
		const catalog = new Json.Snapshot.Consumer<Catalog.Root>({ track: catalogSubscriber });
		expect((await catalog.latest())?.value.audio?.renditions.audio?.enabled).not.toBe(false);
		const live = LaggingAudioEncoder.instances.at(-1);
		if (!live) throw new Error("expected live audio encoder");
		const failure = new Error("audio codec failed");
		live.fail(failure);
		await settle();
		expect(subscriber.closed.peek()).toBe(failure);
		expect(encoder.out.error.peek()).toBe(failure);
		expect((await catalog.latest())?.value.audio?.renditions.audio?.enabled).toBe(false);
		expect(encoder.out.active.peek()).toBe(false);
		const instances = LaggingAudioEncoder.instances.length;
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
		expect(LaggingAudioEncoder.instances).toHaveLength(instances);
		expect(broadcast.net.peek()).toBe(net);
		expect(announced?.active.peek()?.epoch).toBe(epoch);
		expect(siblingTrack.appendGroup().sequence).toBe(1);

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
		expect((await recovered.largest())?.group).toBe(0);
		expect(encoder.out.active.peek()).toBe(true);
		expect(recovered.closed.peek()).toBeUndefined();
		expect(broadcast.net.peek()).toBe(net);
		expect(announced?.active.peek()?.epoch).toBe(epoch);
		expect(siblingTrack.appendGroup().sequence).toBe(2);
	} finally {
		subscriber?.close();
		siblingSubscriber?.close();
		catalogSubscriber?.close();
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
	let catalogSubscriber: Moq.Track.Subscriber | undefined;
	try {
		await configuring;
		await settle();
		const net = broadcast.net.peek();
		if (!net) throw new Error("expected network broadcast");
		const live = LaggingAudioEncoder.instances.at(-1);
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
		catalogSubscriber = net.track(Broadcast.CATALOG_TRACK).subscribe();
		const catalog = new Json.Snapshot.Consumer<Catalog.Root>({ track: catalogSubscriber });
		const config = (await catalog.latest())?.value.audio?.renditions.audio;
		expect(config?.track).toBeDefined();
		expect(config?.track).not.toBe("audio");
		expect(encoder.out.error.peek()).toBeUndefined();
		expect(broadcast.net.peek()).toBe(net);
	} finally {
		catalogSubscriber?.close();
		encoder.close();
		broadcast.close();
	}
});

test.each(["failure first", "config first"])(
	"same-turn audio failure and config recovery keep a fresh track: %s",
	async (order) => {
		using _webcodecs = installFakeWebCodecs();
		using _now = manualAt(0);
		const broadcast = new Broadcast({ origin: new Moq.Origin.Producer(), name: Moq.Path.from("coalesced.hang") });
		const configuring = configured();
		const encoder = new Encoder("audio", { broadcast, capture: fakeCapture(new Feed()) as never });
		let catalogSubscriber: Moq.Track.Subscriber | undefined;
		try {
			await configuring;
			await settle();
			const net = broadcast.net.peek();
			if (!net) throw new Error("expected network broadcast");
			const live = LaggingAudioEncoder.instances.at(-1);
			if (!live) throw new Error("expected idle audio encoder");
			const reconfiguring = configured();
			if (order === "config first") encoder.codec.set({ mime: "opus", bitrate: 64_000 });
			live.fail(new Error("idle encoder failed"));
			if (order === "failure first") encoder.codec.set({ mime: "opus", bitrate: 64_000 });
			await reconfiguring;
			await settle();
			catalogSubscriber = net.track(Broadcast.CATALOG_TRACK).subscribe();
			const catalog = new Json.Snapshot.Consumer<Catalog.Root>({ track: catalogSubscriber });
			const config = (await catalog.latest())?.value.audio?.renditions.audio;
			expect(config?.track).toBeDefined();
			expect(config?.track).not.toBe("audio");
			expect(encoder.out.error.peek()).toBeUndefined();
			// Encoding resumes on the fresh track without waiting for a subscriber.
			expect(encoder.out.active.peek()).toBe(true);
			expect(broadcast.net.peek()).toBe(net);
		} finally {
			catalogSubscriber?.close();
			encoder.close();
			broadcast.close();
		}
	},
);

// Audio encoded with nobody listening waits in the cache for a late subscriber's maxDelay, and the
// last subscriber leaving ends no epoch. On demand, a subscriber finds only what its arrival started.
test.each([false, true])(
	"a late subscriber reaches audio encoded before it arrived unless on demand (%p)",
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
			expect(encoder.out.active.peek()).toBe(!onDemand);

			// One 20ms Opus frame per push, each its own group.
			let index = 0;
			const push = async (count: number) => {
				for (let i = 0; i < count; i++, index++) {
					await feed.push({ timestamp: Micro(index * 20_000), channels: [new Float32Array(960)] });
				}
				await feed.drain();
			};

			await push(4); // unwatched: two written, two held
			late = net.track("audio").subscribe({ maxDelay: Milli(10_000) });
			await settle();
			expect(encoder.out.active.peek()).toBe(true);
			await push(3);

			const group = await late.ordered().nextGroup();
			if (!group) throw new Error("expected a group");
			const frame = await group.readFrame();
			if (!frame) throw new Error("expected a frame");
			const [timestamp] = Moq.Varint.decode(frame.payload);
			// On demand, nothing was submitted before the subscriber arrived.
			expect(group.sequence).toBe(0);
			expect(timestamp).toBe(onDemand ? 80_000 : 0);

			// Only on demand does the last subscriber leaving end the epoch.
			late.close();
			late = undefined;
			await settle();
			expect(discontinuity).toHaveBeenCalledTimes(onDemand ? 1 : 0);
			expect(encoder.out.active.peek()).toBe(!onDemand);
		} finally {
			discontinuity.mockRestore();
			late?.close();
			encoder.close();
			broadcast.close();
		}
	},
);

test("Opus description is probed with the exact live config before advertising", async () => {
	using _codecs = installFakeWebCodecs();
	LaggingAudioEncoder.description = (config) => new TextEncoder().encode(JSON.stringify(config));
	using env = await setup(new Order(), {
		mime: "opus",
		frameDuration: Milli(2.5),
		complexity: 3,
		packetlossperc: 7,
		useinbandfec: true,
		bitrate: 64000,
	});
	expect(env.encoder.out.catalog.peek()?.description).toBe(Buffer.from(JSON.stringify(env.config)).toString("hex"));
	expect(LaggingAudioEncoder.probes).toEqual([env.config]);
	expect(LaggingAudioEncoder.instances).toHaveLength(1);
	expect(env.written).toHaveLength(0);
	const configuring = configured();
	env.capture.in.source.set({ track: {}, kind: "voice" } as never);
	const voice = await configuring;
	expect(voice.opus).toMatchObject({ application: "voip", signal: "voice" });
	expect(LaggingAudioEncoder.probes.at(-1)).toEqual(voice);
	expect(env.encoder.out.catalog.peek()?.description).toBe(Buffer.from(JSON.stringify(voice)).toString("hex"));
});

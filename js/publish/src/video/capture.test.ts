import { expect, mock, test } from "bun:test";
import { Manual, Milli, Nano } from "@moq/clock";
import * as Moq from "@moq/net";
import { Effect, Signal } from "@moq/signals";
import { Order } from "../order";
import type { StreamTrack } from "./types";

mock.module("./capture-worker.ts?worklet", () => ({ default: async () => "blob:fake-worker" }));
const { Capture } = await import("./capture");
const { Encoder } = await import("./encoder");
const { Renderer } = await import("../preview");

/** Install a Manual clock reading `ms`, set forward the way a mocked `performance.now()` was. */
function manualAt(ms: number) {
	const clock = new Manual();
	const installed = clock.install();
	const set = (to: number) => clock.jump(Nano(Nano.fromMilli(Milli(to)) - clock.now()));
	set(ms);
	return { set, close: () => installed.close(), [Symbol.dispose]: () => installed.close() };
}

class Frame {
	// Every frame built, so a test can assert none is left open.
	static all: Frame[] = [];

	codedWidth: number;
	codedHeight: number;
	rotation?: number;
	timestamp: number;
	closed = false;

	constructor(source?: Frame, init?: { timestamp?: number; rotation?: number }) {
		this.codedWidth = source?.codedWidth ?? 1920;
		this.codedHeight = source?.codedHeight ?? 1080;
		// Like WebCodecs, a constructor rotation adds to the source frame's own.
		this.rotation =
			init?.rotation === undefined ? source?.rotation : ((source?.rotation ?? 0) + init.rotation) % 360;
		this.timestamp = init?.timestamp ?? source?.timestamp ?? 0;
		Frame.all.push(this);
	}

	clone(): Frame {
		return new Frame(this);
	}

	close(): void {
		this.closed = true;
	}
}

test("capture resamples live scale when frame dimensions stay unchanged", async () => {
	const processor = Object.getOwnPropertyDescriptor(globalThis, "MediaStreamTrackProcessor");
	const videoFrame = Object.getOwnPropertyDescriptor(globalThis, "VideoFrame");
	let controller!: ReadableStreamDefaultController<VideoFrame>;
	const readable = new ReadableStream<VideoFrame>({
		start: (value) => {
			controller = value;
		},
	});
	Object.defineProperty(globalThis, "MediaStreamTrackProcessor", {
		configurable: true,
		value: class {
			readonly readable = readable;
		},
	});
	Object.defineProperty(globalThis, "VideoFrame", { configurable: true, value: Frame });
	let scale = 2;
	const capture = new Capture({
		source: {
			track: {} as StreamTrack,
			get scale() {
				return scale;
			},
		},
	});
	try {
		let changed = capture.out.display.changed();
		controller.enqueue(new Frame() as unknown as VideoFrame);
		expect(await changed).toMatchObject({ width: 1920, height: 1080, scale: 2 });
		scale = 1;
		changed = capture.out.display.changed();
		controller.enqueue(new Frame() as unknown as VideoFrame);
		expect(await changed).toMatchObject({ width: 1920, height: 1080, scale: 1 });
	} finally {
		capture.close();
		if (processor) Object.defineProperty(globalThis, "MediaStreamTrackProcessor", processor);
		else Reflect.deleteProperty(globalThis, "MediaStreamTrackProcessor");
		if (videoFrame) Object.defineProperty(globalThis, "VideoFrame", videoFrame);
		else Reflect.deleteProperty(globalThis, "VideoFrame");
	}
});

// Let signal effects settle without waiting on the wall clock.
async function settle(times = 20): Promise<void> {
	for (let i = 0; i < times; i++) await new Promise<void>((resolve) => queueMicrotask(resolve));
}

function sized(width: number, height: number, rotation?: number): VideoFrame {
	return Object.assign(new Frame(), { codedWidth: width, codedHeight: height, rotation }) as unknown as VideoFrame;
}

test("rotation follows the live frames, not the track's settings", async () => {
	const processor = Object.getOwnPropertyDescriptor(globalThis, "MediaStreamTrackProcessor");
	const videoFrame = Object.getOwnPropertyDescriptor(globalThis, "VideoFrame");
	let controller!: ReadableStreamDefaultController<VideoFrame>;
	const readable = new ReadableStream<VideoFrame>({
		start: (value) => {
			controller = value;
		},
	});
	Object.defineProperty(globalThis, "MediaStreamTrackProcessor", {
		configurable: true,
		value: class {
			readonly readable = readable;
		},
	});
	Object.defineProperty(globalThis, "VideoFrame", { configurable: true, value: Frame });

	// Safari reports the requested landscape size until the first frame, then delivers portrait.
	const track = { getSettings: () => ({ width: 1280, height: 720 }) } as unknown as StreamTrack;
	const orientation = new Signal(90);
	const capture = new Capture({ source: track, orientation });
	try {
		const display = capture.out.display.changed();
		controller.enqueue(sized(720, 1280));
		expect(await display).toMatchObject({ width: 720, height: 1280 });
		await settle();
		expect(capture.out.rotation.peek()).toBe(0);

		// A sideways landscape picture on a portrait screen takes the screen's turn.
		let rotation = capture.out.rotation.changed();
		controller.enqueue(sized(1280, 720));
		expect(await rotation).toBe(90);

		rotation = capture.out.rotation.changed();
		orientation.set(270);
		expect(await rotation).toBe(270);

		rotation = capture.out.rotation.changed();
		orientation.set(0);
		expect(await rotation).toBe(0);

		// A frame that declares its own rotation wins over the screen, and readers get it untagged so
		// they turn it only once.
		orientation.set(270);
		expect(await capture.out.rotation.changed()).toBe(270);
		rotation = capture.out.rotation.changed();
		controller.enqueue(sized(1280, 720, 90));
		expect(await rotation).toBe(90);

		const effect = new Effect();
		try {
			const reader = capture.out.frames.peek()?.subscribe(effect).getReader();
			const held = (await reader?.read())?.value as unknown as Frame | undefined;
			expect(held?.rotation).toBe(0);
			held?.close();
		} finally {
			effect.close();
		}

		// A browser that reports an upright frame is believed, even on a portrait screen.
		rotation = capture.out.rotation.changed();
		controller.enqueue(sized(1280, 720, 0));
		expect(await rotation).toBe(0);
	} finally {
		capture.close();
		if (processor) Object.defineProperty(globalThis, "MediaStreamTrackProcessor", processor);
		else Reflect.deleteProperty(globalThis, "MediaStreamTrackProcessor");
		if (videoFrame) Object.defineProperty(globalThis, "VideoFrame", videoFrame);
		else Reflect.deleteProperty(globalThis, "VideoFrame");
	}
	expect(capture.out.rotation.peek()).toBe(0);
});

// Installs Frame as the global VideoFrame and returns a FrameSource fed by hand.
function frameSource() {
	const original = Object.getOwnPropertyDescriptor(globalThis, "VideoFrame");
	Object.defineProperty(globalThis, "VideoFrame", { configurable: true, value: Frame, writable: true });
	Frame.all = [];

	let controller!: ReadableStreamDefaultController<VideoFrame>;
	const frames = new ReadableStream<VideoFrame>({
		start: (c) => {
			controller = c;
		},
	});

	return {
		source: { frames, frameRate: 30 },
		push: (timestamp: number) => controller.enqueue(new Frame(undefined, { timestamp }) as unknown as VideoFrame),
		[Symbol.dispose]() {
			if (original) Object.defineProperty(globalThis, "VideoFrame", original);
			else Reflect.deleteProperty(globalThis, "VideoFrame");
		},
	};
}

// Let the capture pump drain everything pushed so far. The pipeline runs on promises alone, so one
// macrotask turn settles every microtask queued before it, with no wall-clock wait.
const flush = () => new Promise((resolve) => setImmediate(resolve));

const open = () => Frame.all.filter((frame) => !frame.closed);

test("a reader attaching to a still source starts with the current picture, stamped now", async () => {
	using input = frameSource();
	const clock = manualAt(1_000);
	const capture = new Capture({ source: input.source });
	const effect = new Effect();

	try {
		// The only frame the source sends, with nobody reading yet.
		input.push(1_000_000);
		await flush();

		// Attach 90s later.
		clock.set(91_000);
		const fanout = capture.out.frames.peek();
		if (!fanout) throw new Error("no fanout");
		const reader = fanout.subscribe(effect).getReader();

		const first = (await reader.read()).value as unknown as Frame;
		expect(first.timestamp).toBe(91_000_000);
		first.close();

		// Captured before the copy's stamp, so it would run time backwards: skipped.
		input.push(90_999_000);
		input.push(91_033_000);
		const next = (await reader.read()).value as unknown as Frame;
		expect(next.timestamp).toBe(91_033_000);
		next.close();
	} finally {
		effect.close();
		capture.close();
		clock.close();
	}

	expect(open()).toEqual([]);
});

test("a reader attaching after another never starts below what that one read", async () => {
	using input = frameSource();
	// The capture clock runs 200ms ahead of the installed clock, as it does when the first frame,
	// which anchors the capture clock, arrived late.
	const clock = manualAt(1_000);
	const capture = new Capture({ source: input.source });
	const first = new Effect();
	const second = new Effect();

	try {
		const fanout = capture.out.frames.peek() ?? (await capture.out.frames.changed());
		if (!fanout) throw new Error("no fanout");

		const before = fanout.subscribe(first).getReader();
		input.push(1_200_000);
		const read = (await before.read()).value as unknown as Frame;
		expect(read.timestamp).toBe(1_200_000);
		read.close();
		first.close();

		// A demand gap shorter than the clocks' offset.
		clock.set(1_050);
		const after = fanout.subscribe(second).getReader();
		const resumed = (await after.read()).value as unknown as Frame;
		expect(resumed.timestamp).toBe(1_250_000);
		resumed.close();
	} finally {
		second.close();
		capture.close();
		clock.close();
	}

	expect(open()).toEqual([]);
});

test("a reader attaching to a frame held late still starts at now", async () => {
	using input = frameSource();
	// The frame reached the fanout 200ms after its stamp, as on a busy main thread.
	const clock = manualAt(1_200);
	const capture = new Capture({ source: input.source });
	const first = new Effect();
	const second = new Effect();

	try {
		const fanout = capture.out.frames.peek() ?? (await capture.out.frames.changed());
		if (!fanout) throw new Error("no fanout");

		const before = fanout.subscribe(first).getReader();
		input.push(1_000_000);
		(await before.read()).value?.close();
		first.close();

		clock.set(1_250);
		const after = fanout.subscribe(second).getReader();
		const resumed = (await after.read()).value as unknown as Frame;
		expect(resumed.timestamp).toBe(1_250_000);
		resumed.close();
	} finally {
		second.close();
		capture.close();
		clock.close();
	}

	expect(open()).toEqual([]);
});

test("a replay never starts below an earlier replay", async () => {
	using input = frameSource();
	const clock = new Manual();
	const installed = clock.install();
	const advance = (ms: number) => clock.jump(Nano.fromMilli(Milli(ms)));
	const capture = new Capture({ source: input.source });
	const watch = new Effect();
	const first = new Effect();
	const second = new Effect();

	try {
		const fanout = capture.out.frames.peek() ?? (await capture.out.frames.changed());
		if (!fanout) throw new Error("no fanout");
		// Reads every live frame, so awaiting it proves the fanout held that frame.
		const live = fanout.subscribe(watch).getReader();

		// Capture stamps 200ms ahead of the clock.
		advance(1_000);
		input.push(1_200_000);
		(await live.read()).value?.close();

		advance(50);
		const before = fanout.subscribe(first).getReader();
		const replayed = (await before.read()).value as unknown as Frame;
		expect(replayed.timestamp).toBe(1_250_000);
		replayed.close();
		first.close();

		// The next frame reaches the fanout late, so it holds a stamp below that replay.
		advance(50);
		input.push(1_210_000);
		(await live.read()).value?.close();

		advance(1);
		const after = fanout.subscribe(second).getReader();
		const resumed = (await after.read()).value as unknown as Frame;
		expect(resumed.timestamp).toBe(1_250_000);
		resumed.close();
	} finally {
		watch.close();
		second.close();
		capture.close();
		installed.close();
	}

	expect(open()).toEqual([]);
});

test("the held frame is closed when replaced, when an unread copy is dropped, and on close", async () => {
	using input = frameSource();
	const capture = new Capture({ source: input.source });

	input.push(1_000);
	input.push(2_000);
	await flush();

	// Only the newest is held: the source frames reached no reader and the first hold was replaced.
	expect(open()).toHaveLength(1);

	// A reader that leaves without reading its copy releases it.
	const effect = new Effect();
	capture.out.frames.peek()?.subscribe(effect);
	expect(open()).toHaveLength(2);
	effect.close();
	expect(open()).toHaveLength(1);

	capture.close();
	expect(open()).toEqual([]);
});

test("closing the capture releases a copy its reader never read", async () => {
	using input = frameSource();
	const capture = new Capture({ source: input.source });
	const effect = new Effect();

	try {
		input.push(1_000);
		await flush();

		const reader = capture.out.frames.peek()?.subscribe(effect).getReader();
		if (!reader) throw new Error("no fanout");

		// The reader outlives the capture, like the fanout's own queued frames.
		capture.close();
		expect(open()).toEqual([]);
		expect((await reader.read()).done).toBe(true);
	} finally {
		effect.close();
	}
});

// https://github.com/moq-dev/moq/issues/4778
test("an encoder resuming on a still source encodes a keyframe of the current picture, stamped now", async () => {
	using input = frameSource();
	const clock = manualAt(1_000);

	const keys: number[] = [];
	class RecordingVideoEncoder {
		state: CodecState = "unconfigured";
		#output: VideoEncoderInit["output"];
		#codec?: string;

		constructor(init: VideoEncoderInit) {
			this.#output = init.output;
		}

		static async isConfigSupported(config: VideoEncoderConfig): Promise<{ supported: boolean }> {
			return { supported: config.codec.startsWith("avc1") };
		}

		configure(config: VideoEncoderConfig): void {
			this.state = "configured";
			this.#codec = config.codec;
		}

		encode(frame: VideoFrame, options?: VideoEncoderEncodeOptions): void {
			if (options?.keyFrame) keys.push(frame.timestamp);
		}

		// Only the probe flushes; report the configured codec, as Chrome does.
		async flush(): Promise<void> {
			const chunk = { type: "key", timestamp: 0, byteLength: 1, copyTo: () => {} };
			this.#output(chunk as never, { decoderConfig: { codec: this.#codec } } as never);
		}

		close(): void {
			this.state = "closed";
		}
	}
	const original = Object.getOwnPropertyDescriptor(globalThis, "VideoEncoder");
	Object.defineProperty(globalThis, "VideoEncoder", {
		configurable: true,
		value: RecordingVideoEncoder,
		writable: true,
	});

	const capture = new Capture({ source: input.source });
	const track = new Moq.Track.Producer("video").accept({ timescale: Moq.Time.Timescale.MILLI });
	// No subscriber yet, so the encoder idles while the source sends its only frame.
	const live = new Signal<Moq.Track.Producer | undefined>(undefined);
	const rendition = { config: new Signal(undefined), track: live, close: () => {} };
	const encoder = new Encoder("video", {
		enabled: true,
		broadcast: { video: () => rendition, order: new Order() } as never,
		capture,
	});

	try {
		input.push(1_000_000);
		// Wait for the probe to resolve a config from the captured dimensions.
		for (let i = 0; i < 50 && !encoder.out.resolved.peek(); i++) await flush();
		expect(encoder.out.resolved.peek()).toBeDefined();

		// Drop the probe's own encode.
		keys.length = 0;

		clock.set(91_000);
		live.set(track);
		await flush();

		expect(keys).toEqual([91_000_000]);
	} finally {
		encoder.close();
		capture.close();
		track.finish();
		track.close();
		clock.close();
		if (original) Object.defineProperty(globalThis, "VideoEncoder", original);
		else Reflect.deleteProperty(globalThis, "VideoEncoder");
	}
});

// Detaches a reader at every microtask offset after a push, since a demand gap or a hidden preview can
// land anywhere while a frame crosses the capture. A frame already read is the reader's to close, even
// when the teardown wins, so only the capture's held picture stays open between readers.
async function sweep(input: ReturnType<typeof frameSource>, attach: (attached: boolean) => void): Promise<void> {
	let timestamp = 2_000_000;
	for (let yields = 0; yields < 40; yields++) {
		attach(true);
		await flush();

		timestamp += 100_000;
		input.push(timestamp);
		for (let i = 0; i < yields; i++) await Promise.resolve();
		attach(false);
		await flush();

		expect({ yields, open: open().length }).toEqual({ yields, open: 1 });
	}
}

test("a preview hidden while a frame is in flight closes that frame", async () => {
	using input = frameSource();
	const capture = new Capture({ source: input.source });
	const enabled = new Signal(false);
	const preview = new Renderer({ frames: capture.out.frames, enabled });

	try {
		input.push(1_000_000);
		await flush();
		await sweep(input, (attached) => enabled.set(attached));
	} finally {
		preview.close();
		capture.close();
	}
	expect(open()).toEqual([]);
});

test("an encoder torn down while a frame is in flight closes that frame", async () => {
	using input = frameSource();

	class NullVideoEncoder {
		state: CodecState = "unconfigured";
		#output: VideoEncoderInit["output"];
		#codec?: string;

		constructor(init: VideoEncoderInit) {
			this.#output = init.output;
		}

		static async isConfigSupported(config: VideoEncoderConfig): Promise<{ supported: boolean }> {
			return { supported: config.codec.startsWith("avc1") };
		}

		configure(config: VideoEncoderConfig): void {
			this.state = "configured";
			this.#codec = config.codec;
		}

		encode(): void {}

		// Only the probe flushes; report the configured codec, as Chrome does.
		async flush(): Promise<void> {
			const chunk = { type: "key", timestamp: 0, byteLength: 1, copyTo: () => {} };
			this.#output(chunk as never, { decoderConfig: { codec: this.#codec } } as never);
		}

		close(): void {
			this.state = "closed";
		}
	}
	const original = Object.getOwnPropertyDescriptor(globalThis, "VideoEncoder");
	Object.defineProperty(globalThis, "VideoEncoder", { configurable: true, value: NullVideoEncoder, writable: true });

	const capture = new Capture({ source: input.source });
	const track = new Moq.Track.Producer("video").accept({ timescale: Moq.Time.Timescale.MILLI });
	const live = new Signal<Moq.Track.Producer | undefined>(undefined);
	const rendition = { config: new Signal(undefined), track: live, close: () => {} };
	const encoder = new Encoder("video", {
		enabled: true,
		broadcast: { video: () => rendition, order: new Order() } as never,
		capture,
	});

	try {
		input.push(1_000_000);
		for (let i = 0; i < 50 && !encoder.out.resolved.peek(); i++) await flush();
		expect(encoder.out.resolved.peek()).toBeDefined();

		await sweep(input, (attached) => live.set(attached ? track : undefined));
	} finally {
		encoder.close();
		capture.close();
		track.finish();
		track.close();
		if (original) Object.defineProperty(globalThis, "VideoEncoder", original);
		else Reflect.deleteProperty(globalThis, "VideoEncoder");
	}
	expect(open()).toEqual([]);
});

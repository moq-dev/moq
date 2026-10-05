import { expect, mock, spyOn, test } from "bun:test";
import * as Moq from "@moq/net";
import { Effect, Signal } from "@moq/signals";
import { Baseline } from "../jitter";
import type { StreamTrack } from "./types";

mock.module("./capture-worker.ts?worklet", () => ({ default: async () => "blob:fake-worker" }));
const { Capture } = await import("./capture");
const { Encoder } = await import("./encoder");

class Frame {
	// Every frame built, so a test can assert none is left open.
	static all: Frame[] = [];

	codedWidth = 1920;
	codedHeight = 1080;
	timestamp: number;
	closed = false;

	constructor(source?: Frame, init?: { timestamp: number }) {
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

// Let the capture pump drain everything pushed so far.
const flush = () => new Promise((resolve) => setTimeout(resolve, 10));

const open = () => Frame.all.filter((frame) => !frame.closed);

test("a reader attaching to a still source starts with the current picture, stamped now", async () => {
	using input = frameSource();
	const clock = spyOn(performance, "now").mockReturnValue(1_000);
	const capture = new Capture({ source: input.source });
	const effect = new Effect();

	try {
		// The only frame the source sends, with nobody reading yet.
		input.push(1_000_000);
		await flush();

		// Attach 90s later.
		clock.mockReturnValue(91_000);
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
		clock.mockRestore();
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

// https://github.com/moq-dev/moq/issues/4778
test("an encoder resuming on a still source encodes a keyframe of the current picture, stamped now", async () => {
	using input = frameSource();
	const clock = spyOn(performance, "now").mockReturnValue(1_000);

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
	const track = new Moq.Track.Producer("video").accept();
	// No subscriber yet, so the encoder idles while the source sends its only frame.
	const live = new Signal<Moq.Track.Producer | undefined>(undefined);
	const rendition = { config: new Signal(undefined), track: live, close: () => {} };
	const encoder = new Encoder("video", {
		enabled: true,
		broadcast: { video: () => rendition, baseline: new Baseline() } as never,
		capture,
	});

	try {
		input.push(1_000_000);
		// Wait for the probe to resolve a config from the captured dimensions.
		for (let i = 0; i < 50 && !encoder.out.resolved.peek(); i++) await flush();
		expect(encoder.out.resolved.peek()).toBeDefined();

		// Drop the probe's own encode.
		keys.length = 0;

		clock.mockReturnValue(91_000);
		live.set(track);
		await flush();

		expect(keys).toEqual([91_000_000]);
	} finally {
		encoder.close();
		capture.close();
		track.close();
		clock.mockRestore();
		if (original) Object.defineProperty(globalThis, "VideoEncoder", original);
		else Reflect.deleteProperty(globalThis, "VideoEncoder");
	}
});

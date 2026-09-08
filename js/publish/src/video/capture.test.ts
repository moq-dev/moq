import { expect, mock, test } from "bun:test";
import type { StreamTrack } from "./types";

mock.module("./capture-worker.ts?worker&inline", () => ({ default: class {} }));
const { Capture } = await import("./capture");

class Frame {
	codedWidth = 1920;
	codedHeight = 1080;
	timestamp: number;
	closed = false;

	constructor(source?: Frame, init?: { timestamp: number }) {
		this.timestamp = init?.timestamp ?? source?.timestamp ?? 0;
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

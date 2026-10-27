import { afterEach, beforeEach, expect, test } from "bun:test";
import { Signal } from "@moq/signals";
import { Fanout } from "./fanout";
import { Transcode } from "./preview";

const encoders: Encoder[] = [];
const decoders: Decoder[] = [];
class Encoder {
	state: CodecState = "unconfigured";
	configs: VideoEncoderConfig[] = [];
	readonly callbacks: VideoEncoderInit;
	constructor(callbacks: VideoEncoderInit) {
		this.callbacks = callbacks;
		encoders.push(this);
	}
	configure(config: VideoEncoderConfig) {
		this.configs.push({ ...config });
		this.state = "configured";
	}
	fail = false;
	encoded = 0;
	encode() {
		if (this.fail) throw new DOMException("bad frame", "DataError");
		this.encoded++;
	}
	close() {
		this.state = "closed";
	}
}
class Decoder {
	state: CodecState = "unconfigured";
	readonly callbacks: VideoDecoderInit;
	constructor(callbacks: VideoDecoderInit) {
		this.callbacks = callbacks;
		decoders.push(this);
	}
	configure() {
		this.state = "configured";
	}
	fail = false;
	decode() {
		if (this.fail) throw new DOMException("bad chunk", "DataError");
	}
	close() {
		this.state = "closed";
	}
}
const originalEncoder = Object.getOwnPropertyDescriptor(globalThis, "VideoEncoder");
const originalDecoder = Object.getOwnPropertyDescriptor(globalThis, "VideoDecoder");
beforeEach(() => {
	encoders.length = 0;
	decoders.length = 0;
	Object.defineProperty(globalThis, "VideoEncoder", { configurable: true, value: Encoder });
	Object.defineProperty(globalThis, "VideoDecoder", { configurable: true, value: Decoder });
});
afterEach(() => {
	if (originalEncoder) Object.defineProperty(globalThis, "VideoEncoder", originalEncoder);
	else Reflect.deleteProperty(globalThis, "VideoEncoder");
	if (originalDecoder) Object.defineProperty(globalThis, "VideoDecoder", originalDecoder);
	else Reflect.deleteProperty(globalThis, "VideoDecoder");
});
const base: VideoEncoderConfig = { codec: "vp8", width: 640, height: 360, bitrate: 1000000 };
const settle = async () => {
	for (let i = 0; i < 5; i++) await new Promise<void>((r) => queueMicrotask(r));
};

test("bitrate reconfiguration keeps the codecs and last decoded frame", async () => {
	const config = new Signal<VideoEncoderConfig | undefined>(base);
	const transcode = new Transcode({ config });
	try {
		await settle();
		const encoder = encoders[0],
			decoder = decoders[0];
		let closed = 0;
		const frame = { close: () => closed++ } as unknown as VideoFrame;
		decoder.callbacks.output(frame);
		config.set({ ...base, bitrate: 2000000 });
		await settle();
		expect(encoders).toHaveLength(1);
		expect(decoders).toHaveLength(1);
		expect(encoder.configs.at(-1)?.bitrate).toBe(2000000);
		expect(transcode.out.frame.peek()).toBe(frame);
		expect(closed).toBe(0);
	} finally {
		transcode.close();
	}
});

test.each(["encode", "decode"] as const)(
	"a preview %s error recovers on the next resolved config change",
	async (kind) => {
		const config = new Signal<VideoEncoderConfig | undefined>(base);
		const transcode = new Transcode({ config });
		try {
			await settle();
			if (kind === "encode") encoders[0].callbacks.error(new DOMException("encoder lost", "OperationError"));
			else decoders[0].callbacks.error(new DOMException("decoder lost", "OperationError"));
			await settle();
			expect(encoders[0].state).toBe("closed");
			expect(decoders[0].state).toBe("closed");
			expect(encoders).toHaveLength(1); // No retry without new input.
			config.set({ ...base, bitrate: 2000000 });
			await settle();
			expect(encoders).toHaveLength(2);
			expect(decoders).toHaveLength(2);
			expect(encoders[1].state).toBe("configured");
		} finally {
			transcode.close();
		}
	},
);

test("a size change replaces the codec scope and discards late output from it", async () => {
	const config = new Signal<VideoEncoderConfig | undefined>(base);
	const transcode = new Transcode({ config });
	try {
		await settle();
		const previous = decoders[0];
		let closed = 0;
		previous.callbacks.output({ close: () => closed++ } as unknown as VideoFrame);
		config.set({ ...base, width: 1280, height: 720 });
		await settle();
		expect(encoders).toHaveLength(2);
		expect(decoders).toHaveLength(2);
		expect(encoders[0].state).toBe("closed");
		expect(previous.state).toBe("closed");
		expect(closed).toBe(1);
		expect(transcode.out.frame.peek()).toBeUndefined();
		previous.callbacks.output({ close: () => closed++ } as unknown as VideoFrame);
		expect(closed).toBe(2);
		expect(transcode.out.frame.peek()).toBeUndefined();
	} finally {
		transcode.close();
	}
});

test("a synchronous encode failure waits for new config and resumes the frame reader", async () => {
	let push!: (frame: VideoFrame) => void;
	const source = new Fanout(
		new ReadableStream<VideoFrame>({
			start: (controller) => {
				push = (frame) => controller.enqueue(frame);
			},
		}),
		{ release: (frame) => frame.close() },
	);
	const config = new Signal<VideoEncoderConfig | undefined>(base);
	const transcode = new Transcode({ source, config });
	const frame = (timestamp: number) => ({ timestamp, close() {} }) as unknown as VideoFrame;
	try {
		await settle();
		encoders[0].fail = true;
		push(frame(0));
		await settle();
		expect(encoders[0].state).toBe("closed");
		push(frame(10000));
		await settle();
		expect(encoders).toHaveLength(1);
		config.set({ ...base, bitrate: 2000000 });
		await settle();
		push(frame(20000));
		await settle();
		expect(encoders).toHaveLength(2);
		expect(encoders[1].encoded).toBe(1);
	} finally {
		transcode.close();
		source.close();
	}
});

test("a synchronous decode failure closes its codec scope and rearms on config", async () => {
	const config = new Signal<VideoEncoderConfig | undefined>(base);
	const transcode = new Transcode({ config });
	try {
		await settle();
		decoders[0].fail = true;
		expect(() => encoders[0].callbacks.output({} as EncodedVideoChunk)).not.toThrow();
		expect(encoders[0].state).toBe("closed");
		config.set({ ...base, bitrate: 2000000 });
		await settle();
		expect(decoders).toHaveLength(2);
		expect(decoders[1].state).toBe("configured");
	} finally {
		transcode.close();
	}
});

/** Real WebCodecs preview across config changes and controlled codec failures. @module */
import { Preview, Signals } from "@moq/publish";
import { Fanout } from "../../../../../js/publish/src/fanout";

const NativeEncoder = VideoEncoder,
	NativeDecoder = VideoDecoder;
const encoders: VideoEncoder[] = [],
	decoders: VideoDecoder[] = [];
const encoderCallbacks: VideoEncoderInit[] = [],
	decoderCallbacks: VideoDecoderInit[] = [];
globalThis.VideoEncoder = class extends NativeEncoder {
	constructor(init: VideoEncoderInit) {
		super(init);
		encoders.push(this);
		encoderCallbacks.push(init);
	}
};
globalThis.VideoDecoder = class extends NativeDecoder {
	constructor(init: VideoDecoderInit) {
		super(init);
		decoders.push(this);
		decoderCallbacks.push(init);
	}
};
const canvas = document.createElement("canvas");
canvas.width = 64;
canvas.height = 64;
const context = canvas.getContext("2d");
if (!context) throw new Error("missing canvas context");
context.fillStyle = "red";
context.fillRect(0, 0, canvas.width, canvas.height);
let push!: (frame: VideoFrame) => void;
const source = new Fanout(
	new ReadableStream<VideoFrame>({
		start: (c) => {
			push = (f) => c.enqueue(f);
		},
	}),
	{ release: (f) => f.close() },
);
const config = new Signals.Signal<VideoEncoderConfig>({
	codec: "vp8",
	width: 64,
	height: 64,
	bitrate: 1000000,
	framerate: 30,
	latencyMode: "realtime",
	hardwareAcceleration: "prefer-software",
});
const transcode = new Preview.Transcode({ source, config });
let timestamp = 0;
const settle = async () => {
	for (let i = 0; i < 5; i++) await new Promise<void>((r) => queueMicrotask(r));
};
/** Snapshot of codec ownership and the latest decoded frame. */
export function state() {
	const frame = transcode.out.frame.peek();
	return {
		encoders: encoders.length,
		decoders: decoders.length,
		frame: !!frame,
		timestamp: frame?.timestamp,
		width: frame?.displayWidth,
	};
}
/** Feed a frame and await its actual encode/decode completion. */
export async function emit() {
	await settle();
	const next = transcode.out.frame.changed();
	timestamp += 33333;
	push(new VideoFrame(canvas, { timestamp }));
	await next;
	return state();
}
/** Change bitrate on the live codec pair and report whether its output was retained. */
export async function bitrate(value: number) {
	const previous = transcode.out.frame.peek();
	config.update((c) => ({ ...c, bitrate: value }));
	await settle();
	return { ...state(), retained: transcode.out.frame.peek() === previous };
}
/** Deliver the same callback a native codec failure invokes. */
export async function fail(kind: "encode" | "decode") {
	const error = new DOMException("controlled codec failure", "OperationError");
	if (kind === "encode") encoderCallbacks.at(-1)?.error(error);
	else decoderCallbacks.at(-1)?.error(error);
	await settle();
	// Ordinary captured frames must not restart a failed codec scope.
	timestamp += 33333;
	push(new VideoFrame(canvas, { timestamp }));
	await settle();
	return state();
}
/** Resize capture and config together to replace the codec pair. */
export async function resize() {
	canvas.width = 96;
	context.fillStyle = "blue";
	context.fillRect(0, 0, 96, 64);
	config.update((c) => ({ ...c, width: 96 }));
	await settle();
	return state();
}
/** Release every owned frame and codec. */
export function close() {
	transcode.close();
	source.close();
	globalThis.VideoEncoder = NativeEncoder;
	globalThis.VideoDecoder = NativeDecoder;
	return {
		state: state(),
		closed: encoders.every((e) => e.state === "closed") && decoders.every((d) => d.state === "closed"),
	};
}

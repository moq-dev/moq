import { expect, test } from "bun:test";
import { Manual, Milli, Nano } from "@moq/clock";
import { u53 } from "@moq/hang/catalog";
import * as Moq from "@moq/net";
import { Signal } from "@moq/signals";
import { Fanout } from "../fanout";
import { Baseline } from "../jitter";
import { Encoder } from "./encoder";

// Model an encoder that cannot accept more work until the test completes its pending input.
// The codec probe completes immediately, independently of the live encoder's service rate.
class SlowEncoder {
	static instances: SlowEncoder[] = [];
	state: CodecState = "unconfigured";
	pending: { timestamp: number; key: boolean }[] = [];
	output: VideoEncoderInit["output"];
	codec?: string;

	constructor(init: VideoEncoderInit) {
		this.output = init.output;
		SlowEncoder.instances.push(this);
	}

	get encodeQueueSize(): number {
		return this.pending.length;
	}

	static async isConfigSupported(config: VideoEncoderConfig) {
		return { supported: config.codec.startsWith("avc1") };
	}

	configure(config: VideoEncoderConfig) {
		this.state = "configured";
		this.codec = config.codec;
	}

	encode(frame: VideoFrame, options?: VideoEncoderEncodeOptions) {
		if (frame instanceof Input) this.pending.push({ timestamp: frame.timestamp, key: !!options?.keyFrame });
	}

	finish() {
		const frame = this.pending.shift();
		if (frame) {
			this.output(
				{
					type: frame.key ? "key" : "delta",
					timestamp: frame.timestamp,
					byteLength: 1,
					copyTo: () => {},
				} as never,
				{},
			);
		}
	}

	async flush() {
		this.output(
			{ type: "key", timestamp: 0, byteLength: 1, copyTo: () => {} } as never,
			{ decoderConfig: { codec: this.codec } } as never,
		);
	}

	close() {
		this.state = "closed";
		this.pending = [];
	}
}

class Input {
	static all: Input[] = [];
	closed = false;
	readonly timestamp: number;
	constructor(timestamp: number) {
		this.timestamp = timestamp;
		Input.all.push(this);
	}
	clone() {
		return new Input(this.timestamp);
	}
	close() {
		this.closed = true;
	}
}

// Probe frames have no capture timestamp and never enter the live work queue.
class ProbeFrame {
	timestamp = 0;
	close() {}
}

test("a slower-than-realtime encoder drops input instead of ratcheting catalog latency", async () => {
	const originals = ["VideoEncoder", "VideoFrame"].map((name) => Object.getOwnPropertyDescriptor(globalThis, name));
	Object.defineProperty(globalThis, "VideoEncoder", { configurable: true, value: SlowEncoder });
	Object.defineProperty(globalThis, "VideoFrame", { configurable: true, value: ProbeFrame });
	SlowEncoder.instances = [];
	Input.all = [];
	const clock = new Manual();
	const installed = clock.install();
	let controller!: ReadableStreamDefaultController<VideoFrame>;
	const fanout = new Fanout(
		new ReadableStream<VideoFrame>({
			start: (stream) => {
				controller = stream;
			},
		}),
		{ clone: (frame) => frame.clone(), release: (frame) => frame.close() },
	);
	const track = new Moq.Track.Producer("video").accept({ timescale: Moq.Time.Timescale.MICRO });
	const baseline = new Baseline();
	const rendition = { config: new Signal(undefined), track: new Signal(track), close: () => track.close() };
	const encoder = new Encoder("video", {
		broadcast: { video: () => rendition, baseline } as never,
		capture: {
			in: { source: new Signal({ getSettings: () => ({ frameRate: 25 }), getConstraints: () => ({}) }) },
			out: { display: new Signal({ width: 640, height: 480 }), frames: new Signal(fanout) },
		} as never,
		config: { frameRate: 25 },
	});
	const send = async (millis = 40) => {
		await clock.advance(Nano.fromMilli(Milli(millis)));
		const timestamp = Nano.toMicro(Nano(clock.now()));
		baseline.observe(0, timestamp); // An on-time audio rendition shares the broadcast.
		controller.enqueue(new Input(timestamp) as never);
		await clock.advance(Nano.zero);
	};
	try {
		await clock.advance(Nano.zero);
		expect(encoder.out.resolved.peek()).toBeDefined();
		const live = SlowEncoder.instances.find((instance) => instance.state === "configured");
		if (!live) throw new Error("live encoder was not configured");
		let largestQueue = 0;
		// 25 captured fps, but only 8 1/3 encoded fps, for 24 seconds.
		for (let frame = 0; frame < 600; frame++) {
			await send();
			largestQueue = Math.max(largestQueue, live.encodeQueueSize);
			if (frame % 3 === 2) {
				live.finish();
				await clock.advance(Nano.zero);
			}
		}
		// A kept frame waits exactly two capture intervals, independent of stream duration.
		const catalog = encoder.out.catalog.peek();
		expect({ largestQueue, jitter: catalog?.jitter, delay: catalog?.delay }).toEqual({
			largestQueue: 1,
			jitter: undefined,
			delay: u53(80),
		});
		expect(encoder.out.stats.peek().frames).toBe(200);
		expect(Input.all.every((frame) => frame.closed)).toBe(true);

		// A keyframe requested while busy belongs to the next accepted frame.
		await send();
		encoder.cut();
		await send();
		expect(live.pending).toHaveLength(1);
		live.finish();
		// A dropped input must not move the pacing cursor either: this is only 10ms
		// after that input, but 50ms after the last accepted one.
		await send(10);
		expect(live.pending).toHaveLength(1);
		expect(live.pending[0].key).toBe(true);
	} finally {
		encoder.close();
		fanout.close();
		track.close();
		await clock.advance(Nano.zero);
		installed.close();
		for (const [index, name] of ["VideoEncoder", "VideoFrame"].entries()) {
			const original = originals[index];
			if (original) Object.defineProperty(globalThis, name, original);
			else Reflect.deleteProperty(globalThis, name);
		}
	}
});

import { Manual, Micro, Milli, Nano, yieldNow } from "@moq/clock";
import * as Moq from "@moq/net";
import { Signal } from "@moq/signals";
import type { AudioFrame, Capture, Format } from "../src/audio/capture";
import { Encoder } from "../src/audio/encoder";
import { Broadcast } from "../src/broadcast";

// Observe the native queue's completion without adding a flush API to the publisher.
const Native = AudioEncoder;
const codecs = new Set<AudioEncoder>();
globalThis.AudioEncoder = class extends Native {
	constructor(init: AudioEncoderInit) {
		super(init);
		codecs.add(this);
	}
};
const flush = () =>
	Promise.all([...codecs].filter((codec) => codec.state === "configured").map((codec) => codec.flush()));

class Feed {
	#deliver?: (frame: AudioFrame) => void;
	#requested!: () => void;
	#request = this.#next();
	readonly stream = new ReadableStream<AudioFrame>(
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
	#next(): Promise<void> {
		return new Promise((resolve) => {
			this.#requested = resolve;
		});
	}
	async push(frame: AudioFrame): Promise<void> {
		await this.#request;
		this.#request = this.#next();
		this.#deliver?.(frame);
		await this.#request;
	}
}

async function measure(tracks: number, seconds: number, onDemand: boolean) {
	const clock = new Manual();
	const installed = clock.install();
	const broadcast = new Broadcast({
		origin: new Moq.Origin.Producer(),
		name: Moq.Path.from("audio.hang"),
		maxAge: Milli(seconds * 1000),
		onDemand,
	});
	const feeds = Array.from({ length: tracks }, () => new Feed());
	const encoders = feeds.map((feed, index) => {
		const capture = {
			in: { source: new Signal(undefined) },
			blocked: new Signal(false),
			out: {
				root: new Signal(undefined),
				format: new Signal<Format>({ sampleRate: 48_000, channelCount: 2 }),
				frames: new Signal({ subscribe: () => feed.stream }),
			},
		} as unknown as Capture;
		return new Encoder(`audio/${index}`, { broadcast, capture });
	});
	const subscribers: Moq.Track.Subscriber[] = [];
	try {
		for (const encoder of encoders) while (!encoder.settled.peek()) await encoder.settled.changed();
		await yieldNow();
		for (const encoder of encoders) if (encoder.out.error.peek()) throw encoder.out.error.peek();
		const channels = Array.from({ length: 2 }, () =>
			Float32Array.from({ length: 960 }, (_, index) => Math.sin(index * 0.1) * 0.1),
		);
		const frames = seconds * 100;
		const start = performance.now();
		for (let index = 0; index < frames; index++) {
			for (const feed of feeds) await feed.push({ timestamp: Micro(index * 20_000), channels });
			await clock.advance(Nano.fromMilli(Milli(20)));
		}
		await flush();
		const idle = performance.now() - start;
		for (const encoder of encoders) {
			if (encoder.out.stats.peek().frames !== (onDemand ? 0 : frames)) throw new Error("idle encode count");
		}
		const joining = performance.now();
		const net = broadcast.net.peek();
		if (!net) throw new Error("missing broadcast");
		for (const encoder of encoders)
			subscribers.push(net.track(encoder.name).subscribe({ maxDelay: Milli(seconds * 1000) }));
		await yieldNow();
		await flush();
		const catchup = performance.now() - joining;
		for (const subscriber of subscribers) {
			const reader = subscriber.ordered();
			const group = await reader.nextGroup();
			const frame = await group?.readFrame();
			if (!frame) throw new Error("missing retained audio");
			const timestamp = Moq.Varint.decode(frame.payload)[0];
			if (Math.abs(timestamp - seconds * 1_000_000) > 20_000)
				throw new Error(`retained audio starts at ${timestamp}`);
			group?.close();
		}
		return { tracks, seconds, onDemand, idle, catchup };
	} finally {
		for (const subscriber of subscribers) subscriber.close();
		for (const encoder of encoders) encoder.close();
		broadcast.close();
		codecs.clear();
		installed.close();
	}
}

Object.assign(globalThis, { measure });

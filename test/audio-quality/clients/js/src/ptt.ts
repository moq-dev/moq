/** Measure the shipped push-to-talk path on one monotonic page clock. @module */
import "@moq/publish/element";
import "@moq/watch/element";
import { Milli } from "@moq/clock";
import * as Container from "@moq/hang/container";
import { Track } from "@moq/net";
import type MoqPublish from "@moq/publish/element";
import { Derived } from "@moq/signals";
import type MoqWatch from "@moq/watch/element";
import { Capture } from "./capture";
import type { Arrival } from "./schema";

/** One timestamped encoder input or output, on the page and media clocks. */
type Frame = { at: number; timestamp: number; bytes?: number; duration?: number | null };

const records = {
	submitted: [] as Frame[],
	encoded: [] as Frame[],
	sent: [] as Frame[],
	decoding: [] as Frame[],
	decoded: [] as Frame[],
	subscriptions: [] as { at: number; name: string }[],
	audible: [] as { at: number; rendered: number; received: number }[],
	// The newest captured chunk delivered before a press, mapping the press onto the media clock.
	presses: [] as { at: number; on: boolean; capture?: { at: number; end: number } }[],
	captures: [] as { at: number; audio: boolean }[],
	encoders: [] as number[],
	errors: [] as string[],
};

// Observe WebCodecs without changing its input, output, or scheduling. A probe or a rebuilt codec
// constructs a new encoder; the live one flushes at each release, so a flush alone is no probe.
const NativeEncoder = globalThis.AudioEncoder;
globalThis.AudioEncoder = class extends NativeEncoder {
	constructor(init: AudioEncoderInit) {
		records.encoders.push(performance.now());
		super({
			...init,
			output: (chunk, metadata) => {
				records.encoded.push({ at: performance.now(), timestamp: chunk.timestamp, bytes: chunk.byteLength });
				init.output(chunk, metadata);
			},
		});
	}
	override encode(data: AudioData): void {
		records.submitted.push({ at: performance.now(), timestamp: data.timestamp, duration: data.duration });
		super.encode(data);
	}
};
const NativeDecoder = globalThis.AudioDecoder;
globalThis.AudioDecoder = class extends NativeDecoder {
	constructor(init: AudioDecoderInit) {
		super({
			...init,
			output: (data) => {
				records.decoded.push({ at: performance.now(), timestamp: data.timestamp, duration: data.duration });
				init.output(data);
			},
		});
	}
	override decode(chunk: EncodedAudioChunk): void {
		records.decoding.push({ at: performance.now(), timestamp: chunk.timestamp });
		super.decode(chunk);
	}
};
// Note each captured chunk as the publisher's graph delivers it, without consuming it.
let captured: { at: number; end: number } | undefined;
const NativeNode = globalThis.AudioWorkletNode;
globalThis.AudioWorkletNode = class extends NativeNode {
	constructor(context: BaseAudioContext, name: string, options?: AudioWorkletNodeOptions) {
		super(context, name, options);
		if (name !== "capture") return;
		this.port.addEventListener(
			"message",
			({ data }: MessageEvent<{ timestamp: number; channels: Float32Array[] }>) => {
				const frames = data.channels[0]?.length ?? 0;
				captured = { at: performance.now(), end: data.timestamp + (frames / context.sampleRate) * 1_000_000 };
			},
		);
	}
};
const encode = Container.Legacy.Producer.prototype.encode;
Container.Legacy.Producer.prototype.encode = function (frame, timestamp, keyframe) {
	const result = encode.call(this, frame, timestamp, keyframe);
	records.sent.push({ at: performance.now(), timestamp, bytes: frame.byteLength });
	return result;
};
const subscribe = Track.Consumer.prototype.subscribe;
Track.Consumer.prototype.subscribe = function (options) {
	records.subscriptions.push({ at: performance.now(), name: this.name });
	return subscribe.call(this, options);
};
const getUserMedia = navigator.mediaDevices.getUserMedia.bind(navigator.mediaDevices);
navigator.mediaDevices.getUserMedia = (constraints) => {
	records.captures.push({ at: performance.now(), audio: !!constraints?.audio });
	return getUserMedia(constraints);
};

const params = new URLSearchParams(location.search);
const required = (name: string) => {
	const value = params.get(name);
	if (!value) throw new Error(`missing ?${name}`);
	return value;
};
const publish = document.createElement("moq-publish") as MoqPublish;
publish.url = required("relay");
publish.name = "ptt.hang";
publish.invisible = true;
publish.audio.codec.set({ mime: "opus", frameDuration: Milli(60), bitrate: 128_000 });
publish.source = "camera";
document.body.append(publish);

const watch = document.createElement("moq-watch") as MoqWatch;
// Connections pool by URL, and a pooled origin resolves a same-page publication locally. A
// listener-only query, which the relay ignores, gives the watcher its own session, so the media
// crosses the relay.
const listener = new URL(required("relay"));
listener.searchParams.set("role", "listener");
watch.url = listener.toString();
watch.name = publish.name;
watch.setAttribute("delay", "300ms");
watch.setAttribute("visible", "always");
watch.volume = 1;
watch.muted = false;
watch.paused = false;
document.body.append(watch);

const audio = watch.player.audio;
const capture = new Capture({
	broadcast: audio.source.in.broadcast,
	track: audio.source.out.track,
	config: audio.source.out.config,
	delay: watch.player.sync.in.delay,
	// Decoder subscriptions include codec warmup beyond Sync's budget. The observer must
	// request the same age so its own subscription cannot discard an otherwise visible frame.
	maxDelay: new Derived([watch.player.sync.out.maxDelay, audio.source.out.config], (max, config) =>
		Milli(max + (config?.warmup ?? 0)),
	),
});
const arrivals: Arrival[] = [];
let attached: AudioNode | undefined;
let tapped: AudioNode | undefined;
async function attach() {
	const root = audio.out.root.peek();
	if (!root || root === attached) return;
	attached = root;
	const context = root.context as AudioContext;
	// A constant microphone tone makes a nonzero output quantum the utterance's audible onset.
	// Map its render timestamp through getOutputTimestamp, rather than timing the port message.
	const source = `class Tap extends AudioWorkletProcessor {
		loud = false;
		process(inputs) {
			const loud = !!inputs[0]?.[0]?.some(s => Math.abs(s) > 0.001);
			if (loud && !this.loud) this.port.postMessage(currentTime);
			this.loud = loud;
			return true;
		}
	}
	registerProcessor('ptt-tap', Tap);`;
	const url = URL.createObjectURL(new Blob([source], { type: "text/javascript" }));
	try {
		await context.audioWorklet.addModule(url);
	} catch (error) {
		if (audio.out.root.peek() !== root) return;
		throw error;
	} finally {
		URL.revokeObjectURL(url);
	}
	if (audio.out.root.peek() !== root) return;
	const tap = new AudioWorkletNode(context, "ptt-tap");
	const mute = new GainNode(context, { gain: 0 });
	root.connect(tap);
	tap.connect(mute);
	mute.connect(context.destination);
	tap.port.onmessage = ({ data }: MessageEvent<number>) => {
		const stamp = context.getOutputTimestamp();
		if (stamp.performanceTime === undefined || stamp.contextTime === undefined) {
			records.errors.push("missing output clock");
			return;
		}
		// Map the onset's render time through the render clock read at this instant, so a late port
		// message moves neither the render onset nor its projected output.
		const received = performance.now();
		records.audible.push({
			at: stamp.performanceTime + (data - stamp.contextTime) * 1000,
			rendered: received - (context.currentTime - data) * 1000,
			received,
		});
	};
	tapped = root;
}
audio.out.root.subscribe(() => void attach().catch((error) => records.errors.push(String(error))));

/** The finite measurement surface driven by ptt.ts. */
export const measurement = {
	press(on: boolean) {
		const at = performance.now();
		records.presses.push(on && captured ? { at, on, capture: { ...captured } } : { at, on });
		publish.muted = !on;
	},
	state() {
		return {
			publisher: publish.connection.transport.peek() as string | undefined,
			viewer: watch.connection.transport.peek() as string | undefined,
			// Distinct origins: the watcher can only reach the publication through the relay.
			separate: watch.connection.origin.peek() !== publish.connection.origin.peek(),
			config: publish.audio.out.catalog.peek(),
			selected: audio.source.out.track.peek(),
			tapped: tapped !== undefined && tapped === audio.out.root.peek(),
			active: publish.audio.out.active.peek(),
			context: audio.out.context.peek()?.state,
			stalled: audio.out.stalled.peek(),
			timestamp: audio.out.timestamp.peek(),
			delay: watch.player.sync.out.delay.peek(),
			error: String(publish.audio.out.error.peek() ?? audio.out.error.peek() ?? ""),
		};
	},
	reset() {
		for (const list of Object.values(records)) list.length = 0;
		capture.drain();
		arrivals.length = 0;
	},
	result() {
		arrivals.push(...capture.drain());
		return { ...records, arrivals, captureError: capture.error() };
	},
};

declare global {
	var ptt: typeof measurement;
}
globalThis.ptt = measurement;

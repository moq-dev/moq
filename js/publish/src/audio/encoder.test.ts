import { expect, mock, spyOn, test } from "bun:test";
import type * as Catalog from "@moq/hang/catalog";
import { Signal } from "@moq/signals";

// The encoder pulls the capture processor in as a `?worklet` blob URL, which the bun test loader can't
// resolve. Stub it so the module imports; the value is only ever passed to our fake addModule.
mock.module("./capture-worklet.ts?worklet", () => ({ default: "blob:fake-capture" }));

const { Encoder } = await import("./encoder.ts");
type Codec = import("./encoder.ts").Codec;

const flush = () => new Promise<void>((resolve) => queueMicrotask(resolve));
async function settle(times = 5): Promise<void> {
	for (let i = 0; i < times; i++) await flush();
}

// Models the WebAudio surface `#runSource` touches. The key detail is `AudioContext.close()`: on
// Firefox and Safari it does NOT synchronously flip `.state` to "closed" (it stays "suspended"), which
// is exactly the browser behavior the old `context.state === "closed"` guard failed to account for.
function installFakeWebAudio() {
	// Never resolves during the test, so the spawned worklet load stays pending until teardown.
	const addModule = () => new Promise<void>(() => {});
	let audioWorkletNodes = 0;
	const requestedRates: (number | undefined)[] = [];

	class FakeAudioContext {
		state: AudioContextState = "suspended";
		audioWorklet = { addModule };
		constructor(options?: AudioContextOptions) {
			requestedRates.push(options?.sampleRate);
		}
		close(): Promise<void> {
			// Firefox/Safari behavior: stays "suspended", never "closed".
			return Promise.resolve();
		}
	}

	class FakeMediaStream {}

	class FakeGraphNode {
		channelCount = 2;
		connect(): void {}
		disconnect(): void {}
	}

	class FakeAudioWorkletNode {
		constructor(_context: unknown, _name: string) {
			audioWorkletNodes++;
			// The real constructor throws when the module registration was abandoned mid-load.
			throw new DOMException("Unknown AudioWorklet name 'capture'", "InvalidStateError");
		}
	}

	const globals: Record<string, unknown> = {
		AudioContext: FakeAudioContext,
		MediaStream: FakeMediaStream,
		MediaStreamAudioSourceNode: FakeGraphNode,
		GainNode: FakeGraphNode,
		AudioWorkletNode: FakeAudioWorkletNode,
	};

	const originals = new Map<string, PropertyDescriptor | undefined>();
	for (const [name, value] of Object.entries(globals)) {
		originals.set(name, Object.getOwnPropertyDescriptor(globalThis, name));
		Object.defineProperty(globalThis, name, { configurable: true, writable: true, value });
	}

	return {
		get audioWorkletNodes() {
			return audioWorkletNodes;
		},
		get requestedRates() {
			return requestedRates;
		},
		[Symbol.dispose]() {
			for (const [name, original] of originals) {
				if (original) Object.defineProperty(globalThis, name, original);
				else Reflect.deleteProperty(globalThis, name);
			}
		},
	};
}

function installEncodingHarness(description: Uint8Array) {
	let worklet: FakeAudioWorkletNode | undefined;
	let audioEncoders = 0;

	class FakePort extends EventTarget {
		start(): void {}

		emit(data: unknown): void {
			this.dispatchEvent(new MessageEvent("message", { data }));
		}
	}

	class FakeAudioContext {
		readonly sampleRate: number;
		state: AudioContextState = "running";
		currentTime = 0;
		audioWorklet = { addModule: () => Promise.resolve() };

		constructor(options?: AudioContextOptions) {
			this.sampleRate = options?.sampleRate ?? 48_000;
		}

		close(): Promise<void> {
			this.state = "closed";
			return Promise.resolve();
		}
	}

	class FakeMediaStream {}

	class FakeGraphNode {
		channelCount = 2;
		connect(): void {}
		disconnect(): void {}
	}

	class FakeGainNode extends FakeGraphNode {
		readonly context: FakeAudioContext;
		gain = {
			cancelScheduledValues: () => {},
			exponentialRampToValueAtTime: () => {},
			setValueAtTime: () => {},
		};

		constructor(context: FakeAudioContext) {
			super();
			this.context = context;
		}
	}

	class FakeAudioWorkletNode extends FakeGraphNode {
		readonly port = new FakePort();
		readonly context: FakeAudioContext;

		constructor(context: FakeAudioContext) {
			super();
			this.context = context;
			worklet = this;
		}
	}

	class FakeAudioData {
		readonly timestamp: number;

		constructor(init: AudioDataInit) {
			this.timestamp = init.timestamp;
		}

		close(): void {}
	}

	class FakeAudioEncoder {
		readonly #output: EncodedAudioChunkOutputCallback;

		constructor(init: AudioEncoderInit) {
			audioEncoders++;
			this.#output = init.output;
		}

		configure(_config: AudioEncoderConfig): void {}

		encode(frame: AudioData): void {
			const storage = new Uint8Array(description.byteLength + 2);
			storage.set(description, 1);
			const view = new DataView(storage.buffer, 1, description.byteLength);
			const chunk = {
				type: "key",
				timestamp: frame.timestamp,
				byteLength: 1,
				copyTo: (buffer: Uint8Array) => {
					buffer[0] = 0;
				},
			} as EncodedAudioChunk;

			const metadata = {
				decoderConfig: {
					codec: "opus",
					sampleRate: 48_000,
					numberOfChannels: 1,
					description: view,
				},
			};
			queueMicrotask(() => this.#output(chunk, metadata));
		}

		close(): void {}
	}

	class FakeAudioDecoder {}

	const globals: Record<string, unknown> = {
		AudioContext: FakeAudioContext,
		MediaStream: FakeMediaStream,
		MediaStreamAudioSourceNode: FakeGraphNode,
		GainNode: FakeGainNode,
		AudioWorkletNode: FakeAudioWorkletNode,
		AudioData: FakeAudioData,
		AudioEncoder: FakeAudioEncoder,
		AudioDecoder: FakeAudioDecoder,
	};
	const originals = new Map<string, PropertyDescriptor | undefined>();
	for (const [name, value] of Object.entries(globals)) {
		originals.set(name, Object.getOwnPropertyDescriptor(globalThis, name));
		Object.defineProperty(globalThis, name, { configurable: true, writable: true, value });
	}

	return {
		get worklet() {
			return worklet;
		},
		get audioEncoders() {
			return audioEncoders;
		},
		[Symbol.dispose]() {
			for (const [name, original] of originals) {
				if (original) Object.defineProperty(globalThis, name, original);
				else Reflect.deleteProperty(globalThis, name);
			}
		},
	};
}

function fakeSource(sampleRate: number | undefined = 48_000) {
	return {
		kind: "audio",
		getSettings: () => ({ deviceId: "", groupId: "", sampleRate }),
		getConstraints: () => ({}),
	} as unknown as MediaStreamTrack;
}

// Regression: when the current run of #runSource is torn down while `audioWorklet.addModule` is still
// pending, no AudioWorkletNode may be constructed for that abandoned run. The old guard keyed off
// `context.state === "closed"`, which is never true on Firefox/Safari, so it fell through and threw.
test("does not construct an AudioWorkletNode when torn down mid worklet load", async () => {
	using webaudio = installFakeWebAudio();
	const error = spyOn(console, "error").mockImplementation(() => {});

	const encoder = new Encoder("audio", {
		enabled: true,
		source: new Signal(fakeSource()) as never,
	});

	// Let #runSource spawn the task and park it on the pending addModule race.
	await settle();

	// Tear the run down before the module finishes loading. cleanup() calls context.close(), which on
	// Firefox/Safari leaves .state === "suspended", then effect.cancel wins the race.
	encoder.close();
	await settle();

	expect(webaudio.audioWorkletNodes).toBe(0);
	expect(error).not.toHaveBeenCalled();
});

// Regression: a Bluetooth mic on macOS reports 44100 after an A2DP flip. Capturing at that rate means
// the encoder silently resamples to 48000 while the catalog advertises 44100, which no Opus decoder can
// honor: Safari's AudioDecoder fails every decode with InternalAudioDecoderCocoa.
async function requestedRate(sampleRate: number | undefined, codec?: "opus" | "aac") {
	using webaudio = installFakeWebAudio();

	const encoder = new Encoder("audio", {
		enabled: true,
		source: new Signal(fakeSource(sampleRate)) as never,
		...(codec ? { codec } : {}),
	});
	await settle();
	encoder.close();
	await settle();

	return webaudio.requestedRates.at(-1);
}

test("snaps the capture rate to one Opus supports", async () => {
	expect(await requestedRate(44_100)).toBe(48_000);
	expect(await requestedRate(22_050)).toBe(24_000);
});

test("leaves an Opus-native capture rate alone", async () => {
	expect(await requestedRate(16_000)).toBe(16_000);
	expect(await requestedRate(48_000)).toBe(48_000);
});

// captureStream() tracks report no rate, which would otherwise let the AudioContext fall back to the
// machine's output rate (44100 on most Macs).
test("requests full-band Opus when the source reports no rate", async () => {
	expect(await requestedRate(undefined)).toBe(48_000);
});

// 44100 is in the AAC sampling frequency table, so it must survive untouched.
test("leaves an AAC-native capture rate alone", async () => {
	expect(await requestedRate(44_100, "aac")).toBe(44_100);
});

// Regression: only the codec's mime picks the capture rate, so tweaking an encode-only knob must not
// tear down the microphone. Subscribing #runSource to the whole codec signal rebuilt the AudioContext
// on every change, which dropped #worklet and closed the track being published. The demo writes this
// signal from live bitrate/complexity sliders, so it fired on every slider tick.
test("does not rebuild the capture graph when an encode-only knob changes", async () => {
	using webaudio = installFakeWebAudio();

	const codec = new Signal<Codec>({ mime: "opus", bitrate: 32_000 });
	const encoder = new Encoder("audio", {
		enabled: true,
		source: new Signal(fakeSource()) as never,
		codec,
	});
	await settle();
	expect(webaudio.requestedRates.length).toBe(1);

	codec.set({ mime: "opus", bitrate: 64_000 });
	await settle();
	expect(webaudio.requestedRates.length).toBe(1);

	// A real codec switch still has to rebuild: AAC captures at rates Opus can't.
	codec.set({ mime: "aac" });
	await settle();
	expect(webaudio.requestedRates.length).toBe(2);

	encoder.close();
	await settle();
});

test("publishes the Opus decoder description reported by the encoder", async () => {
	const description = Uint8Array.from([
		0x4f, 0x70, 0x75, 0x73, 0x48, 0x65, 0x61, 0x64, 0x01, 0x01, 0x38, 0x01, 0x80, 0xbb, 0x00, 0x00, 0x00, 0x00,
		0x00,
	]);
	using harness = installEncodingHarness(description);

	let writes = 0;
	const track = {
		writeFrame: () => {
			writes++;
		},
		close: () => {},
	};
	const rendition = {
		config: new Signal<Catalog.AudioConfig | undefined>(undefined),
		track: new Signal(track),
		close: () => {},
	};
	const broadcast = { audio: () => rendition };

	const encoder = new Encoder("audio", {
		enabled: true,
		source: new Signal(fakeSource()) as never,
		broadcast: new Signal(broadcast) as never,
	});
	await settle();

	const worklet = harness.worklet;
	expect(worklet).toBeDefined();
	worklet?.port.emit({ timestamp: 0, channels: [new Float32Array(960)] });
	await settle();
	expect(rendition.config.peek()?.description).toBeUndefined();

	worklet?.port.emit({ timestamp: 20_000, channels: [new Float32Array(960)] });
	await settle();

	expect(rendition.config.peek()?.description).toBe("4f707573486561640101380180bb0000000000");
	expect(harness.audioEncoders).toBe(1);
	expect(writes).toBe(1);

	encoder.close();
	await settle();
});

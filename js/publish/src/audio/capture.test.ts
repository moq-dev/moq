import { expect, mock, spyOn, test } from "bun:test";
import { Effect, Signal } from "@moq/signals";

// The capture pulls its processor in as a `?worklet` blob URL, which the bun test loader can't
// resolve. Stub it so the module imports; the value is only ever passed to our fake addModule.
mock.module("./capture-worklet.ts?worklet", () => ({ default: "blob:fake-capture" }));

const { Capture } = await import("./capture.ts");

const flush = () => new Promise<void>((resolve) => queueMicrotask(resolve));
async function settle(times = 5): Promise<void> {
	for (let i = 0; i < times; i++) await flush();
}

// Models the WebAudio surface the audio Capture touches. The key detail is `AudioContext.close()`: on
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

	class FakeMediaStream {
		constructor(_tracks?: unknown) {}
	}

	class FakeGraphNode {
		channelCount = 2;
		constructor(_context?: unknown, _options?: unknown) {}
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

function fakeSource(sampleRate: number | undefined = 48_000) {
	return {
		kind: "audio",
		getSettings: () => ({ deviceId: "", groupId: "", sampleRate }),
		getConstraints: () => ({}),
	} as unknown as MediaStreamTrack;
}

// Regression: when the current run of Capture's track path is torn down while `audioWorklet.addModule` is still
// pending, no AudioWorkletNode may be constructed for that abandoned run. The old guard keyed off
// `context.state === "closed"`, which is never true on Firefox/Safari, so it fell through and threw.
test("does not construct an AudioWorkletNode when torn down mid worklet load", async () => {
	using webaudio = installFakeWebAudio();
	const error = spyOn(console, "error").mockImplementation(() => {});

	const capture = new Capture({
		enabled: true,
		source: new Signal(fakeSource()) as never,
	});

	// Let the capture spawn its task and park it on the pending addModule race.
	await settle();

	// Tear the run down before the module finishes loading. cleanup() calls context.close(), which on
	// Firefox/Safari leaves .state === "suspended", then effect.cancel wins the race.
	capture.close();
	await settle();

	expect(webaudio.audioWorkletNodes).toBe(0);
	expect(error).not.toHaveBeenCalled();
});

// Regression: a Bluetooth mic on macOS reports 44100 after an A2DP flip. Capturing at that rate means
// the encoder silently resamples to 48000 while the catalog advertises 44100, which no Opus decoder can
// honor: Safari's AudioDecoder fails every decode with InternalAudioDecoderCocoa.
async function requestedRate(sampleRate: number | undefined, override?: number) {
	using webaudio = installFakeWebAudio();

	const capture = new Capture({
		enabled: true,
		source: new Signal(fakeSource(sampleRate)) as never,
		...(override !== undefined ? { sampleRate: override } : {}),
	});
	await settle();
	capture.close();
	await settle();

	return webaudio.requestedRates.at(-1);
}

test("snaps a rate Opus can't decode up to full band", async () => {
	expect(await requestedRate(44_100)).toBe(48_000);
	expect(await requestedRate(22_050)).toBe(48_000);
});

// Every rate Opus takes is also in the AAC table, so a narrowband mic is left alone rather than
// upsampled: a voice track has nothing above 8kHz to recover.
test("leaves a rate both codecs support alone", async () => {
	expect(await requestedRate(16_000)).toBe(16_000);
	expect(await requestedRate(48_000)).toBe(48_000);
});

// captureStream() tracks report no rate, which would otherwise let the AudioContext fall back to the
// machine's output rate (44100 on most Macs).
test("requests full band when the source reports no rate", async () => {
	expect(await requestedRate(undefined)).toBe(48_000);
});

test("honors an explicit rate override", async () => {
	expect(await requestedRate(48_000, 16_000)).toBe(16_000);
});

// Regression: tweaking an encoder knob must not tear down the microphone. The capture used to take
// the codec as an input, so it rebuilt the AudioContext on a codec change, dropping the worklet and
// closing the track being published. Now the capture is codec-independent, which it has to be:
// several renditions share one, and they can encode differently.
test("never rebuilds the capture graph for encoder settings", async () => {
	using webaudio = installFakeWebAudio();

	const rate = new Signal<number | undefined>(undefined);
	const capture = new Capture({
		enabled: true,
		source: new Signal(fakeSource()) as never,
		sampleRate: rate,
	});
	await settle();
	expect(webaudio.requestedRates.length).toBe(1);

	// Only something that changes the capture itself may rebuild it.
	rate.set(16_000);
	await settle();
	expect(webaudio.requestedRates.length).toBe(2);

	capture.close();
	await settle();
});

// The gap this fanout closes: a decoded file exposes one ReadableStream, so the second rendition
// used to throw on a locked stream and publish nothing while still advertising a catalog.
test("feeds every rendition off one decoded source", async () => {
	let push!: (value: number) => void;
	const samples = new ReadableStream<AudioData>({
		start(controller) {
			push = (value) =>
				controller.enqueue({
					timestamp: value * 1000,
					numberOfFrames: 4,
					numberOfChannels: 1,
					copyTo: (dest: Float32Array) => dest.fill(value),
					close: () => {},
				} as unknown as AudioData);
		},
	});

	const capture = new Capture({
		enabled: true,
		source: new Signal({ samples, sampleRate: 48_000, channelCount: 1, kind: "auto" }) as never,
	});
	await settle();

	const fanout = capture.out.frames.peek();
	expect(fanout).toBeDefined();

	// Both renditions attach before anything is published; a late one only sees what comes next.
	const first = new Effect();
	const second = new Effect();
	const a = fanout?.subscribe(first).getReader();
	const b = fanout?.subscribe(second).getReader();

	for (const value of [1, 2, 3]) {
		push(value);

		// Both renditions see the same audio; neither steals it from the other.
		expect((await a?.read())?.value?.channels[0][0]).toBe(value);
		expect((await b?.read())?.value?.channels[0][0]).toBe(value);
	}

	first.close();
	second.close();
	capture.close();
	await settle();
});

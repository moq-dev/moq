import { expect, mock, spyOn, test } from "bun:test";
import { Signal } from "@moq/signals";
import type { StreamTrack } from "./types";

// The encoder pulls the capture processor in as a `?worklet` blob URL, which the bun test loader can't
// resolve. Stub it so the module imports; the value is only ever passed to our fake addModule.
mock.module("./capture-worklet.ts?worklet", () => ({ default: "blob:fake-capture" }));

const { Encoder } = await import("./encoder.ts");

const flush = () => new Promise<void>((resolve) => queueMicrotask(resolve));
async function settle(times = 5): Promise<void> {
	for (let i = 0; i < times; i++) await flush();
}

// Models the WebAudio surface `#runSource` touches. The key detail is `AudioContext.close()`: on
// Firefox and Safari it does NOT synchronously flip `.state` to "closed" (it stays "suspended"), which
// is exactly the browser behavior the old `context.state === "closed"` guard failed to account for.
function installFakeWebAudio(props: { loaded?: boolean } = {}) {
	const addModule = props.loaded ? () => Promise.resolve() : () => new Promise<void>(() => {});
	let audioWorkletNodes = 0;
	const sampleRates: Array<number | undefined> = [];

	class FakeAudioContext {
		state: AudioContextState = "suspended";
		audioWorklet = { addModule };
		sampleRate: number;
		currentTime = 0;
		constructor(options?: AudioContextOptions) {
			sampleRates.push(options?.sampleRate);
			this.sampleRate = options?.sampleRate ?? 44_100;
		}
		close(): Promise<void> {
			// Firefox/Safari behavior: stays "suspended", never "closed".
			return Promise.resolve();
		}
	}

	class FakeMediaStream {}

	class FakeGraphNode {
		channelCount = 2;
		context: FakeAudioContext;
		gain = {
			cancelScheduledValues: () => {},
			exponentialRampToValueAtTime: () => {},
			setValueAtTime: () => {},
		};
		constructor(context: FakeAudioContext) {
			this.context = context;
		}
		connect(): void {}
		disconnect(): void {}
	}

	class FakePort extends EventTarget {
		#started = false;

		start(): void {
			if (this.#started) return;
			this.#started = true;
			queueMicrotask(() => {
				this.dispatchEvent(
					new MessageEvent("message", {
						data: { timestamp: 0, channels: [new Float32Array(128)] },
					}),
				);
			});
		}
	}

	class FakeAudioWorkletNode {
		context: FakeAudioContext;
		port = new FakePort();
		constructor(context: FakeAudioContext, _name: string) {
			audioWorkletNodes++;
			this.context = context;
			if (props.loaded) return;
			// The real constructor throws when the module registration was abandoned mid-load.
			throw new DOMException("Unknown AudioWorklet name 'capture'", "InvalidStateError");
		}
		connect(): void {}
		disconnect(): void {}
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
		get sampleRates() {
			return sampleRates;
		},
		[Symbol.dispose]() {
			for (const [name, original] of originals) {
				if (original) Object.defineProperty(globalThis, name, original);
				else Reflect.deleteProperty(globalThis, name);
			}
		},
	};
}

function fakeSource(sampleRate: number | undefined = 48_000): StreamTrack {
	return {
		kind: "audio",
		getSettings: () => ({ deviceId: "", groupId: "", sampleRate }),
		getConstraints: () => ({}),
	} as unknown as StreamTrack;
}

test("normalizes a 44.1kHz Opus source to 48kHz before capture", async () => {
	using webaudio = installFakeWebAudio({ loaded: true });
	const encoder = new Encoder("audio", {
		enabled: true,
		source: fakeSource(44_100),
		codec: "opus",
	});

	await settle();
	expect(webaudio.sampleRates).toEqual([48_000]);
	expect(Number(encoder.out.catalog.peek()?.sampleRate)).toBe(48_000);

	encoder.close();
	await settle();
});

test("preserves native Opus capture rates", async () => {
	using webaudio = installFakeWebAudio();
	const encoder = new Encoder("audio", {
		enabled: true,
		source: fakeSource(16_000),
		codec: "opus",
	});

	await settle();
	expect(webaudio.sampleRates).toEqual([16_000]);

	encoder.close();
	await settle();
});

test("defaults Opus capture to 48kHz when the source has no rate", async () => {
	using webaudio = installFakeWebAudio();
	const encoder = new Encoder("audio", {
		enabled: true,
		source: fakeSource(undefined),
		codec: "opus",
	});

	await settle();
	expect(webaudio.sampleRates).toEqual([48_000]);

	encoder.close();
	await settle();
});

test("does not normalize AAC capture rates", async () => {
	using webaudio = installFakeWebAudio();
	const encoder = new Encoder("audio", {
		enabled: true,
		source: fakeSource(44_100),
		codec: "aac",
	});

	await settle();
	expect(webaudio.sampleRates).toEqual([44_100]);

	encoder.close();
	await settle();
});

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

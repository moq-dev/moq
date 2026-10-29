import { afterEach, beforeEach, expect, spyOn, test } from "bun:test";
import { type Installed, Manual, Milli, Nano, sleep } from "@moq/clock";
import * as Catalog from "@moq/hang/catalog";
import * as Container from "@moq/hang/container";
import * as Json from "@moq/json";
import * as Moq from "@moq/net";
import { Signal } from "@moq/signals";
import { Broadcast } from "../broadcast";
import { Estimator, Order } from "../order";
import { Encoder } from "./encoder";

// How long a fake hardware probe takes on the installed clock.
const PROBE = Nano.fromMilli(Milli(5));

let clock = new Manual();
let installed: Installed | undefined;
beforeEach(() => {
	clock = new Manual();
	installed = clock.install();
});
afterEach(() => installed?.close());

class FakeVideoEncoder {
	static instances: FakeVideoEncoder[] = [];
	// How many times the hardware has been probed, so a test can assert it isn't re-probed.
	static probes = 0;

	// Every accepted probe, so a test can assert a published config was actually validated.
	static accepted: { codec: string; width: number; height: number }[] = [];

	// The codec string the encoder reports in its output for a configured one. Echoes by default,
	// like Chrome and Firefox.
	static report: (codec: string) => string | undefined = (codec) => codec;

	state: CodecState = "unconfigured";
	#output: VideoEncoderInit["output"];
	#codec?: string;
	#error: VideoEncoderInit["error"];

	constructor(init: VideoEncoderInit) {
		this.#output = init.output;
		this.#error = init.error;
		FakeVideoEncoder.instances.push(this);
	}

	fail(error: Error): void {
		this.state = "closed";
		this.#error(error as DOMException);
	}

	static async isConfigSupported(config: VideoEncoderConfig): Promise<{ supported: boolean }> {
		FakeVideoEncoder.probes++;
		// Pretend the GPU takes a while, like a real probe under load.
		await sleep(PROBE);

		const supported = config.codec.startsWith("avc1");
		if (supported)
			FakeVideoEncoder.accepted.push({ codec: config.codec, width: config.width, height: config.height });
		return { supported };
	}

	configure(config: VideoEncoderConfig): void {
		this.state = "configured";
		this.#codec = config.codec;
	}

	encode(): void {}

	// Only the probe flushes, so only it sees an output; the live encoder holds every frame.
	async flush(): Promise<void> {
		const codec = this.#codec && FakeVideoEncoder.report(this.#codec);
		const chunk = { type: "key", timestamp: 0, byteLength: 1, copyTo: () => {} };
		this.#output(chunk as never, (codec ? { decoderConfig: { codec } } : {}) as never);
	}

	close(): void {
		this.state = "closed";
	}
}

class FakeVideoFrame {
	timestamp = 0;
	close(): void {}
}

function installFakeVideoEncoder() {
	FakeVideoEncoder.instances = [];
	const original = Object.getOwnPropertyDescriptor(globalThis, "VideoEncoder");
	const frame = Object.getOwnPropertyDescriptor(globalThis, "VideoFrame");
	Object.defineProperty(globalThis, "VideoEncoder", {
		configurable: true,
		value: FakeVideoEncoder,
		writable: true,
	});
	Object.defineProperty(globalThis, "VideoFrame", { configurable: true, value: FakeVideoFrame, writable: true });

	return {
		[Symbol.dispose]() {
			FakeVideoEncoder.report = (codec) => codec;
			if (original) {
				Object.defineProperty(globalThis, "VideoEncoder", original);
			} else {
				Reflect.deleteProperty(globalThis, "VideoEncoder");
			}
			if (frame) Object.defineProperty(globalThis, "VideoFrame", frame);
			else Reflect.deleteProperty(globalThis, "VideoFrame");
		},
	};
}

test("encoding tracks encoder config in its child effect", async () => {
	using _videoEncoder = installFakeVideoEncoder();
	const warn = spyOn(console, "warn").mockImplementation(() => {});

	const track = new Moq.Track.Producer("video/hd").accept({ timescale: Moq.Time.Timescale.MILLI });
	const rendition = {
		config: new Signal(undefined),
		track: new Signal<Moq.Track.Producer | undefined>(track),
		close: () => {
			track.finish();
			track.close();
		},
	};
	const broadcast = { video: () => rendition, order: new Order() };
	const capture = {
		in: { source: new Signal(undefined) },
		out: {
			display: new Signal<{ width: number; height: number } | undefined>(undefined),
			frames: new Signal(undefined),
		},
	};
	const encoder = new Encoder("video/hd", {
		enabled: true,
		broadcast: broadcast as never,
		capture: capture as never,
	});

	try {
		for (let i = 0; i < 5; i++) await Promise.resolve();

		expect(warn).not.toHaveBeenCalledWith(
			"Effect did not subscribe to any signals; it will never rerun.",
			expect.anything(),
		);
	} finally {
		encoder.close();
		warn.mockRestore();
	}
});

test("an on-demand gap marks a discontinuity and leaves the broadcast-owned track open for resume", async () => {
	using _videoEncoder = installFakeVideoEncoder();
	const discontinuity = spyOn(Container.Legacy.Producer.prototype, "discontinuity");

	const track = new Moq.Track.Producer("video").accept({ timescale: Moq.Time.Timescale.MILLI, priority: 60 });
	const live = new Signal<Moq.Track.Producer | undefined>(track);
	const rendition = {
		config: new Signal(undefined),
		track: live,
		close: () => {
			track.finish();
			track.close();
		},
	};
	const capture = {
		in: { source: new Signal(undefined) },
		out: {
			display: new Signal({ width: 640, height: 480 }),
			frames: new Signal(undefined),
		},
	};
	const encoder = new Encoder("video", {
		enabled: true,
		broadcast: { video: () => rendition, order: new Order() } as never,
		capture: capture as never,
	});

	try {
		await settle();
		live.set(undefined);
		await settle();
		expect(track.closed.peek()).toBeUndefined();
		expect(discontinuity).toHaveBeenCalledTimes(1);

		live.set(track);
		await settle();
		expect(track.closed.peek()).toBeUndefined();

		discontinuity.mockClear();
		track.finish();
		track.close();
		encoder.close();
		expect(discontinuity).not.toHaveBeenCalled();
	} finally {
		encoder.close();
		track.finish();
		track.close();
		discontinuity.mockRestore();
	}
});

// A bandwidth sample used to rerun the whole resolve effect, which blanked the resolved config (and
// with it the catalog entry) and re-probed the hardware for a codec. A subscriber returning during
// that window got a VideoEncoder that was never configured, so every captured frame was dropped.
// https://github.com/moq-dev/moq/issues/2635
test("a bandwidth estimate updates the bitrate without blanking the config or re-probing", async () => {
	using _videoEncoder = installFakeVideoEncoder();

	const track = new Moq.Track.Producer("video").accept({ timescale: Moq.Time.Timescale.MILLI, priority: 60 });
	const sub = track.subscribe();
	const rendition = {
		config: new Signal(undefined),
		track: new Signal<Moq.Track.Producer | undefined>(track),
		close: () => {
			track.finish();
			track.close();
		},
	};
	const capture = {
		in: {
			source: new Signal({
				getSettings: () => ({ frameRate: 30 }),
				getConstraints: () => ({}),
			} as never),
		},
		out: {
			display: new Signal({ width: 640, height: 480 }),
			frames: new Signal(undefined),
		},
	};
	const estimate = new Signal<number | undefined>(10_000_000);
	const bandwidth = new Moq.Bandwidth.Allocator(estimate);

	const encoder = new Encoder("video", {
		enabled: true,
		broadcast: { video: () => rendition, order: new Order() } as never,
		capture: capture as never,
		bandwidth,
	});

	try {
		await settle();

		const resolved = encoder.out.resolved.peek();
		expect(resolved).toBeDefined();
		expect(encoder.out.catalog.peek()).toBeDefined();

		const probes = FakeVideoEncoder.probes;
		expect(probes).toBeGreaterThan(0);

		// A poll lands with an estimate low enough to cap the bitrate. The grant
		// is the whole estimate: the 10% headroom was the old stand-in for audio.
		estimate.set(200_000);
		await settle();

		// The cap applied, and nothing went blank on the way there.
		expect(encoder.out.resolved.peek()?.bitrate).toBe(200_000);
		expect(encoder.out.resolved.peek()?.codec).toBe(resolved?.codec);
		expect(encoder.out.catalog.peek()).toBeDefined();

		// The codec doesn't depend on bandwidth, so the hardware is not asked again.
		expect(FakeVideoEncoder.probes).toBe(probes);

		// Repeated samples, as the 100ms poll delivers. The config stays live throughout.
		for (let i = 0; i < 20; i++) {
			estimate.set(1_000_000 + i * 13_000);
			await Promise.resolve();
			await Promise.resolve();
			expect(encoder.out.resolved.peek()).toBeDefined();
			expect(encoder.out.catalog.peek()).toBeDefined();
		}

		expect(FakeVideoEncoder.probes).toBe(probes);
	} finally {
		encoder.close();
		bandwidth.close();
		sub.close();
	}
});

// Step through 40 back-to-back probes, which outlasts any chain of re-probes a test triggers.
async function settle(): Promise<void> {
	for (let i = 0; i < 40; i++) await clock.advance(PROBE);
}

// The codec probe only speaks for the codec and coded-size ceiling it ran against. Those live in
// separate effects, which observe a change in whatever order they happen to be subscribed in, so
// the resolved config must never pair a fresh dimension with a stale probe it outgrew.
// https://github.com/moq-dev/moq/issues/2635
test("every published config fits a probe for its own codec", async () => {
	using _videoEncoder = installFakeVideoEncoder();
	FakeVideoEncoder.accepted = [];

	const track = new Moq.Track.Producer("video").accept({ timescale: Moq.Time.Timescale.MILLI, priority: 60 });
	const sub = track.subscribe();
	const rendition = {
		config: new Signal(undefined),
		track: new Signal<Moq.Track.Producer | undefined>(track),
		close: () => {
			track.finish();
			track.close();
		},
	};
	const capture = {
		in: {
			source: new Signal({
				getSettings: () => ({ frameRate: 30 }),
				getConstraints: () => ({}),
			} as never),
		},
		out: {
			display: new Signal({ width: 1280, height: 720 }),
			frames: new Signal(undefined),
		},
	};
	const estimate = new Signal<number | undefined>(10_000_000);
	const bandwidth = new Moq.Bandwidth.Allocator(estimate);

	const encoder = new Encoder("video", {
		enabled: true,
		broadcast: { video: () => rendition, order: new Order() } as never,
		capture: capture as never,
		bandwidth,
	});

	// Check at emission time, not afterwards: a probe that lands later would otherwise excuse a
	// config that was already handed to a subscriber against a stale one.
	const published: string[] = [];
	const violations: string[] = [];
	const unsubscribe = encoder.out.resolved.subscribe((config) => {
		if (!config) return;

		const key = probeKey(config);
		published.push(key);
		const fits = FakeVideoEncoder.accepted.some(
			(probe) => probe.codec === config.codec && probe.width >= config.width && probe.height >= config.height,
		);
		if (!fits) violations.push(key);
	});

	try {
		encoder.config.set({ maxScale: 0.25 });
		await settle();
		expect(published.length).toBeGreaterThan(0);

		// A resize landing in the same batch as a bandwidth sample, which is likely given the
		// connection polls the estimate every 100ms. The resize schedules the dimensions effect and
		// the sample schedules the resolve effect, so the resolve effect runs with the new
		// dimensions already written while the probe still holds the result for the old ones.
		capture.out.display.set({ width: 640, height: 480 });
		estimate.set(3_000_000);
		await settle();
		expect(published.length).toBeGreaterThan(1);

		// Outgrowing the ceiling the same way, while the re-probe at the new ceiling runs.
		encoder.config.set({});
		capture.out.display.set({ width: 1920, height: 1080 });
		estimate.set(4_000_000);
		await settle();
		expect(encoder.out.resolved.peek()).toMatchObject({ width: 1920, height: 1072 });

		expect(violations).toEqual([]);
	} finally {
		unsubscribe();
		encoder.close();
		bandwidth.close();
		sub.close();
	}
});

function probeKey(config: { codec: string; width: number; height: number }): string {
	return `${config.codec}@${config.width}x${config.height}`;
}

test("hardware encoding takes priority over software H.264", async () => {
	using _videoEncoder = installFakeVideoEncoder();
	const probe = spyOn(FakeVideoEncoder, "isConfigSupported").mockImplementation(async (config) => ({
		supported: config.codec.startsWith("avc1"),
	}));
	const capture = {
		in: { source: new Signal({ getSettings: () => ({ frameRate: 30 }), getConstraints: () => ({}) }) },
		out: { display: new Signal({ width: 1920, height: 1080 }) },
	};
	const encoder = new Encoder("video", { enabled: true, capture: capture as never });
	try {
		await settle();
		expect(encoder.out.resolved.peek()?.hardwareAcceleration).toBe("prefer-hardware");
		expect(probe.mock.calls.every(([config]) => config.hardwareAcceleration === "prefer-hardware")).toBe(true);
		expect(encoder.settled.peek()).toBe(true);
	} finally {
		encoder.close();
		probe.mockRestore();
	}
});

test("software-only AV1 is refused even when explicitly requested", async () => {
	using _videoEncoder = installFakeVideoEncoder();
	const probe = spyOn(FakeVideoEncoder, "isConfigSupported").mockImplementation(async (config) => ({
		supported: config.codec.startsWith("av01") && config.hardwareAcceleration === "prefer-software",
	}));
	const error = spyOn(console, "error").mockImplementation(() => {});
	const capture = {
		in: { source: new Signal({ getSettings: () => ({ frameRate: 30 }), getConstraints: () => ({}) }) },
		out: { display: new Signal({ width: 1920, height: 1080 }) },
	};
	const encoder = new Encoder("video", { enabled: true, capture: capture as never, config: { codec: "av01" } });
	try {
		await settle();
		expect(probe).toHaveBeenCalled();
		expect(probe.mock.calls.some(([config]) => config.hardwareAcceleration === "prefer-software")).toBe(false);
		expect(encoder.out.resolved.peek()).toBeUndefined();
		expect(error).toHaveBeenCalled();
		// No codec fits, so `<moq-publish>` must stop waiting on this rendition to announce.
		expect(encoder.settled.peek()).toBe(true);
	} finally {
		encoder.close();
		probe.mockRestore();
		error.mockRestore();
	}
});

// Regression: only the codec probe and `maxScale` settled the encoder on failure, so a knob that broke
// resolution later (a negative bitrate) held `<moq-publish>`'s announce forever.
test("an invalid knob settles the encoder without a config", async () => {
	using _videoEncoder = installFakeVideoEncoder();
	const error = spyOn(console, "error").mockImplementation(() => {});
	const capture = {
		in: { source: new Signal({ getSettings: () => ({ frameRate: 30 }), getConstraints: () => ({}) }) },
		out: { display: new Signal({ width: 1920, height: 1080 }) },
	};
	const config = new Signal<{ bitrateScale?: number } | undefined>({ bitrateScale: -1 });
	const probe = spyOn(FakeVideoEncoder, "isConfigSupported");
	const encoder = new Encoder("video", { enabled: true, capture: capture as never, config });
	try {
		await settle();
		expect(encoder.out.catalog.peek()).toBeUndefined();
		expect(encoder.settled.peek()).toBe(true);
		expect(error).toHaveBeenCalled();
		// Refused before the browser could coerce it into an unsigned bitrate.
		expect(probe).not.toHaveBeenCalled();

		// Fixing the knob clears the failure and resolves a config.
		config.set(undefined);
		await settle();
		expect(encoder.out.catalog.peek()).toBeDefined();
		expect(encoder.settled.peek()).toBe(true);
	} finally {
		encoder.close();
		error.mockRestore();
		probe.mockRestore();
	}
});

test("screen encoders default to logical pixels without scaling an already reduced capture twice", async () => {
	using _videoEncoder = installFakeVideoEncoder();
	const capture = {
		in: {
			source: new Signal({
				scale: 2,
				track: {
					getSettings: () => ({ frameRate: 30 }),
					getConstraints: () => ({}),
					getCapabilities: () => ({ width: { max: 5120 }, height: { max: 2880 } }),
				},
			}),
		},
		out: { display: new Signal({ width: 5120, height: 2880, scale: 2 }) },
	};
	const encoder = new Encoder("video", { enabled: true, capture: capture as never });
	try {
		await settle();
		expect(encoder.out.resolved.peek()).toMatchObject({ width: 2560, height: 1440 });
		capture.out.display.set({ width: 2560, height: 1440, scale: 2 });
		await settle();
		expect(encoder.out.resolved.peek()).toMatchObject({ width: 2560, height: 1440 });

		capture.out.display.set({ width: 5120, height: 2880, scale: 2 });
		await settle();
		capture.out.display.set({ width: 5120, height: 2880, scale: 1 });
		await settle();
		expect(encoder.out.resolved.peek()).toMatchObject({ width: 5120, height: 2880 });
		capture.out.display.set({ width: 5120, height: 2880, scale: 2 });
		encoder.config.set({ maxScale: 1 });
		await settle();
		expect(encoder.out.resolved.peek()).toMatchObject({ width: 5120, height: 2880 });
		encoder.config.set({ maxPixels: 1920 * 1080 });
		await settle();
		expect(encoder.out.resolved.peek()).toMatchObject({ width: 1920, height: 1072 });
	} finally {
		encoder.close();
	}
});

for (const version of [140, 142, 143, 152]) {
	test(`Firefox ${version} uses trustworthy hardware probes only`, async () => {
		using _videoEncoder = installFakeVideoEncoder();
		const userAgent = Object.getOwnPropertyDescriptor(navigator, "userAgent");
		Object.defineProperty(navigator, "userAgent", {
			configurable: true,
			value: `Mozilla/5.0 Firefox/${version}.0`,
		});
		const probe = spyOn(FakeVideoEncoder, "isConfigSupported").mockImplementation(async () => ({
			supported: true,
		}));
		const capture = {
			in: { source: new Signal({ getSettings: () => ({ frameRate: 30 }), getConstraints: () => ({}) }) },
			out: { display: new Signal({ width: 1920, height: 1080 }) },
		};
		const encoder = new Encoder("video", { enabled: true, capture: capture as never });
		try {
			await settle();
			expect(encoder.out.resolved.peek()?.hardwareAcceleration).toBe(
				version < 143 ? "prefer-software" : "prefer-hardware",
			);
			if (version < 143) expect(encoder.out.resolved.peek()?.codec.startsWith("avc1")).toBe(true);
		} finally {
			encoder.close();
			probe.mockRestore();
			if (userAgent) Object.defineProperty(navigator, "userAgent", userAgent);
			else Reflect.deleteProperty(navigator, "userAgent");
		}
	});
}

test("frame sources retain their dimensions and nominal frame rate", async () => {
	using _videoEncoder = installFakeVideoEncoder();
	const frames = new ReadableStream<VideoFrame>();
	const capture = {
		in: { source: new Signal({ frames, frameRate: 24 }) },
		out: { display: new Signal({ width: 1920, height: 1080 }) },
	};
	const encoder = new Encoder("video", { enabled: true, capture: capture as never });
	try {
		await settle();
		expect(encoder.out.resolved.peek()).toMatchObject({ width: 1920, height: 1072, framerate: 24 });
	} finally {
		encoder.close();
		await frames.cancel();
	}
});

// A viewer released before `enabled` keeps selecting a disabled rendition, so the last picture it
// decodes has to be black rather than a frozen frame. The rendition stays in the catalog with its last
// config, even once the capture is released.
test("disabling encodes one black keyframe and keeps the rendition with enabled: false", async () => {
	const encoded: { timestamp: number; keyFrame: boolean; black: boolean }[] = [];

	class RecordingVideoFrame {
		readonly timestamp: number;
		readonly black: boolean;
		constructor(data: unknown, init?: { timestamp?: number }) {
			this.timestamp = init?.timestamp ?? 0;
			this.black = data instanceof Uint8Array && data[0] === 0x10;
		}
		close(): void {}
	}

	class RecordingVideoEncoder {
		state: CodecState = "unconfigured";
		#output: VideoEncoderInit["output"];
		#codec?: string;
		#last = 0;

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

		encode(frame: { timestamp: number; black?: boolean }, options?: VideoEncoderEncodeOptions): void {
			this.#last = frame.timestamp;
			encoded.push({ timestamp: frame.timestamp, keyFrame: !!options?.keyFrame, black: !!frame.black });
		}

		// Emits the newest frame, and reports the configured codec as Chrome does.
		async flush(): Promise<void> {
			const chunk = { type: "key", timestamp: this.#last, byteLength: 1, copyTo: () => {} };
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
	const videoFrame = Object.getOwnPropertyDescriptor(globalThis, "VideoFrame");
	Object.defineProperty(globalThis, "VideoFrame", {
		configurable: true,
		value: RecordingVideoFrame,
		writable: true,
	});

	class Frame {
		readonly timestamp: number;
		constructor(timestamp: number) {
			this.timestamp = timestamp;
		}
		clone(): Frame {
			return new Frame(this.timestamp);
		}
		close(): void {}
	}

	const { Fanout } = await import("../fanout");
	let controller!: ReadableStreamDefaultController<VideoFrame>;
	const fanout = new Fanout(
		new ReadableStream<VideoFrame>({
			start: (c) => {
				controller = c;
			},
		}),
		{ clone: (frame) => frame.clone(), release: (frame) => frame.close() },
	);

	const track = new Moq.Track.Producer("video").accept({ timescale: Moq.Time.Timescale.MILLI });
	const rendition = {
		config: new Signal(undefined),
		track: new Signal<Moq.Track.Producer | undefined>(track),
		close: () => {
			track.finish();
			track.close();
		},
	};
	const source = new Signal<unknown>({ getSettings: () => ({ frameRate: 30 }), getConstraints: () => ({}) });
	const capture = {
		in: { source },
		out: { display: new Signal({ width: 640, height: 480 }), frames: new Signal(fanout) },
	};
	const enabled = new Signal(true);
	const order = new Order();
	const encoder = new Encoder("video", {
		enabled,
		broadcast: { video: () => rendition, order } as never,
		capture: capture as never,
	});

	try {
		await settle();
		const before = encoder.out.catalog.peek();
		expect(before?.codedWidth).toBe(Catalog.u53(640));
		expect(before?.enabled).toBeUndefined();

		for (const millis of [0, 33, 66]) {
			controller.enqueue(new Frame(millis * 1000) as unknown as VideoFrame);
			await clock.advance(PROBE);
		}
		expect(encoded.filter((frame) => frame.black)).toEqual([]);

		enabled.set(false);
		await settle();
		expect(encoded.filter((frame) => frame.black)).toEqual([
			{ timestamp: 66_000 + 33_333, keyFrame: true, black: true },
		]);
		expect(encoder.out.active.peek()).toBe(false);
		// The black keyframe is written during teardown, which must not seat the stopped rendition again.
		expect(new Estimator().write(0, order)).toBe(false);

		// Muting releases the capture; the rendition keeps advertising what it last published.
		source.set(undefined);
		await settle();
		const disabled = encoder.out.catalog.peek();
		expect(disabled?.enabled).toBe(false);
		expect(disabled?.codec).toBe(before?.codec);
		expect(disabled?.codedWidth).toBe(before?.codedWidth);

		// Re-enabled before the capture reopens: still the same rendition, still disabled.
		enabled.set(true);
		await settle();
		expect(encoder.out.catalog.peek()).toEqual(disabled);

		source.set({ getSettings: () => ({ frameRate: 30 }), getConstraints: () => ({}) });
		await settle();
		expect(encoder.out.catalog.peek()?.enabled).toBeUndefined();
		expect(encoded.filter((frame) => frame.black)).toHaveLength(1);
	} finally {
		encoder.close();
		fanout.close();
		track.finish();
		track.close();
		if (original) Object.defineProperty(globalThis, "VideoEncoder", original);
		else Reflect.deleteProperty(globalThis, "VideoEncoder");
		if (videoFrame) Object.defineProperty(globalThis, "VideoFrame", videoFrame);
		else Reflect.deleteProperty(globalThis, "VideoFrame");
	}
});

// Every browser refuses a bare hint, so probing one only delays the fallback, and a browser that
// accepted it would publish a codec no native player can decode.
test("the probe only offers full codec strings", async () => {
	using _videoEncoder = installFakeVideoEncoder();
	const probe = spyOn(FakeVideoEncoder, "isConfigSupported").mockImplementation(async () => ({ supported: false }));
	const error = spyOn(console, "error").mockImplementation(() => {});
	const capture = {
		in: { source: new Signal({ getSettings: () => ({ frameRate: 30 }), getConstraints: () => ({}) }) },
		out: { display: new Signal({ width: 1280, height: 720 }) },
	};
	const encoder = new Encoder("video", { enabled: true, capture: capture as never });
	try {
		await settle();
		const probed = probe.mock.calls.map(([config]) => config.codec);
		expect(probed.length).toBeGreaterThan(0);
		expect(probed.filter((codec) => codec !== "vp8" && !codec.includes("."))).toEqual([]);
		expect(encoder.settled.peek()).toBe(true);
	} finally {
		encoder.close();
		probe.mockRestore();
		error.mockRestore();
	}
});

// The catalog is written before any subscriber starts the real encoder, so it has to carry what the
// bitstream will say from the first announce, not a probe input a native player may refuse.
test("the catalog advertises the codec string a probe encode reports", async () => {
	using _videoEncoder = installFakeVideoEncoder();
	// Refine the configured string, as an encoder that picks its own level would.
	const REPORTED: Record<string, string> = {
		"vp09.00.10.08": "vp09.00.31.08",
		"avc1.640028": "avc1.64001F",
	};
	const probe = spyOn(FakeVideoEncoder, "isConfigSupported").mockImplementation(async (config) => ({
		supported: config.codec in REPORTED,
	}));
	FakeVideoEncoder.report = (codec) => REPORTED[codec];

	const capture = {
		in: { source: new Signal({ getSettings: () => ({ frameRate: 30 }), getConstraints: () => ({}) }) },
		out: { display: new Signal({ width: 1280, height: 720 }) },
	};
	const encoder = new Encoder("video", { enabled: true, capture: capture as never });
	const advertised: string[] = [];
	const unsubscribe = encoder.out.catalog.subscribe((catalog) => {
		if (catalog) advertised.push(catalog.codec);
	});

	try {
		await settle();
		expect(encoder.out.resolved.peek()?.codec).toBe("vp09.00.10.08");
		expect(encoder.out.catalog.peek()?.codec).toBe("vp09.00.31.08");

		// A codec change re-probes and advertises the new encoder's string.
		encoder.config.set({ codec: "avc1" });
		await settle();
		expect(encoder.out.resolved.peek()?.codec).toBe("avc1.640028");
		expect(encoder.out.catalog.peek()?.codec).toBe("avc1.64001F");

		// The configured strings were never advertised, not even briefly.
		expect([...new Set(advertised)]).toEqual(["vp09.00.31.08", "avc1.64001F"]);
	} finally {
		unsubscribe();
		encoder.close();
		probe.mockRestore();
	}
});

test("a probe encode that reports no codec string fails loud", async () => {
	using _videoEncoder = installFakeVideoEncoder();
	FakeVideoEncoder.report = () => undefined;
	const error = spyOn(console, "error").mockImplementation(() => {});
	const capture = {
		in: { source: new Signal({ getSettings: () => ({ frameRate: 30 }), getConstraints: () => ({}) }) },
		out: { display: new Signal({ width: 1280, height: 720 }) },
	};
	const encoder = new Encoder("video", { enabled: true, capture: capture as never });
	try {
		await settle();
		expect(encoder.out.catalog.peek()).toBeUndefined();
		expect(encoder.settled.peek()).toBe(true);
		expect(error).toHaveBeenCalled();
	} finally {
		encoder.close();
		error.mockRestore();
	}
});

// An encoder may pick its level from the frame rate or bitrate, so the probe encodes with both. The
// bitrate is the ceiling, so a bandwidth grant caps the live encoder without re-probing.
test("the probe encodes with the frame rate and bitrate ceiling", async () => {
	using _videoEncoder = installFakeVideoEncoder();
	const probe = spyOn(FakeVideoEncoder, "isConfigSupported");

	const track = new Moq.Track.Producer("video").accept({ timescale: Moq.Time.Timescale.MILLI, priority: 60 });
	const sub = track.subscribe();
	const rendition = {
		config: new Signal(undefined),
		track: new Signal<Moq.Track.Producer | undefined>(track),
		close: () => {
			track.finish();
			track.close();
		},
	};
	const capture = {
		in: { source: new Signal({ getSettings: () => ({ frameRate: 30 }), getConstraints: () => ({}) }) },
		out: { display: new Signal({ width: 1280, height: 720 }), frames: new Signal(undefined) },
	};
	const estimate = new Signal<number | undefined>(100_000_000);
	const bandwidth = new Moq.Bandwidth.Allocator(estimate);
	const encoder = new Encoder("video", {
		enabled: true,
		broadcast: { video: () => rendition, order: new Order() } as never,
		capture: capture as never,
		bandwidth,
	});
	const accepted = () =>
		probe.mock.calls.map(([config]) => config).filter((config) => config.codec.startsWith("avc1"));

	try {
		await settle();
		const first = accepted().at(-1);
		expect(first).toMatchObject({ framerate: 30 });
		expect(first?.bitrate).toBeGreaterThan(0);
		expect(encoder.out.resolved.peek()?.bitrate).toBe(first?.bitrate);

		// A grant below the ceiling caps the live config but leaves the probe alone.
		const probes = probe.mock.calls.length;
		estimate.set(500_000);
		await settle();
		expect(encoder.out.resolved.peek()?.bitrate).toBe(500_000);
		expect(probe.mock.calls.length).toBe(probes);

		// A frame rate change re-probes with the new rate and its larger ceiling.
		encoder.config.set({ frameRate: 60 });
		await settle();
		expect(probe.mock.calls.length).toBeGreaterThan(probes);
		const second = accepted().at(-1);
		expect(second).toMatchObject({ framerate: 60 });
		expect(second?.bitrate).toBeGreaterThan(first?.bitrate ?? 0);
		expect(encoder.out.catalog.peek()?.framerate).toBe(60);
	} finally {
		encoder.close();
		bandwidth.close();
		sub.close();
		probe.mockRestore();
	}
});

// A re-probe keeps the rendition advertised while it runs: dropping it, even for a moment, would
// tell every subscriber the track went away.
test("a re-probe swaps the catalog entry in one update", async () => {
	using _videoEncoder = installFakeVideoEncoder();
	const probe = spyOn(FakeVideoEncoder, "isConfigSupported");
	const capture = {
		in: { source: new Signal({ getSettings: () => ({ frameRate: 30 }), getConstraints: () => ({}) }) },
		out: { display: new Signal({ width: 1280, height: 720 }) },
	};
	const encoder = new Encoder("video", { enabled: true, capture: capture as never });

	try {
		await settle();
		expect(encoder.out.catalog.peek()?.framerate).toBe(30);

		const published: (number | undefined)[] = [];
		const unsubscribe = encoder.out.catalog.subscribe((catalog) => published.push(catalog?.framerate));
		try {
			// A frame rate change re-probes, and the catalog goes straight from the old entry to the new.
			let probes = probe.mock.calls.length;
			encoder.config.set({ frameRate: 60 });
			await settle();
			expect(probe.mock.calls.length).toBeGreaterThan(probes);
			expect(published).toEqual([60]);

			// A re-probe that lands on the same config leaves the catalog untouched.
			probes = probe.mock.calls.length;
			encoder.config.set({ frameRate: 60, maxPixels: 1280 * 720 });
			await settle();
			expect(probe.mock.calls.length).toBeGreaterThan(probes);
			expect(published).toEqual([60]);
		} finally {
			unsubscribe();
		}
	} finally {
		encoder.close();
		probe.mockRestore();
	}
});

// Drive a live encoder over a recording `VideoEncoder` that outputs every frame it is handed, so the
// track holds real groups: a keyframe of 100 bytes, each delta 60. `run` sends one frame per call
// at `millis` of media time and reads back which frames were forced to be keyframes.
// A captured frame, stamped in microseconds.
class Frame {
	readonly timestamp: number;
	constructor(timestamp: number) {
		this.timestamp = timestamp;
	}
	clone(): Frame {
		return new Frame(this.timestamp);
	}
	close(): void {}
}

class RecordingVideoEncoder {
	// The timestamps (ms) of the test frames the encoder was asked to make keyframes.
	static keys: number[] = [];

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

	// Records the test's frames only, not the probe's, and outputs each a task later as WebCodecs does.
	encode(frame: VideoFrame, options?: VideoEncoderEncodeOptions): void {
		if (!(frame instanceof Frame)) return;
		const key = !!options?.keyFrame;
		if (key) RecordingVideoEncoder.keys.push(frame.timestamp / 1000);
		const chunk = { type: key ? "key" : "delta", timestamp: frame.timestamp, byteLength: key ? 100 : 60 };
		queueMicrotask(() => this.#output({ ...chunk, copyTo: () => {} } as never, {} as never));
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

// Install the recording encoder and a capture whose frames `send` feeds, one per call.
async function installRecording() {
	RecordingVideoEncoder.keys = [];
	const original = Object.getOwnPropertyDescriptor(globalThis, "VideoEncoder");
	Object.defineProperty(globalThis, "VideoEncoder", {
		configurable: true,
		value: RecordingVideoEncoder,
		writable: true,
	});
	const videoFrame = Object.getOwnPropertyDescriptor(globalThis, "VideoFrame");
	Object.defineProperty(globalThis, "VideoFrame", { configurable: true, value: FakeVideoFrame, writable: true });

	const { Fanout } = await import("../fanout");
	let controller!: ReadableStreamDefaultController<VideoFrame>;
	const fanout = new Fanout(
		new ReadableStream<VideoFrame>({
			start: (c) => {
				controller = c;
			},
		}),
		{ clone: (frame) => frame.clone(), release: (frame) => frame.close() },
	);

	const capture = {
		in: { source: new Signal({ getSettings: () => ({ frameRate: 30 }), getConstraints: () => ({}) } as never) },
		out: { display: new Signal({ width: 640, height: 480 }), frames: new Signal(fanout) },
	};

	const send = async (millis: number) => {
		controller.enqueue(new Frame(millis * 1000) as unknown as VideoFrame);
		await clock.advance(PROBE);
	};

	return {
		capture,
		send,
		keys: RecordingVideoEncoder.keys,
		[Symbol.dispose]() {
			fanout.close();
			if (original) Object.defineProperty(globalThis, "VideoEncoder", original);
			else Reflect.deleteProperty(globalThis, "VideoEncoder");
			if (videoFrame) Object.defineProperty(globalThis, "VideoFrame", videoFrame);
			else Reflect.deleteProperty(globalThis, "VideoFrame");
		},
	};
}

async function recordKeyframes(
	info: Partial<Moq.Track.Info>,
	run: (ctx: {
		encoder: Encoder;
		track: Moq.Track.Producer;
		send: (millis: number) => Promise<void>;
	}) => Promise<void>,
): Promise<number[]> {
	using recording = await installRecording();

	const track = new Moq.Track.Producer("video").accept({ timescale: Moq.Time.Timescale.MICRO, ...info });
	const rendition = {
		config: new Signal(undefined),
		track: new Signal<Moq.Track.Producer | undefined>(track),
		close: () => {
			track.finish();
			track.close();
		},
	};
	const encoder = new Encoder("video", {
		enabled: true,
		broadcast: { video: () => rendition, order: new Order() } as never,
		capture: recording.capture as never,
	});

	try {
		await settle();
		await run({ encoder, track, send: recording.send });
		return recording.keys;
	} finally {
		encoder.close();
		track.finish();
		track.close();
	}
}

test("cut forces a keyframe on the next frame, coalescing requests without spacing them", async () => {
	const keys = await recordKeyframes({}, async ({ encoder, send }) => {
		// The opening keyframe serves a request made before it.
		encoder.cut();
		await send(0);
		await send(100);
		// Several requests before one frame produce one keyframe.
		encoder.cut();
		encoder.cut();
		await send(200);
		// The very next frame can be cut again.
		encoder.cut();
		await send(300);
		await send(400);
	});
	expect(keys).toEqual([0, 200, 300]);
});

test("a repeated capture timestamp is dropped rather than failing the track", async () => {
	let closed: unknown;
	const keys = await recordKeyframes({}, async ({ encoder, track, send }) => {
		await send(0);
		await send(0);
		encoder.cut();
		await send(100);
		closed = track.closed.peek();
	});
	expect(keys).toEqual([0, 100]);
	expect(closed).toBeUndefined();
});

test("a join over the refresh ratio forces a keyframe on the next frame", async () => {
	const keys = await recordKeyframes({ refresh: 2 }, async ({ track, send }) => {
		const watching = track.subscribe();
		const joined: Moq.Track.Subscriber[] = [];
		try {
			await send(0);
			await send(50);
			// One 60 byte delta is under twice the 100 byte keyframe.
			joined.push(track.subscribe());
			await send(100);
			await send(150);
			await send(200);
			// Four deltas are over it, 200ms after the keyframe.
			joined.push(track.subscribe());
			await send(250);
			// A join after the fresh keyframe reads its group.
			joined.push(track.subscribe());
			await send(300);
		} finally {
			watching.close();
			for (const subscriber of joined) subscriber.close();
		}
	});
	expect(keys).toEqual([0, 250]);
});

test("a join that a written group serves forces no keyframe, even one encoded without a subscriber", async () => {
	const keys = await recordKeyframes({ refresh: 2 }, async ({ encoder, track, send }) => {
		await send(0);
		encoder.cut();
		await send(100);
		// One 60 byte delta is under twice the 100 byte keyframe, so the join reads the open group.
		await send(150);
		const joined = track.subscribe();
		try {
			await send(200);
			await send(250);
		} finally {
			joined.close();
		}
	});
	expect(keys).toEqual([0, 100]);
});

// Groups encoded with nobody watching wait in the cache for a late subscriber's maxDelay, while on
// demand a subscriber finds only what its arrival started.
test.each([false, true])(
	"a late subscriber reaches groups encoded before it arrived unless on demand (%p)",
	async (onDemand) => {
		using recording = await installRecording();
		const broadcast = new Broadcast({
			origin: new Moq.Origin.Producer(),
			name: Moq.Path.from("eager.hang"),
			onDemand,
		});
		const encoder = new Encoder("video", { enabled: true, broadcast, capture: recording.capture as never });
		let late: Moq.Track.Subscriber | undefined;
		try {
			await settle();
			const net = broadcast.net.peek();
			if (!net) throw new Error("expected network broadcast");
			expect(encoder.out.active.peek()).toBe(!onDemand);

			await recording.send(0);
			encoder.cut();
			await recording.send(100);
			await recording.send(150);
			expect(recording.keys).toEqual(onDemand ? [] : [0, 100]);

			late = net.track("video").subscribe({ maxDelay: Milli(10_000) });
			await settle();
			expect(encoder.out.active.peek()).toBe(true);
			await recording.send(200);

			const ordered = late.ordered();
			const groups: number[] = [];
			const firsts: number[] = [];
			for (let i = 0; i < (onDemand ? 1 : 2); i++) {
				const group = await ordered.nextGroup();
				if (!group) throw new Error("expected a group");
				groups.push(group.sequence);
				const frame = await group.readFrame();
				if (!frame) throw new Error("expected a frame");
				firsts.push(Moq.Varint.decode(frame.payload)[0] / 1000);
			}
			if (onDemand) {
				// The subscriber's arrival started the encoder, so its first frame opened the only group.
				expect(recording.keys).toEqual([200]);
				expect(groups).toEqual([0]);
				expect(firsts).toEqual([200]);
			} else {
				// Both groups were cached before it arrived, and its join forced no fresh keyframe.
				expect(recording.keys).toEqual([0, 100]);
				expect(groups).toEqual([0, 1]);
				expect(firsts).toEqual([0, 100]);
			}
		} finally {
			late?.close();
			encoder.close();
			broadcast.close();
		}
	},
);

test("an unwatched rendition reserves nothing and encodes at its configured bitrate", async () => {
	using _videoEncoder = installFakeVideoEncoder();
	const broadcast = new Broadcast({ origin: new Moq.Origin.Producer(), name: Moq.Path.from("unwatched.hang") });
	const capture = {
		in: { source: new Signal({ getSettings: () => ({ frameRate: 30 }), getConstraints: () => ({}) } as never) },
		out: { display: new Signal({ width: 640, height: 480 }), frames: new Signal(undefined) },
	};
	const bandwidth = new Moq.Bandwidth.Allocator(new Signal<number | undefined>(200_000));
	const encoder = new Encoder("video", { enabled: true, broadcast, capture: capture as never, bandwidth });
	let subscriber: Moq.Track.Subscriber | undefined;
	try {
		await settle();
		const net = broadcast.net.peek();
		if (!net) throw new Error("expected network broadcast");
		expect(encoder.out.active.peek()).toBe(true);
		const configured = encoder.out.resolved.peek()?.bitrate;
		if (configured === undefined) throw new Error("expected a resolved config");
		expect(configured).toBeGreaterThan(200_000);

		subscriber = net.track("video").subscribe();
		await settle();
		expect(encoder.out.resolved.peek()?.bitrate).toBe(200_000);

		subscriber.close();
		subscriber = undefined;
		await settle();
		expect(encoder.out.active.peek()).toBe(true);
		expect(encoder.out.resolved.peek()?.bitrate).toBe(configured);
	} finally {
		subscriber?.close();
		encoder.close();
		bandwidth.close();
		broadcast.close();
	}
});

test("a failed video encoder disables only its rendition and recovers on a fresh track", async () => {
	using _videoEncoder = installFakeVideoEncoder();
	using _probe = spyOn(FakeVideoEncoder, "isConfigSupported").mockResolvedValue({ supported: true });
	using _now = spyOn(performance, "now").mockReturnValue(0);
	const flush = async () => {
		for (let i = 0; i < 10; i++) await new Promise<void>((resolve) => queueMicrotask(resolve));
	};
	const broadcast = new Broadcast({ origin: new Moq.Origin.Producer(), name: Moq.Path.from("failure.hang") });
	const capture = {
		in: { source: new Signal({ getSettings: () => ({ frameRate: 30 }), getConstraints: () => ({}) } as never) },
		out: { display: new Signal({ width: 640, height: 480 }), frames: new Signal(undefined) },
	};
	const estimate = new Signal<number | undefined>(10_000_000);
	const bandwidth = new Moq.Bandwidth.Allocator(estimate);
	const enabled = new Signal(true);
	const encoder = new Encoder("video/hd", { broadcast, capture: capture as never, bandwidth, enabled, config: {} });
	let subscriber: Moq.Track.Subscriber | undefined;
	let siblingSubscriber: Moq.Track.Subscriber | undefined;
	let catalogSubscriber: Moq.Track.Subscriber | undefined;
	let recovered: Moq.Track.Subscriber | undefined;
	let announced: Moq.Origin.Requesting | undefined;
	try {
		await encoder.out.catalog.changed();
		const net = broadcast.net.peek();
		if (!net) throw new Error("expected network broadcast");
		announced = broadcast.in.origin.peek()?.follow(broadcast.in.name.peek());
		const epoch = announced?.active.peek()?.epoch;
		const sibling = broadcast.video("video/sd");
		sibling.config.set({ codec: "avc1.640028", container: { kind: "legacy" } });
		await flush();
		siblingSubscriber = net.track("video/sd").subscribe();
		subscriber = net.track("video/hd").subscribe();
		// It encodes while enabled, so it was already active before anyone subscribed.
		expect(encoder.out.active.peek()).toBe(true);
		await flush();
		const siblingTrack = sibling.track.peek();
		if (!siblingTrack) throw new Error("expected demanded sibling track");
		expect(siblingTrack.appendGroup().sequence).toBe(0);
		catalogSubscriber = net.track(Broadcast.CATALOG_TRACK).subscribe();
		const catalog = new Json.Snapshot.Consumer<Catalog.Root>({ track: catalogSubscriber });
		expect((await catalog.latest())?.value.video?.renditions["video/hd"]?.enabled).not.toBe(false);
		const live = FakeVideoEncoder.instances.find((instance) => instance.state === "configured");
		if (!live) throw new Error("expected live video encoder");
		const failure = new Error("GPU encoder failed");
		live.fail(failure);
		await flush();
		expect(subscriber.closed.peek()).toBe(failure);
		expect(encoder.out.error.peek()).toBe(failure);
		expect(encoder.out.catalog.peek()?.enabled).toBe(false);
		expect((await catalog.latest())?.value.video?.renditions["video/hd"]?.enabled).toBe(false);
		expect(encoder.out.active.peek()).toBe(false);
		expect(broadcast.net.peek()).toBe(net);
		expect(announced?.active.peek()?.epoch).toBe(epoch);
		expect(siblingTrack.closed.peek()).toBeUndefined();
		expect(siblingTrack.appendGroup().sequence).toBe(1);
		const instances = FakeVideoEncoder.instances.length;
		broadcast.audio("audio/new");
		estimate.set(20_000_000);
		enabled.set(false);
		await flush();
		enabled.set(true);
		await flush();
		expect(encoder.out.error.peek()).toBe(failure);
		expect(encoder.out.catalog.peek()?.enabled).toBe(false);
		expect(FakeVideoEncoder.instances).toHaveLength(instances);
		expect(broadcast.net.peek()).toBe(net);

		// The same capture handed back, as Video.Capture does when its source is released and restored,
		// is a gap rather than a change.
		const source = capture.in.source.peek();
		capture.in.source.set(undefined as never);
		capture.out.display.set(undefined as never);
		await flush();
		expect(encoder.out.error.peek()).toBe(failure);
		capture.in.source.set(source);
		capture.out.display.set({ width: 640, height: 480 });
		await flush();
		expect(encoder.out.error.peek()).toBe(failure);
		expect(FakeVideoEncoder.instances).toHaveLength(instances);

		// Only a changed caller input re-arms it, under a fresh track name.
		// Mutated in place, so the settings object keeps its identity.
		encoder.config.mutate((config) => {
			if (config) config.maxPixels = 320 * 240;
		});
		await flush();
		expect(encoder.out.error.peek()).toBeUndefined();
		const config = (await catalog.latest())?.value.video?.renditions["video/hd"];
		if (!config?.track) throw new Error("expected a recovered track name");
		expect(config.track).not.toBe("video/hd");
		expect(config.enabled).not.toBe(false);
		recovered = net.track(config.track).subscribe();
		await flush();
		expect(encoder.out.active.peek()).toBe(true);
		expect(recovered.closed.peek()).toBeUndefined();
		expect(broadcast.net.peek()).toBe(net);
		expect(announced?.active.peek()?.epoch).toBe(epoch);
		expect(siblingTrack.appendGroup().sequence).toBe(2);
	} finally {
		subscriber?.close();
		siblingSubscriber?.close();
		catalogSubscriber?.close();
		recovered?.close();
		encoder.close();
		bandwidth.close();
		announced?.close();
		broadcast.close();
	}
});

// A track's coded size is a ceiling fixed for its name: a smaller capture resizes in band, opening a
// group whose keyframe carries the new size, and the catalog never hears about it.
test("a shrinking capture resizes in band below the advertised ceiling", async () => {
	const encoded: { width?: number; keyFrame: boolean }[] = [];
	class RecordingVideoEncoder {
		state: CodecState = "unconfigured";
		#output: VideoEncoderInit["output"];
		#config?: VideoEncoderConfig;

		constructor(init: VideoEncoderInit) {
			this.#output = init.output;
		}

		static async isConfigSupported(config: VideoEncoderConfig): Promise<{ supported: boolean }> {
			return { supported: config.codec.startsWith("avc1") };
		}

		configure(config: VideoEncoderConfig): void {
			this.state = "configured";
			this.#config = config;
		}

		// Records the test's frames only, not the probe's.
		encode(frame: VideoFrame, options?: VideoEncoderEncodeOptions): void {
			if (frame instanceof Frame) encoded.push({ width: this.#config?.width, keyFrame: !!options?.keyFrame });
		}

		// Only the probe flushes; report the configured codec, as Chrome does.
		async flush(): Promise<void> {
			const chunk = { type: "key", timestamp: 0, byteLength: 1, copyTo: () => {} };
			this.#output(chunk as never, { decoderConfig: { codec: this.#config?.codec } } as never);
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
	const videoFrame = Object.getOwnPropertyDescriptor(globalThis, "VideoFrame");
	Object.defineProperty(globalThis, "VideoFrame", { configurable: true, value: FakeVideoFrame, writable: true });

	class Frame {
		readonly timestamp: number;
		constructor(timestamp: number) {
			this.timestamp = timestamp;
		}
		clone(): Frame {
			return new Frame(this.timestamp);
		}
		close(): void {}
	}

	const { Fanout } = await import("../fanout");
	let controller!: ReadableStreamDefaultController<VideoFrame>;
	const fanout = new Fanout(
		new ReadableStream<VideoFrame>({
			start: (c) => {
				controller = c;
			},
		}),
		{ clone: (frame) => frame.clone(), release: (frame) => frame.close() },
	);

	const track = new Moq.Track.Producer("video").accept({ timescale: Moq.Time.Timescale.MILLI });
	const rendition = {
		config: new Signal(undefined),
		track: new Signal<Moq.Track.Producer | undefined>(track),
		close: () => {
			track.finish();
			track.close();
		},
	};
	const capture = {
		in: { source: new Signal({ getSettings: () => ({ frameRate: 30 }), getConstraints: () => ({}) } as never) },
		out: { display: new Signal({ width: 1280, height: 720 }), frames: new Signal(fanout) },
	};
	const encoder = new Encoder("video", {
		enabled: true,
		broadcast: { video: () => rendition, order: new Order() } as never,
		capture: capture as never,
	});
	const send = async (millis: number) => {
		controller.enqueue(new Frame(millis * 1000) as unknown as VideoFrame);
		await clock.advance(PROBE);
	};

	try {
		await settle();
		const before = encoder.out.catalog.peek();
		expect(before).toMatchObject({ codedWidth: 1280, codedHeight: 720 });
		await send(0);
		await send(100);

		capture.out.display.set({ width: 640, height: 360 });
		await settle();
		expect(encoder.out.resolved.peek()).toMatchObject({ width: 640, height: 352 });
		expect(encoder.out.catalog.peek()).toEqual(before);
		await send(200);
		await send(300);

		// Back up to the ceiling: still in band.
		capture.out.display.set({ width: 1280, height: 720 });
		await settle();
		await send(400);
		expect(encoder.out.catalog.peek()).toEqual(before);

		expect(encoded).toEqual([
			{ width: 1280, keyFrame: true },
			{ width: 1280, keyFrame: false },
			{ width: 640, keyFrame: true },
			{ width: 640, keyFrame: false },
			{ width: 1280, keyFrame: true },
		]);

		// Outgrowing the ceiling advertises a larger one.
		capture.out.display.set({ width: 1920, height: 1080 });
		await settle();
		expect(encoder.out.catalog.peek()).toMatchObject({ codedWidth: 1920, codedHeight: 1072 });
	} finally {
		encoder.close();
		fanout.close();
		track.finish();
		track.close();
		if (original) Object.defineProperty(globalThis, "VideoEncoder", original);
		else Reflect.deleteProperty(globalThis, "VideoEncoder");
		if (videoFrame) Object.defineProperty(globalThis, "VideoFrame", videoFrame);
		else Reflect.deleteProperty(globalThis, "VideoFrame");
	}
});

// `bitrate` and `framerate` are maxima, so neither a bandwidth grant nor a lowered knob reaches the
// catalog. Only a rise does.
test("the catalog bitrate and frame rate only rise", async () => {
	using _videoEncoder = installFakeVideoEncoder();
	const track = new Moq.Track.Producer("video").accept({ timescale: Moq.Time.Timescale.MILLI, priority: 60 });
	const sub = track.subscribe();
	const rendition = {
		config: new Signal(undefined),
		track: new Signal<Moq.Track.Producer | undefined>(track),
		close: () => {
			track.finish();
			track.close();
		},
	};
	const capture = {
		in: { source: new Signal({ getSettings: () => ({ frameRate: 30 }), getConstraints: () => ({}) }) },
		out: { display: new Signal({ width: 1280, height: 720 }), frames: new Signal(undefined) },
	};
	const estimate = new Signal<number | undefined>(100_000_000);
	const bandwidth = new Moq.Bandwidth.Allocator(estimate);
	const encoder = new Encoder("video", {
		enabled: true,
		broadcast: { video: () => rendition, order: new Order() } as never,
		capture: capture as never,
		bandwidth,
	});

	try {
		await settle();
		const cap = encoder.out.catalog.peek()?.bitrate;
		expect(cap).toBeGreaterThan(500_000);
		expect(encoder.out.catalog.peek()?.framerate).toBe(30);

		// A grant caps the encoder, not the catalog.
		estimate.set(500_000);
		await settle();
		expect(encoder.out.resolved.peek()?.bitrate).toBe(500_000);
		expect(encoder.out.catalog.peek()?.bitrate).toBe(cap);

		// Lowering the knobs retunes the encoder, not the catalog.
		estimate.set(100_000_000);
		encoder.config.set({ maxBitrate: 400_000, frameRate: 15 });
		await settle();
		expect(encoder.out.resolved.peek()).toMatchObject({ bitrate: 400_000, framerate: 15 });
		expect(encoder.out.catalog.peek()).toMatchObject({ bitrate: cap, framerate: 30 });

		// Raising them past the maxima raises the catalog.
		encoder.config.set({ frameRate: 60 });
		await settle();
		expect(encoder.out.catalog.peek()?.framerate).toBe(60);
		expect(encoder.out.catalog.peek()?.bitrate).toBeGreaterThan(cap ?? 0);
	} finally {
		encoder.close();
		bandwidth.close();
		sub.close();
	}
});

// Another broadcast is another track, so the ceiling and maxima start over there, while returning to a
// broadcast continues the track it kept.
test("each broadcast keeps its own ceiling and maxima", async () => {
	using _videoEncoder = installFakeVideoEncoder();
	const rendition = () => ({
		config: new Signal(undefined),
		track: new Signal<Moq.Track.Producer | undefined>(undefined),
		close: () => {},
	});
	const first = { video: rendition, order: new Order() };
	const broadcast = new Signal<unknown>(first);
	const capture = {
		in: { source: new Signal({ getSettings: () => ({ frameRate: 30 }), getConstraints: () => ({}) }) },
		out: { display: new Signal({ width: 1920, height: 1080 }), frames: new Signal(undefined) },
	};
	const enabled = new Signal(true);
	const encoder = new Encoder("video", {
		enabled,
		broadcast: broadcast as never,
		capture: capture as never,
		config: { frameRate: 60 },
	});

	try {
		await settle();
		encoder.config.set({ frameRate: 30 });
		capture.out.display.set({ width: 1280, height: 720 });
		await settle();
		expect(encoder.out.catalog.peek()).toMatchObject({ codedWidth: 1920, codedHeight: 1072, framerate: 60 });
		const bitrate = encoder.out.catalog.peek()?.bitrate ?? 0;

		broadcast.set({ video: rendition, order: new Order() });
		// The previous broadcast's probe never describes the new track, even while its own runs.
		for (let i = 0; i < 5; i++) await Promise.resolve();
		expect(encoder.out.catalog.peek()).toBeUndefined();
		await settle();
		expect(encoder.out.catalog.peek()).toMatchObject({ codedWidth: 1280, codedHeight: 720, framerate: 30 });
		expect(encoder.out.catalog.peek()?.bitrate).toBeLessThan(bitrate);

		// Returning to the first broadcast continues its track, so its ceiling and maxima return too.
		broadcast.set(first);
		await settle();
		expect(encoder.out.catalog.peek()).toMatchObject({
			codedWidth: 1920,
			codedHeight: 1072,
			framerate: 60,
			bitrate,
		});

		// A disabled rendition moved to another broadcast advertises nothing there, rather than the
		// last config of the track it left.
		enabled.set(false);
		await settle();
		expect(encoder.out.catalog.peek()?.enabled).toBe(false);
		broadcast.set({ video: rendition, order: new Order() });
		await settle();
		expect(encoder.out.catalog.peek()).toBeUndefined();
	} finally {
		encoder.close();
	}
});

// A raised knob outruns the probe that validated the live config, so until its re-probe lands the
// live bitrate stays within what the hardware accepted and the catalog advertises.
test("a pending re-probe never raises the live bitrate past the accepted probe", async () => {
	using _videoEncoder = installFakeVideoEncoder();
	const capture = {
		in: { source: new Signal({ getSettings: () => ({ frameRate: 30 }), getConstraints: () => ({}) }) },
		out: { display: new Signal({ width: 1280, height: 720 }) },
	};
	const encoder = new Encoder("video", { enabled: true, capture: capture as never });

	try {
		await settle();
		const accepted = encoder.out.resolved.peek()?.bitrate;
		expect(accepted).toBeGreaterThan(0);

		encoder.config.set({ frameRate: 60 });
		for (let i = 0; i < 5; i++) await Promise.resolve();
		expect(encoder.out.resolved.peek()?.bitrate).toBe(accepted);

		await settle();
		expect(encoder.out.resolved.peek()?.bitrate).toBeGreaterThan(accepted ?? 0);
	} finally {
		encoder.close();
	}
});

// Outgrowing the ceiling to a size the hardware refuses keeps the accepted ceiling rather than
// leaving a ceiling no probe can satisfy, so the rendition stays up and a shrink recovers in band.
test("an unsupported ceiling growth keeps the accepted ceiling", async () => {
	using _videoEncoder = installFakeVideoEncoder();
	const probe = spyOn(FakeVideoEncoder, "isConfigSupported").mockImplementation(async (config) => ({
		supported: config.codec.startsWith("avc1") && config.width <= 1280,
	}));
	const warn = spyOn(console, "warn").mockImplementation(() => {});
	const capture = {
		in: { source: new Signal({ getSettings: () => ({ frameRate: 30 }), getConstraints: () => ({}) }) },
		out: { display: new Signal({ width: 1280, height: 720 }) },
	};
	const encoder = new Encoder("video", { enabled: true, capture: capture as never });

	try {
		await settle();
		const before = encoder.out.catalog.peek();
		expect(before).toMatchObject({ codedWidth: 1280, codedHeight: 720 });

		capture.out.display.set({ width: 1920, height: 1080 });
		await settle();
		expect(warn).toHaveBeenCalled();
		expect(encoder.out.catalog.peek()).toEqual(before);
		expect(encoder.out.resolved.peek()).toMatchObject({ width: 1280, height: 704 });

		capture.out.display.set({ width: 1280, height: 720 });
		await settle();
		expect(encoder.out.catalog.peek()).toEqual(before);
		expect(encoder.out.resolved.peek()).toMatchObject({ width: 1280, height: 720 });
	} finally {
		encoder.close();
		probe.mockRestore();
		warn.mockRestore();
	}
});

// Outgrowing the ceiling while disabled behaves as it does while enabled: the unmute encodes scaled
// to the accepted ceiling instead of staying down.
test("an unsupported ceiling growth while disabled keeps the accepted ceiling", async () => {
	using _videoEncoder = installFakeVideoEncoder();
	const probe = spyOn(FakeVideoEncoder, "isConfigSupported").mockImplementation(async (config) => ({
		supported: config.codec.startsWith("avc1") && config.width <= 1280,
	}));
	const warn = spyOn(console, "warn").mockImplementation(() => {});
	const capture = {
		in: { source: new Signal({ getSettings: () => ({ frameRate: 30 }), getConstraints: () => ({}) }) },
		out: { display: new Signal({ width: 1280, height: 720 }) },
	};
	const enabled = new Signal(true);
	const encoder = new Encoder("video", { enabled, capture: capture as never });

	try {
		await settle();
		const before = encoder.out.catalog.peek();
		expect(before).toMatchObject({ codedWidth: 1280, codedHeight: 720 });

		enabled.set(false);
		capture.out.display.set({ width: 1920, height: 1080 });
		await settle();
		enabled.set(true);
		await settle();
		expect(encoder.out.catalog.peek()).toEqual(before);
		expect(encoder.out.resolved.peek()).toMatchObject({ width: 1280, height: 704 });
	} finally {
		encoder.close();
		probe.mockRestore();
		warn.mockRestore();
	}
});

// The scaled fallback only covers a refused size. A codec the hardware refuses still fails loud
// rather than leaving the old codec in place.
test("an unsupported codec fails even after a refused ceiling growth", async () => {
	using _videoEncoder = installFakeVideoEncoder();
	const probe = spyOn(FakeVideoEncoder, "isConfigSupported").mockImplementation(async (config) => ({
		supported: config.codec.startsWith("avc1") && config.width <= 1280,
	}));
	const warn = spyOn(console, "warn").mockImplementation(() => {});
	const error = spyOn(console, "error").mockImplementation(() => {});
	const capture = {
		in: { source: new Signal({ getSettings: () => ({ frameRate: 30 }), getConstraints: () => ({}) }) },
		out: { display: new Signal({ width: 1280, height: 720 }) },
	};
	const encoder = new Encoder("video", { enabled: true, capture: capture as never });

	try {
		await settle();
		capture.out.display.set({ width: 1920, height: 1080 });
		await settle();
		expect(encoder.out.resolved.peek()).toMatchObject({ width: 1280, height: 704 });

		encoder.config.set({ codec: "av01" });
		await settle();
		expect(encoder.out.catalog.peek()).toBeUndefined();
		expect(encoder.out.resolved.peek()).toBeUndefined();
	} finally {
		encoder.close();
		probe.mockRestore();
		warn.mockRestore();
		error.mockRestore();
	}
});

// A refused first size commits no ceiling, so a smaller one recovers the rendition.
test("a refused first ceiling recovers at a smaller size", async () => {
	using _videoEncoder = installFakeVideoEncoder();
	const probe = spyOn(FakeVideoEncoder, "isConfigSupported").mockImplementation(async (config) => ({
		supported: config.codec.startsWith("avc1") && config.width <= 1280,
	}));
	const error = spyOn(console, "error").mockImplementation(() => {});
	const capture = {
		in: { source: new Signal({ getSettings: () => ({ frameRate: 30 }), getConstraints: () => ({}) }) },
		out: { display: new Signal({ width: 1920, height: 1080 }) },
	};
	const encoder = new Encoder("video", { enabled: true, capture: capture as never });

	try {
		await settle();
		expect(encoder.out.catalog.peek()).toBeUndefined();
		expect(encoder.settled.peek()).toBe(true);

		encoder.config.set({ maxPixels: 1280 * 720 });
		await settle();
		expect(encoder.out.catalog.peek()).toMatchObject({ codedWidth: 1280, codedHeight: 720 });
	} finally {
		encoder.close();
		probe.mockRestore();
		error.mockRestore();
	}
});

// A decoder follows a resize in band, so outgrowing the advertised size re-probes for a codec that
// covers it and updates the served track in place while the codec string holds. A shrink stays in band.
test("ceiling growth updates the served track in place", async () => {
	using _videoEncoder = installFakeVideoEncoder();
	using _probe = spyOn(FakeVideoEncoder, "isConfigSupported").mockResolvedValue({ supported: true });
	using _now = spyOn(performance, "now").mockReturnValue(0);
	const broadcast = new Broadcast({ origin: new Moq.Origin.Producer(), name: Moq.Path.from("ceiling.hang") });
	const capture = {
		in: { source: new Signal({ getSettings: () => ({ frameRate: 30 }), getConstraints: () => ({}) }) },
		out: { display: new Signal({ width: 640, height: 480 }), frames: new Signal(undefined) },
	};
	const encoder = new Encoder("video", { broadcast, capture: capture as never, config: { frameRate: 60 } });
	const net = broadcast.net.peek() ?? (await broadcast.net.changed());
	if (!net) throw new Error("expected a network broadcast");
	const subscriber = net.track(Broadcast.CATALOG_TRACK).subscribe();
	const catalog = new Json.Snapshot.Consumer<Catalog.Root>({ track: subscriber });
	const next = async (done: (config: Catalog.VideoConfig) => boolean) => {
		for (;;) {
			const config = (await catalog.latest())?.value.video?.renditions.video;
			if (config && done(config)) return config;
		}
	};
	try {
		const initial = await next(() => true);
		expect(initial).toMatchObject({ codedWidth: 640, codedHeight: 480, framerate: 60 });
		expect(initial.track).toBeUndefined();

		capture.out.display.set({ width: 320, height: 240 });
		encoder.config.set({ frameRate: 15 });
		let live = encoder.out.resolved.peek();
		while (live?.width !== 320 || live.framerate !== 15) live = await encoder.out.resolved.changed();
		expect(encoder.out.catalog.peek()).toMatchObject({ codedWidth: 640, codedHeight: 480, framerate: 60 });

		capture.out.display.set({ width: 1280, height: 720 });
		const grown = await next((config) => config.codedWidth === 1280);
		expect(grown).toMatchObject({ codec: initial.codec, codedHeight: 720, framerate: 60 });
		expect(grown.track).toBeUndefined();
		expect(broadcast.net.peek()).toBe(net);
	} finally {
		subscriber.close();
		encoder.close();
		broadcast.close();
	}
});

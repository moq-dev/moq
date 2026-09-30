import { expect, spyOn, test } from "bun:test";
import * as Container from "@moq/hang/container";
import * as Moq from "@moq/net";
import { Signal } from "@moq/signals";
import { Baseline } from "../jitter";
import { Encoder } from "./encoder";

class FakeVideoEncoder {
	// How many times the hardware has been probed, so a test can assert it isn't re-probed.
	static probes = 0;

	// Every accepted probe, so a test can assert a published config was actually validated.
	static accepted: string[] = [];

	// The codec string the encoder reports in its output for a configured one. Echoes by default,
	// like Chrome and Firefox.
	static report: (codec: string) => string | undefined = (codec) => codec;

	state: CodecState = "unconfigured";
	#output: VideoEncoderInit["output"];
	#codec?: string;

	constructor(init: VideoEncoderInit) {
		this.#output = init.output;
	}

	static async isConfigSupported(config: VideoEncoderConfig): Promise<{ supported: boolean }> {
		FakeVideoEncoder.probes++;
		// Pretend the GPU takes a while, like a real probe under load.
		await new Promise((resolve) => setTimeout(resolve, 5));

		const supported = config.codec.startsWith("avc1");
		if (supported) FakeVideoEncoder.accepted.push(probeKey(config));
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

	const track = new Moq.Track.Producer("video/hd").accept();
	const rendition = {
		config: new Signal(undefined),
		track: new Signal<Moq.Track.Producer | undefined>(track),
		close: () => track.close(),
	};
	const broadcast = { video: () => rendition, baseline: new Baseline() };
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

test("a demand gap cuts the group and leaves the broadcast-owned track open for resume", async () => {
	using _videoEncoder = installFakeVideoEncoder();
	const cut = spyOn(Container.Legacy.Producer.prototype, "cut");

	const track = new Moq.Track.Producer("video").accept({ priority: 60 });
	const live = new Signal<Moq.Track.Producer | undefined>(track);
	const rendition = {
		config: new Signal(undefined),
		track: live,
		close: () => track.close(),
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
		broadcast: { video: () => rendition, baseline: new Baseline() } as never,
		capture: capture as never,
	});

	try {
		await settle();
		live.set(undefined);
		await settle();
		expect(track.closed.peek()).toBeUndefined();
		expect(cut).toHaveBeenCalledTimes(1);

		live.set(track);
		await settle();
		expect(track.closed.peek()).toBeUndefined();

		cut.mockClear();
		track.close();
		encoder.close();
		expect(cut).not.toHaveBeenCalled();
	} finally {
		encoder.close();
		track.close();
		cut.mockRestore();
	}
});

// A bandwidth sample used to rerun the whole resolve effect, which blanked the resolved config (and
// with it the catalog entry) and re-probed the hardware for a codec. A subscriber returning during
// that window got a VideoEncoder that was never configured, so every captured frame was dropped.
// https://github.com/moq-dev/moq/issues/2635
test("a bandwidth estimate updates the bitrate without blanking the config or re-probing", async () => {
	using _videoEncoder = installFakeVideoEncoder();

	const track = new Moq.Track.Producer("video").accept({ priority: 60 });
	const sub = track.subscribe();
	const rendition = {
		config: new Signal(undefined),
		track: new Signal<Moq.Track.Producer | undefined>(track),
		close: () => track.close(),
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
		broadcast: { video: () => rendition, baseline: new Baseline() } as never,
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

async function settle(): Promise<void> {
	await new Promise((resolve) => setTimeout(resolve, 200));
}

// The codec probe only speaks for the dimensions and codec filter it ran against. Those live in
// separate effects, which observe a change in whatever order they happen to be subscribed in, so
// the resolved config must never pair a fresh dimension with a stale probe.
// https://github.com/moq-dev/moq/issues/2635
test("every published config was probed for its own codec and dimensions", async () => {
	using _videoEncoder = installFakeVideoEncoder();
	FakeVideoEncoder.accepted = [];

	const track = new Moq.Track.Producer("video").accept({ priority: 60 });
	const sub = track.subscribe();
	const rendition = {
		config: new Signal(undefined),
		track: new Signal<Moq.Track.Producer | undefined>(track),
		close: () => track.close(),
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
		broadcast: { video: () => rendition, baseline: new Baseline() } as never,
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
		if (!FakeVideoEncoder.accepted.includes(key)) violations.push(key);
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

test.each(["encoder lag", "quiet startup"])("marks a rendition stalled for %s", async (reason) => {
	const clock = spyOn(performance, "now").mockReturnValue(0);
	// The fake holds every live output, like a throttled encoder that never hands frames to the session.
	using _videoEncoder = installFakeVideoEncoder();

	class Frame {
		codedWidth = 640;
		codedHeight = 480;
		timestamp: number;
		closed = false;
		constructor(timestamp: number) {
			this.timestamp = timestamp;
		}
		clone(): Frame {
			return new Frame(this.timestamp);
		}
		close(): void {
			this.closed = true;
		}
	}

	const { Fanout } = await import("../fanout");
	let controller!: ReadableStreamDefaultController<VideoFrame>;
	const stream = new ReadableStream<VideoFrame>({
		start: (c) => {
			controller = c;
		},
	});
	const fanout = new Fanout(stream, {
		clone: (frame) => frame.clone(),
		release: (frame) => frame.close(),
	});

	const track = new Moq.Track.Producer("video/hd").accept();
	const rendition = {
		config: new Signal(undefined),
		track: new Signal<Moq.Track.Producer | undefined>(track),
		close: () => track.close(),
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
			frames: new Signal(fanout),
		},
	};
	const encoder = new Encoder("video/hd", {
		enabled: true,
		broadcast: { video: () => rendition, baseline: new Baseline() } as never,
		capture: capture as never,
	});

	try {
		await settle();
		expect(encoder.out.catalog.peek()?.stalled).toBeUndefined();

		if (reason === "encoder lag") {
			// Five frames at 30fps exceed three frame intervals of unaccepted capture.
			for (let i = 0; i < 5; i++) {
				controller.enqueue(new Frame(i * 33_333) as unknown as VideoFrame);
				await new Promise((resolve) => setTimeout(resolve, 10));
			}
		} else {
			clock.mockReturnValue(200);
			await new Promise((resolve) => setTimeout(resolve, 60));
		}
		await settle();
		expect(encoder.out.catalog.peek()?.stalled).toBe(true);
	} finally {
		encoder.close();
		fanout.close();
		clock.mockRestore();
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

	const track = new Moq.Track.Producer("video").accept({ priority: 60 });
	const sub = track.subscribe();
	const rendition = {
		config: new Signal(undefined),
		track: new Signal<Moq.Track.Producer | undefined>(track),
		close: () => track.close(),
	};
	const capture = {
		in: { source: new Signal({ getSettings: () => ({ frameRate: 30 }), getConstraints: () => ({}) }) },
		out: { display: new Signal({ width: 1280, height: 720 }), frames: new Signal(undefined) },
	};
	const estimate = new Signal<number | undefined>(100_000_000);
	const bandwidth = new Moq.Bandwidth.Allocator(estimate);
	const encoder = new Encoder("video", {
		enabled: true,
		broadcast: { video: () => rendition, baseline: new Baseline() } as never,
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

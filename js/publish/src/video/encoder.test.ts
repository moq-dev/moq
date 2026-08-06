import { expect, spyOn, test } from "bun:test";
import * as Moq from "@moq/net";
import { Signal } from "@moq/signals";
import { Encoder } from "./encoder";

class FakeVideoEncoder {
	// How many times the hardware has been probed, so a test can assert it isn't re-probed.
	static probes = 0;

	// Every accepted probe, so a test can assert a published config was actually validated.
	static accepted: string[] = [];

	state: CodecState = "unconfigured";

	static async isConfigSupported(config: VideoEncoderConfig): Promise<{ supported: boolean }> {
		FakeVideoEncoder.probes++;
		// Pretend the GPU takes a while, like a real probe under load.
		await new Promise((resolve) => setTimeout(resolve, 5));

		const supported = config.codec.startsWith("avc1");
		if (supported) FakeVideoEncoder.accepted.push(probeKey(config));
		return { supported };
	}

	configure(): void {
		this.state = "configured";
	}

	encode(): void {}

	close(): void {
		this.state = "closed";
	}
}

function installFakeVideoEncoder() {
	const original = Object.getOwnPropertyDescriptor(globalThis, "VideoEncoder");
	Object.defineProperty(globalThis, "VideoEncoder", {
		configurable: true,
		value: FakeVideoEncoder,
		writable: true,
	});

	return {
		[Symbol.dispose]() {
			if (original) {
				Object.defineProperty(globalThis, "VideoEncoder", original);
			} else {
				Reflect.deleteProperty(globalThis, "VideoEncoder");
			}
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
	const broadcast = { video: () => rendition };
	const capture = {
		in: { source: new Signal(undefined) },
		out: { frame: new Signal<VideoFrame | undefined>(undefined) },
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

// A bandwidth sample used to rerun the whole resolve effect, which blanked the resolved config (and
// with it the catalog entry) and re-probed the hardware for a codec. A subscriber returning during
// that window got a VideoEncoder that was never configured, so every captured frame was dropped.
// https://github.com/moq-dev/moq/issues/2635
test("a bandwidth estimate updates the bitrate without blanking the config or re-probing", async () => {
	using _videoEncoder = installFakeVideoEncoder();

	const track = new Moq.Track.Producer("video").accept();
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
			frame: new Signal<VideoFrame | undefined>({ codedWidth: 640, codedHeight: 480 } as never),
		},
	};
	const bandwidth = new Signal<number | undefined>(10_000_000);

	const encoder = new Encoder("video", {
		enabled: true,
		broadcast: { video: () => rendition } as never,
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

		// A poll lands with an estimate low enough to cap the bitrate.
		bandwidth.set(200_000);
		await settle();

		// The cap applied, and nothing went blank on the way there.
		expect(encoder.out.resolved.peek()?.bitrate).toBe(180_000);
		expect(encoder.out.resolved.peek()?.codec).toBe(resolved?.codec);
		expect(encoder.out.catalog.peek()).toBeDefined();

		// The codec doesn't depend on bandwidth, so the hardware is not asked again.
		expect(FakeVideoEncoder.probes).toBe(probes);

		// Repeated samples, as the 100ms poll delivers. The config stays live throughout.
		for (let i = 0; i < 20; i++) {
			bandwidth.set(1_000_000 + i * 13_000);
			await Promise.resolve();
			await Promise.resolve();
			expect(encoder.out.resolved.peek()).toBeDefined();
			expect(encoder.out.catalog.peek()).toBeDefined();
		}

		expect(FakeVideoEncoder.probes).toBe(probes);
	} finally {
		encoder.close();
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

	const track = new Moq.Track.Producer("video").accept();
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
			frame: new Signal<VideoFrame | undefined>({ codedWidth: 1280, codedHeight: 720 } as never),
		},
	};
	const bandwidth = new Signal<number | undefined>(10_000_000);

	const encoder = new Encoder("video", {
		enabled: true,
		broadcast: { video: () => rendition } as never,
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
		// element polls the estimate every 100ms. The resize schedules the dimensions effect and
		// the sample schedules the resolve effect, so the resolve effect runs with the new
		// dimensions already written while the probe still holds the result for the old ones.
		capture.out.frame.set({ codedWidth: 640, codedHeight: 480 } as never);
		bandwidth.set(3_000_000);
		await settle();

		expect(published.length).toBeGreaterThan(1);
		expect(violations).toEqual([]);
	} finally {
		unsubscribe();
		encoder.close();
	}
});

function probeKey(config: { codec: string; width: number; height: number }): string {
	return `${config.codec}@${config.width}x${config.height}`;
}

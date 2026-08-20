import { describe, expect, it } from "bun:test";
import * as Catalog from "@moq/hang/catalog";
import { Path } from "@moq/net";
import { Signal } from "@moq/signals";
import { Broadcast } from "../broadcast";
import { decoderConfigKey, supportCacheKey } from "./config";
import { Source } from "./source";

const flush = () => new Promise((resolve) => setTimeout(resolve, 0));

async function settle(): Promise<void> {
	for (let i = 0; i < 5; i++) await flush();
}

function config(codec: string, fields: Record<string, unknown> = {}): Catalog.VideoConfig {
	return Catalog.VideoConfigSchema.parse({ codec, container: { kind: "legacy" }, ...fields });
}

function mockBroadcast(renditions: Record<string, Catalog.VideoConfig>): Broadcast {
	return {
		in: {
			connection: new Signal(undefined),
		},
		out: {
			catalog: new Signal({ video: { renditions } }),
		},
	} as unknown as Broadcast;
}

function mutableBroadcast(renditions: Record<string, Catalog.VideoConfig>): {
	broadcast: Broadcast;
	catalog: Signal<Catalog.Root>;
} {
	const catalog = new Signal<Catalog.Root>({ video: { renditions } });
	return {
		broadcast: {
			in: { connection: new Signal(undefined) },
			out: { catalog },
		} as unknown as Broadcast,
		catalog,
	};
}

async function withoutWarnings(fn: () => Promise<void>): Promise<void> {
	const warn = console.warn;
	console.warn = () => {};
	try {
		await fn();
	} finally {
		console.warn = warn;
	}
}

describe("Source error signal", () => {
	it("is unsupported when the catalog has video renditions but none are supported", async () => {
		await withoutWarnings(async () => {
			const source = new Source({
				broadcast: mockBroadcast({ hd: config("hev1.1.6.L120.90") }),
				supported: async () => false,
			});

			await settle();
			expect(source.out.error.peek()).toBe("unsupported");
			expect(source.out.available.peek()).toEqual({});

			source.close();
		});
	});

	it("treats a support probe throw as unsupported without aborting the remaining renditions", async () => {
		await withoutWarnings(async () => {
			const source = new Source({
				broadcast: mockBroadcast({
					bad: config("not-a-codec"),
					good: config("avc1.640028"),
				}),
				supported: async (rendition) => {
					if (rendition.codec === "not-a-codec") throw new Error("probe failed");
					return true;
				},
			});

			await settle();
			expect(source.out.error.peek()).toBeUndefined();
			expect(Object.keys(source.out.available.peek())).toEqual(["good"]);

			source.close();
		});
	});

	it("does not wedge the effect when a support probe never settles", async () => {
		// `supported` is consumer-supplied, and a rerun waits for the tasks it spawned, so a probe
		// that never settles would hold the next run shut for good unless it races the teardown.
		await withoutWarnings(async () => {
			const supported = new Signal<(config: Catalog.VideoConfig) => Promise<boolean>>(
				() => new Promise<boolean>(() => {}), // never settles
			);

			const source = new Source({
				broadcast: mockBroadcast({ hd: config("avc1.640028") }),
				supported,
			});

			await settle();
			expect(source.out.available.peek()).toEqual({});

			// Swapping the probe reruns the effect, which has to tear the parked one down first.
			// If it cannot, this rerun never opens and the new probe never runs.
			supported.set(async () => true);
			await settle();

			expect(Object.keys(source.out.available.peek())).toEqual(["hd"]);

			source.close();
		});
	});

	it("is undefined when the catalog has no video renditions", async () => {
		const source = new Source({
			broadcast: mockBroadcast({}),
			supported: async () => false,
		});

		await settle();
		expect(source.out.error.peek()).toBeUndefined();
		expect(source.out.available.peek()).toEqual({});

		source.close();
	});

	// The catalog goes as a whole rather than losing the one rendition, so a valid sibling
	// is no rescue: the reference names content outside the consumer's authorized subtree,
	// and selecting the fallback would leave a publisher bug to surface later as a track
	// that never fills.
	it("selects nothing when an escaping rendition rejects the catalog", async () => {
		const invalidVideo = { ...config("avc1.640028"), broadcast: "../../source" };
		const validVideo = { ...config("avc1.640028"), broadcast: "./source" };
		const audioConfig = Catalog.AudioConfigSchema.parse({
			codec: "opus",
			container: { kind: "legacy" },
			sampleRate: 48_000,
			numberOfChannels: 2,
		});
		const broadcast = new Broadcast({
			enabled: true,
			name: Path.from("room/catalog.hang"),
			catalogFormat: "manual",
			catalog: {
				video: { renditions: { invalid: invalidVideo, fallback: validVideo } },
				audio: {
					renditions: {
						invalid: { ...audioConfig, broadcast: "../../source" },
						fallback: { ...audioConfig, broadcast: "./source" },
					},
				},
			},
		});
		const source = new Source({ broadcast, supported: async () => true });

		const error = console.error;
		console.error = () => {};
		try {
			await settle();
			expect(broadcast.out.catalog.peek()).toBeUndefined();
			expect(Object.keys(source.out.available.peek())).toEqual([]);
			expect(source.out.track.peek()).toBeUndefined();
		} finally {
			console.error = error;
		}

		source.close();
		broadcast.close();
	});
});

describe("Source stalled rendition selection", () => {
	it("skips a stalled manual target while an unstalled rendition exists", async () => {
		const source = new Source({
			broadcast: mockBroadcast({
				low: config("avc1.64001e", { bitrate: 1_000_000 }),
				high: config("avc1.640028", { bitrate: 2_000_000, stalled: true }),
			}),
			target: { name: "high" },
			supported: async () => true,
		});

		await settle();
		expect(source.out.track.peek()).toBe("low");
		expect(Object.keys(source.out.available.peek())).toEqual(["low", "high"]);
		source.close();
	});

	it("selects the lowest rendition when every supported rendition is stalled", async () => {
		const source = new Source({
			broadcast: mockBroadcast({
				high: config("avc1.640028", { bitrate: 2_000_000, stalled: true }),
				low: config("avc1.64001e", { bitrate: 1_000_000, stalled: true }),
			}),
			supported: async () => true,
		});

		await settle();
		expect(source.out.track.peek()).toBe("low");
		source.close();
	});

	it("uses coded dimensions when stalled renditions omit bitrate", async () => {
		const source = new Source({
			broadcast: mockBroadcast({
				high: config("avc1.640028", { codedWidth: 1920, codedHeight: 1080, stalled: true }),
				low: config("avc1.64001e", { codedWidth: 854, codedHeight: 480, stalled: true }),
			}),
			supported: async () => true,
		});

		await settle();
		expect(source.out.track.peek()).toBe("low");
		source.close();
	});

	it("does not repeat support probes for metadata-only changes", async () => {
		const state = mutableBroadcast({ high: config("avc1.640028", { bitrate: 2_000_000 }) });
		let probes = 0;
		const supported = async () => {
			probes++;
			return true;
		};
		Object.assign(supported, { [supportCacheKey]: decoderConfigKey });
		const source = new Source({
			broadcast: state.broadcast,
			supported,
		});

		await settle();
		expect(probes).toBe(1);

		state.catalog.set({
			video: {
				renditions: {
					high: config("avc1.640028", {
						bitrate: 1_500_000,
						stalled: true,
						codedWidth: 1280,
						codedHeight: 720,
					}),
				},
			},
		});
		await settle();

		expect(probes).toBe(1);
		expect(Number(source.out.config.peek()?.bitrate)).toBe(1_500_000);
		expect(source.out.config.peek()?.stalled).toBe(true);
		source.close();
	});

	it("repeats custom support probes when metadata changes", async () =>
		withoutWarnings(async () => {
			const state = mutableBroadcast({ high: config("avc1.640028", { codedWidth: 1280 }) });
			let probes = 0;
			const source = new Source({
				broadcast: state.broadcast,
				supported: async (rendition) => {
					probes++;
					return (rendition.codedWidth ?? 0) <= 1920;
				},
			});

			await settle();
			expect(probes).toBe(1);
			expect(Object.keys(source.out.available.peek())).toEqual(["high"]);

			state.catalog.set({
				video: { renditions: { high: config("avc1.640028", { codedWidth: 3840 }) } },
			});
			await settle();

			expect(probes).toBe(2);
			expect(source.out.available.peek()).toEqual({});
			source.close();
		}));
});

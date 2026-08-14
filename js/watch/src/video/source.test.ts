import { describe, expect, it } from "bun:test";
import * as Catalog from "@moq/hang/catalog";
import { Path } from "@moq/net";
import { Signal } from "@moq/signals";
import { Broadcast } from "../broadcast";
import { Source } from "./source";

const flush = () => new Promise((resolve) => setTimeout(resolve, 0));

async function settle(): Promise<void> {
	for (let i = 0; i < 5; i++) await flush();
}

function config(codec: string): Catalog.VideoConfig {
	return { codec, container: { kind: "legacy" } };
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

	it("ignores escaping renditions before selecting a valid fallback", async () => {
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

		await settle();
		expect(Object.keys(broadcast.out.catalog.peek()?.video?.renditions ?? {})).toEqual(["fallback"]);
		expect(Object.keys(broadcast.out.catalog.peek()?.audio?.renditions ?? {})).toEqual(["fallback"]);
		expect(Object.keys(source.out.available.peek())).toEqual(["fallback"]);
		expect(source.out.track.peek()).toBe("fallback");

		source.close();
		broadcast.close();
	});
});

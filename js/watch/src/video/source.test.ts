import { afterEach, beforeEach, describe, expect, it } from "bun:test";
import { yieldNow } from "@moq/clock";
import * as Catalog from "@moq/hang/catalog";
import { Path } from "@moq/net";
import { Signal } from "@moq/signals";
import { Broadcast } from "../broadcast";
import { stubSupport } from "../webcodecs.test-util";
import { Source, type Target } from "./source";

// What the stubbed WebCodecs decoder says about each config the source probes: every rendition plays
// unless a test says otherwise.
let answer: (config: VideoDecoderConfig) => boolean | Promise<boolean>;
let decoder: Disposable;
beforeEach(() => {
	answer = () => true;
	decoder = stubSupport({ video: (config) => answer(config) });
});
afterEach(() => decoder[Symbol.dispose]());

const flush = () => yieldNow();

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
			answer = () => false;
			const source = new Source({
				broadcast: mockBroadcast({ hd: config("hev1.1.6.L120.90") }),
			});

			await settle();
			expect(source.out.error.peek()?.name).toBe("NotSupportedError");
			expect(source.out.available.peek()).toEqual({});

			source.close();
		});
	});

	it("treats a support probe throw as unsupported without aborting the remaining renditions", async () => {
		await withoutWarnings(async () => {
			answer = (probed) => {
				if (probed.codec === "not-a-codec") throw new Error("probe failed");
				return true;
			};
			const source = new Source({
				broadcast: mockBroadcast({
					bad: config("not-a-codec"),
					good: config("avc1.640028"),
				}),
			});

			await settle();
			expect(source.out.error.peek()).toBeUndefined();
			expect(Object.keys(source.out.available.peek())).toEqual(["good"]);

			source.close();
		});
	});

	it("does not wedge the effect when a support probe never settles", async () => {
		// A rerun waits for the tasks it spawned, so a probe that never settles would hold the next
		// run shut for good unless it races the teardown.
		await withoutWarnings(async () => {
			answer = () => new Promise<boolean>(() => {}); // never settles
			const state = mutableBroadcast({ hd: config("avc1.640028") });
			const source = new Source({ broadcast: state.broadcast });

			await settle();
			expect(source.out.available.peek()).toEqual({});

			// A catalog update reruns the effect, which has to tear the parked probe down first.
			// If it cannot, this rerun never opens and the new probe never runs.
			answer = () => true;
			state.catalog.set({ video: { renditions: { hd: config("avc1.64001f") } } });
			await settle();

			expect(Object.keys(source.out.available.peek())).toEqual(["hd"]);

			source.close();
		});
	});

	it("is undefined when the catalog has no video renditions", async () => {
		answer = () => false;
		const source = new Source({
			broadcast: mockBroadcast({}),
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
		const invalidVideo = { ...config("avc1.640028"), broadcast: Path.normalizeRelative("../../source") };
		const validVideo = { ...config("avc1.640028"), broadcast: Path.normalizeRelative("./source") };
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
						invalid: { ...audioConfig, broadcast: Path.normalizeRelative("../../source") },
						fallback: { ...audioConfig, broadcast: Path.normalizeRelative("./source") },
					},
				},
			},
		});
		const source = new Source({ broadcast });

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

describe("Source disabled rendition selection", () => {
	it("moves to an enabled rendition when the selected one is disabled, and back", async () => {
		const state = mutableBroadcast({
			low: config("avc1.64001e", { bitrate: 1_000_000 }),
			high: config("avc1.640028", { bitrate: 2_000_000 }),
		});
		const source = new Source({
			broadcast: state.broadcast,
		});

		await settle();
		expect(source.out.track.peek()).toBe("high");

		state.catalog.set({
			video: {
				renditions: {
					low: config("avc1.64001e", { bitrate: 1_000_000 }),
					high: config("avc1.640028", { bitrate: 2_000_000, enabled: false }),
				},
			},
		});
		await settle();
		expect(source.out.track.peek()).toBe("low");

		state.catalog.set({
			video: {
				renditions: {
					low: config("avc1.64001e", { bitrate: 1_000_000 }),
					high: config("avc1.640028", { bitrate: 2_000_000 }),
				},
			},
		});
		await settle();
		expect(source.out.track.peek()).toBe("high");
		source.close();
	});

	it("never selects a disabled manual target", async () => {
		const source = new Source({
			broadcast: mockBroadcast({
				low: config("avc1.64001e", { bitrate: 1_000_000 }),
				high: config("avc1.640028", { bitrate: 2_000_000, enabled: false }),
			}),
			target: { name: "high" },
		});

		await settle();
		expect(source.out.track.peek()).toBe("low");
		source.close();
	});

	it("selects nothing when every rendition is disabled", async () => {
		const source = new Source({
			broadcast: mockBroadcast({
				high: config("avc1.640028", { bitrate: 2_000_000, enabled: false }),
				low: config("avc1.64001e", { bitrate: 1_000_000, enabled: false }),
			}),
		});

		await settle();
		expect(source.out.track.peek()).toBeUndefined();
		expect(source.out.config.peek()).toBeUndefined();
		source.close();
	});

	it("deselects the selected rendition once every rendition is disabled", async () => {
		const state = mutableBroadcast({ high: config("avc1.640028", { bitrate: 2_000_000 }) });
		const source = new Source({ broadcast: state.broadcast });

		await settle();
		expect(source.out.track.peek()).toBe("high");

		state.catalog.set({
			video: { renditions: { high: config("avc1.640028", { bitrate: 2_000_000, enabled: false }) } },
		});
		await settle();
		expect(source.out.track.peek()).toBeUndefined();
		expect(source.out.config.peek()).toBeUndefined();
		source.close();
	});

	it("ignores a legacy stalled flag", async () => {
		const source = new Source({
			broadcast: mockBroadcast({
				low: config("avc1.64001e", { bitrate: 1_000_000 }),
				high: config("avc1.640028", { bitrate: 2_000_000, stalled: true }),
			}),
		});

		await settle();
		expect(source.out.track.peek()).toBe("high");
		source.close();
	});

	it("does not repeat support probes for metadata-only changes", async () => {
		const state = mutableBroadcast({ high: config("avc1.640028", { bitrate: 2_000_000 }) });
		let probes = 0;
		answer = () => {
			probes++;
			return true;
		};
		const source = new Source({
			broadcast: state.broadcast,
		});

		await settle();
		expect(probes).toBe(1);

		state.catalog.set({
			video: {
				renditions: {
					high: config("avc1.640028", {
						bitrate: 1_500_000,
						label: "Main camera",
						codedWidth: 1280,
						codedHeight: 720,
					}),
				},
			},
		});
		await settle();

		expect(probes).toBe(1);
		expect(Number(source.out.config.peek()?.bitrate)).toBe(1_500_000);
		expect(source.out.config.peek()?.label).toBe("Main camera");
		source.close();
	});

	it("repeats the support probe when a decoder field changes", async () =>
		withoutWarnings(async () => {
			const state = mutableBroadcast({ high: config("avc1.640028") });
			let probes = 0;
			answer = (probed) => {
				probes++;
				return probed.codec.startsWith("avc1");
			};
			const source = new Source({ broadcast: state.broadcast });

			await settle();
			expect(probes).toBe(1);
			expect(Object.keys(source.out.available.peek())).toEqual(["high"]);

			state.catalog.set({ video: { renditions: { high: config("hvc1.1.6.L93.B0") } } });
			await settle();

			expect(probes).toBe(2);
			expect(source.out.available.peek()).toEqual({});
			source.close();
		}));
});

describe("Source rendition rank", () => {
	it("breaks an exact tie by name, not catalog order", async () => {
		const source = new Source({
			broadcast: mockBroadcast({
				e: config("avc1.640028", { codedWidth: 1280, codedHeight: 720, bitrate: 3_000_000 }),
				d: config("avc1.64001e", { codedWidth: 1280, codedHeight: 720, bitrate: 3_000_000 }),
			}),
		});

		await settle();
		expect(source.out.track.peek()).toBe("d");
		source.close();
	});

	it("ranks a known zero bitrate above an unknown one", async () => {
		const source = new Source({
			broadcast: mockBroadcast({
				unknown: config("avc1.640028", { codedWidth: 1280, codedHeight: 720 }),
				zero: config("avc1.64001e", { codedWidth: 1280, codedHeight: 720, bitrate: 0 }),
			}),
		});

		await settle();
		expect(source.out.track.peek()).toBe("zero");
		source.close();
	});

	it("prefers a known picture over a higher bitrate with no size", async () => {
		const source = new Source({
			broadcast: mockBroadcast({
				a: config("avc1.640028", { bitrate: 9_000_000 }),
				b: config("avc1.64001e", { codedWidth: 640, codedHeight: 360, bitrate: 1_000_000 }),
			}),
		});

		await settle();
		expect(source.out.track.peek()).toBe("b");
		source.close();
	});
});

describe("Source rendition preference", () => {
	// An H.265 source beside its own H.264 compatibility transcode.
	const renditions = () => ({
		source: config("hvc1.1.6.L93.B0", { codedWidth: 1280, codedHeight: 720, bitrate: 2_000_000 }),
		fallback: config("avc1.64001f", { codedWidth: 1280, codedHeight: 720, bitrate: 4_000_000, preference: -1 }),
	});
	const decodes = (prefix: string) => (config: VideoDecoderConfig) => !config.codec.startsWith(prefix);

	it("selects a supported source over a lower preference with a higher bitrate", async () => {
		const source = new Source({ broadcast: mockBroadcast(renditions()) });

		await settle();
		expect(source.out.track.peek()).toBe("source");
		source.close();
	});

	it("selects a supported source over a lower preference that fits the bitrate budget", async () => {
		const source = new Source({
			broadcast: mockBroadcast({
				source: config("hvc1.1.6.L93.B0", { bitrate: 2_000_000 }),
				fallback: config("avc1.64001f", { bitrate: 1_000_000, preference: -1 }),
			}),
			target: { bitrate: 1_500_000 },
		});

		await settle();
		expect(source.out.track.peek()).toBe("source");
		source.close();
	});

	it("selects a lower preference when the source is unsupported", async () => {
		answer = decodes("hvc1");
		const source = new Source({ broadcast: mockBroadcast(renditions()) });

		await settle();
		expect(source.out.track.peek()).toBe("fallback");
		source.close();
	});

	it("resolves three tiers to the highest supported one", async () => {
		const broadcast = mockBroadcast({
			av1: config("av01.0.08M.08", { codedWidth: 1280, codedHeight: 720, preference: 1 }),
			...renditions(),
		});
		const all = new Source({ broadcast });
		await settle();
		expect(all.out.track.peek()).toBe("av1");
		all.close();

		answer = decodes("av01");
		const noAv1 = new Source({ broadcast });
		await settle();
		expect(noAv1.out.track.peek()).toBe("source");
		noAv1.close();
	});

	it("lets a manual target select a lower preference", async () => {
		const source = new Source({
			broadcast: mockBroadcast(renditions()),
			target: { name: "fallback" },
		});

		await settle();
		expect(source.out.track.peek()).toBe("fallback");
		source.close();
	});

	it("steps down a transcoded ladder that shares its source's preference", async () => {
		const source = new Source({
			broadcast: mockBroadcast({
				video: config("hvc1.1.6.L93.B0", {
					codedWidth: 1920,
					codedHeight: 1080,
					bitrate: 5_000_000,
					preference: 1,
				}),
				"video/360p": config("avc1.64001e", {
					codedWidth: 640,
					codedHeight: 360,
					bitrate: 600_000,
					preference: 1,
				}),
				other: config("avc1.640028", { codedWidth: 1280, codedHeight: 720, bitrate: 900_000 }),
			}),
			target: { bitrate: 1_000_000 },
		});

		await settle();
		expect(source.out.track.peek()).toBe("video/360p");
		source.close();
	});
});

describe("Source rendition orientation", () => {
	// A landscape program ranked first, beside a lower-ranked portrait one, each with a ladder.
	const renditions = () => ({
		wide: config("avc1.640028", { codedWidth: 1920, codedHeight: 1080, bitrate: 5_000_000 }),
		"wide/360p": config("avc1.64001e", { codedWidth: 640, codedHeight: 360, bitrate: 600_000 }),
		tall: config("vp09.00.10.08", { codedWidth: 1080, codedHeight: 1920, bitrate: 5_000_000, preference: -1 }),
		"tall/360p": config("vp09.00.10.08", { codedWidth: 360, codedHeight: 640, bitrate: 600_000, preference: -1 }),
	});

	it("follows the viewport across rotations, and takes the ranking without one", async () => {
		const target = new Signal<Target | undefined>(undefined);
		const source = new Source({ broadcast: mockBroadcast(renditions()), target });

		await settle();
		expect(source.out.track.peek()).toBe("wide");

		target.set({ width: 1080, height: 1920 });
		await settle();
		expect(source.out.track.peek()).toBe("tall");

		target.set({ width: 1920, height: 1080 });
		await settle();
		expect(source.out.track.peek()).toBe("wide");

		// A square box has no orientation, so the ranking decides.
		target.set({ width: 1920, height: 1920 });
		await settle();
		expect(source.out.track.peek()).toBe("wide");
		source.close();
	});

	it("steps down within the orientation for the rate", async () => {
		const source = new Source({
			broadcast: mockBroadcast(renditions()),
			target: { width: 1080, height: 1920 },
			rate: 1_000_000,
		});

		await settle();
		expect(source.out.track.peek()).toBe("tall/360p");
		source.close();
	});

	it("treats a square rendition as either orientation", async () => {
		const source = new Source({
			broadcast: mockBroadcast({
				...renditions(),
				square: config("avc1.640028", {
					codedWidth: 1440,
					codedHeight: 1440,
					bitrate: 5_000_000,
					preference: 1,
				}),
			}),
			target: { width: 1080, height: 1920 },
		});

		await settle();
		expect(source.out.track.peek()).toBe("square");
		source.close();
	});

	it("falls back to the other orientation when its own can't be decoded", async () => {
		answer = (config) => !config.codec.startsWith("vp09");
		const source = new Source({ broadcast: mockBroadcast(renditions()), target: { width: 1080, height: 1920 } });

		await settle();
		// Only the 360p landscape rendition fits a 1080-wide box.
		expect(source.out.track.peek()).toBe("wide/360p");
		source.close();
	});
});

describe("rotated renditions", () => {
	it("fits the box by the size as shown, the coded size turned by the rotation", async () => {
		const renditions = {
			"1080p": config("avc1.640028", { codedWidth: 1920, codedHeight: 1080 }),
			"720p": config("avc1.64001f", { codedWidth: 1280, codedHeight: 720 }),
			"360p": config("avc1.64001e", { codedWidth: 640, codedHeight: 360 }),
		};
		for (const [rotation, track] of [
			[90, "720p"],
			[270, "720p"],
			// Unturned, a landscape 720p is too wide for the portrait box.
			[0, "360p"],
			[180, "360p"],
		] as const) {
			const broadcast = {
				in: { connection: new Signal(undefined) },
				out: { catalog: new Signal({ video: { renditions, rotation } }) },
			} as unknown as Broadcast;
			const source = new Source({ broadcast, target: { width: 720, height: 1280 } });
			await settle();
			expect([rotation, source.out.track.peek()]).toEqual([rotation, track]);
			source.close();
		}
	});
});

describe("conflicting rendition metadata", () => {
	it("keeps the first ranking's best picture when bitrate metadata has no common name", async () => {
		const source = new Source({
			broadcast: mockBroadcast({
				picture: config("avc1.640028", { codedWidth: 640, codedHeight: 360 }),
				bitrate: config("avc1.640028", { bitrate: 500000 }),
			}),
			target: { width: 640, height: 360, bitrate: 1000000 },
		});
		try {
			if (!source.pending.peek()) await source.pending.changed();
			while (source.pending.peek()) await source.pending.changed();
			expect(source.out.track.peek()).toBe("picture");
		} finally {
			source.close();
		}
	});
});

describe("Source viewer ceiling", () => {
	const ladder = () => ({
		"360p": config("avc1.640028", { codedWidth: 640, codedHeight: 360, bitrate: 800_000 }),
		"720p": config("avc1.640028", { codedWidth: 1280, codedHeight: 720, bitrate: 2_400_000 }),
		"1080p": config("avc1.640028", { codedWidth: 1920, codedHeight: 1080, bitrate: 4_000_000 }),
	});

	it("follows the highest enabled rendition through enable and disable flips", async () => {
		const renditions = ladder();
		const { broadcast, catalog } = mutableBroadcast(renditions);
		const source = new Source({ broadcast });
		try {
			await settle();
			// Over the selection margin, so a rate that reaches it selects 1080p.
			expect(source.out.ceiling.peek()).toBe(5_000_000);

			catalog.set({
				video: { renditions: { ...renditions, "1080p": { ...renditions["1080p"], enabled: false } } },
			});
			await settle();
			expect(source.out.ceiling.peek()).toBe(3_000_000);

			catalog.set({ video: { renditions } });
			await settle();
			expect(source.out.ceiling.peek()).toBe(5_000_000);
		} finally {
			source.close();
		}
	});

	it("is a rate that selects that rendition", async () => {
		const rate = new Signal<number | undefined>(undefined);
		const source = new Source({ broadcast: mockBroadcast(ladder()), rate });
		try {
			await settle();
			const ceiling = source.out.ceiling.peek() ?? 0;
			rate.set(ceiling);
			await settle();
			expect(source.out.track.peek()).toBe("1080p");
			rate.set(ceiling - 1);
			await settle();
			expect(source.out.track.peek()).toBe("720p");
		} finally {
			source.close();
		}
	});

	it("holds the last rate while it is unknown, and ranks alone before the first", async () => {
		const rate = new Signal<number | undefined>(undefined);
		const source = new Source({ broadcast: mockBroadcast(ladder()), rate });
		try {
			await settle();
			expect(source.out.track.peek()).toBe("1080p");

			rate.set(1_000_000);
			await settle();
			expect(source.out.track.peek()).toBe("360p");

			rate.set(undefined);
			await settle();
			expect(source.out.track.peek()).toBe("360p");
		} finally {
			source.close();
		}
	});

	it("skips renditions without a catalog bitrate, and is unset when none has one", async () => {
		const { broadcast, catalog } = mutableBroadcast({
			"360p": config("avc1.640028", { codedWidth: 640, codedHeight: 360, bitrate: 800_000 }),
			"4k": config("avc1.640028", { codedWidth: 3840, codedHeight: 2160 }),
		});
		const source = new Source({ broadcast });
		try {
			await settle();
			expect(source.out.ceiling.peek()).toBe(1_000_000);

			catalog.set({
				video: { renditions: { "4k": config("avc1.640028", { codedWidth: 3840, codedHeight: 2160 }) } },
			});
			await settle();
			expect(source.out.ceiling.peek()).toBeUndefined();
		} finally {
			source.close();
		}
	});

	it("stays within the target's caps, without margin when the rate cannot steer", async () => {
		const target = new Signal<Target | undefined>({ height: 720 });
		const source = new Source({ broadcast: mockBroadcast(ladder()), target });
		try {
			await settle();
			expect(source.out.ceiling.peek()).toBe(3_000_000);

			target.set({ name: "360p" });
			await settle();
			expect(source.out.ceiling.peek()).toBe(800_000);

			target.set({ bitrate: 3_000_000 });
			await settle();
			expect(source.out.ceiling.peek()).toBe(2_400_000);

			// A zero bitrate is explicit too: the lowest rendition, not the rate's pick.
			target.set({ bitrate: 0 });
			await settle();
			expect(source.out.ceiling.peek()).toBe(800_000);
		} finally {
			source.close();
		}
	});
});

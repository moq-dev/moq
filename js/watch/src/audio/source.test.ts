import { expect, test } from "bun:test";
import * as Catalog from "@moq/hang/catalog";
import { Signal } from "@moq/signals";
import type { Broadcast } from "../broadcast";
import { supportCacheKey } from "../support-cache";
import { decoderConfigKey } from "./config";
import { Source } from "./source";

function config(codec: string): Catalog.AudioConfig {
	return Catalog.AudioConfigSchema.parse({
		codec,
		sampleRate: 48000,
		numberOfChannels: 2,
		container: { kind: "legacy" },
	});
}

function broadcast(renditions: Record<string, Catalog.AudioConfig>) {
	const catalog = new Signal<Catalog.Root>({ audio: { renditions } });
	return { catalog, broadcast: { out: { catalog } } as unknown as Broadcast };
}

// Wait on the probe's causal boundary, rather than a timer or a guessed number of ticks.
async function settled(source: Source): Promise<void> {
	if (!source.pending.peek()) await source.pending.changed();
	while (source.pending.peek()) await source.pending.changed();
}

test("a rejected rendition probe preserves its supported siblings", async () => {
	const state = broadcast({ bad: config(""), good: config("opus"), other: config("mp4a.40.2") });
	const source = new Source({
		broadcast: state.broadcast,
		supported: async (c) => {
			if (!c.codec) throw new TypeError("empty codec");
			return true;
		},
	});
	try {
		await settled(source);
		expect(Object.keys(source.out.available.peek())).toEqual(["good", "other"]);
		expect(source.out.track.peek()).toBe("good");
	} finally {
		source.close();
	}
});

test("a rejected replacement catalog clears previously available audio", async () => {
	const state = broadcast({ old: config("opus") });
	const source = new Source({
		broadcast: state.broadcast,
		supported: async (c) => {
			if (!c.codec) throw new TypeError("empty codec");
			return true;
		},
	});
	try {
		await settled(source);
		expect(source.out.track.peek()).toBe("old");
		state.catalog.set({ audio: { renditions: { bad: config("") } } });
		await settled(source);
		expect(source.out.available.peek()).toEqual({});
		expect(source.out.track.peek()).toBeUndefined();
		expect(source.out.config.peek()).toBeUndefined();
		state.catalog.set({ audio: { renditions: { recovered: config("opus") } } });
		await settled(source);
		expect(source.out.track.peek()).toBe("recovered");
	} finally {
		source.close();
	}
});

test("unmute reuses the decoder support verdict and publishes the new metadata", async () => {
	const initial = config("opus");
	const state = broadcast({ audio: initial });
	let probes = 0;
	const supported = Object.assign(
		async () => {
			probes++;
			return true;
		},
		{ [supportCacheKey]: decoderConfigKey },
	);
	const source = new Source({ broadcast: state.broadcast, supported });
	try {
		await settled(source);
		for (const enabled of [false, true, false, true]) {
			const selected = source.out.track.changed();
			const changed = source.out.available.changed();
			state.catalog.set({
				audio: {
					renditions: {
						audio: Catalog.AudioConfigSchema.parse({ ...initial, enabled, jitter: 20, bitrate: 96_000 }),
					},
				},
			});
			await changed;
			await selected;
			expect(source.out.available.peek().audio.enabled).toBe(enabled);
			expect(source.out.track.peek()).toBe(enabled ? "audio" : undefined);
			expect(probes).toBe(1);
		}
	} finally {
		source.close();
	}
});

for (const [field, patch] of Object.entries({
	codec: { codec: "mp4a.40.2" },
	description: { description: "1190" },
	sampleRate: { sampleRate: 44100 },
	numberOfChannels: { numberOfChannels: 1 },
	container: { container: { kind: "loc" as const } },
	init: { container: { kind: "cmaf" as const, init: "AA==" } },
	warmup: { warmup: 80 },
})) {
	test(`a ${field} change invalidates the decoder support verdict`, async () => {
		const initial = config("opus");
		const state = broadcast({ audio: initial });
		let probes = 0;
		const supported = Object.assign(
			async () => {
				probes++;
				return true;
			},
			{ [supportCacheKey]: decoderConfigKey },
		);
		const source = new Source({ broadcast: state.broadcast, supported });
		try {
			await settled(source);
			const updated = Catalog.AudioConfigSchema.parse({ ...initial, ...patch });
			const changed = source.out.available.changed();
			state.catalog.set({ audio: { renditions: { audio: updated } } });
			await changed;
			expect(probes).toBe(2);
			expect(source.out.available.peek().audio).toEqual(updated);
		} finally {
			source.close();
		}
	});
}

test("custom support probes include metadata in their cache key", async () => {
	const initial = config("opus");
	const state = broadcast({ audio: initial });
	let probes = 0;
	const source = new Source({
		broadcast: state.broadcast,
		supported: async (c) => {
			probes++;
			return c.enabled !== false;
		},
	});
	try {
		await settled(source);
		state.catalog.set({ audio: { renditions: { audio: { ...initial, enabled: false } } } });
		await settled(source);
		expect(probes).toBe(2);
		expect(source.out.available.peek()).toEqual({});
	} finally {
		source.close();
	}
});

test("unsupported verdicts are cached, rejected probes are not, and removed renditions are pruned", async () => {
	const initial = { good: config("opus"), unsupported: config("flac"), rejected: config("mp4a.40.2") };
	const state = broadcast(initial);
	const probes: Record<string, number> = {};
	const supported = Object.assign(
		async (c: Catalog.AudioConfig) => {
			probes[c.codec] = (probes[c.codec] ?? 0) + 1;
			if (c.codec === "mp4a.40.2" && probes[c.codec] === 1) throw new Error("probe failed");
			return c.codec !== "flac";
		},
		{ [supportCacheKey]: decoderConfigKey },
	);
	const source = new Source({ broadcast: state.broadcast, supported });
	try {
		await settled(source);
		const changed = source.out.available.changed();
		state.catalog.set({
			audio: {
				renditions: Object.fromEntries(
					Object.entries(initial).map(([name, c]) => [name, { ...c, label: "updated" }]),
				),
			},
		});
		await changed;
		expect(probes).toEqual({ opus: 1, flac: 1, "mp4a.40.2": 2 });
		expect(Object.keys(source.out.available.peek())).toEqual(["good", "rejected"]);

		const removed = source.out.available.changed();
		state.catalog.set({ audio: { renditions: { good: initial.good } } });
		await removed;
		const restored = source.out.available.changed();
		state.catalog.set({ audio: { renditions: initial } });
		await restored;
		expect(probes).toEqual({ opus: 1, flac: 2, "mp4a.40.2": 3 });
	} finally {
		source.close();
	}
});

test("a replacement support function cannot reuse another function's verdict", async () => {
	const state = broadcast({ audio: config("opus") });
	const supported = new Signal<((c: Catalog.AudioConfig) => Promise<boolean>) | undefined>(async () => true);
	const source = new Source({ broadcast: state.broadcast, supported });
	try {
		await settled(source);
		supported.set(async () => false);
		await settled(source);
		expect(source.out.available.peek()).toEqual({});
	} finally {
		source.close();
	}
});

test("catalog replacement and close release hanging probes without publishing stale verdicts", async () => {
	const initial = config("opus");
	const state = broadcast({ audio: initial });
	const started = Promise.withResolvers<void>();
	const pending = Promise.withResolvers<boolean>();
	let probes = 0;
	const supported = Object.assign(
		async () => {
			probes++;
			if (probes === 1) {
				started.resolve();
				return pending.promise;
			}
			return true;
		},
		{ [supportCacheKey]: decoderConfigKey },
	);
	const source = new Source({ broadcast: state.broadcast, supported });
	try {
		await started.promise;
		const changed = source.out.available.changed();
		state.catalog.set({ audio: { renditions: { audio: { ...initial, enabled: false } } } });
		await changed;
		expect(probes).toBe(2);
		pending.resolve(false);
		const unmuted = source.out.available.changed();
		state.catalog.set({ audio: { renditions: { audio: { ...initial, enabled: true } } } });
		await unmuted;
		expect(probes).toBe(2);
		expect(source.out.available.peek().audio.enabled).toBe(true);
	} finally {
		source.close();
	}

	const closing = Promise.withResolvers<void>();
	const never = Promise.withResolvers<boolean>();
	const closed = new Source({
		broadcast: state.broadcast,
		supported: async () => {
			closing.resolve();
			return never.promise;
		},
	});
	await closing.promise;
	closed.close();
	while (closed.pending.peek()) await closed.pending.changed();
	never.resolve(true);
	expect(closed.out.available.peek()).toEqual({});
	expect(closed.out.track.peek()).toBeUndefined();
});

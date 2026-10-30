import { expect, test } from "bun:test";
import * as Catalog from "@moq/hang/catalog";
import { Signal } from "@moq/signals";
import type { Broadcast } from "../broadcast";
import { Source } from "./source";

function config(codec: string, fields: Record<string, unknown> = {}): Catalog.AudioConfig {
	return Catalog.AudioConfigSchema.parse({
		codec,
		sampleRate: 48000,
		numberOfChannels: 2,
		container: { kind: "legacy" },
		...fields,
	});
}

function broadcast(renditions: Record<string, Catalog.AudioConfig>) {
	const catalog = new Signal<Catalog.Root>({ audio: { renditions } });
	return { catalog, broadcast: { out: { catalog } } as unknown as Broadcast };
}

const tick = () => new Promise((resolve) => setTimeout(resolve, 0));

// Wait out the probe, then a tick for the effects its result wakes. A probe served wholly from
// the cache sets and clears `pending` in one run, which notifies nothing, so start with a tick
// that lets the rerun happen rather than waiting on a change that may never come.
async function settled(source: Source): Promise<void> {
	await tick();
	while (source.pending.peek()) {
		await source.pending.changed();
		await tick();
	}
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

test("skips a disabled rendition on the initial selection", async () => {
	const state = broadcast({ mic: config("opus", { enabled: false }) });
	const source = new Source({ broadcast: state.broadcast, supported: async () => true });
	try {
		await settled(source);
		expect(source.out.track.peek()).toBeUndefined();

		state.catalog.set({ audio: { renditions: { mic: config("opus") } } });
		await settled(source);
		expect(source.out.track.peek()).toBe("mic");
	} finally {
		source.close();
	}
});

test("holds the selected rendition while every rendition is disabled", async () => {
	const state = broadcast({ mic: config("opus") });
	const source = new Source({ broadcast: state.broadcast, supported: async () => true });
	try {
		await settled(source);
		expect(source.out.track.peek()).toBe("mic");

		state.catalog.set({ audio: { renditions: { mic: config("opus", { enabled: false }) } } });
		await settled(source);
		expect(source.out.track.peek()).toBe("mic");
		expect(source.out.config.peek()?.enabled).toBe(false);

		state.catalog.set({ audio: { renditions: { mic: config("opus") } } });
		await settled(source);
		expect(source.out.track.peek()).toBe("mic");
		expect(source.out.config.peek()?.enabled).toBeUndefined();
	} finally {
		source.close();
	}
});

test("an enabled alternative takes over from a disabled rendition", async () => {
	const state = broadcast({ mic: config("opus") });
	const source = new Source({ broadcast: state.broadcast, supported: async () => true });
	try {
		await settled(source);
		expect(source.out.track.peek()).toBe("mic");

		state.catalog.set({
			audio: { renditions: { mic: config("opus", { enabled: false }), backup: config("opus") } },
		});
		await settled(source);
		expect(source.out.track.peek()).toBe("backup");
	} finally {
		source.close();
	}
});

test("drops a held rendition once it is removed", async () => {
	const state = broadcast({ mic: config("opus") });
	const source = new Source({ broadcast: state.broadcast, supported: async () => true });
	try {
		await settled(source);
		state.catalog.set({ audio: { renditions: { mic: config("opus", { enabled: false }) } } });
		await settled(source);
		expect(source.out.track.peek()).toBe("mic");

		state.catalog.set({ audio: { renditions: { other: config("opus", { enabled: false }) } } });
		await settled(source);
		expect(source.out.track.peek()).toBeUndefined();
		expect(source.out.config.peek()).toBeUndefined();
	} finally {
		source.close();
	}
});

test("does not carry a hold to another broadcast", async () => {
	const first = broadcast({ mic: config("opus") });
	const current = new Signal<Broadcast | undefined>(first.broadcast);
	const source = new Source({ broadcast: current, supported: async () => true });
	try {
		await settled(source);
		first.catalog.set({ audio: { renditions: { mic: config("opus", { enabled: false }) } } });
		await settled(source);
		expect(source.out.track.peek()).toBe("mic");

		// The same catalog on another broadcast probes nothing new, so wait on a timer, not the probe.
		current.set(broadcast({ mic: config("opus", { enabled: false }) }).broadcast);
		await tick();
		expect(source.out.track.peek()).toBeUndefined();
	} finally {
		source.close();
	}
});

test("does not hold an enabled selection into another broadcast's disabled rendition", async () => {
	const current = new Signal<Broadcast | undefined>(broadcast({ mic: config("opus") }).broadcast);
	const source = new Source({ broadcast: current, supported: async () => true });
	try {
		await settled(source);
		expect(source.out.track.peek()).toBe("mic");

		current.set(broadcast({ mic: config("opus", { enabled: false }) }).broadcast);
		for (let i = 0; i < 5; i++) await tick();
		expect(source.out.track.peek()).toBeUndefined();
		expect(source.out.config.peek()).toBeUndefined();
	} finally {
		source.close();
	}
});

test("holds on a broadcast whose renditions match the previous one", async () => {
	const current = new Signal<Broadcast | undefined>(broadcast({ mic: config("opus") }).broadcast);
	const source = new Source({ broadcast: current, supported: async () => true });
	try {
		await settled(source);

		// The new probe publishes an equal `available`, so only its broadcast tag changes.
		const next = broadcast({ mic: config("opus") });
		current.set(next.broadcast);
		await settled(source);
		expect(source.out.track.peek()).toBe("mic");

		next.catalog.set({ audio: { renditions: { mic: config("opus", { enabled: false }) } } });
		await settled(source);
		expect(source.out.track.peek()).toBe("mic");
		expect(source.out.config.peek()?.enabled).toBe(false);
	} finally {
		source.close();
	}
});

async function quietly(fn: () => Promise<void>): Promise<void> {
	const warn = console.warn;
	console.warn = () => {};
	try {
		await fn();
	} finally {
		console.warn = warn;
	}
}

test("reports NotSupportedError when no rendition is supported", () =>
	quietly(async () => {
		const state = broadcast({ mic: config("opus") });
		const source = new Source({ broadcast: state.broadcast, supported: async () => false });
		try {
			await settled(source);
			expect(source.out.error.peek()?.name).toBe("NotSupportedError");
			expect(source.out.available.peek()).toEqual({});

			state.catalog.set({ audio: { renditions: {} } });
			await settled(source);
			expect(source.out.error.peek()).toBeUndefined();
		} finally {
			source.close();
		}
	}));

test("a rejecting probe alone is unsupported", () =>
	quietly(async () => {
		const source = new Source({
			broadcast: broadcast({ mic: config("opus") }).broadcast,
			supported: async () => {
				throw new TypeError("probe failed");
			},
		});
		try {
			await settled(source);
			expect(source.out.error.peek()?.name).toBe("NotSupportedError");
			expect(source.out.track.peek()).toBeUndefined();
		} finally {
			source.close();
		}
	}));

test("does not wedge the effect when a support probe never settles", async () => {
	const supported = new Signal<(config: Catalog.AudioConfig) => Promise<boolean>>(() => new Promise(() => {}));
	const source = new Source({ broadcast: broadcast({ mic: config("opus") }).broadcast, supported });
	try {
		if (!source.pending.peek()) await source.pending.changed();
		expect(source.out.available.peek()).toEqual({});
		expect(source.out.error.peek()).toBeUndefined();

		// Swapping the probe reruns the effect, which has to tear the parked one down first.
		supported.set(async () => true);
		await settled(source);
		expect(source.out.track.peek()).toBe("mic");
	} finally {
		source.close();
	}
});

test("a catalog replaced while its probe is pending publishes only the replacement", async () => {
	const probes = new Map<string, (supported: boolean) => void>();
	const state = broadcast({ old: config("opus") });
	const source = new Source({
		broadcast: state.broadcast,
		supported: (c) => new Promise((resolve) => probes.set(c.codec, resolve)),
	});
	try {
		if (!source.pending.peek()) await source.pending.changed();
		const old = probes.get("opus");
		expect(old).toBeDefined();

		state.catalog.set({ audio: { renditions: { next: config("mp4a.40.2") } } });
		while (!probes.has("mp4a.40.2")) await tick();

		// The torn-down probe settles late; its result must not land.
		old?.(true);
		probes.get("mp4a.40.2")?.(true);
		await settled(source);
		expect(Object.keys(source.out.available.peek())).toEqual(["next"]);
		expect(source.out.track.peek()).toBe("next");
	} finally {
		source.close();
	}
});

test("serves unchanged configs from the cache when the catalog changes", async () => {
	const state = broadcast({ mic: config("opus") });
	const probed: string[] = [];
	const source = new Source({
		broadcast: state.broadcast,
		supported: async (c) => {
			probed.push(c.codec);
			return true;
		},
	});
	try {
		await settled(source);
		expect(probed).toEqual(["opus"]);

		state.catalog.set({ audio: { renditions: { mic: config("opus"), backup: config("mp4a.40.2") } } });
		await settled(source);
		expect(probed).toEqual(["opus", "mp4a.40.2"]);
		expect(Object.keys(source.out.available.peek())).toEqual(["mic", "backup"]);
	} finally {
		source.close();
	}
});

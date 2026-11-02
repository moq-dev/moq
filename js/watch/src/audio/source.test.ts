import { expect, test } from "bun:test";
import { yieldNow } from "@moq/clock";
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
		await yieldNow();
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
		for (let i = 0; i < 5; i++) await yieldNow();
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

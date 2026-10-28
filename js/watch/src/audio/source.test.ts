import { expect, test } from "bun:test";
import * as Catalog from "@moq/hang/catalog";
import { Signal } from "@moq/signals";
import type { Broadcast } from "../broadcast";
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

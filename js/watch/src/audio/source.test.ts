import { afterEach, beforeEach, expect, test } from "bun:test";
import { yieldNow } from "@moq/clock";
import * as Catalog from "@moq/hang/catalog";
import { Signal } from "@moq/signals";
import type { Broadcast } from "../broadcast";
import { stubSupport } from "../webcodecs.test-util";
import { Source } from "./source";

// What the stubbed WebCodecs decoder says about each config the source probes: every rendition plays
// unless a test says otherwise.
let answer: (config: AudioDecoderConfig) => boolean | Promise<boolean>;
let decoder: Disposable;
beforeEach(() => {
	answer = () => true;
	decoder = stubSupport({ audio: (config) => answer(config) });
});
afterEach(() => decoder[Symbol.dispose]());

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

// Wait out the probe, then a tick for the effects its result wakes. A probe served wholly from
// the cache sets and clears `pending` in one run, which notifies nothing, so start with a tick
// that lets the rerun happen rather than waiting on a change that may never come.
async function settled(source: Source): Promise<void> {
	await yieldNow();
	while (source.pending.peek()) {
		await source.pending.changed();
		await yieldNow();
	}
}

// Wait for the stubbed decoder to be asked. `pending` turns true before that: the probe first
// awaits the libav polyfill, which takes longer once an earlier test file has loaded it. A probe
// that settles without asking, such as one that rejects first, clears `pending` and fails the wait.
async function probed(source: Source, asked: () => boolean): Promise<void> {
	if (!source.pending.peek()) await source.pending.changed();
	while (!asked()) {
		expect(source.pending.peek()).toBe(true);
		await yieldNow();
	}
}

test("a rejected rendition probe preserves its supported siblings", async () => {
	const state = broadcast({ bad: config(""), good: config("opus"), other: config("mp4a.40.2") });
	answer = (c) => {
		if (!c.codec) throw new TypeError("empty codec");
		return true;
	};
	const source = new Source({ broadcast: state.broadcast });
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
	answer = (c) => {
		if (!c.codec) throw new TypeError("empty codec");
		return true;
	};
	const source = new Source({ broadcast: state.broadcast });
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

test("prefers a timestamped container, falling back to raw and then cts", async () => {
	const container = (kind: string) => config("opus", { container: { kind } });
	const state = broadcast({ cts: container("cts"), raw: container("raw"), loc: container("loc") });
	const source = new Source({ broadcast: state.broadcast });
	try {
		await settled(source);
		expect(source.out.track.peek()).toBe("loc");

		state.catalog.set({ audio: { renditions: { cts: container("cts"), raw: container("raw") } } });
		await settled(source);
		expect(source.out.track.peek()).toBe("raw");
	} finally {
		source.close();
	}
});

test("skips a disabled rendition on the initial selection", async () => {
	const state = broadcast({ mic: config("opus", { enabled: false }) });
	const source = new Source({ broadcast: state.broadcast });
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

test("deselects a rendition once it is disabled, and selects it again once re-enabled", async () => {
	const state = broadcast({ mic: config("opus") });
	const source = new Source({ broadcast: state.broadcast });
	try {
		await settled(source);
		expect(source.out.track.peek()).toBe("mic");

		state.catalog.set({ audio: { renditions: { mic: config("opus", { enabled: false }) } } });
		await settled(source);
		expect(source.out.track.peek()).toBeUndefined();
		expect(source.out.config.peek()).toBeUndefined();

		state.catalog.set({ audio: { renditions: { mic: config("opus") } } });
		await settled(source);
		expect(source.out.track.peek()).toBe("mic");
	} finally {
		source.close();
	}
});

test("an enabled alternative takes over from a disabled rendition", async () => {
	const state = broadcast({ mic: config("opus") });
	const source = new Source({ broadcast: state.broadcast });
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
		answer = () => false;
		const source = new Source({ broadcast: state.broadcast });
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
		answer = () => {
			throw new TypeError("probe failed");
		};
		const source = new Source({ broadcast: broadcast({ mic: config("opus") }).broadcast });
		try {
			await settled(source);
			expect(source.out.error.peek()?.name).toBe("NotSupportedError");
			expect(source.out.track.peek()).toBeUndefined();
		} finally {
			source.close();
		}
	}));

test("does not wedge the effect when a support probe never settles", async () => {
	let asked = false;
	answer = () => {
		asked = true;
		return new Promise(() => {});
	};
	const state = broadcast({ mic: config("opus") });
	const source = new Source({ broadcast: state.broadcast });
	try {
		await probed(source, () => asked);
		expect(source.out.available.peek()).toEqual({});
		expect(source.out.error.peek()).toBeUndefined();

		// A catalog update reruns the effect, which has to tear the parked probe down first.
		answer = () => true;
		state.catalog.set({ audio: { renditions: { mic: config("opus", { numberOfChannels: 1 }) } } });
		await settled(source);
		expect(source.out.track.peek()).toBe("mic");
	} finally {
		source.close();
	}
});

test("a catalog replaced while its probe is pending publishes only the replacement", async () => {
	const probes = new Map<string, (supported: boolean) => void>();
	const state = broadcast({ old: config("opus") });
	answer = (c) => new Promise((resolve) => probes.set(c.codec, resolve));
	const source = new Source({ broadcast: state.broadcast });
	try {
		await probed(source, () => probes.has("opus"));
		const old = probes.get("opus");

		state.catalog.set({ audio: { renditions: { next: config("mp4a.40.2") } } });
		await probed(source, () => probes.has("mp4a.40.2"));

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
	answer = (c) => {
		probed.push(c.codec);
		return true;
	};
	const source = new Source({ broadcast: state.broadcast });
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

test("a disable and re-enable reuse the decoder support verdict and publish the new metadata", async () => {
	const initial = config("opus");
	const state = broadcast({ mic: initial });
	let probes = 0;
	answer = () => {
		probes++;
		return true;
	};
	const source = new Source({ broadcast: state.broadcast });
	try {
		await settled(source);
		for (const enabled of [false, true, false, true]) {
			state.catalog.set({
				audio: { renditions: { mic: config("opus", { enabled, jitter: 20, bitrate: 96_000 }) } },
			});
			await settled(source);
			expect(source.out.available.peek().mic.enabled).toBe(enabled);
			expect(source.out.track.peek()).toBe(enabled ? "mic" : undefined);
			expect(probes).toBe(1);
		}

		// A field the decoder depends on still invalidates the verdict.
		state.catalog.set({ audio: { renditions: { mic: config("opus", { numberOfChannels: 1 }) } } });
		await settled(source);
		expect(probes).toBe(2);
	} finally {
		source.close();
	}
});

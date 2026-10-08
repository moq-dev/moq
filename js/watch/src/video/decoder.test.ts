import { describe, expect, it, spyOn } from "bun:test";
import * as Catalog from "@moq/hang/catalog";
import * as Container from "@moq/hang/container";
import * as Moq from "@moq/net";
import { Time } from "@moq/net";
import { Signal } from "@moq/signals";
import type { Broadcast } from "../broadcast";
import { Sync } from "../sync";
import { Decoder } from "./decoder";
import { Source } from "./source";

const flush = () => new Promise((resolve) => setTimeout(resolve, 0));

async function settle(): Promise<void> {
	for (let i = 0; i < 8; i++) await flush();
}

function config(fields: Record<string, unknown>): Catalog.VideoConfig {
	return Catalog.VideoConfigSchema.parse({
		codec: "avc1.640028",
		container: { kind: "legacy" },
		...fields,
	});
}

// The subscription is already closed, so the track never reaches VideoDecoder.
function closedConsumer(): Moq.Broadcast.Consumer {
	return { closed: new Signal("ended") } as unknown as Moq.Broadcast.Consumer;
}

function watchBroadcast(catalog: Signal<Catalog.Root>): Broadcast {
	return {
		out: { catalog },
		relativeBroadcast: () => closedConsumer(),
	} as unknown as Broadcast;
}

async function play(catalog: Signal<Catalog.Root>): Promise<{
	broadcast: Signal<Broadcast | undefined>;
	source: Source;
	sync: Sync;
	decoder: Decoder;
}> {
	const broadcast = new Signal<Broadcast | undefined>(watchBroadcast(catalog));
	const source = new Source({
		broadcast,
		supported: async () => true,
	});
	const sync = new Sync({ delay: Time.Milli(0) });
	await settle();
	const decoder = new Decoder({ source, sync });
	await settle();
	return { broadcast, source, sync, decoder };
}

describe("Decoder jitter across a source switch", () => {
	it("keeps the outgoing floor when the next source reuses the track name", async () => {
		const outgoing = new Signal<Catalog.Root>({
			video: { renditions: { video: config({ delay: 200, jitter: 60 }) } },
		});
		const { broadcast, source, sync, decoder } = await play(outgoing);
		try {
			expect(decoder.out.jitter.peek()).toBe(Time.Milli(260));

			// A rise on the catalog this rendition subscribed to still resizes the floor.
			outgoing.set({
				video: { renditions: { video: config({ delay: 300, jitter: 60 }) } },
			});
			await settle();
			expect(decoder.out.jitter.peek()).toBe(Time.Milli(360));

			broadcast.set(
				watchBroadcast(
					new Signal<Catalog.Root>({
						video: { renditions: { video: config({ jitter: 20 }) } },
					}),
				),
			);
			await settle();
			// The old frames are still on screen. The new catalog's same name is not their floor.
			expect(decoder.out.jitter.peek()).toBe(Time.Milli(360));
		} finally {
			decoder.close();
			source.close();
			sync.close();
		}
	});

	it("follows a newer pending catalog when the track name stays the same", async () => {
		const outgoing = new Signal<Catalog.Root>({
			video: { renditions: { video: config({ jitter: 20 }) } },
		});
		const { broadcast, source, sync, decoder } = await play(outgoing);
		try {
			expect(decoder.out.jitter.peek()).toBe(Time.Milli(20));

			broadcast.set(
				watchBroadcast(
					new Signal<Catalog.Root>({
						video: { renditions: { video: config({ delay: 200 }) } },
					}),
				),
			);
			await settle();
			expect(decoder.out.jitter.peek()).toBe(Time.Milli(200));

			const newest = new Signal<Catalog.Root>({
				video: { renditions: { video: config({ delay: 500 }) } },
			});
			broadcast.set(watchBroadcast(newest));
			await settle();
			// The first switch is still pending, so the name did not change. The new catalog still has to win.
			expect(decoder.out.jitter.peek()).toBe(Time.Milli(500));

			newest.set({
				video: { renditions: { video: config({ delay: 700 }) } },
			});
			await settle();
			expect(decoder.out.jitter.peek()).toBe(Time.Milli(700));
		} finally {
			decoder.close();
			source.close();
			sync.close();
		}
	});

	it("keeps the outgoing floor when the next source uses a different name", async () => {
		const outgoing = new Signal<Catalog.Root>({
			video: { renditions: { hd: config({ delay: 200, jitter: 60 }) } },
		});
		const { broadcast, source, sync, decoder } = await play(outgoing);
		try {
			expect(decoder.out.jitter.peek()).toBe(Time.Milli(260));

			broadcast.set(
				watchBroadcast(
					new Signal<Catalog.Root>({
						video: { renditions: { sd: config({ jitter: 20 }) } },
					}),
				),
			);
			await settle();
			expect(decoder.out.jitter.peek()).toBe(Time.Milli(260));
		} finally {
			decoder.close();
			source.close();
			sync.close();
		}
	});
});

// Drain reactive work without advancing playback time.
async function microtasks() {
	for (let i = 0; i < 40; i++) await Promise.resolve();
}

class Picture {
	closed = false;
	displayWidth = 16;
	displayHeight = 16;
	readonly timestamp: number;
	constructor(timestamp: number) {
		this.timestamp = timestamp;
	}
	clone() {
		return new Picture(this.timestamp);
	}
	close() {
		this.closed = true;
	}
}

type Read = NonNullable<Awaited<ReturnType<Container.Consumer["next"]>>>;
function sample(group: number, timestamp: number, discontinuity = 0, keyframe = true): Read {
	return {
		group,
		discontinuity,
		continuous: true,
		frame: { timestamp: Time.Micro(timestamp), payload: new Uint8Array([1]), keyframe },
	};
}

async function guardedPlayback(kind: "legacy" | "cmaf", reads: Read[], hold: readonly number[] = []) {
	const originalDecoder = Object.getOwnPropertyDescriptor(globalThis, "VideoDecoder");
	const originalChunk = Object.getOwnPropertyDescriptor(globalThis, "EncodedVideoChunk");
	const submitted: number[] = [];
	const outputs: number[] = [];
	const owed = new Set(hold);
	const pending: Array<() => void> = [];
	let resets = 0;
	const next = spyOn(Container.Consumer.prototype, "next").mockImplementation(async () => reads.shift());
	class Codec {
		state = "unconfigured";
		// Like WebCodecs, a configure requires a key chunk before any delta.
		#keyRequired = true;
		readonly callbacks: VideoDecoderInit;
		constructor(callbacks: VideoDecoderInit) {
			this.callbacks = callbacks;
		}
		configure() {
			this.state = "configured";
			this.#keyRequired = true;
		}
		// Pictures still inside the decoder come out ahead of a chunk decoded later, unless reset
		// discarded them. That is the gap the generation guard misses: it is read when the frame
		// comes out, which is after the playhead bump.
		decode(chunk: EncodedVideoChunk) {
			if (this.state !== "configured") throw new Error("decode on an unconfigured decoder");
			if (this.#keyRequired && chunk.type !== "key") throw new Error("a key chunk is required");
			this.#keyRequired = false;
			submitted.push(chunk.timestamp);
			const emit = () => {
				outputs.push(chunk.timestamp);
				void this.callbacks.output(new Picture(chunk.timestamp) as unknown as VideoFrame);
			};
			if (owed.delete(chunk.timestamp)) {
				pending.push(emit);
				return;
			}
			for (const earlier of pending) earlier();
			pending.length = 0;
			emit();
		}
		reset() {
			resets++;
			pending.length = 0;
			this.state = "unconfigured";
		}
		close() {
			this.state = "closed";
		}
	}
	Object.defineProperty(globalThis, "VideoDecoder", { configurable: true, value: Codec });
	Object.defineProperty(globalThis, "EncodedVideoChunk", {
		configurable: true,
		value: class {
			timestamp: number;
			type: EncodedVideoChunkType;
			constructor(init: EncodedVideoChunkInit) {
				this.timestamp = init.timestamp;
				this.type = init.type;
			}
		},
	});
	const video = config({ codedWidth: 16, codedHeight: 16, description: "01640028ffe100046764002801000268ee" });
	if (kind === "cmaf")
		video.container = { kind, init: Buffer.from(Container.Cmaf.createVideoInitSegment(video)).toString("base64") };
	const producer = new Moq.Broadcast.Producer();
	const consumer = producer.consume();
	const catalog = new Signal<Catalog.Root>({ video: { renditions: { video } } });
	const broadcast = new Signal<Broadcast | undefined>({
		out: { catalog },
		relativeBroadcast: () => consumer,
	} as unknown as Broadcast);
	const source = new Source({ broadcast, supported: async () => true });
	const sync = new Sync({ delay: "instant" });
	const enabled = new Signal(true);
	const decoder = new Decoder({ source, sync, enabled });
	await microtasks();
	return {
		decoder,
		enabled,
		catalog,
		submitted,
		outputs,
		get resets() {
			return resets;
		},
		close() {
			decoder.close();
			source.close();
			sync.close();
			consumer.close();
			producer.close();
			next.mockRestore();
			for (const [name, original] of [
				["VideoDecoder", originalDecoder],
				["EncodedVideoChunk", originalChunk],
			] as const) {
				if (original) Object.defineProperty(globalThis, name, original);
				else Reflect.deleteProperty(globalThis, name);
			}
		},
	};
}

it("promoting from no active track holds its picture and timestamp until a frame arrives", async () => {
	const playback = await guardedPlayback("legacy", [sample(10, 1000)]);
	try {
		const held = playback.decoder.out.frame.peek();
		expect(held).toBeDefined();
		expect(playback.decoder.out.timestamp.peek()).toBe(Time.Milli(1));
		playback.enabled.set(false);
		await microtasks();
		playback.enabled.set(true);
		await microtasks();
		expect(playback.decoder.out.frame.peek()).toBe(held);
		expect(playback.decoder.out.timestamp.peek()).toBe(Time.Milli(1));
		expect((held as unknown as Picture).closed).toBe(false);
	} finally {
		playback.close();
	}
});

it("clears the picture once no rendition is selectable", async () => {
	const playback = await guardedPlayback("legacy", [sample(10, 1000)]);
	try {
		const held = playback.decoder.out.frame.peek();
		expect(held).toBeDefined();

		const video = playback.catalog.peek().video?.renditions.video;
		if (!video) throw new Error("missing rendition");
		playback.catalog.set({ video: { renditions: { video: { ...video, enabled: false } } } });
		await microtasks();
		expect(playback.decoder.out.frame.peek()).toBeUndefined();
		expect(playback.decoder.out.timestamp.peek()).toBeUndefined();
		expect((held as unknown as Picture).closed).toBe(true);
	} finally {
		playback.close();
	}
});

// A late marker from older history raises a discontinuity without making older groups newer.
function marker(group: number, end: number, discontinuity: number): Read {
	return { group, discontinuity, continuous: false, frame: undefined, end: Time.Micro(end) };
}

for (const kind of ["legacy", "cmaf"] as const) {
	it(`${kind} skips older groups before decode, even across a stale marker's discontinuity`, async () => {
		const playback = await guardedPlayback(kind, [
			sample(10, 1000),
			sample(9, 900),
			sample(10, 1100),
			marker(8, 850, 1),
			sample(7, 700, 1),
			sample(11, 1200, 1),
		]);
		try {
			expect(playback.submitted).toEqual([1000, 1100, 1200]);
			// Rejected payloads were still received, but never counted as frames.
			expect(playback.decoder.out.stats.peek()).toEqual({ frameCount: 3, bytesReceived: 5 });
		} finally {
			playback.close();
		}
	});

	// A group may restart at its own start, so a picture already queued from later in that
	// group is above the new keyframe. It must not come out afterwards and late-reject it.
	it(`${kind} drops a picture queued before a discontinuity`, async () => {
		const queued = 60_000_000;
		const playback = await guardedPlayback(kind, [sample(1, 0), sample(1, queued), sample(2, 0, 1)], [queued]);
		try {
			expect(playback.submitted).toEqual([0, queued, 0]);
			expect(playback.outputs).toEqual([0, 0]);
			expect(playback.resets).toBe(1);
			expect(playback.decoder.out.timestamp.peek()).toBe(Time.Milli(0));
		} finally {
			playback.close();
		}
	});

	// A stale marker (or a max-delay hole) can bump the playhead while the live group is only
	// partly delivered, so the next frame is a delta that still needs the decoder's references.
	it(`${kind} keeps decoding a delta that follows a discontinuity`, async () => {
		const playback = await guardedPlayback(kind, [
			sample(10, 1000),
			sample(10, 1100, 0, false),
			marker(8, 850, 1),
			sample(10, 1200, 1, false),
			sample(11, 1300, 1),
		]);
		try {
			expect(playback.submitted).toEqual([1000, 1100, 1200, 1300]);
			expect(playback.resets).toBe(0);
		} finally {
			playback.close();
		}
	});
}

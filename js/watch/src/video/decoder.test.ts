import { describe, expect, it, spyOn } from "bun:test";
import { readFileSync } from "node:fs";
import { Manual, Micro, Milli, Nano, now, type Second, yieldNow } from "@moq/clock";
import * as Catalog from "@moq/hang/catalog";
import * as Container from "@moq/hang/container";
import * as Moq from "@moq/net";
import { Effect, Signal } from "@moq/signals";
import { ringSamples } from "../audio/latency";
import { AudioRingBuffer } from "../audio/ring-buffer";
import type { Broadcast } from "../broadcast";
import { type Delay, Sync } from "../sync";
import { Decoder } from "./decoder";
import { Source } from "./source";

const flush = () => yieldNow();

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
function closedConsumer(path: string): Moq.Broadcast.Consumer {
	return { closed: new Signal("ended"), path } as unknown as Moq.Broadcast.Consumer;
}

// Each source is another broadcast, at its own path.
let sources = 0;
function watchBroadcast(catalog: Signal<Catalog.Root>): Broadcast {
	const media = closedConsumer(`source-${sources++}`);
	return {
		out: { catalog, active: new Signal(undefined) },
		relativeBroadcast: () => media,
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
	const sync = new Sync({ delay: Milli(0) });
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
			expect(decoder.out.jitter.peek()).toBe(Milli(260));

			// A rise on the catalog this rendition subscribed to still resizes the floor.
			outgoing.set({
				video: { renditions: { video: config({ delay: 300, jitter: 60 }) } },
			});
			await settle();
			expect(decoder.out.jitter.peek()).toBe(Milli(360));

			broadcast.set(
				watchBroadcast(
					new Signal<Catalog.Root>({
						video: { renditions: { video: config({ jitter: 20 }) } },
					}),
				),
			);
			await settle();
			// The old frames are still on screen. The new catalog's same name is not their floor.
			expect(decoder.out.jitter.peek()).toBe(Milli(360));
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
			expect(decoder.out.jitter.peek()).toBe(Milli(20));

			broadcast.set(
				watchBroadcast(
					new Signal<Catalog.Root>({
						video: { renditions: { video: config({ delay: 200 }) } },
					}),
				),
			);
			await settle();
			expect(decoder.out.jitter.peek()).toBe(Milli(200));

			const newest = new Signal<Catalog.Root>({
				video: { renditions: { video: config({ delay: 500 }) } },
			});
			broadcast.set(watchBroadcast(newest));
			await settle();
			// The first switch is still pending, so the name did not change. The new catalog still has to win.
			expect(decoder.out.jitter.peek()).toBe(Milli(500));

			newest.set({
				video: { renditions: { video: config({ delay: 700 }) } },
			});
			await settle();
			expect(decoder.out.jitter.peek()).toBe(Milli(700));
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
			expect(decoder.out.jitter.peek()).toBe(Milli(260));

			broadcast.set(
				watchBroadcast(
					new Signal<Catalog.Root>({
						video: { renditions: { sd: config({ jitter: 20 }) } },
					}),
				),
			);
			await settle();
			expect(decoder.out.jitter.peek()).toBe(Milli(260));
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

type Read = Container.Read;

// A manual clock installed at `ms`, so parked sleeps only wake when a test advances it.
function manualAt(ms: number) {
	const clock = new Manual();
	clock.jump(Nano.fromMilli(Milli(ms)));
	return { advance: (by: number) => clock.advance(Nano.fromMilli(Milli(by))), installed: clock.install() };
}
function sample(group: number, timestamp: number, keyframe = true): Read {
	return {
		kind: "frame",
		group,
		frame: {
			arrival: Nano.toMilli(Nano(now())),
			timestamp: Micro(timestamp),
			payload: new Uint8Array([1]),
			keyframe,
		},
	};
}
const gap: Read = { kind: "gap", timestamp: Micro(0), duration: Micro(0) };

// `reads` stands in for the container consumer, unless `live`, which reads what tests publish into
// `producer` through real consumers instead.
async function guardedPlayback(
	kind: "legacy" | "cmaf",
	reads: Read[],
	hold: readonly number[] = [],
	live = false,
	buffer = 0,
) {
	// Load the optional container before the fixture drains only reactive microtasks.
	if (kind === "cmaf") await import("@moq/hang/cmaf");
	const originalDecoder = Object.getOwnPropertyDescriptor(globalThis, "VideoDecoder");
	const originalChunk = Object.getOwnPropertyDescriptor(globalThis, "EncodedVideoChunk");
	const submitted: number[] = [];
	const outputs: number[] = [];
	const pictures: Picture[] = [];
	const owed = new Set(hold);
	const pending: Array<() => void> = [];
	let resets = 0;
	const errors: VideoDecoderInit["error"][] = [];
	const frames: VideoDecoderInit["output"][] = [];
	const configures: VideoDecoderConfig[] = [];
	// A read that is an Error stands in for a malformed container read.
	const next = spyOn(Container.Consumer.prototype, "next").mockImplementation(async () => {
		const read = reads.shift() as Read | Error | undefined;
		if (read instanceof Error) throw read;
		return read;
	});
	if (live) next.mockRestore();
	class Codec {
		state = "unconfigured";
		// Like WebCodecs, a configure requires a key chunk before any delta.
		#keyRequired = true;
		readonly callbacks: VideoDecoderInit;
		constructor(callbacks: VideoDecoderInit) {
			this.callbacks = callbacks;
			errors.push(callbacks.error);
			frames.push(callbacks.output);
		}
		configure(config: VideoDecoderConfig) {
			configures.push(config);
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
				const picture = new Picture(chunk.timestamp);
				pictures.push(picture);
				void this.callbacks.output(picture as unknown as VideoFrame);
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
		video.container = {
			kind,
			init: JSON.parse(readFileSync(new URL("../../../hang/src/cmaf/fixtures.json", import.meta.url), "utf8"))
				.videoInit,
		};
	// The catalog's own broadcast, and the one the rendition reads. They differ once only one of
	// them is republished, as with a rendition's `broadcast` override.
	let producer = new Moq.Broadcast.Producer();
	let mediaProducer = producer;
	const active = new Signal<Moq.Broadcast.Consumer>(producer.consume());
	const media = new Signal<Moq.Broadcast.Consumer>(producer.consume());
	const catalog = new Signal<Catalog.Root>({ video: { renditions: { video } } });
	const broadcast = new Signal<Broadcast | undefined>({
		out: { catalog, active },
		relativeBroadcast: (effect: Effect) => effect.get(media),
	} as unknown as Broadcast);
	const closeAll = () => {
		active.peek().close();
		media.peek().close();
		producer.close();
		mediaProducer.close();
	};
	const source = new Source({ broadcast, supported: async () => true });
	const delay = new Signal<Delay>(buffer ? Milli(100) : "instant");
	const sync = new Sync({ delay, buffer: Milli(buffer) });
	const enabled = new Signal(true);
	// Published before the decoder subscribes, since a broadcast with no request handler refuses
	// an unknown track.
	const track = live ? mediaProducer.createTrack("video", { timescale: Moq.Time.Timescale.MILLI }) : undefined;
	const decoder = new Decoder({ source, sync, enabled });
	await microtasks();
	return {
		decoder,
		delay,
		fail: (error: DOMException, index = -1) => errors.at(index)?.(error),
		emit: (timestamp: number, index = -1) => frames.at(index)?.(new Picture(timestamp) as unknown as VideoFrame),
		get codecs() {
			return errors.length;
		},
		switchRendition(track = "replacement", fields: Partial<Catalog.VideoConfig> = {}) {
			catalog.set({ video: { renditions: { [track]: { ...video, ...fields } } } });
		},
		configures,
		enabled,
		catalog,
		// The broadcast the rendition reads, and its first track, for a live playback to publish into.
		get producer() {
			return mediaProducer;
		},
		track,
		submitted,
		outputs,
		pictures,
		sync,
		get resets() {
			return resets;
		},
		release() {
			for (const emit of pending.splice(0)) emit();
		},
		// A restarted publisher takes the name: another instance of the same broadcast.
		republish() {
			closeAll();
			producer = new Moq.Broadcast.Producer();
			mediaProducer = producer;
			active.set(producer.consume());
			media.set(producer.consume());
		},
		// Only the catalog's broadcast restarts; the media the rendition reads stays up.
		republishCatalog() {
			active.peek().close();
			producer = new Moq.Broadcast.Producer();
			active.set(producer.consume());
		},
		// Only the media the rendition reads restarts, under the same catalog, or moves to `path`. A
		// live playback gets its video track back, published before the decoder asks for it.
		republishMedia(path?: string): Moq.Track.Producer | undefined {
			media.peek().close();
			mediaProducer = path
				? new Moq.Origin.Producer().createBroadcast(Moq.Path.from(path)).broadcast
				: new Moq.Broadcast.Producer();
			const track = live
				? mediaProducer.createTrack("video", { timescale: Moq.Time.Timescale.MILLI })
				: undefined;
			media.set(mediaProducer.consume());
			return track;
		},
		close() {
			decoder.close();
			source.close();
			sync.close();
			closeAll();
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
		expect(playback.decoder.out.timestamp.peek()).toBe(Milli(1));
		playback.enabled.set(false);
		await microtasks();
		playback.enabled.set(true);
		await microtasks();
		expect(playback.decoder.out.frame.peek()).toBe(held);
		expect(playback.decoder.out.timestamp.peek()).toBe(Milli(1));
		expect((held as unknown as Picture).closed).toBe(false);
	} finally {
		playback.close();
	}
});

it("decodes an interlaced rendition in software and marks its frames interlaced", async () => {
	const reads: Read[] = [];
	const playback = await guardedPlayback("legacy", reads);
	try {
		expect(playback.configures.at(-1)?.hardwareAcceleration).toBeUndefined();

		reads.push(sample(10, 1000));
		playback.switchRendition("video", { interlaced: true });
		await microtasks();
		expect(playback.configures.at(-1)?.hardwareAcceleration).toBe("prefer-software");
		expect(playback.decoder.out.frame.peek()).toBeDefined();
		expect(playback.decoder.out.interlaced.peek()).toBe(true);
	} finally {
		playback.close();
	}
});

it("switches to a republished broadcast at once, its timeline starting over", async () => {
	const reads = [sample(10, 10_000_000)];
	const playback = await guardedPlayback("legacy", reads);
	try {
		expect(playback.decoder.out.timestamp.peek()).toBe(Milli(10_000));
		const reset = spyOn(playback.sync, "reset");

		reads.push(sample(0, 1000));
		playback.republish();
		await microtasks();

		// Waiting to catch up to the old picture would hold it until the new timeline passed 10s.
		expect(playback.decoder.out.timestamp.peek()).toBe(Milli(1));
		expect(reset).toHaveBeenCalledTimes(1);
	} finally {
		playback.close();
	}
});

it("switches at once when only the media a rendition reads is republished", async () => {
	const reads = [sample(10, 10_000_000)];
	const playback = await guardedPlayback("legacy", reads);
	try {
		const reset = spyOn(playback.sync, "reset");

		reads.push(sample(0, 1000));
		playback.republishMedia();
		await microtasks();

		expect(playback.decoder.out.timestamp.peek()).toBe(Milli(1));
		expect(reset).toHaveBeenCalledTimes(1);
	} finally {
		playback.close();
	}
});

it("keeps the clock when only the catalog's broadcast is republished", async () => {
	const reads = [sample(10, 10_000_000)];
	const playback = await guardedPlayback("legacy", reads);
	try {
		const reset = spyOn(playback.sync, "reset");

		playback.republishCatalog();
		await microtasks();

		// The media is the same instance, so its timeline did not start over.
		expect(playback.decoder.out.timestamp.peek()).toBe(Milli(10_000));
		expect(reset).not.toHaveBeenCalled();
	} finally {
		playback.close();
	}
});

it("holds the picture and decoder while the only rendition is disabled", async () => {
	const playback = await guardedPlayback("legacy", [sample(10, 1000)]);
	try {
		const held = playback.decoder.out.frame.peek();
		expect(held).toBeDefined();
		const codecs = playback.codecs;

		const video = playback.catalog.peek().video?.renditions.video;
		if (!video) throw new Error("missing rendition");
		playback.catalog.set({ video: { renditions: { video: { ...video, enabled: false } } } });
		await microtasks();
		// The last picture before the disable stays up; the stock publisher makes it black.
		expect(playback.decoder.out.frame.peek()).toBe(held);

		playback.catalog.set({ video: { renditions: { video } } });
		await microtasks();
		expect(playback.codecs).toBe(codecs);
	} finally {
		playback.close();
	}
});

it("clears the picture once no rendition is selectable", async () => {
	const playback = await guardedPlayback("legacy", [sample(10, 1000)]);
	try {
		const held = playback.decoder.out.frame.peek();
		expect(held).toBeDefined();

		playback.catalog.set({});
		await microtasks();
		expect(playback.decoder.out.frame.peek()).toBeUndefined();
		expect(playback.decoder.out.timestamp.peek()).toBeUndefined();
		expect((held as unknown as Picture).closed).toBe(true);
	} finally {
		playback.close();
	}
});

// A marker from older history cannot make older groups newer.
function marker(group: number, end: number): Read {
	return { kind: "end", group, end: Micro(end) };
}

for (const kind of ["legacy", "cmaf"] as const) {
	// Another track skips the shared playhead to `skipTo` while a 1s picture is still inside
	// WebCodecs (`queued`) or parked in Sync.wait, with a 100ms delay and no buffer. Reports what
	// was showing before the skip and whether the picture presented once its deadline came due.
	async function skipDuring(stage: "queued" | "parked", skipTo: number) {
		const clock = manualAt(5000);
		const reads: Read[] = [];
		const picture = 1_000_000;
		const playback = await guardedPlayback(kind, reads, stage === "queued" ? [picture] : []);
		try {
			playback.enabled.set(false);
			playback.delay.set(Milli(100));
			await microtasks();
			reads.push(sample(1, picture));
			playback.enabled.set(true);
			await microtasks();
			expect(playback.outputs).toEqual(stage === "queued" ? [] : [picture]);
			const before = playback.decoder.out.frame.peek();

			playback.sync.received(Milli(skipTo), Milli(5000));
			playback.release();
			await microtasks();
			const due = playback.decoder.out.frame.peek();
			await clock.advance(100);
			const after = playback.decoder.out.frame.peek();
			return { before, after, presented: after !== due };
		} finally {
			playback.close();
			await microtasks();
			clock.installed.close();
		}
	}

	for (const stage of ["queued", "parked"] as const) {
		it(`${kind} drops a ${stage} picture another track skips the playhead past`, async () => {
			const skip = await skipDuring(stage, 10_000);
			expect(skip.presented).toBe(false);
			expect(skip.after).toBe(skip.before);
		});

		// A startup burst: audio 50ms newer re-anchors the playhead to 950ms, still behind the picture.
		it(`${kind} presents a ${stage} picture still ahead of the playhead a skip moved to`, async () => {
			const skip = await skipDuring(stage, 1050);
			expect(skip.presented).toBe(true);
		});
	}

	it(`${kind} instant presents a queued picture after a newer frame arrives`, async () => {
		const queued = 1_000_000;
		const playback = await guardedPlayback(kind, [sample(1, queued)], [queued]);
		try {
			expect(playback.outputs).toEqual([]);

			// "instant" has no cap, so every newer frame skips the playhead without passing over anything.
			playback.sync.received(Milli(10_000), Nano.toMilli(Nano(now())));
			playback.release();
			await microtasks();

			expect(playback.decoder.out.timestamp.peek()).toBe(Milli(1000));
		} finally {
			playback.close();
		}
	});

	it(`${kind} skips older groups before decode, even across a stale marker`, async () => {
		const playback = await guardedPlayback(kind, [
			sample(10, 1000),
			sample(9, 900),
			sample(10, 1100),
			marker(8, 850),
			sample(7, 700),
			sample(11, 1200),
		]);
		try {
			expect(playback.submitted).toEqual([1000, 1100, 1200]);
			// Rejected payloads were still received, but never counted as frames.
			expect(playback.decoder.out.stats.peek()).toEqual({ frameCount: 3, bytesReceived: 5 });
		} finally {
			playback.close();
		}
	});

	// A declared end permits the next group to start a fresh timeline. Pictures still inside
	// the codec must not come out above that new keyframe and late-reject it.
	it.each([false, true])(`${kind} drops codec-queued pictures at a declared restart (hole=%s)`, async (hole) => {
		const queued = 60_000_000;
		const playback = await guardedPlayback(
			kind,
			[sample(1, 0), sample(1, queued), marker(1, queued), ...(hole ? [gap] : []), sample(2, 0)],
			[queued],
		);
		try {
			expect(playback.submitted).toEqual([0, queued, 0]);
			expect(playback.outputs).toEqual([0, 0]);
			expect(playback.resets).toBe(1);
			expect(playback.decoder.out.timestamp.peek()).toBe(Milli(0));
		} finally {
			playback.close();
		}
	});

	for (const boundary of ["hole", "end"] as const) {
		it(`${kind} keeps parked pictures and the anchor across a ${boundary}`, async () => {
			const clock = manualAt(1000);
			const reads: Read[] = [];
			const playback = await guardedPlayback(kind, reads);
			const reset = spyOn(playback.sync, "reset");
			try {
				playback.enabled.set(false);
				playback.delay.set(Milli(1000));
				await microtasks();
				reads.push(sample(0, 0), sample(0, 100_000, false), boundary === "hole" ? gap : marker(0, 200_000));
				if (boundary === "hole") reads.push(sample(2, 200_000));
				playback.enabled.set(true);
				await microtasks();
				expect(reset).not.toHaveBeenCalled();
				expect(playback.pictures.find((picture) => picture.timestamp === 100_000)?.closed).toBe(false);
				expect(playback.resets).toBe(boundary === "hole" ? 1 : 0);
				await clock.advance(1200);
				expect(playback.decoder.out.timestamp.peek()).toBe(Milli(boundary === "hole" ? 200 : 100));
			} finally {
				reset.mockRestore();
				playback.close();
				await microtasks();
				clock.installed.close();
			}
		});
	}

	// A stale marker can arrive while the live group is only partly delivered. Its next delta
	// still needs the decoder's references.
	it(`${kind} keeps decoding a delta that follows a stale marker`, async () => {
		const playback = await guardedPlayback(kind, [
			sample(10, 1000),
			sample(10, 1100, false),
			marker(8, 850),
			sample(10, 1200, false),
			sample(11, 1300),
		]);
		try {
			expect(playback.submitted).toEqual([1000, 1100, 1200, 1300]);
			expect(playback.resets).toBe(0);
		} finally {
			playback.close();
		}
	});
}

it("releases a failed video subscription and recovers only on a new source", async () => {
	const close = spyOn(Moq.Track.Subscriber.prototype, "close");
	const playback = await guardedPlayback("legacy", []);
	try {
		const before = close.mock.calls.length;
		const failure = new DOMException("invalid encoded video", "EncodingError");
		playback.fail(failure);
		await microtasks();
		expect(playback.decoder.out.error.peek()).toBe(failure);
		expect(close.mock.calls.length).toBeGreaterThan(before);
		playback.enabled.set(false);
		await microtasks();
		playback.enabled.set(true);
		await microtasks();
		expect(playback.decoder.out.error.peek()).toBe(failure);
		playback.republish();
		await microtasks();
		expect(playback.decoder.out.error.peek()).toBeUndefined();
	} finally {
		playback.close();
		close.mockRestore();
	}
});

it("clears a video failure when a source without video is selected while hidden", async () => {
	const playback = await guardedPlayback("legacy", []);
	try {
		const failure = new DOMException("invalid encoded video", "EncodingError");
		playback.fail(failure);
		await microtasks();
		playback.enabled.set(false);
		await microtasks();
		expect(playback.decoder.out.error.peek()).toBe(failure);
		// Another broadcast, one advertising no video rendition.
		playback.republishCatalog();
		playback.catalog.set({});
		await microtasks();
		expect(playback.decoder.out.error.peek()).toBeUndefined();
	} finally {
		playback.close();
	}
});

it("keeps a video failure while the same rendition is muted by its publisher", async () => {
	const playback = await guardedPlayback("legacy", []);
	try {
		const failure = new DOMException("invalid encoded video", "EncodingError");
		playback.fail(failure);
		await microtasks();
		playback.catalog.set({});
		await microtasks();
		playback.switchRendition("video");
		await microtasks();
		expect(playback.decoder.out.error.peek()).toBe(failure);
		expect(playback.codecs).toBe(1);
	} finally {
		playback.close();
	}
});

for (const kind of ["legacy", "cmaf"] as const) {
	// A failure in the read loop, not a WebCodecs callback, must still reach `error` and release
	// the subscription, instead of leaving the player apparently live.
	for (const [failure, reads] of [
		["a malformed read", [new Error("malformed frame")] as unknown as Read[]],
		["a decode that throws", [sample(1, 100_000, false)]],
	] as const) {
		it(`reports ${failure} on ${kind} and releases its subscription`, async () => {
			const close = spyOn(Moq.Track.Subscriber.prototype, "close");
			const playback = await guardedPlayback(kind, [...reads]);
			try {
				await microtasks();
				expect(playback.decoder.out.error.peek()).toBeInstanceOf(Error);
				expect(close).toHaveBeenCalled();
			} finally {
				playback.close();
				close.mockRestore();
			}
		});
	}

	it(`reports the outgoing ${kind} decoder failing during a rendition handover`, async () => {
		const playback = await guardedPlayback(kind, [sample(1, 100_000)]);
		try {
			expect(playback.decoder.out.timestamp.peek()).toBe(Milli(100));
			playback.switchRendition();
			await microtasks();
			expect(playback.codecs).toBe(2);
			const failure = new DOMException("outgoing decoder failed", "EncodingError");
			playback.fail(failure, 0);
			await microtasks();
			expect(playback.decoder.out.error.peek()).toBe(failure);
			playback.emit(200_000, 1);
			await microtasks();
			expect(playback.decoder.out.timestamp.peek()).toBe(Milli(200));
			expect(playback.decoder.out.error.peek()).toBeUndefined();
			playback.switchRendition("video");
			await microtasks();
			expect(playback.codecs).toBe(3);
		} finally {
			playback.close();
		}
	});

	it(`keeps a failed pending ${kind} rendition out of the active slot`, async () => {
		const playback = await guardedPlayback(kind, [sample(1, 100_000)]);
		try {
			playback.switchRendition();
			await microtasks();
			const failure = new DOMException("replacement decoder failed", "EncodingError");
			playback.fail(failure, 1);
			await microtasks();
			expect(playback.decoder.out.error.peek()).toBe(failure);
			// Healthy outgoing frames must not conceal the selected rendition's failure.
			playback.emit(200_000, 0);
			await microtasks();
			expect(playback.decoder.out.error.peek()).toBe(failure);
			playback.fail(new DOMException("outgoing decoder failed", "EncodingError"), 0);
			await microtasks();
			expect(playback.decoder.out.jitter.peek()).toBeUndefined();
			expect(playback.decoder.out.timestamp.peek()).toBeUndefined();
			expect(playback.decoder.out.error.peek()).toBe(failure);
			playback.enabled.set(false);
			await microtasks();
			playback.enabled.set(true);
			await microtasks();
			expect(playback.codecs).toBe(2);
		} finally {
			playback.close();
		}
	});
}

it("a shared audio clock reset cannot present a parked video frame", async () => {
	const clock = manualAt(1000);
	const reads: Read[] = [];
	const playback = await guardedPlayback("legacy", reads);
	try {
		playback.enabled.set(false);
		playback.delay.set(Milli(1000));
		await microtasks();
		reads.push(sample(0, 0), sample(0, 100_000, false));
		playback.enabled.set(true);
		await microtasks();
		expect(playback.submitted).toEqual([0, 100_000]);
		expect(playback.decoder.out.timestamp.peek()).toBe(Milli(0));
		// Audio can reset the shared reference while this video's own container
		// discontinuity remains unchanged.
		playback.sync.reset();
		await microtasks();
		expect(playback.decoder.out.timestamp.peek()).toBe(Milli(0));
		// The next utterance anchors the clock past the parked frame's deadline.
		playback.sync.received(Milli(2000));
		await microtasks();
		expect(playback.decoder.out.timestamp.peek()).toBe(Milli(0));
	} finally {
		playback.close();
		await microtasks();
		clock.installed.close();
	}
});

it("closing the track releases a parked video wait", async () => {
	const clock = manualAt(1000);
	const reads: Read[] = [];
	const playback = await guardedPlayback("legacy", reads);
	const waits: Promise<boolean>[] = [];
	const original = playback.sync.wait.bind(playback.sync);
	const wait = spyOn(playback.sync, "wait").mockImplementation((timestamp, signal) => {
		const waiting = original(timestamp, signal);
		waits.push(waiting);
		return waiting;
	});
	try {
		playback.enabled.set(false);
		playback.delay.set(Milli(1000));
		await microtasks();
		reads.push(sample(0, 0), sample(0, 100_000, false));
		playback.enabled.set(true);
		await microtasks();
		expect(playback.submitted).toEqual([0, 100_000]);
		expect(waits.map((waiting) => Bun.peek.status(waiting))).toContain("pending");
		// Nothing moves the shared clock, but the closed track must not leave its wait behind.
		playback.enabled.set(false);
		await microtasks();
		expect(waits.map((waiting) => Bun.peek.status(waiting))).not.toContain("pending");
	} finally {
		wait.mockRestore();
		playback.close();
		await microtasks();
		clock.installed.close();
	}
});

it("a reset drops a parked video frame even when the next anchor restores the reference", async () => {
	const clock = manualAt(1000);
	const reads: Read[] = [];
	const playback = await guardedPlayback("legacy", reads);
	try {
		playback.enabled.set(false);
		playback.delay.set(Milli(1000));
		await microtasks();
		reads.push(sample(0, 0), sample(0, 100_000, false));
		playback.enabled.set(true);
		await microtasks();
		expect(playback.submitted).toEqual([0, 100_000]);
		// In one turn, so the reference never visibly changes and the parked sleep keeps its timer.
		playback.sync.reset();
		playback.sync.received(Milli(0));
		await microtasks();
		// The parked sleep comes due; its frame belongs to the timeline the reset ended.
		await clock.advance(1000);
		expect(playback.decoder.out.timestamp.peek()).toBe(Milli(0));
	} finally {
		playback.close();
		await microtasks();
		clock.installed.close();
	}
});

for (const kind of ["legacy", "cmaf"] as const) {
	it(`${kind} trims orphan leading pictures on join and later gap, preserving continuous groups`, async () => {
		const reads: Read[] = [
			sample(10, 1000),
			sample(10, 800, false),
			sample(10, 900, false),
			sample(10, 1100, false),
			sample(11, 2000),
			sample(11, 1800, false),
			sample(11, 1900, false),
			sample(11, 2100, false),
			gap,
			sample(13, 4000),
			sample(13, 3800, false),
			sample(13, 3900, false),
			sample(13, 4100, false),
			{ kind: "end", group: 14, end: Micro(4200) },
			sample(15, 5000),
			sample(15, 4800, false),
			sample(15, 4900, false),
			sample(15, 5100, false),
		];
		const playback = await guardedPlayback(kind, reads);
		try {
			await microtasks();
			expect(playback.submitted).toEqual([
				1000, 1100, 2000, 1800, 1900, 2100, 4000, 4100, 5000, 4800, 4900, 5100,
			]);
		} finally {
			playback.close();
		}
	});
}

// A legacy container frame: the media timestamp, then one payload byte.
function legacyFrame(media: Micro): Moq.Group.Frame {
	const timestamp = Moq.Varint.encode(media);
	const payload = new Uint8Array(timestamp.byteLength + 1);
	payload.set(timestamp, 0);
	return { payload, timestamp: Moq.Time.Timestamp.fromMicros(media) };
}

// A live video track on a manual clock: one frame per group every 33ms, flushed in runs of `burst`,
// so each run's first frame lands that much late. A replacement track continues the same timeline.
function liveVideo(clock: ReturnType<typeof manualAt>, track: Moq.Track.Producer, burst: number) {
	const FRAME = 33;
	let frame = 0;
	let sequence = 0;
	let held: number[] = [];
	return {
		burst,
		replace(next: Moq.Track.Producer) {
			track = next;
			sequence = 0;
		},
		// Advances the clock by `step` to `t`, publishing every frame due by then.
		async advance(t: number, step: number) {
			await clock.advance(step);
			for (; frame * FRAME <= t; frame++) {
				held.push(frame * FRAME);
				if (held.length < this.burst) continue;
				for (const media of held) {
					const group = track.createGroup(sequence++);
					group.writeFrame(legacyFrame(Micro.fromMilli(Milli(media))));
					group.finish();
				}
				held = [];
			}
			await new Promise((resolve) => setImmediate(resolve));
			await microtasks();
		},
	};
}

// A codec change renames the video track, and the replacement's consumer starts reading the same
// path. Restarting its arrival estimate from the cold-start prior dropped the shared "auto" delay
// at promotion and climbed back as the new track relearned the lateness, so the audio ring
// skipped ahead on the drop and went silent refilling on each rise. moq-dev/moq#5635.
it("a renamed video track keeps the delay, so audio neither skips nor goes silent", async () => {
	const clock = manualAt(0);
	const playback = await guardedPlayback("legacy", [], [], true);

	// The audio ring, following the shared delay the way the audio decoder drives the worklet.
	const RATE = 48_000;
	const STEP = 20; // ms of audio inserted and rendered per tick, one Opus frame
	const samples = (RATE * STEP) / 1000;
	const ring = new AudioRingBuffer({ rate: RATE, channels: 1, latency: Milli(STEP) });
	const follow = new Effect();
	follow.subscribe(playback.sync.out.delay, (delay) => {
		ring.setLatency(Milli.fromSecond((ringSamples(RATE, delay) / RATE) as Second));
	});

	// Flushed in runs of 12, so each run's first frame lands ~360ms late.
	const video = liveVideo(clock, playback.track as Moq.Track.Producer, 12);

	let inserted = 0;
	let played = 0;
	let silent = 0;
	const output = [new Float32Array(samples)];

	const run = async (from: number, until: number) => {
		for (let t = from + STEP; t <= until; t += STEP) {
			await video.advance(t, STEP);
			ring.write(Micro.fromMilli(Milli(t - STEP)), [new Float32Array(samples).fill(0.5)]);
			inserted += samples;
			const count = ring.read(output);
			played += count;
			if (count < samples) silent++;
		}
	};

	try {
		playback.delay.set("auto");
		await microtasks();

		// Long enough for the estimate to settle on the lateness this path shows.
		await run(0, 20_000);
		const delay = playback.sync.out.delay.peek();
		expect(delay).toBeGreaterThan(Milli(300));

		const before = { inserted, played, buffered: ring.length };
		silent = 0;

		(playback.track as Moq.Track.Producer).finish();
		(playback.track as Moq.Track.Producer).close();
		playback.switchRendition("video/1");
		video.replace(playback.producer.createTrack("video/1", { timescale: Moq.Time.Timescale.MILLI }));
		await run(20_000, 26_000);

		// The replacement read the new track and showed it.
		expect(playback.decoder.out.timestamp.peek()).toBeGreaterThan(Milli(25_000));
		const skipped = inserted - before.inserted - (played - before.played) - (ring.length - before.buffered);
		expect({ skipped, silent }).toEqual({ skipped: 0, silent: 0 });
		expect(playback.sync.out.delay.peek()).toBeGreaterThanOrEqual(delay);
	} finally {
		follow.close();
		playback.close();
		await microtasks();
		clock.installed.close();
	}
});

// Another path's arrivals say nothing about this one's, so a rendition read from elsewhere, such as
// through a `broadcast` override, learns its own estimate instead of inheriting the outgoing one.
it("a video track read from another path does not inherit the outgoing delay", async () => {
	const clock = manualAt(0);
	const playback = await guardedPlayback("legacy", [], [], true);
	const video = liveVideo(clock, playback.track as Moq.Track.Producer, 12);
	const run = async (from: number, until: number) => {
		for (let t = from + 20; t <= until; t += 20) await video.advance(t, 20);
	};

	try {
		playback.delay.set("auto");
		await microtasks();

		await run(0, 20_000);
		const warm = playback.sync.out.delay.peek();
		expect(warm).toBeGreaterThan(Milli(300));

		// The same media from another path, where every frame lands on time.
		video.replace(playback.republishMedia("elsewhere") as Moq.Track.Producer);
		video.burst = 1;
		await run(20_000, 22_000);

		expect(playback.decoder.out.timestamp.peek()).toBeGreaterThan(Milli(21_000));
		expect(playback.sync.out.delay.peek()).toBeLessThan(warm);
	} finally {
		playback.close();
		await microtasks();
		clock.installed.close();
	}
});

// The first picture is useful before the startup floor, but elapsed wall time is not buffered media.
it("buffered video holds its poster until its own span reaches the floor", async () => {
	const clock = manualAt(0);
	const playback = await guardedPlayback("legacy", [], [], true, 1000);
	if (!playback.track) throw new Error("missing video track");
	const group = playback.track.createGroup(0);
	try {
		group.writeFrame(legacyFrame(Micro(0)));
		group.writeFrame(legacyFrame(Micro(40_000)));
		await microtasks();
		expect(playback.decoder.out.frame.peek()?.timestamp).toBe(0);
		await clock.advance(500);
		expect(playback.decoder.out.frame.peek()?.timestamp).toBe(0);
		group.writeFrame(legacyFrame(Micro(100_000)));
		await microtasks();
		await clock.advance(40);
		expect(playback.decoder.out.frame.peek()?.timestamp).toBe(40_000);
	} finally {
		group.finish();
		playback.close();
		await microtasks();
		clock.installed.close();
	}
});

for (const kind of ["legacy", "cmaf"] as const) {
	it(`${kind} bounds startup across disconnected ranges, then starts on the retained timeline`, async () => {
		const clock = manualAt(0);
		const reads: Read[] = [];
		const playback = await guardedPlayback(kind, reads, [], false, 1000);
		const audio = new Signal<{ start: Milli; end: Milli } | null>(null);
		const remove = playback.sync.register(new Signal(Milli(100)), audio);
		const reset = spyOn(playback.sync, "reset");
		try {
			playback.enabled.set(false);
			await microtasks();
			for (let group = 0; group < 10; group++) {
				if (group) reads.push(gap);
				reads.push(sample(group, group * 100_000), sample(group, group * 100_000 + 40_000, false));
			}
			playback.enabled.set(true);
			await microtasks();
			await clock.advance(0);
			// Three received 40ms spans cover the 100ms floor; their holes count for nothing.
			expect(playback.submitted).toEqual([0, 40_000, 100_000, 140_000, 200_000, 240_000]);
			expect(playback.sync.playing.peek()).toBe(false);
			expect(reset).not.toHaveBeenCalled();
			audio.set({ start: Milli(0), end: Milli(100) });
			await microtasks();
			await clock.advance(0);
			expect(playback.sync.playing.peek()).toBe(true);
			expect(playback.submitted).toHaveLength(20);
			await clock.advance(1000);
			expect(playback.decoder.out.timestamp.peek()).toBe(Milli(940));
			expect(reset).not.toHaveBeenCalled();
		} finally {
			remove();
			reset.mockRestore();
			playback.close();
			await microtasks();
			clock.installed.close();
		}
	});
}

// Single-picture groups split by holes never build a received span, so coverage alone never stops
// startup decoding. Parked pictures must stay within the max delay of the opening one instead.
for (const kind of ["legacy", "cmaf"] as const) {
	it(`${kind} bounds startup pictures by the max delay when holes keep coverage empty`, async () => {
		const clock = manualAt(0);
		const reads: Read[] = [];
		for (let group = 0; group < 64; group++) {
			if (group) reads.push(gap);
			reads.push(sample(group * 2, group * 40_000));
		}
		const playback = await guardedPlayback(kind, reads, [], false, 1000);
		const remove = playback.sync.register(new Signal(Milli(100)), new Signal(null));
		try {
			await microtasks();
			await clock.advance(0);
			// The 1100ms max delay holds the opening picture through the first one at or past it.
			expect(playback.submitted).toHaveLength(29);
			expect(playback.submitted.at(-1)).toBe(1_120_000);
			expect(playback.sync.playing.peek()).toBe(false);
		} finally {
			remove();
			playback.close();
			await microtasks();
			clock.installed.close();
		}
	});
}

it("bounds startup decoding while another track is missing and releases it on close", async () => {
	const clock = manualAt(0);
	const playback = await guardedPlayback("legacy", [], [], true, 1000);
	const remove = playback.sync.register(new Signal(Milli(100)), new Signal(null));
	if (!playback.track) throw new Error("missing video track");
	const group = playback.track.appendGroup();
	try {
		for (let i = 0; i < 20; i++) group.writeFrame(legacyFrame(Micro(i * 40_000)));
		await microtasks();
		await clock.advance(1000);
		expect(playback.submitted).toEqual([0, 40_000, 80_000, 120_000]);
		expect(playback.decoder.out.frame.peek()?.timestamp).toBe(0);
		playback.close();
		await microtasks();
		expect(playback.pictures.every((frame) => frame.closed)).toBe(true);
	} finally {
		remove();
		group.finish();
		playback.close();
		await microtasks();
		clock.installed.close();
	}
});

// Resuming only once the held floor has played out would leave no decoded lookahead.
it("resumes decoding the moment shared startup releases", async () => {
	const clock = manualAt(0);
	const playback = await guardedPlayback("legacy", [], [], true, 1000);
	const audio = new Signal<{ start: Milli; end: Milli } | null>(null);
	const remove = playback.sync.register(new Signal(Milli(100)), audio);
	if (!playback.track) throw new Error("missing video track");
	const group = playback.track.appendGroup();
	try {
		for (let i = 0; i < 8; i++) group.writeFrame(legacyFrame(Micro(i * 40_000)));
		await microtasks();
		await clock.advance(0);
		expect(playback.submitted).toEqual([0, 40_000, 80_000, 120_000]);
		audio.set({ start: Milli(0), end: Milli(100) });
		await microtasks();
		await clock.advance(0);
		expect(playback.submitted).toHaveLength(8);
	} finally {
		remove();
		group.finish();
		playback.close();
		await microtasks();
		clock.installed.close();
	}
});

it("refills the video floor after a discontinuity instead of waiting on the previous poster timestamp", async () => {
	const clock = manualAt(0);
	const reads: Read[] = [
		sample(0, 0),
		sample(0, 100_000),
		marker(0, 100_000),
		sample(1, 1_000_000),
		sample(1, 1_100_000),
	];
	const playback = await guardedPlayback("legacy", reads, [], false, 1000);
	try {
		await clock.advance(100);
		await microtasks();
		await clock.advance(100);
		await microtasks();
		expect(playback.decoder.out.frame.peek()?.timestamp).toBeGreaterThanOrEqual(1_000_000);
	} finally {
		playback.close();
		await microtasks();
		clock.installed.close();
	}
});

it("continues filling when the shared floor rises while video waits on audio", async () => {
	const clock = manualAt(0);
	const playback = await guardedPlayback("legacy", [], [], true, 1000);
	const audio = new Signal<{ start: Milli; end: Milli } | null>(null);
	const remove = playback.sync.register(new Signal(Milli(100)), audio);
	if (!playback.track) throw new Error("missing video track");
	const group = playback.track.appendGroup();
	try {
		for (let i = 0; i < 7; i++) group.writeFrame(legacyFrame(Micro(i * 40_000)));
		await microtasks();
		await clock.advance(0);
		expect(playback.submitted).toEqual([0, 40_000, 80_000, 120_000]);
		playback.delay.set(Milli(200));
		audio.set({ start: Milli(0), end: Milli(200) });
		await microtasks();
		await clock.advance(240);
		expect(playback.decoder.out.frame.peek()?.timestamp).toBeGreaterThanOrEqual(200_000);
	} finally {
		remove();
		group.finish();
		playback.close();
		await microtasks();
		clock.installed.close();
	}
});

it("keeps feeding a decoder that has not output its first picture yet", async () => {
	const clock = manualAt(0);
	const playback = await guardedPlayback("legacy", [], [0, 40_000, 80_000, 120_000], true, 1000);
	if (!playback.track) throw new Error("missing video track");
	const group = playback.track.appendGroup();
	try {
		for (let i = 0; i < 5; i++) group.writeFrame(legacyFrame(Micro(i * 40_000)));
		await microtasks();
		await clock.advance(160);
		expect(playback.decoder.out.frame.peek()?.timestamp).toBe(160_000);
	} finally {
		group.finish();
		playback.close();
		await microtasks();
		clock.installed.close();
	}
});

for (const kind of ["legacy", "cmaf"] as const) {
	it(`cancels a ${kind} startup wait on disable and can enable a fresh decoder`, async () => {
		const clock = manualAt(0);
		const playback = await guardedPlayback(kind, [sample(0, 0), sample(0, 100_000)], [0, 100_000], false, 1000);
		const other = new Signal<{ start: Milli; end: Milli } | null>(null);
		const remove = playback.sync.register(new Signal(Milli(100)), other);
		try {
			playback.release();
			await microtasks();
			expect(playback.sync.playing.peek()).toBe(false);
			expect(playback.decoder.out.frame.peek()?.timestamp).toBe(0);
			playback.enabled.set(false);
			await microtasks();
			expect(playback.pictures.every((frame) => frame.closed)).toBe(true);
			const codecs = playback.codecs;
			playback.enabled.set(true);
			await microtasks();
			expect(playback.codecs).toBe(codecs + 1);
			// A late output from the cancelled codec must not replace the retained poster.
			playback.emit(900_000, 0);
			await microtasks();
			expect(playback.decoder.out.frame.peek()?.timestamp).toBe(0);
		} finally {
			remove();
			playback.close();
			await microtasks();
			clock.installed.close();
		}
	});
}

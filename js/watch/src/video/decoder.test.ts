import { describe, expect, it, spyOn } from "bun:test";
import { Manual, Micro, Milli, Nano, now } from "@moq/clock";
import * as Catalog from "@moq/hang/catalog";
import * as Container from "@moq/hang/container";
import * as Moq from "@moq/net";
import { type Effect, Signal } from "@moq/signals";
import type { Broadcast } from "../broadcast";
import { type Delay, Sync } from "../sync";
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
	clock.advance(Nano.fromMilli(Milli(ms)));
	const restore = clock.install();
	return { advance: (by: number) => clock.advanceAsync(Nano.fromMilli(Milli(by))), restore };
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

async function guardedPlayback(kind: "legacy" | "cmaf", reads: Read[], hold: readonly number[] = []) {
	const originalDecoder = Object.getOwnPropertyDescriptor(globalThis, "VideoDecoder");
	const originalChunk = Object.getOwnPropertyDescriptor(globalThis, "EncodedVideoChunk");
	const submitted: number[] = [];
	const outputs: number[] = [];
	const owed = new Set(hold);
	const pending: Array<() => void> = [];
	let resets = 0;
	const errors: VideoDecoderInit["error"][] = [];
	const frames: VideoDecoderInit["output"][] = [];
	const next = spyOn(Container.Consumer.prototype, "next").mockImplementation(async () => reads.shift());
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
	const delay = new Signal<Delay>("instant");
	const sync = new Sync({ delay });
	const enabled = new Signal(true);
	const decoder = new Decoder({ source, sync, enabled });
	// Opened before the reads below, so it observes every one of them.
	const arrivals = decoder.arrivals();
	await microtasks();
	return {
		decoder,
		arrivals,
		delay,
		fail: (error: DOMException, index = -1) => errors.at(index)?.(error),
		emit: (timestamp: number, index = -1) => frames.at(index)?.(new Picture(timestamp) as unknown as VideoFrame),
		get codecs() {
			return errors.length;
		},
		switchRendition(track = "replacement") {
			catalog.set({ video: { renditions: { [track]: video } } });
		},
		enabled,
		catalog,
		submitted,
		outputs,
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
		// Only the media the rendition reads restarts, under the same catalog.
		republishMedia() {
			media.peek().close();
			mediaProducer = new Moq.Broadcast.Producer();
			media.set(mediaProducer.consume());
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

// A late marker from older history breaks the timeline without making older groups newer.
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

			playback.sync.received(Milli(skipTo), "audio", Milli(5000));
			playback.release();
			await microtasks();
			const due = playback.decoder.out.frame.peek();
			await clock.advance(100);
			const after = playback.decoder.out.frame.peek();
			return { before, after, presented: after !== due };
		} finally {
			playback.close();
			await microtasks();
			clock.restore();
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
			playback.sync.received(Milli(10_000), "video", Nano.toMilli(Nano(now())));
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
			for (const timestamp of [1, 1.1, 1.2])
				expect(await playback.arrivals.next()).toMatchObject({
					kind: "frame",
					timestamp: Milli(timestamp),
				});
			// Rejected payloads were still received, but never counted as frames.
			expect(playback.decoder.out.stats.peek()).toEqual({ frameCount: 3, bytesReceived: 5 });
		} finally {
			playback.close();
		}
	});

	// A group may restart at its own start, so a picture already queued from later in that
	// group is above the new keyframe. It must not come out afterwards and late-reject it.
	it(`${kind} drops a picture queued before a gap`, async () => {
		const queued = 60_000_000;
		const playback = await guardedPlayback(kind, [sample(1, 0), sample(1, queued), gap, sample(2, 0)], [queued]);
		try {
			expect(playback.submitted).toEqual([0, queued, 0]);
			expect(playback.outputs).toEqual([0, 0]);
			expect(playback.resets).toBe(1);
			expect(playback.decoder.out.timestamp.peek()).toBe(Milli(0));
		} finally {
			playback.close();
		}
	});

	// A stale marker can break the timeline while the live group is only partly delivered, so the
	// next frame is a delta that still needs the decoder's references.
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
		clock.restore();
	}
});

it("closing the track releases a video wait parked across a reset", async () => {
	const clock = manualAt(1000);
	const reads: Read[] = [];
	const playback = await guardedPlayback("legacy", reads);
	const waits: Promise<void>[] = [];
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
		playback.sync.reset();
		await microtasks();
		expect(waits.map((waiting) => Bun.peek.status(waiting))).toContain("pending");
		// No frame re-anchors the shared clock, but the closed track must not leave its wait behind.
		playback.enabled.set(false);
		await microtasks();
		expect(waits.map((waiting) => Bun.peek.status(waiting))).not.toContain("pending");
	} finally {
		wait.mockRestore();
		playback.close();
		await microtasks();
		clock.restore();
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
		clock.restore();
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

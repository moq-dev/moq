/** Sweep frame and chunk size for a group stream decoded into a group and drained by a reader. */
import type { Consumer as GroupConsumer } from "../src/group.ts";
import { Producer } from "../src/group.ts";
import { NativeSession } from "../src/ietf/adapter.ts";
import { Group as IetfGroup } from "../src/ietf/object.ts";
import { Subscribe, SubscribeOk } from "../src/ietf/subscribe.ts";
import { Subscriber as IetfSubscriber } from "../src/ietf/subscriber.ts";
import { ALPN, Version as IetfVersion } from "../src/ietf/version.ts";
import { readFrames } from "../src/lite/group.ts";
import { createMockTransportPair } from "../src/mock.ts";
import * as Path from "../src/path.ts";
import { Reader, Stream } from "../src/stream.ts";
import * as Varint from "../src/varint.ts";

const frameSizes = [16, 100, 1000];
const chunkSizes = [1200, 16 * 1024, 64 * 1024];
const framesPerGroup = 50;
// A backlog of tiny frames in one large chunk: the worst case for how long batching holds the first.
const burst = { frameSize: 16, chunkSize: 64 * 1024, frames: 3000 };
// A chunk carrying many frames must cost less per frame than one carrying a single frame, or
// decoding has gone back to paying a wakeup per frame. Loose, since the fixed per-chunk cost is
// what the batched case amortizes and the machine is noisy.
const maxBatchedRatio = 0.8;
let checksum = 0;

/** Turns one group stream into a readable group, resolving `done` once the stream is handled. */
type Open = (stream: ReadableStream<Uint8Array>) => { group: Promise<GroupConsumer | undefined>; done: Promise<void> };

/** A wire format's group stream: how a frame is encoded, and where its groups are read. */
interface Protocol {
	name: string;
	// Decode about this many frames per row so each takes similar time.
	frames: number;
	encode(frameSize: number, index: number): Uint8Array[];
	// A fresh subscription per row, so one row's retained groups don't slow the next.
	subscribe(): Promise<{ open: Open; close: () => void }>;
}

// A lite-05+ group: a zigzag timestamp delta, a size, and the payload.
const lite: Protocol = {
	name: "lite",
	frames: 200_000,
	encode: (frameSize, index) => [
		Varint.encode(2 * 33_333),
		Varint.encode(frameSize),
		new Uint8Array(frameSize).fill(index),
	],
	async subscribe() {
		const open: Open = (stream) => {
			const producer = new Producer(0);
			const done = readFrames(new Reader(stream), producer, 1_000_000).then(() => producer.close());
			return { group: Promise.resolve(producer.consume()), done };
		};
		return { open, close: () => {} };
	},
};

// A moq-transport subgroup stream with no properties, through the subscriber that routes it to its
// track. Every retained group costs each later one a timeline scan (#4246), so a row stays at a
// few hundred groups.
const IETF_VERSION = IetfVersion.DRAFT_19;
const IETF_ALIAS = 9n;
const IETF_FLAGS = {
	hasExtensions: false,
	hasSubgroup: false,
	hasSubgroupObject: false,
	hasEnd: true,
	hasPriority: true,
	firstObject: true,
};

const ietf: Protocol = {
	name: "ietf",
	frames: 20_000,
	encode: (frameSize, index) => [
		Varint.encodeLeadingOnes(0),
		Varint.encodeLeadingOnes(frameSize),
		new Uint8Array(frameSize).fill(index),
	],
	async subscribe() {
		const pair = createMockTransportPair(ALPN.DRAFT_19);
		const subscriber = new IetfSubscriber({ session: new NativeSession(pair.server, IETF_VERSION, true) });
		const track = subscriber.consume(Path.from("bench")).track("video").subscribe();

		const peer = await Stream.accept(pair.client, IETF_VERSION);
		if (!peer) throw new Error("the subscriber never subscribed");
		await peer.reader.u53();
		const request = await Subscribe.decode(peer.reader, IETF_VERSION);
		await peer.writer.u53(SubscribeOk.id);
		await new SubscribeOk({ requestId: request.requestId, trackAlias: IETF_ALIAS }).encode(
			peer.writer,
			IETF_VERSION,
		);

		const ordered = track.ordered();
		let groupId = 0;
		const open: Open = (stream) => {
			const header = new IetfGroup({
				trackAlias: IETF_ALIAS,
				groupId: groupId++,
				subGroupId: 0,
				publisherPriority: 0,
				flags: IETF_FLAGS,
			});
			const done = subscriber.handleGroup(header, new Reader(stream, undefined, IETF_VERSION));
			return { group: ordered.nextGroup(), done };
		};
		return { open, close: () => track.close() };
	},
};

/** `count` frames of `frameSize` bytes, split into chunks. */
function encode(protocol: Protocol, frameSize: number, chunkSize: number, count: number): Uint8Array[] {
	const parts: Uint8Array[] = [];
	for (let index = 0; index < count; index++) parts.push(...protocol.encode(frameSize, index));
	const bytes = new Uint8Array(parts.reduce((sum, part) => sum + part.byteLength, 0));
	let offset = 0;
	for (const part of parts) {
		bytes.set(part, offset);
		offset += part.byteLength;
	}

	const chunks: Uint8Array[] = [];
	for (let start = 0; start < bytes.byteLength; start += chunkSize) {
		chunks.push(bytes.subarray(start, Math.min(start + chunkSize, bytes.byteLength)));
	}
	return chunks;
}

/** Decode one group, returning nanoseconds until the reader has every frame and until it had the first. */
async function run(open: Open, chunks: Uint8Array[], count: number): Promise<{ total: number; first: number }> {
	// Queue every chunk up front so only the decode and the handoff are timed, not the source.
	const stream = new ReadableStream<Uint8Array>({
		start(controller) {
			for (const chunk of chunks) controller.enqueue(chunk);
			controller.close();
		},
	});

	const start = performance.now();
	const { group, done } = open(stream);
	const consumer = await group;
	if (!consumer) throw new Error("no group");

	let first = 0;
	let read = 0;
	for (;;) {
		const frame = await consumer.readFrame();
		if (!frame) break;
		if (read++ === 0) first = performance.now() - start;
		checksum += frame.payload[0];
	}
	await done;
	const total = performance.now() - start;
	if (read !== count) throw new Error(`read ${read} of ${count} frames`);
	return { total: total * 1e6, first: first * 1e6 };
}

/** Decode the protocol's frames in groups of `count`, returning ns per frame and the mean µs to the first frame. */
async function measure(
	protocol: Protocol,
	chunks: Uint8Array[],
	count: number,
): Promise<{ ns: number; first: number }> {
	const groups = Math.max(1, Math.floor(protocol.frames / count));
	const { open, close } = await protocol.subscribe();

	// Warm up so the first row doesn't pay for the JIT.
	for (let index = 0; index < 20; index++) await run(open, chunks, count);

	let elapsed = 0;
	let first = 0;
	for (let index = 0; index < groups; index++) {
		const result = await run(open, chunks, count);
		elapsed += result.total;
		first += result.first;
	}
	close();
	return { ns: elapsed / (groups * count), first: first / groups / 1000 };
}

console.log("protocol,frame_bytes,chunk_bytes,frames_per_chunk,ns_per_frame,first_frame_us");

for (const protocol of [lite, ietf]) {
	const row = (frameSize: number, chunkSize: number, perChunk: number, result: { ns: number; first: number }) =>
		console.log(
			`${protocol.name},${frameSize},${chunkSize},${perChunk.toFixed(1)},${result.ns.toFixed(0)},${result.first.toFixed(2)}`,
		);

	for (const frameSize of frameSizes) {
		let single: number | undefined;
		for (const chunkSize of [frameSize + 8, ...chunkSizes]) {
			const chunks = encode(protocol, frameSize, chunkSize, framesPerGroup);
			const perChunk = framesPerGroup / chunks.length;
			const result = await measure(protocol, chunks, framesPerGroup);
			row(frameSize, chunkSize, perChunk, result);

			if (single === undefined) {
				single = result.ns;
			} else if (perChunk >= 10 && result.ns > single * maxBatchedRatio) {
				throw new Error(
					`${protocol.name}: ${frameSize} byte frames at ${perChunk.toFixed(1)} per chunk cost ${result.ns.toFixed(0)} ns/frame, over ${maxBatchedRatio}x the ${single.toFixed(0)} ns of one per chunk`,
				);
			}
		}
	}

	const chunks = encode(protocol, burst.frameSize, burst.chunkSize, burst.frames);
	row(burst.frameSize, burst.chunkSize, burst.frames / chunks.length, await measure(protocol, chunks, burst.frames));
}

if (checksum === 0) throw new Error("benchmark did no work");

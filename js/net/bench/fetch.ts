/** Sweep cached groups, independent FETCH peers, and small or chunked frame payloads. */
import { Milli } from "@moq/clock";
import { Fetch, FetchHeader, FetchOk } from "../src/ietf/fetch.ts";
import { decodeFetchObject } from "../src/ietf/object.ts";
import { Publisher } from "../src/ietf/publisher.ts";
import { NativeSession } from "../src/ietf/session.ts";
import { ALPN, Version } from "../src/ietf/version.ts";
import { createMockTransportPair } from "../src/mock.ts";
import { Producer } from "../src/origin.ts";
import * as Path from "../src/path.ts";
import { Reader, Stream } from "../src/stream.ts";
import { SEGMENT_BYTES } from "../src/util/limits.ts";

console.debug = () => {};
const version = Version.DRAFT_22;
const frames = 4;
console.log("groups,subscribers,frame_bytes,fetch_us");
for (const groups of [2, 128]) {
	for (const subscribers of [1, 8]) {
		for (const size of [16, 1000, SEGMENT_BYTES * 2]) {
			const origin = new Producer();
			const route = origin.route(Path.from("fetch"));
			const track = route.broadcast.createTrack("video", { maxAge: Milli(60_000) });
			for (let sequence = 0; sequence < groups; sequence++) {
				const group = track.appendGroup();
				// Only the selected group needs payloads; the rest expose cache-table scans.
				if (sequence === groups - 1) {
					for (let index = 0; index < frames; index++) {
						if (size <= SEGMENT_BYTES) group.writeFrame({ payload: new Uint8Array(size).fill(index) });
						else {
							const frame = group.createFrame({ size });
							for (let offset = 0; offset < size; offset += SEGMENT_BYTES)
								frame.write(new Uint8Array(Math.min(SEGMENT_BYTES, size - offset)).fill(index));
							frame.finish();
						}
					}
				}
				group.finish();
			}
			const peers = Array.from({ length: subscribers }, () => {
				const pair = createMockTransportPair(ALPN.DRAFT_22);
				const publisher = new Publisher({
					quic: pair.server,
					session: new NativeSession(pair.server, version, false),
					publish: origin.consume(),
					requiresSolicitation: true,
				});
				return { pair, publisher, uni: pair.client.incomingUnidirectionalStreams.getReader() };
			});
			const samples: number[] = [];
			try {
				for (let round = 0; round < 7; round++) {
					const start = performance.now();
					await Promise.all(
						peers.map(async ({ pair, publisher, uni }) => {
							const client = await Stream.open(pair.client, { version });
							const server = await Stream.accept(pair.server, version);
							if (!server) throw new Error("missing FETCH request stream");
							const requestId = BigInt(round);
							const groupId = BigInt(groups - 1);
							const running = publisher.runFetch(
								new Fetch({
									requestId,
									type: {
										kind: "filtered",
										trackNamespace: Path.from("fetch"),
										trackName: "video",
										filter: {
											kind: "absolute",
											startGroup: groupId,
											startObject: 0n,
											endGroup: groupId,
											endObject: BigInt(frames - 1),
										},
									},
								}),
								server,
							);
							try {
								if ((await client.reader.u53()) !== FetchOk.id) throw new Error("missing FETCH_OK");
								await FetchOk.decode(client.reader, version);
								const stream = (await uni.read()).value;
								if (!stream) throw new Error("missing FETCH data stream");
								const reader = new Reader(stream, undefined, version);
								if ((await reader.u53()) !== FetchHeader.type) throw new Error("wrong data stream");
								if ((await FetchHeader.decode(reader, version)).requestId !== requestId)
									throw new Error("wrong FETCH ID");
								for (let index = 0; index < frames; index++) {
									const object = await reader.decode((cursor) =>
										decodeFetchObject(cursor, undefined),
									);
									if (object.kind !== "object" || object.size !== size)
										throw new Error("wrong FETCH object");
									if (object.payload) {
										if (object.payload.some((byte) => byte !== index))
											throw new Error("wrong FETCH payload");
									} else {
										for (let left = size; left > 0; ) {
											const chunk = await reader.readChunk(Math.min(left, SEGMENT_BYTES));
											if (chunk.some((byte) => byte !== index))
												throw new Error("wrong FETCH chunk");
											left -= chunk.byteLength;
										}
									}
								}
								if (!(await reader.done())) throw new Error("extra FETCH objects");
							} finally {
								client.close();
								await running;
							}
						}),
					);
					if (round > 1) samples.push((performance.now() - start) * 1000);
				}
				samples.sort((a, b) => a - b);
				console.log(`${groups},${subscribers},${size},${samples[2].toFixed(2)}`);
			} finally {
				for (const peer of peers) peer.uni.releaseLock();
				origin.close();
			}
		}
	}
}

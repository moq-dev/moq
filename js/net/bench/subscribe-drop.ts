/** Sweep failed group sends over independent publishers and subscribers. */
import { Milli } from "@moq/clock";
import { Producer as GroupProducer } from "../src/group.ts";
import { randomHop } from "../src/hop.ts";
import { Publisher } from "../src/lite/publisher.ts";
import { decodeSubscribeResponseMaybe, Subscribe } from "../src/lite/subscribe.ts";
import { ALPN_07_WIP, Version } from "../src/lite/version.ts";
import { createMockTransportPair, textFrame } from "../src/mock.ts";
import { Producer as OriginProducer } from "../src/origin.ts";
import * as Path from "../src/path.ts";
import { Stream } from "../src/stream.ts";
import { Timescale } from "../src/time.ts";

const groups = 128;
const counts = [1, 4, 16];
const reps = 3;
const debug = console.debug;
console.debug = () => {};
try {
	console.log("publishers,subscribers,groups,drop_us");
	for (const publishers of counts) {
		for (const subscribers of counts) {
			const times: number[] = [];
			for (let rep = 0; rep < reps; rep++) {
				const origins = Array.from({ length: publishers }, () => {
					const origin = new OriginProducer();
					const broadcastPublication = origin.createBroadcast(Path.from("bench"));
					const broadcast = broadcastPublication.broadcast;
					broadcastPublication.announce();
					const track = broadcast.createTrack("video", { timescale: Timescale.MILLI });
					for (let sequence = 0; sequence < groups; sequence++) {
						const group = new GroupProducer(sequence);
						group.writeFrame(textFrame("frame"));
						group.finish();
						track.writeGroup(group);
					}
					track.finish();
					track.close();
					return { origin, broadcast };
				});
				// Include connection setup, excluding group creation and cache preparation.
				const start = performance.now();
				await Promise.all(
					origins.flatMap(({ origin }) =>
						Array.from({ length: subscribers }, async () => {
							const pair = createMockTransportPair(ALPN_07_WIP);
							pair.server.createUnidirectionalStream = async () => {
								throw new Error("no stream slot");
							};
							const publisher = new Publisher(
								pair.server,
								Version.DRAFT_07,
								randomHop(),
								origin.consume(),
							);
							const client = await Stream.open(pair.client, { version: Version.DRAFT_07 });
							const server = await Stream.accept(pair.server, Version.DRAFT_07);
							if (!server) throw new Error("missing subscribe stream");
							const serving = publisher.runSubscribe(
								new Subscribe({
									id: 0n,
									broadcast: Path.from("bench"),
									track: "video",
									priority: 0,
									maxDelay: Milli(30_000),
									startGroup: 0,
								}),
								server,
							);
							try {
								let dropped = 0;
								for (;;) {
									const response = await decodeSubscribeResponseMaybe(
										client.reader,
										Version.DRAFT_07,
									);
									if (!response) break;
									if ("drop" in response) dropped += response.drop.end - response.drop.start + 1;
								}
								if (dropped !== groups) throw new Error(`dropped ${dropped}, expected ${groups}`);
								client.writer.close();
								await serving;
							} finally {
								publisher.close();
								client.close();
								pair.client.close();
								pair.server.close();
							}
						}),
					),
				);
				times.push(((performance.now() - start) * 1000) / (publishers * subscribers * groups));
				for (const { origin, broadcast } of origins) {
					broadcast.close();
					origin.close();
				}
			}
			times.sort((a, b) => a - b);
			console.log(`${publishers},${subscribers},${groups},${times[Math.floor(reps / 2)].toFixed(2)}`);
		}
	}
} finally {
	console.debug = debug;
}

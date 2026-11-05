/** Sweep cached groups against independent peers requesting draft-22 fills. */
import { Milli } from "@moq/clock";
import { FetchHeader } from "../src/ietf/fetch";
import * as Filter from "../src/ietf/filter";
import { Parameters } from "../src/ietf/parameters";
import { Publisher } from "../src/ietf/publisher";
import { RequestOk } from "../src/ietf/request";
import { NativeSession } from "../src/ietf/session";
import { Subscribe, SubscribeOk, SubscribeUpdate } from "../src/ietf/subscribe";
import { ALPN, Version } from "../src/ietf/version";
import { createMockTransportPair } from "../src/mock";
import { Producer } from "../src/origin";
import * as Path from "../src/path";
import { Reader, Stream } from "../src/stream";

console.debug = () => {};
const version = Version.DRAFT_22;
console.log("groups,subscribers,fill_us");
for (const groups of [2, 16, 128]) {
	for (const subscribers of [1, 8]) {
		const origin = new Producer();
		const publication = origin.createBroadcast(Path.from("fills"));
		publication.announce();
		const track = publication.broadcast.createTrack("video", { maxAge: Milli(60_000) });
		for (let i = 0; i < groups; i++) {
			const group = track.appendGroup();
			group.writeFrame({ payload: new Uint8Array([i % 256]) });
			group.finish();
		}
		const peers = await Promise.all(
			Array.from({ length: subscribers }, async () => {
				const pair = createMockTransportPair(ALPN.DRAFT_22);
				const publisher = new Publisher({
					quic: pair.server,
					session: new NativeSession(pair.server, version, false),
					publish: origin.consume(),
					requiresSolicitation: true,
				});
				const uni = pair.client.incomingUnidirectionalStreams.getReader();
				const client = await Stream.open(pair.client, { version });
				const server = await Stream.accept(pair.server, version);
				if (!server) throw new Error("missing subscribe stream");
				const running = publisher.runSubscribe(
					new Subscribe({
						requestId: 0n,
						trackNamespace: Path.from("fills"),
						trackName: "video",
						subscriberPriority: 0,
						filter: { kind: "absolute", startGroup: BigInt(groups), startObject: 0n },
					}),
					server,
				);
				if ((await client.reader.u53()) !== SubscribeOk.id) throw new Error("missing subscribe OK");
				await SubscribeOk.decode(client.reader, version);
				return { client, uni, running };
			}),
		);
		const samples: number[] = [];
		try {
			for (let round = 0; round < 7; round++) {
				const start = performance.now();
				await Promise.all(
					peers.map(async ({ client, uni }) => {
						const id = BigInt((round + 1) * 2);
						const parameters = new Parameters();
						parameters.fillParameters = Filter.encodeFill(
							{ filter: { kind: "relative", groups: 1n }, groupOrder: "ascending", rangeFilters: false },
							version,
						);
						await client.writer.u53(SubscribeUpdate.id);
						await new SubscribeUpdate({ requestId: id, parameters }).encode(client.writer, version);
						if ((await client.reader.u53()) !== RequestOk.id) throw new Error("missing update OK");
						await RequestOk.decode(client.reader, version);
						const stream = (await uni.read()).value;
						if (!stream) throw new Error("missing fill");
						const reader = new Reader(stream, undefined, version);
						if ((await reader.u53()) !== FetchHeader.type) throw new Error("wrong fill stream");
						if ((await FetchHeader.decode(reader, version)).requestId !== id)
							throw new Error("wrong fill ID");
						if ((await reader.readAll()).byteLength === 0) throw new Error("short fill");
					}),
				);
				if (round > 1) samples.push((performance.now() - start) * 1000);
			}
			samples.sort((a, b) => a - b);
			console.log(`${groups},${subscribers},${samples[2].toFixed(2)}`);
		} finally {
			for (const peer of peers) peer.client.close();
			await Promise.all(peers.map((peer) => peer.running));
			for (const peer of peers) peer.uni.releaseLock();
			origin.close();
		}
	}
}

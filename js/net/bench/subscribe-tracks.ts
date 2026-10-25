/** Sweep origin size and subscribers while listing one newly added track. */
import { StreamCode, StreamError } from "../src/error.ts";
import { NativeSession } from "../src/ietf/adapter.ts";
import { Publisher } from "../src/ietf/publisher.ts";
import { RequestOk } from "../src/ietf/request.ts";
import { PublishSkipped, SubscribeTracks } from "../src/ietf/subscribe_tracks.ts";
import { ALPN, Version } from "../src/ietf/version.ts";
import { createMockTransportPair } from "../src/mock.ts";
import { Producer } from "../src/origin.ts";
import * as Path from "../src/path.ts";
import { Stream } from "../src/stream.ts";

const version = Version.DRAFT_22;
console.log("broadcasts,subscribers,track_added_us");
for (const broadcasts of [1, 16, 128]) {
	for (const subscribers of [1, 4, 16]) {
		const origin = new Producer();
		const sources = Array.from({ length: broadcasts }, (_, i) => {
			const source = origin.createBroadcast(Path.from(`room/${i}`));
			source.announce();
			source.createTrack("initial");
			return source;
		});
		const clients = await Promise.all(
			Array.from({ length: subscribers }, async () => {
				const pair = createMockTransportPair(ALPN.DRAFT_22);
				const publisher = new Publisher({
					quic: pair.server,
					session: new NativeSession(pair.server, version, false),
					publish: origin.consume(),
					requiresSolicitation: true,
				});
				const client = await Stream.open(pair.client, { version });
				const server = await Stream.accept(pair.server, version);
				if (!server) throw new Error("missing stream");
				const task = publisher.runSubscribeTracks(new SubscribeTracks(0n, Path.from("room")), server);
				if ((await client.reader.u53()) !== RequestOk.id) throw new Error("missing REQUEST_OK");
				await RequestOk.decode(client.reader, version);
				for (let i = 0; i < broadcasts; i++) {
					if ((await client.reader.u53()) !== PublishSkipped.id) throw new Error("missing PUBLISH_SKIPPED");
					await PublishSkipped.decode(client.reader, version);
				}
				return { pair, publisher, client, task };
			}),
		);
		const samples = [];
		for (let sample = 0; sample < 21; sample++) {
			const start = performance.now();
			const track = sources[0].createTrack(`added-${sample}`);
			await Promise.all(
				clients.map(async ({ client }) => {
					if ((await client.reader.u53()) !== PublishSkipped.id) throw new Error("missing added track");
					if ((await PublishSkipped.decode(client.reader, version)).trackName !== track.name)
						throw new Error("wrong track");
				}),
			);
			if (sample) samples.push((performance.now() - start) * 1000);
			track.abort(new Error("benchmark finished"));
		}
		samples.sort((a, b) => a - b);
		console.log(`${broadcasts},${subscribers},${samples[10].toFixed(2)}`);
		for (const { client } of clients) client.abort(new StreamError(StreamCode.Cancel));
		await Promise.all(clients.map(({ task }) => task));
		for (const { pair, publisher } of clients) {
			pair.server.close();
			publisher.close();
		}
		origin.close();
	}
}

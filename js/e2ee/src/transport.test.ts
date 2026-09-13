import { expect, test } from "bun:test";
import { Origin, Path, Time } from "@moq/net";
import { accept, connect } from "../../net/src/connection/index.ts";
import * as Ietf from "../../net/src/ietf/index.ts";
import * as Lite from "../../net/src/lite/index.ts";
import { createMockTransportPair } from "../../net/src/mock.ts";
import { Consumer, Credential, opaqueName, Producer } from "./index.ts";

const SECRET = new TextEncoder().encode("moq-e2ee-01 test secret!!!!!!!!!");
const url = new URL("https://localhost:4443/test");

function cred(): Credential {
	return new Credential({
		context: "example.com/meeting-123",
		generation: 1,
		kid: 7,
		secret: Uint8Array.from(SECRET),
	});
}

async function session(protocol: string) {
	const pair = createMockTransportPair(protocol);
	const origin = new Origin.Producer();
	const [client, server] = await Promise.all([
		connect(url, { transport: pair.client }),
		accept(pair.server, url, { publish: origin.consume() }),
	]);
	return {
		client,
		server,
		origin,
		close() {
			client.close();
			server.close();
		},
	};
}

async function groupedRoundTrip(protocol: string) {
	const { client, origin, close } = await session(protocol);
	const credential = cred();
	const name = await opaqueName(credential, "video");
	const broadcast = origin.createBroadcast(Path.from("room.hang.e2ee"));
	broadcast.announce();
	const track = broadcast.createTrack(name);
	const producer = await Producer.create({ track, credential, semanticName: "video" });

	const remote = client.consume(Path.from("room.hang.e2ee"));
	const consumer = await Consumer.create({
		track: remote.track(name).subscribe({ maxAge: 30_000 }),
		credential,
		semanticName: "video",
	});

	const group = producer.appendGroup();
	await group.writeFrame({
		payload: new TextEncoder().encode("frame-zero"),
		timestamp: Time.Timestamp.fromMillis(1),
	});
	await group.writeFrame({ payload: new TextEncoder().encode("frame-one"), timestamp: Time.Timestamp.fromMillis(2) });
	group.close();
	await producer.finish();

	const got = await consumer.nextGroup();
	expect(got?.sequence).toBe(0);
	expect(new TextDecoder().decode((await got?.readFrame())?.payload)).toBe("frame-zero");
	expect(new TextDecoder().decode((await got?.readFrame())?.payload)).toBe("frame-one");
	expect(await got?.readFrame()).toBeUndefined();

	consumer.close();
	remote.close();
	broadcast.close();
	close();
}

test("grouped tracks round-trip over moq-lite", async () => {
	await groupedRoundTrip(Lite.ALPN_05);
});

test("grouped tracks round-trip over MoQ Transport", async () => {
	await groupedRoundTrip(Ietf.ALPN.DRAFT_18);
});

test("datagrams round-trip over moq-lite", async () => {
	const { client, origin, close } = await session(Lite.ALPN_05);
	const credential = cred();
	const name = await opaqueName(credential, "audio");
	const broadcast = origin.createBroadcast(Path.from("room.hang.e2ee"));
	broadcast.announce();
	const track = broadcast.createTrack(name, { timescale: Time.Timescale.MILLI });
	const producer = await Producer.create({ track, credential, semanticName: "audio" });

	const remote = client.consume(Path.from("room.hang.e2ee"));
	const consumer = await Consumer.create({
		track: remote.track(name).subscribe(),
		credential,
		semanticName: "audio",
	});

	const received = consumer.recvDatagram();
	let stop = false;
	const pump = (async () => {
		for (let i = 0; !stop; i++) {
			await producer.appendDatagram(Time.Timestamp.fromMillis(i), new TextEncoder().encode("opus"));
			await new Promise<void>((resolve) => setTimeout(resolve, 2));
		}
	})();

	const got = await received;
	stop = true;
	await pump;
	expect(new TextDecoder().decode(got?.payload)).toBe("opus");

	producer.close();
	consumer.close();
	remote.close();
	broadcast.close();
	close();
});

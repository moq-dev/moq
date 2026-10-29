// Protected tracks over real moq-lite and MoQ Transport sessions: relays forward ciphertext.
import { expect, test } from "bun:test";
import { Milli } from "@moq/clock";
import { type Broadcast, Connection, Origin, type Path, Time } from "@moq/net";
// The in-memory WebTransport pair @moq/net tests with; not part of its published surface.
import { createMockTransportPair } from "../../net/src/mock.ts";
import * as Protected from "./broadcast.ts";
import { Credential } from "./credential.ts";

const encoder = new TextEncoder();
const decoder = new TextDecoder();
const url = new URL("https://localhost:4443/test");
const ms = (value: number) => Time.Timestamp.fromMillis(value);

function credential(): Credential {
	return new Credential({
		context: encoder.encode("example.com/meeting-123"),
		kid: 7,
		secret: encoder.encode("moq-e2ee-00 test secret!!!!!!!!!"),
	});
}

async function session(protocol: string) {
	const pair = createMockTransportPair(protocol);
	const publisher = new Origin.Producer();
	const subscriber = new Origin.Producer();
	const [client, server] = await Promise.all([
		Connection.connect({ url, transport: pair.client, consume: subscriber }),
		Connection.accept({ url, transport: pair.server, publish: publisher.consume() }),
	]);
	return {
		publisher,
		subscriber,
		close() {
			client.abort();
			server.abort();
		},
	};
}

async function active(origin: Origin.Producer, path: Path.Valid): Promise<Broadcast.Consumer> {
	const request = origin.follow(path);
	for (;;) {
		const front = request.active.peek();
		if (front) return front;
		await request.active.changed();
	}
}

// Whether the wire carries the route's epoch, and the track's timestamps. Neither is
// authenticated, so the frames open either way. A lite-05 receiver drops timestamps.
const transports = [
	["moq-lite-07", "moq-lite-07-wip", true, true],
	["moq-lite-05", "moq-lite-05", false, false],
	["moq-transport-18", "moqt-18", true, true],
] as const;

for (const [name, protocol, routed, timed] of transports) {
	test(`${name}: grouped frames roundtrip encrypted`, async () => {
		const { publisher, subscriber, close } = await session(protocol);
		const cred = credential();
		const generation = cred.produce();
		const path = await cred.path("meeting.hang");
		const broadcastPublication = publisher.createBroadcast(path);
		const broadcast = broadcastPublication.broadcast;
		const video = await new Protected.Producer(broadcast, generation).createTrack("video", {
			timescale: Time.Timescale.MILLI,
		});
		broadcastPublication.announce({ epoch: generation.epoch });

		const group = video.appendGroup();
		await group.writeFrame(ms(1), encoder.encode("keyframe"));
		await group.writeFrame(ms(2), encoder.encode("delta"));

		const remote = await active(subscriber, path);
		// The epoch rides the route where the wire carries it; elsewhere the application supplies it.
		if (routed) expect(remote.epoch).toBe(generation.epoch);
		const epoch = routed && remote.epoch ? remote.epoch : generation.epoch;
		const consumer = await new Protected.Consumer(remote, cred.consume(epoch)).subscribe("video", {
			maxDelay: Milli(60_000),
		});

		const received = await consumer.recvGroup();
		if (!received) throw new Error("expected a group");
		expect(received.sequence).toBe(0);
		const first = await received.readFrame();
		expect(decoder.decode(first?.plaintext)).toBe("keyframe");
		expect(first?.timestamp?.as(Time.Timescale.MILLI)).toBe(timed ? 1 : undefined);
		expect(decoder.decode((await received.readFrame())?.plaintext)).toBe("delta");

		// Later frames on the open group, and a second group.
		await group.writeFrame(ms(3), encoder.encode("late"));
		group.finish();
		expect(decoder.decode((await received.readFrame())?.plaintext)).toBe("late");
		expect(await received.readFrame()).toBeUndefined();

		const next = video.appendGroup();
		await next.writeFrame(ms(4), encoder.encode("next"));
		next.finish();
		const second = await consumer.recvGroup();
		expect(second?.sequence).toBe(1);
		expect(decoder.decode((await second?.readFrame())?.plaintext)).toBe("next");

		consumer.close();
		video.finish();
		video.close();
		broadcast.close();
		close();
	});
}

for (const [name, protocol] of [
	["moq-lite-07", "moq-lite-07-wip"],
	["moq-transport-18", "moqt-18"],
] as const)
	test(`${name}: datagrams roundtrip encrypted`, async () => {
		const { publisher, subscriber, close } = await session(protocol);
		const cred = credential();
		const generation = cred.produce();
		const path = await cred.path("meeting.hang");
		const broadcastPublication = publisher.createBroadcast(path);
		const broadcast = broadcastPublication.broadcast;
		const audio = await new Protected.Producer(broadcast, generation).createTrack("audio", {
			timescale: Time.Timescale.MILLI,
		});
		broadcastPublication.announce({ epoch: generation.epoch });

		// A group first, so the subscription is serving before datagrams, which are never cached.
		const group = audio.appendGroup();
		await group.writeFrame(ms(0), encoder.encode("init"));
		group.finish();

		const remote = await active(subscriber, path);
		const consumer = await new Protected.Consumer(remote, cred.consume(remote.epoch ?? generation.epoch)).subscribe(
			"audio",
			{ maxDelay: Milli(60_000) },
		);
		expect(decoder.decode((await (await consumer.recvGroup())?.readFrame())?.plaintext)).toBe("init");

		// Sequences continue past the group, preserved hop to hop by insertDatagram.
		const sequence = await audio.appendDatagram(ms(20), encoder.encode("opus"));
		expect(sequence).toBe(1);
		const event = await consumer.recvDatagram();
		expect(event?.kind).toBe("datagram");
		if (event?.kind !== "datagram") return;
		expect(event.datagram.sequence).toBe(1);
		expect(event.datagram.timestamp?.as(Time.Timescale.MILLI)).toBe(20);
		expect(decoder.decode(event.datagram.plaintext)).toBe("opus");

		consumer.close();
		audio.finish();
		audio.close();
		broadcast.close();
		close();
	});

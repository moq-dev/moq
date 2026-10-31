// Signed tracks over a real moq-lite session: the subscriber delivers only what verifies.
import { expect, test } from "bun:test";
import { Milli } from "@moq/clock";
import { type Broadcast, Connection, Origin, Path, Time } from "@moq/net";
// The in-memory WebTransport pair @moq/net tests with; not part of its published surface.
import { createMockTransportPair } from "../../net/src/mock.ts";
import * as Signed from "./broadcast.ts";
import { Failure } from "./failure.ts";
import type * as Group from "./group.ts";
import { type PublicKey, SecretKey } from "./key.ts";
import { Scope } from "./scope.ts";
import * as Track from "./track.ts";

const encoder = new TextEncoder();
const decoder = new TextDecoder();
const url = new URL("https://localhost:4443/test");
const subscription = { maxDelay: Milli(60_000) };

async function session() {
	const pair = createMockTransportPair("moq-lite-07-wip");
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

/** A signed broadcast at `path` on a fresh session, and the raw net broadcast under it. */
async function publish(key: SecretKey, path: Path.Valid) {
	const net = await session();
	const publication = net.publisher.createBroadcast(path);
	const signed = new Signed.Producer(publication.broadcast, key, path);
	publication.announce({ epoch: signed.epoch });
	return {
		...net,
		signed,
		/** A signed track over a net track the test can also write forged frames to. */
		track(name: string) {
			const raw = publication.broadcast.createTrack(name);
			return { signed: Track.Producer.make(raw, key, new Scope(path, signed.epoch, name), { next: 0 }), raw };
		},
		async consume(expected: PublicKey): Promise<Signed.Consumer> {
			return new Signed.Consumer(await active(net.subscriber, path), expected, path);
		},
	};
}

async function payload(group: Group.Consumer | undefined): Promise<string | undefined> {
	const frame = await group?.readFrame();
	return frame && decoder.decode(frame.payload);
}

test("frames roundtrip signed, timestamps included", async () => {
	const key = await SecretKey.generate();
	const path = Path.from(`mesh/${key.public}/agent`);
	const published = await publish(key, path);
	const video = published.signed.createTrack("video", { timescale: Time.Timescale.MILLI });

	const group = video.appendGroup();
	await group.writeFrame(Time.Timestamp.fromMillis(1), encoder.encode("keyframe"));
	// Signed at the track's scale, rounded as the wire rounds it.
	await group.writeFrame(Time.Timestamp.fromMicros(2_400), new Uint8Array());

	const consumer = await published.consume(key.public);
	expect(consumer.epoch).toBe(published.signed.epoch);
	const track = consumer.subscribe("video", subscription);
	const received = await track.recvGroup();
	const first = await received?.readFrame();
	expect(decoder.decode(first?.payload)).toBe("keyframe");
	expect(first?.timestamp?.as(Time.Timescale.MILLI)).toBe(1);
	const second = await received?.readFrame();
	expect(second?.payload.byteLength).toBe(0);
	expect(second?.timestamp?.value).toBe(2);
	expect(second?.timestamp?.scale).toBe(Time.Timescale.MILLI);

	await group.writeFrame(Time.Timestamp.fromMillis(3), encoder.encode("late"));
	group.finish();
	expect(await payload(received)).toBe("late");
	expect(await received?.readFrame()).toBeUndefined();

	track.close();
	video.finish();
	video.close();
	published.signed.close();
	published.close();
});

test("a forged frame delivers nothing and ends only its group", async () => {
	const key = await SecretKey.generate();
	const path = Path.from("mesh/node/ssh");
	const published = await publish(key, path);
	const { signed, raw } = published.track("stream");
	const track = (await published.consume(key.public)).subscribe("stream", subscription);

	// A relay's forgery: unsigned frames the length of a signed one.
	const forged = raw.appendGroup();
	forged.writeFrame({ payload: new Uint8Array(80) });
	forged.writeFrame({ payload: new Uint8Array(80) });
	forged.finish();
	const group = await track.recvGroup();
	await expect(group?.readFrame()).rejects.toEqual(new Failure("signature"));
	await expect(group?.readFrame()).rejects.toEqual(new Failure("signature"));

	const next = signed.createGroup(1);
	await next.writeFrame(undefined, encoder.encode("after"));
	next.finish();
	expect(await payload(await track.recvGroup())).toBe("after");

	track.close();
	published.close();
});

test("a frame replayed into another group or track is refused", async () => {
	const key = await SecretKey.generate();
	const path = Path.from("mesh/node/ssh");
	const published = await publish(key, path);
	const a = published.track("a");
	const b = published.track("b");
	const tap = a.raw.subscribe();
	const consumer = await published.consume(key.public);
	const readA = consumer.subscribe("a", subscription);
	const readB = consumer.subscribe("b", subscription);

	const group = a.signed.appendGroup();
	await group.writeFrame(undefined, encoder.encode("original"));
	group.finish();
	const body = (await (await tap.recvGroup())?.readFrame())?.payload;
	if (!body) throw new Error("expected the signed body");
	expect(await payload(await readA.recvGroup())).toBe("original");

	// Group 1 of the same track.
	const replay = a.raw.appendGroup();
	replay.writeFrame({ payload: body });
	replay.finish();
	await expect((await readA.recvGroup())?.readFrame()).rejects.toEqual(new Failure("signature"));

	// The same group and index of another track.
	const moved = b.raw.createGroup(0);
	moved.writeFrame({ payload: body });
	moved.finish();
	await expect((await readB.recvGroup())?.readFrame()).rejects.toEqual(new Failure("signature"));

	readA.close();
	readB.close();
	tap.close();
	published.close();
});

test("a group sequence is never signed twice", async () => {
	const key = await SecretKey.generate();
	const published = await publish(key, Path.from("mesh/node/ssh"));
	const track = published.signed.createTrack("stream");
	const group = track.appendGroup();
	await group.writeFrame(undefined, encoder.encode("first"));
	group.abort(new Error("retry"));

	// Net would serve the aborted sequence again, but frames of both incarnations would verify,
	// so a relay holding both could splice them into one group.
	expect(() => track.createGroup(0)).toThrow("duplicate group");
	expect(track.appendGroup().sequence).toBe(1);
	expect(() => track.createGroup(1)).toThrow("duplicate group");
	expect(track.createGroup(5).sequence).toBe(5);
	expect(() => track.createGroup(3)).toThrow("duplicate group");

	// A replacement track continues past its predecessor's sequences under the same epoch.
	track.abort(new Error("replaced"));
	const replacement = published.signed.createTrack("stream");
	expect(() => replacement.createGroup(0)).toThrow("duplicate group");
	expect(replacement.appendGroup().sequence).toBe(6);

	replacement.close();
	published.signed.close();
	published.close();
});

test("a broadcast signed under another key is refused", async () => {
	const impostor = await SecretKey.generate();
	const victim = await SecretKey.generate();
	const path = Path.from(`mesh/${victim.public}/ssh`);
	const published = await publish(impostor, path);
	const stream = published.signed.createTrack("stream");
	const group = stream.appendGroup();
	await group.writeFrame(undefined, encoder.encode("hello"));
	group.finish();

	const track = (await published.consume(victim.public)).subscribe("stream", subscription);
	await expect((await track.recvGroup())?.readFrame()).rejects.toEqual(new Failure("signature"));

	track.close();
	published.close();
});

import { expect, test } from "bun:test";
import { Group, Time, Track } from "@moq/net";
import {
	Consumer,
	Credential,
	DOMAIN_GROUP,
	Failure,
	MAX_GROUPED_PAYLOAD,
	MAX_INVOCATIONS,
	opaqueName,
	open,
	Producer,
	protect,
} from "./index.ts";
import { DatagramWindow, GroupWindow } from "./window.ts";

const SECRET = new TextEncoder().encode("moq-e2ee-01 test secret!!!!!!!!!");
const REPLAY = 30_000;

function cred(generation = 1, kid = 7): Credential {
	return new Credential({
		context: "example.com/meeting-123",
		generation,
		kid,
		secret: Uint8Array.from(SECRET),
	});
}

async function pair(generation = 1) {
	const credential = cred(generation);
	const name = await opaqueName(credential, "video");
	const track = new Track.Producer(name);
	const producer = await Producer.create({ track, credential, semanticName: "video" });
	const consumer = await Consumer.create({
		track: track.subscribe({ maxAge: REPLAY }),
		credential,
		semanticName: "video",
	});
	return { credential, name, track, producer, consumer };
}

test("same-generation restart is refused", async () => {
	const credential = cred();
	const name = await opaqueName(credential, "video");
	const first = await Producer.create({ track: new Track.Producer(name), credential });
	await expect(Producer.create({ track: new Track.Producer(name), credential })).rejects.toMatchObject({
		code: "reuse",
	});
	first.close();
	await expect(Producer.create({ track: new Track.Producer(name), credential })).rejects.toMatchObject({
		code: "reuse",
	});
});

test("a new generation may reuse a transport sequence", async () => {
	const first = await pair(1);
	const group = first.producer.appendGroup();
	await group.writeFrame({ payload: new TextEncoder().encode("a"), timestamp: Time.Timestamp.fromMillis(0) });
	group.close();
	await first.producer.finish();

	const second = await pair(2);
	const restarted = second.producer.appendGroup();
	await restarted.writeFrame({ payload: new TextEncoder().encode("b"), timestamp: Time.Timestamp.fromMillis(0) });
	restarted.close();
	const got = await second.consumer.nextGroup();
	expect(new TextDecoder().decode((await got?.readFrame())?.payload)).toBe("b");
});

test("re-encrypting different bytes at an identity is reuse", async () => {
	const { producer } = await pair();
	const group = producer.appendGroup();
	await group.writeFrame({ payload: new TextEncoder().encode("a"), timestamp: Time.Timestamp.fromMillis(0) });
	await expect(
		group.writeFrame({ payload: new TextEncoder().encode("b"), timestamp: Time.Timestamp.fromMillis(1) }),
	).resolves.toBeUndefined();
	const other = cred();
	const name = await opaqueName(other, "video");
	await protect(other, {
		physicalName: name,
		domain: DOMAIN_GROUP,
		group: 0,
		frame: 0,
		plaintext: new TextEncoder().encode("a"),
		payloadLimit: MAX_GROUPED_PAYLOAD,
	});
	await expect(
		protect(other, {
			physicalName: name,
			domain: DOMAIN_GROUP,
			group: 0,
			frame: 0,
			plaintext: new TextEncoder().encode("b"),
			payloadLimit: MAX_GROUPED_PAYLOAD,
		}),
	).rejects.toMatchObject({ code: "reuse" });
});

test("retransmission writes stored ciphertext without encrypting", async () => {
	const { track, producer, consumer } = await pair();
	const group = producer.appendGroup();
	await group.writeFrame({ payload: new TextEncoder().encode("keep"), timestamp: Time.Timestamp.fromMillis(1) });
	const sealed = group.sealed()[0]?.payload;
	expect(sealed).toBeDefined();
	group.abort();
	producer.retransmit(group);

	const raw = track.subscribe({ maxAge: REPLAY }).ordered();
	const rawGroup = await raw.nextGroup();
	expect(rawGroup?.sequence).toBe(0);
	expect((await rawGroup?.readFrame())?.payload).toEqual(sealed);

	const opened = await consumer.nextGroup();
	expect(new TextDecoder().decode((await opened?.readFrame())?.payload)).toBe("keep");
});

test("per-key invocation exhaustion is refused before AEAD", async () => {
	const credential = cred();
	const name = await opaqueName(credential, "video");
	await credential.primeUsage(name, DOMAIN_GROUP, MAX_INVOCATIONS, 0);
	await expect(
		protect(credential, {
			physicalName: name,
			domain: DOMAIN_GROUP,
			group: 0,
			frame: 0,
			plaintext: new Uint8Array([1]),
			payloadLimit: MAX_GROUPED_PAYLOAD,
		}),
	).rejects.toMatchObject({ code: "exhausted" });
});

test("per-key plaintext-byte exhaustion is refused before AEAD", async () => {
	const credential = cred();
	const name = await opaqueName(credential, "video");
	await credential.primeUsage(name, DOMAIN_GROUP, 0, 2 ** 36 - 1);
	await expect(
		protect(credential, {
			physicalName: name,
			domain: DOMAIN_GROUP,
			group: 0,
			frame: 0,
			plaintext: new Uint8Array([1, 2]),
			payloadLimit: MAX_GROUPED_PAYLOAD,
		}),
	).rejects.toMatchObject({ code: "exhausted" });
});

test("grouped duplicate frames inside the window are suppressed", () => {
	const window = new GroupWindow();
	window.claim(5, 0);
	window.claim(5, 1);
	expect(() => window.claim(5, 0)).toThrow(Failure);
	window.claim(6, 0);
	expect(() => window.claim(5, 1)).toThrow(Failure);
	window.claim(7, 0);
	window.claim(5, 0);
});

test("datagram duplicate window is bounded", () => {
	const window = new DatagramWindow(2);
	window.claim(1);
	window.claim(2);
	expect(() => window.claim(1)).toThrow(Failure);
	window.claim(3);
	window.claim(1);
	expect(() => window.claim(3)).toThrow(Failure);
});

test("application pin rejects the wrong generation or kid", () => {
	expect(
		() =>
			new Credential({
				context: "c",
				generation: 1,
				kid: 7,
				secret: Uint8Array.from(SECRET),
				pin: { generation: 2, kid: 7 },
			}),
	).toThrow(Failure);
	expect(
		() =>
			new Credential({
				context: "c",
				generation: 1,
				kid: 7,
				secret: Uint8Array.from(SECRET),
				pin: { generation: 1, kid: 8 },
			}),
	).toThrow(Failure);
	expect(
		() =>
			new Credential({
				context: "c",
				generation: 1,
				kid: 7,
				secret: Uint8Array.from(SECRET),
				pin: { profile: "moq-e2ee-00", generation: 1, kid: 7 },
			}),
	).toThrow(Failure);
	expect(
		() =>
			new Credential({
				context: "c",
				generation: 1,
				kid: 7,
				secret: Uint8Array.from(SECRET),
				pin: { generation: 1, kid: 7 },
			}),
	).not.toThrow();
});

test("a nonextractable HKDF key is accepted and never serialized", async () => {
	const raw = Uint8Array.from(SECRET);
	const key = await crypto.subtle.importKey("raw", raw, "HKDF", false, ["deriveBits"]);
	expect(key.extractable).toBe(false);
	const credential = new Credential({ context: "c", generation: 1, kid: 1, secret: key });
	expect(JSON.stringify(credential)).not.toContain("secret");
	expect(await opaqueName(credential, "video")).toHaveLength(22);
});

test("grouped authentication failure ends the track", async () => {
	const { track, producer, consumer } = await pair();
	const group = producer.appendGroup();
	await group.writeFrame({ payload: new TextEncoder().encode("ok"), timestamp: Time.Timestamp.fromMillis(0) });
	group.close();

	const flipped = new Group.Producer(1);
	const bad = Uint8Array.from(group.sealed()[0]?.payload ?? new Uint8Array());
	bad[0] ^= 0x01;
	flipped.writeFrame({ payload: bad, timestamp: Time.Timestamp.fromMillis(0) });
	flipped.close();
	track.writeGroup(flipped);

	const first = await consumer.nextGroup();
	expect(new TextDecoder().decode((await first?.readFrame())?.payload)).toBe("ok");
	const second = await consumer.nextGroup();
	await expect(second?.readFrame()).rejects.toMatchObject({ code: "authentication" });
});

test("datagrams round-trip in process", async () => {
	const { producer, consumer } = await pair();
	const pending = consumer.recvDatagram();
	await producer.appendDatagram(Time.Timestamp.fromMillis(0), new TextEncoder().encode("opus"));
	expect(new TextDecoder().decode((await pending)?.payload)).toBe("opus");
});

test("a bad datagram is dropped with an event and the track continues", async () => {
	const { track, producer, consumer } = await pair();
	const firstPending = consumer.recvDatagram();
	await producer.appendDatagram(Time.Timestamp.fromMillis(0), new TextEncoder().encode("one"));
	expect(new TextDecoder().decode((await firstPending)?.payload)).toBe("one");

	const secondPending = consumer.recvDatagram();
	track.writeDatagram({
		sequence: 1,
		timestamp: Time.Timestamp.fromMillis(1),
		payload: new Uint8Array(32).fill(7),
	});
	await producer.insertDatagram(2, Time.Timestamp.fromMillis(2), new TextEncoder().encode("two"));
	expect(new TextDecoder().decode((await secondPending)?.payload)).toBe("two");
	expect(consumer.events.peek()?.code).toBe("authentication");
	expect(consumer.events.peek()?.sequence).toBe(1);
});

test("grouped oversize plaintext is refused before the network", async () => {
	const { producer } = await pair();
	const group = producer.appendGroup();
	await expect(
		group.writeFrame({
			payload: new Uint8Array(MAX_GROUPED_PAYLOAD - 15),
			timestamp: Time.Timestamp.fromMillis(0),
		}),
	).rejects.toMatchObject({ code: "oversize" });
});

test("semantic names are refused when they do not match the physical track", async () => {
	const credential = cred();
	const track = new Track.Producer("video");
	await expect(Producer.create({ track, credential, semanticName: "video" })).rejects.toMatchObject({
		code: "identity",
	});
});

test("open of relocated ciphertext is authentication, not success", async () => {
	const a = cred(1);
	const b = cred(2);
	const name = await opaqueName(a, "video");
	const payload = await protect(a, {
		physicalName: name,
		domain: DOMAIN_GROUP,
		group: 0,
		frame: 0,
		plaintext: new TextEncoder().encode("x"),
		payloadLimit: MAX_GROUPED_PAYLOAD,
	});
	await expect(
		open(b, {
			physicalName: name,
			domain: DOMAIN_GROUP,
			group: 0,
			frame: 0,
			payload,
			payloadLimit: MAX_GROUPED_PAYLOAD,
		}),
	).rejects.toMatchObject({ code: "authentication" });
});

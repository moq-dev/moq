import { expect, test } from "bun:test";
import { Milli } from "@moq/clock";
import { Broadcast, Error as NetError, Group as NetGroup, Track as NetTrack, StreamCode, Time } from "@moq/net";
import * as Protected from "./broadcast.ts";
import { Credential } from "./credential.ts";
import type * as Datagram from "./datagram.ts";
import { DATAGRAM, GROUP } from "./derive.ts";
import { Failure } from "./failure.ts";
import type * as Generation from "./generation.ts";
import * as Group from "./group.ts";
import * as E2EE from "./index.ts";
import { TrackKey, trackKey } from "./key.ts";
import {
	DATAGRAM_WINDOW,
	MAX_DATAGRAM_PAYLOAD,
	MAX_DATAGRAM_PLAINTEXT,
	MAX_GROUPED_PAYLOAD,
	MAX_GROUPED_PLAINTEXT,
	MAX_INVOCATIONS,
	MAX_PLAINTEXT_BYTES,
	MAX_U53,
} from "./limits.ts";
import { Pump } from "./pump.ts";
import { Terminal } from "./terminal.ts";
import type * as Track from "./track.ts";

const encoder = new TextEncoder();
const decoder = new TextDecoder();
const SECRET = encoder.encode("moq-e2ee-00 test secret!!!!!!!!!");

function credential(): Credential {
	return new Credential({ context: encoder.encode("example.com/meeting-123"), kid: 7, secret: SECRET });
}

const ms = (value: number) => Time.Timestamp.fromMillis(value);
const text = (value: string) => encoder.encode(value);

function netTrack(name: string, timed = true): NetTrack.Producer {
	return new Broadcast.Producer().createTrack(name, timed ? { timescale: Time.Timescale.MILLI } : {});
}

function subscribe(net: NetTrack.Producer): NetTrack.Subscriber {
	return net.subscribe({ maxDelay: Milli(60_000) });
}

interface Pair {
	semantic: string;
	generation: Generation.Producer;
	consuming: Generation.Consumer;
	net: NetTrack.Producer;
	producer: Track.Producer;
	consumer: Track.Consumer;
}

async function pair(semantic: string): Promise<Pair> {
	const generation = credential().produce();
	const net = netTrack(await generation.name(semantic));
	const producer = await generation.produce(net, semantic);
	const consuming = generation.credential.consume(generation.epoch);
	const consumer = consuming.consume(subscribe(net), semantic);
	return { semantic, generation, consuming, net, producer, consumer };
}

// Ciphertext written straight to the net track, as a relay or attacker would.
async function sealed(pair: Pair, domain: 0 | 1, group: number, frame: number, plaintext: string, forge = false) {
	const key = trackKey(pair.consuming.credential, pair.consuming.epoch, pair.semantic, domain);
	const limit = domain === GROUP ? MAX_GROUPED_PAYLOAD : MAX_DATAGRAM_PAYLOAD;
	const bytes = text(plaintext);
	const payload = await key.encrypt(key.admitSeal(group, frame, bytes.byteLength, limit), bytes);
	if (forge) payload[0] ^= 1;
	return payload;
}

async function failure(promise: Promise<unknown>): Promise<string> {
	try {
		await promise;
	} catch (err) {
		if (err instanceof Failure) return err.code;
		throw err;
	}
	throw new Error("expected a failure");
}

function thrown(fn: () => unknown): string {
	try {
		fn();
	} catch (err) {
		if (err instanceof Failure) return err.code;
		throw err;
	}
	throw new Error("expected a failure");
}

// Hold the first open until released, counting every open that starts.
function holdFirstOpen() {
	const decrypt = TrackKey.prototype.decrypt;
	let release!: () => void;
	const gate = new Promise<void>((resolve) => {
		release = resolve;
	});
	let entered!: () => void;
	const opening = new Promise<void>((resolve) => {
		entered = resolve;
	});
	const hold = {
		calls: 0,
		opening,
		release,
		restore: () => {
			TrackKey.prototype.decrypt = decrypt;
			release();
		},
	};
	TrackKey.prototype.decrypt = async function (this: TrackKey, ...args: Parameters<TrackKey["decrypt"]>) {
		if (hold.calls++ === 0) {
			entered();
			await gate;
		}
		return decrypt.apply(this, args);
	};
	return hold;
}

// Resolves once `count` opens are admitted. Awaiting it lets the admitting read run on
// synchronously to its next suspension: the pump, or the open itself.
function admitted(count: number) {
	const admitOpen = TrackKey.prototype.admitOpen;
	let calls = 0;
	let resolve!: () => void;
	const done = new Promise<void>((r) => {
		resolve = r;
	});
	TrackKey.prototype.admitOpen = function (this: TrackKey, ...args: Parameters<TrackKey["admitOpen"]>) {
		const iv = admitOpen.apply(this, args);
		if (++calls === count) resolve();
		return iv;
	};
	return {
		done,
		restore: () => {
			TrackKey.prototype.admitOpen = admitOpen;
		},
	};
}

async function readText(group: Group.Consumer): Promise<string | undefined> {
	const frame = await group.readFrame();
	return frame && decoder.decode(frame.plaintext);
}

async function recvGroup(consumer: Track.Consumer): Promise<Group.Consumer> {
	const group = await consumer.recvGroup();
	if (!group) throw new Error("expected a group");
	return group;
}

function datagramText(event: Datagram.Event | undefined): string {
	if (event?.kind !== "datagram") throw new Error(`expected a datagram, got ${JSON.stringify(event)}`);
	return decoder.decode(event.datagram.plaintext);
}

test("path is opaque", async () => {
	const path = await credential().path("meeting.hang");
	expect(path.length).toBe(22);
	expect(path).not.toContain("meeting");
});

test("grouped roundtrip", async () => {
	const p = await pair("video");
	const group = p.producer.appendGroup();
	expect(group.sequence).toBe(0);
	await group.writeFrame(ms(1), text("frame-zero"));
	await group.writeFrame(ms(2), text("frame-one"));
	group.finish();

	const received = await recvGroup(p.consumer);
	expect(received.sequence).toBe(0);
	const first = await received.readFrame();
	expect(decoder.decode(first?.plaintext)).toBe("frame-zero");
	expect(first?.timestamp?.as(Time.Timescale.MILLI)).toBe(1);
	expect(await readText(received)).toBe("frame-one");
	expect(await received.readFrame()).toBeUndefined();
});

test("datagram roundtrip", async () => {
	const p = await pair("audio");
	expect(await p.producer.appendDatagram(ms(5), text("opus"))).toBe(0);
	const event = await p.consumer.recvDatagram();
	expect(event?.kind).toBe("datagram");
	if (event?.kind !== "datagram") return;
	expect(event.datagram.sequence).toBe(0);
	expect(event.datagram.timestamp?.as(Time.Timescale.MILLI)).toBe(5);
	expect(decoder.decode(event.datagram.plaintext)).toBe("opus");
});

test("sequences allocate monotonically", async () => {
	const p = await pair("video");
	const subscriber = subscribe(p.net);
	expect(p.producer.appendGroup().sequence).toBe(0);
	expect(p.producer.createGroup(5).sequence).toBe(5);
	expect(await p.producer.appendDatagram(ms(1), text("x"))).toBe(6);
	expect(p.producer.appendGroup().sequence).toBe(7);
	expect(thrown(() => p.producer.createGroup(7))).toBe("reuse");
	expect(thrown(() => p.producer.createGroup(3))).toBe("reuse");
	expect(await failure(p.producer.insertDatagram(6, ms(1), text("x")))).toBe("reuse");
	expect(thrown(() => p.producer.createGroup(MAX_U53 + 1))).toBe("identity");
	expect(thrown(() => p.producer.createGroup(1.5))).toBe("identity");

	// Only the one admitted datagram reached the net track: a later sentinel is next.
	expect((await subscriber.recvDatagram())?.sequence).toBe(6);
	p.net.insertDatagram(8, ms(2), new Uint8Array());
	expect((await subscriber.recvDatagram())?.sequence).toBe(8);
});

test("oversize payloads are refused before an identity is spent", async () => {
	const p = await pair("audio");
	expect(await failure(p.producer.appendDatagram(ms(1), new Uint8Array(MAX_DATAGRAM_PLAINTEXT + 1)))).toBe(
		"oversize",
	);
	expect(await p.producer.appendDatagram(ms(1), new Uint8Array(MAX_DATAGRAM_PLAINTEXT))).toBe(0);

	const group = p.producer.appendGroup();
	expect(await failure(group.writeFrame(ms(1), new Uint8Array(MAX_GROUPED_PLAINTEXT + 1)))).toBe("oversize");
	await group.writeFrame(ms(1), text("ok"));
	group.finish();
	expect(await readText(await recvGroup(p.consumer))).toBe("ok");
});

test("a broadcast speaks semantic names and puts physical names on the wire", async () => {
	const generation = credential().produce();
	const net = new Broadcast.Producer();
	const producer = new Protected.Producer(net, generation);
	const video = await producer.createTrack("video", { timescale: Time.Timescale.MILLI });
	expect(video.name).toBe(await generation.name("video"));

	const consumer = await new Protected.Consumer(
		net.consume(),
		generation.credential.consume(generation.epoch),
	).subscribe("video", { maxDelay: Milli(60_000) });
	const group = video.appendGroup();
	await group.writeFrame(ms(1), text("frame"));
	group.finish();
	expect(await readText(await recvGroup(consumer))).toBe("frame");
	consumer.close();
	video.finish();
	video.close();
	net.close();
});

// A broadcast as a session hands it out: no producer, so it ends once no handle holds it.
class SessionBroadcast extends Broadcast.Consumer {
	// biome-ignore lint/complexity/noUselessConstructor: widens the protected base constructor to public
	constructor() {
		super();
	}
}

test("closing a protected consumer releases its broadcast handle", () => {
	const generation = credential().produce();
	const net = new SessionBroadcast();
	const consumer = new Protected.Consumer(net, generation.credential.consume(generation.epoch));
	expect(consumer.closed.peek()).toBeUndefined();
	consumer.close();
	consumer.close();
	expect(consumer.closed.peek()).toBeNull();
});

test("closing a protected consumer leaves a produced broadcast to its producer", () => {
	const generation = credential().produce();
	const net = new Broadcast.Producer();
	const consumer = new Protected.Consumer(net.consume(), generation.credential.consume(generation.epoch));
	consumer.close();
	expect(net.closed.peek()).toBeUndefined();
	net.close();
	expect(consumer.closed.peek()).toBeNull();
});

test("closing a protected producer ends its broadcast", () => {
	const producer = new Protected.Producer(new Broadcast.Producer(), credential().produce());
	expect(producer.closed.peek()).toBeUndefined();
	producer.close();
	producer.close();
	expect(producer.closed.peek()).toBeNull();
});

test("a broadcast refuses a name its generation already claimed elsewhere", async () => {
	const generation = credential().produce();
	await new Protected.Producer(new Broadcast.Producer(), generation).createTrack("video");

	expect(await failure(new Protected.Producer(new Broadcast.Producer(), generation).createTrack("video"))).toBe(
		"reuse",
	);
	// A refused net track is closed with the failure, so its subscribers see why.
	const refused = netTrack(await generation.name("video"));
	expect(await failure(generation.produce(refused, "video"))).toBe("reuse");
	expect(refused.closed.peek()).toBeInstanceOf(Failure);
});

test("a track is produced once per generation", async () => {
	const generation = credential().produce();
	const name = await generation.name("video");
	const net = netTrack(name);
	expect((await generation.produce(net, "video")).name).toBe(name);
	expect(await failure(generation.produce(net, "video"))).toBe("reuse");
	expect(await failure(generation.produce(netTrack(name), "video"))).toBe("reuse");

	// A new epoch is a new publisher instance.
	const fresh = generation.credential.produce();
	await fresh.produce(netTrack(await fresh.name("video")), "video");

	// Consuming never claims.
	const consuming = generation.credential.consume(generation.epoch);
	consuming.consume(subscribe(net), "video");
	consuming.consume(subscribe(net), "video");
});

test("a lone surrogate is refused rather than aliasing U+FFFD", async () => {
	const generation = credential().produce();
	expect(await failure(generation.name("\ud800"))).toBe("identity");
	expect(await failure(generation.credential.path("\ud800"))).toBe("identity");
	const producer = new Protected.Producer(new Broadcast.Producer(), generation);
	await producer.createTrack("�");
	expect(await failure(producer.createTrack("\ud800"))).toBe("identity");
	// Its UTF-8 would be U+FFFD's, so claiming it would share that track's key and nonces.
	const aliased = netTrack(await generation.name("�"));
	expect(await failure(generation.produce(aliased, "\ud800"))).toBe("identity");
	expect(aliased.closed.peek()).toBeInstanceOf(Failure);
});

test("recovering a name claims nothing", async () => {
	const generation = credential().produce();
	const name = await generation.name("video");
	expect(await generation.open(name)).toBe("video");
	// Opening a name neither claims it nor derives its payload keys.
	await generation.produce(netTrack(name), "video");
	// The same semantic name under another epoch of the same credential does not open.
	const other = await credential().produce().name("video");
	expect(await failure(generation.open(other))).toBe("authentication");
});

test("a new epoch authenticates while the old one does not", async () => {
	const cred = credential();
	const old = cred.consume(cred.produce().epoch);
	const fresh = cred.produce();
	expect(await old.name("video")).not.toBe(await fresh.name("video"));

	const net = netTrack(await fresh.name("video"));
	const producer = await fresh.produce(net, "video");
	const group = producer.appendGroup();
	await group.writeFrame(ms(1), text("fresh"));
	group.finish();

	const consumer = cred.consume(fresh.epoch).consume(subscribe(net), "video");
	expect(await readText(await recvGroup(consumer))).toBe("fresh");

	// The old generation derives a different key for the same semantic name.
	const stale = old.consume(subscribe(net), "video");
	expect(await failure((await recvGroup(stale)).readFrame())).toBe("authentication");
});

test("invocation exhaustion", async () => {
	const generation = credential().produce();
	const key = trackKey(generation.credential, generation.epoch, "video", GROUP);
	key.invocations = MAX_INVOCATIONS - 1;
	key.admitSeal(0, 0, 4, MAX_GROUPED_PAYLOAD);
	expect(thrown(() => key.admitSeal(0, 1, 4, MAX_GROUPED_PAYLOAD))).toBe("exhausted");
	expect(key.invocations).toBe(MAX_INVOCATIONS);
});

test("plaintext byte exhaustion", async () => {
	const generation = credential().produce();
	const key = trackKey(generation.credential, generation.epoch, "video", GROUP);
	key.plaintextBytes = MAX_PLAINTEXT_BYTES - 3;
	key.admitSeal(0, 0, 2, MAX_GROUPED_PAYLOAD);
	expect(thrown(() => key.admitSeal(0, 1, 4, MAX_GROUPED_PAYLOAD))).toBe("exhausted");
});

test("failed opens spend the key", async () => {
	const generation = credential().produce();
	const key = trackKey(generation.credential, generation.epoch, "video", GROUP);
	const plaintext = text("ok");
	const good = await key.encrypt(key.admitSeal(0, 0, plaintext.byteLength, MAX_GROUPED_PAYLOAD), plaintext);
	const forged = good.slice();
	forged[0] ^= 1;

	key.invocations = MAX_INVOCATIONS - 1;
	const iv = key.admitOpen(0, 0, forged.byteLength, MAX_GROUPED_PAYLOAD);
	expect(await failure(key.decrypt(iv, forged))).toBe("authentication");
	expect(key.invocations).toBe(MAX_INVOCATIONS);
	expect(thrown(() => key.admitOpen(0, 0, good.byteLength, MAX_GROUPED_PAYLOAD))).toBe("exhausted");

	// Refusals before AEAD spend nothing.
	const fresh = new TrackKey(Promise.reject(new Error("never derived")));
	expect(thrown(() => fresh.admitOpen(0, 0, 3, MAX_GROUPED_PAYLOAD))).toBe("oversize");
	expect(thrown(() => fresh.admitOpen(0, 2 ** 32, good.byteLength, MAX_GROUPED_PAYLOAD))).toBe("identity");
	expect(fresh.invocations).toBe(0);
});

test("grouped authentication failure ends the track", async () => {
	const p = await pair("video");
	const group = p.producer.appendGroup();
	await group.writeFrame(ms(1), text("ok"));
	group.finish();

	// A forged second group, written straight to the net track.
	const bad = p.net.createGroup(1);
	bad.writeFrame({ timestamp: ms(2), payload: await sealed(p, GROUP, 1, 0, "ok", true) });
	bad.finish();

	expect(await readText(await recvGroup(p.consumer))).toBe("ok");
	const forged = await recvGroup(p.consumer);
	expect(await failure(forged.readFrame())).toBe("authentication");
	expect(await failure(p.consumer.recvGroup())).toBe("authentication");
	expect(await failure(p.consumer.recvDatagram())).toBe("authentication");
});

test("grouped failure wakes pending reads and releases demand", async () => {
	const p = await pair("video");
	const demand = p.net.demand();
	expect(demand.wanted.peek()).toBeDefined();

	// One authentic frame, then silence. The group and the track stay open.
	const open = p.net.createGroup(0);
	open.writeFrame({ timestamp: ms(1), payload: await sealed(p, GROUP, 0, 0, "ok") });
	const bad = p.net.createGroup(1);
	const badDemand = bad.demand();
	bad.writeFrame({ timestamp: ms(2), payload: await sealed(p, GROUP, 1, 0, "nope", true) });

	const good = await recvGroup(p.consumer);
	expect(await readText(good)).toBe("ok");
	const forged = await recvGroup(p.consumer);
	expect(badDemand.wanted.peek()).toBe(true);

	// Sibling frame, next group, and datagram reads have nothing to return until the failure.
	const frame = good.readFrame();
	const nextGroup = p.consumer.recvGroup();
	const datagram = p.consumer.recvDatagram();

	expect(await failure(forged.readFrame())).toBe("authentication");
	expect(await failure(frame)).toBe("authentication");
	expect(await failure(nextGroup)).toBe("authentication");
	expect(await failure(datagram)).toBe("authentication");

	expect(demand.wanted.peek()).toBeUndefined();
	expect(badDemand.wanted.peek()).toBe(false);
	expect(open.demand().wanted.peek()).toBe(false);
	expect(p.net.closed.peek()).toBeUndefined();
});

test("a grouped failure ahead of a datagram open rejects the datagram", async () => {
	const p = await pair("audio");
	const bad = p.net.createGroup(0);
	bad.writeFrame({ timestamp: ms(1), payload: await sealed(p, GROUP, 0, 0, "nope", true) });
	p.net.insertDatagram(0, ms(2), await sealed(p, DATAGRAM, 0, 0, "late"));
	const forged = await recvGroup(p.consumer);

	const hold = holdFirstOpen();
	const admit = admitted(2);
	try {
		const read = forged.readFrame();
		await hold.opening;
		// The datagram passes its terminal check, then queues behind the forged open.
		const datagram = p.consumer.recvDatagram();
		await admit.done;
		hold.release();
		expect(await Promise.all([failure(read), failure(datagram)])).toEqual(["authentication", "authentication"]);
	} finally {
		hold.restore();
		admit.restore();
	}
});

test("opens across a track's groups run one at a time", async () => {
	const p = await pair("video");
	const first = p.producer.appendGroup();
	await first.writeFrame(ms(1), text("a"));
	const second = p.producer.appendGroup();
	await second.writeFrame(ms(2), text("b"));
	const a = await recvGroup(p.consumer);
	const b = await recvGroup(p.consumer);

	const hold = holdFirstOpen();
	const admit = admitted(2);
	try {
		const readA = readText(a);
		await hold.opening;
		const readB = readText(b);
		await admit.done;
		// B is admitted but waits for A's open instead of starting its own.
		expect(hold.calls).toBe(1);
		hold.release();
		expect(await Promise.all([readA, readB])).toEqual(["a", "b"]);
		expect(hold.calls).toBe(2);
	} finally {
		hold.restore();
		admit.restore();
	}
});

test("closing the track consumer releases demand while a group is held", async () => {
	const p = await pair("video");
	const demand = p.net.demand();
	const open = p.producer.appendGroup();
	await open.writeFrame(ms(1), text("ok"));

	const group = await recvGroup(p.consumer);
	p.consumer.close();
	// The unsubscribe lands on the next microtask.
	while (demand.wanted.peek() !== undefined) await Promise.resolve();

	// The held group is not failed by the unsubscribe: it ends after what it holds, as in @moq/net.
	expect(await readText(group)).toBe("ok");
	await open.writeFrame(ms(2), text("next"));
	expect(await group.readFrame()).toBeUndefined();
});

test("a bad datagram stays an event and keeps demand", async () => {
	const p = await pair("audio");
	const demand = p.net.demand();
	const open = p.producer.appendGroup();
	await open.writeFrame(ms(1), text("ok"));
	p.net.insertDatagram(1, ms(2), await sealed(p, DATAGRAM, 1, 0, "nope", true));

	const good = await recvGroup(p.consumer);
	expect(await readText(good)).toBe("ok");
	// The parked grouped read survives the bad datagram: it later resolves with a newer frame.
	const frame = good.readFrame();

	expect(await p.consumer.recvDatagram()).toEqual({ kind: "authentication", sequence: 1 });
	expect(demand.wanted.peek()).toBeDefined();

	await p.producer.insertDatagram(2, ms(3), text("next"));
	expect(datagramText(await p.consumer.recvDatagram())).toBe("next");
	expect(demand.wanted.peek()).toBeDefined();

	await open.writeFrame(ms(4), text("still"));
	expect(decoder.decode((await frame)?.plaintext)).toBe("still");
});

test("a forged datagram does not burn the identity", async () => {
	const p = await pair("audio");
	await p.producer.appendDatagram(ms(1), text("one"));
	p.net.insertDatagram(1, ms(2), await sealed(p, DATAGRAM, 1, 0, "two", true));
	await p.producer.insertDatagram(1, ms(2), text("two"));

	expect(datagramText(await p.consumer.recvDatagram())).toBe("one");
	expect(await p.consumer.recvDatagram()).toEqual({ kind: "authentication", sequence: 1 });
	const event = await p.consumer.recvDatagram();
	expect(datagramText(event)).toBe("two");
});

test("a datagram no publisher could seal is dropped with its code", async () => {
	const p = await pair("audio");
	const sealedAt = await sealed(p, DATAGRAM, 0, 0, "x");
	p.net.insertDatagram(1, ms(1), new Uint8Array(MAX_DATAGRAM_PAYLOAD + 1));
	p.net.insertDatagram(2, ms(2), sealedAt.subarray(0, 3));
	p.net.insertDatagram(2 ** 53, ms(3), sealedAt);
	p.net.insertDatagram(2 ** 53 + 2, ms(3), sealedAt.subarray(0, 3));
	await p.producer.insertDatagram(2 ** 53 - 1, ms(4), text("last"));

	expect(await p.consumer.recvDatagram()).toEqual({ kind: "oversize", sequence: 1 });
	expect(await p.consumer.recvDatagram()).toEqual({ kind: "oversize", sequence: 2 });
	expect(await p.consumer.recvDatagram()).toEqual({ kind: "identity", sequence: 2 ** 53 });
	// Identity is checked before size, as in every core.
	expect(await p.consumer.recvDatagram()).toEqual({ kind: "identity", sequence: 2 ** 53 + 2 });
	expect(datagramText(await p.consumer.recvDatagram())).toBe("last");
});

test("resubscribing shares a key's budget", async () => {
	const p = await pair("audio");
	const admitOpen = TrackKey.prototype.admitOpen;
	const opened: TrackKey[] = [];
	TrackKey.prototype.admitOpen = function (this: TrackKey, ...args: Parameters<TrackKey["admitOpen"]>) {
		opened.push(this);
		return admitOpen.apply(this, args);
	};
	try {
		await p.producer.appendDatagram(ms(1), text("one"));
		expect(datagramText(await p.consumer.recvDatagram())).toBe("one");
		p.consumer.close();
		opened[0].invocations = MAX_INVOCATIONS;

		const again = p.consuming.consume(subscribe(p.net), p.semantic);
		await p.producer.appendDatagram(ms(2), text("two"));
		expect(await failure(again.recvDatagram())).toBe("exhausted");
	} finally {
		TrackKey.prototype.admitOpen = admitOpen;
	}
});

test("duplicate datagrams are suppressed inside a bounded window", async () => {
	const p = await pair("audio");
	const replay = await sealed(p, DATAGRAM, 0, 0, "once");
	p.net.insertDatagram(0, ms(1), replay);
	p.net.insertDatagram(0, ms(1), replay);
	expect(datagramText(await p.consumer.recvDatagram())).toBe("once");
	expect(await p.consumer.recvDatagram()).toEqual({ kind: "duplicate", sequence: 0 });

	// Once the window slides past it, the same ciphertext opens again.
	for (let sequence = 1; sequence <= DATAGRAM_WINDOW; sequence++) {
		p.net.insertDatagram(sequence, ms(sequence), await sealed(p, DATAGRAM, sequence, 0, "x"));
		expect((await p.consumer.recvDatagram())?.kind).toBe("datagram");
	}
	p.net.insertDatagram(0, ms(1), replay);
	expect(datagramText(await p.consumer.recvDatagram())).toBe("once");
});

test("releasing an unfinished producer cancels queued writes and held groups", async () => {
	const p = await pair("video");
	const group = p.producer.appendGroup();
	await group.writeFrame(ms(1), text("first"));
	const received = await recvGroup(p.consumer);
	expect(await readText(received)).toBe("first");
	const tail = received.readFrame().catch((error) => error);
	const datagram = p.producer.appendDatagram(ms(2), text("queued"));
	const pending = Promise.allSettled([datagram]);
	p.producer.close();
	const [result] = await pending;
	expect(result.status).toBe("rejected");
	const cancelled = p.net.closed.peek();
	expect(cancelled).toBeInstanceOf(NetError.Stream);
	expect(cancelled).toMatchObject({ code: StreamCode.Cancel });
	expect(await tail).toBe(cancelled);
	await expect(group.writeFrame(ms(3), text("late"))).rejects.toBe(cancelled);
});

test("finish then release drains pending writes before the end", async () => {
	const p = await pair("video");
	const group = p.producer.appendGroup();
	const writes = [group.writeFrame(ms(1), text("a")), group.writeFrame(ms(2), text("b"))];
	group.finish();
	p.producer.finish();
	p.producer.close();
	await Promise.all(writes);

	const received = await recvGroup(p.consumer);
	expect(await readText(received)).toBe("a");
	expect(await readText(received)).toBe("b");
	expect(await received.readFrame()).toBeUndefined();
	expect(await p.consumer.recvGroup()).toBeUndefined();
	expect(await group.closed).toBeNull();
	expect(await p.producer.closed).toBeNull();
	expect(await failure(group.writeFrame(ms(3), text("c"))).catch((err: Error) => err.message)).toBe(
		"group is closed",
	);
});

test("a declared track end leaves existing groups writable and abortable", async () => {
	const p = await pair("video");
	const group = p.producer.appendGroup();
	await group.writeFrame(ms(1), text("first"));
	const received = await recvGroup(p.consumer);
	expect(await readText(received)).toBe("first");
	p.producer.finish();
	await group.writeFrame(ms(2), text("last"));
	expect(await readText(received)).toBe("last");
	expect(p.producer.closed.peek()).toBeUndefined();
	expect(() => p.producer.appendGroup()).toThrow("track is closed");
	await expect(p.producer.appendDatagram(ms(3), text("late"))).rejects.toThrow("track is closed");
	const aborted = new Error("unfinished group");
	p.producer.abort(aborted);
	expect(await p.producer.closed).toBe(aborted);
	expect(await group.closed).toBe(aborted);
	await expect(received.readFrame()).rejects.toBe(aborted);
});

test("abort after declaring an end stops an in-flight seal and its queued writes", async () => {
	const p = await pair("video");
	const group = p.producer.appendGroup();
	const encrypt = TrackKey.prototype.encrypt;
	let release!: () => void;
	const gate = new Promise<void>((resolve) => {
		release = resolve;
	});
	let entered!: () => void;
	const sealing = new Promise<void>((resolve) => {
		entered = resolve;
	});
	let seals = 0;
	TrackKey.prototype.encrypt = async function (this: TrackKey, ...args: Parameters<TrackKey["encrypt"]>) {
		seals++;
		entered();
		await gate;
		return encrypt.apply(this, args);
	};
	try {
		const frame = group.writeFrame(ms(1), text("in flight"));
		await sealing;
		const datagram = p.producer.appendDatagram(ms(2), text("queued"));
		const pending = Promise.allSettled([frame, datagram]);
		p.producer.finish();
		expect(p.producer.closed.peek()).toBeUndefined();
		const aborted = new Error("cancel during encryption");
		p.producer.abort(aborted);
		release();
		expect(await pending).toEqual([
			{ status: "rejected", reason: aborted },
			{ status: "rejected", reason: aborted },
		]);
		expect(seals).toBe(1);
		expect(await group.closed).toBe(aborted);
		expect(await p.producer.closed).toBe(aborted);
	} finally {
		TrackKey.prototype.encrypt = encrypt;
		release();
	}
});

test("the pump keeps frame ordinals in submit order", async () => {
	const p = await pair("video");
	const groups = [p.producer.appendGroup(), p.producer.appendGroup()];
	const writes: Promise<void>[] = [];
	const datagrams: Promise<number>[] = [];
	for (let i = 0; i < 200; i++) {
		writes.push(groups[i % 2].writeFrame(ms(i), text(`frame-${i}`)));
		if (i % 50 === 0) datagrams.push(p.producer.appendDatagram(ms(i), text(`datagram-${i}`)));
	}
	for (const group of groups) group.finish();
	await Promise.all(writes);
	expect(await Promise.all(datagrams)).toEqual([2, 3, 4, 5]);

	for (let g = 0; g < 2; g++) {
		const received = await recvGroup(p.consumer);
		for (let i = g; i < 200; i += 2) expect(await readText(received)).toBe(`frame-${i}`);
		expect(await received.readFrame()).toBeUndefined();
	}
	for (let i = 0; i < 200; i += 50) expect(datagramText(await p.consumer.recvDatagram())).toBe(`datagram-${i}`);
});

test("concurrent reads open in transport order", async () => {
	const p = await pair("video");
	const group = p.producer.appendGroup();
	for (let i = 0; i < 50; i++) void group.writeFrame(ms(i), text(`frame-${i}`));
	group.finish();
	const received = await recvGroup(p.consumer);
	const reads = Array.from({ length: 51 }, () => received.readFrame());
	const frames = await Promise.all(reads);
	expect(frames.slice(0, 50).map((frame) => decoder.decode(frame?.plaintext))).toEqual(
		Array.from({ length: 50 }, (_, i) => `frame-${i}`),
	);
	expect(frames[50]).toBeUndefined();
});

test("aborting a group rejects its pending writes", async () => {
	const p = await pair("video");
	const group = p.producer.appendGroup();
	const first = group.writeFrame(ms(1), text("a"));
	const second = group.writeFrame(ms(2), text("b"));
	const abort = new Error("cancelled");
	group.abort(abort);
	expect(await Promise.allSettled([first, second])).toEqual([
		{ status: "rejected", reason: abort },
		{ status: "rejected", reason: abort },
	]);
	await expect(group.writeFrame(ms(3), text("c"))).rejects.toBe(abort);

	// Other groups on the track carry on.
	const next = p.producer.appendGroup();
	await next.writeFrame(ms(4), text("next"));
	next.finish();
	const received = await recvGroup(p.consumer);
	expect(received.sequence).toBe(0);
	expect(await received.readFrame().catch((err) => err)).toBe(abort);
	expect(await readText(await recvGroup(p.consumer))).toBe("next");
});

test("aborting a track rejects pending writes on it and its groups", async () => {
	const p = await pair("video");
	const group = p.producer.appendGroup();
	const frame = group.writeFrame(ms(1), text("a"));
	const datagram = p.producer.appendDatagram(ms(1), text("d"));
	const abort = new Error("cancelled");
	p.producer.abort(abort);
	expect(await Promise.allSettled([frame, datagram])).toEqual([
		{ status: "rejected", reason: abort },
		{ status: "rejected", reason: abort },
	]);
	expect(() => p.producer.appendGroup()).toThrow(abort);
	expect(p.net.closed.peek()).toBe(abort);
});

test("an abort stops queued datagrams before they seal", async () => {
	const p = await pair("audio");
	const encrypt = TrackKey.prototype.encrypt;
	let seals = 0;
	TrackKey.prototype.encrypt = function (this: TrackKey, ...args: Parameters<TrackKey["encrypt"]>) {
		seals++;
		return encrypt.apply(this, args);
	};
	try {
		const abort = new Error("abort");
		const write = p.producer.insertDatagram(0, ms(1), text("x"));
		p.producer.abort(abort);
		await expect(write).rejects.toBe(abort);
		expect(seals).toBe(0);
	} finally {
		TrackKey.prototype.encrypt = encrypt;
	}
});

test("a finished group and a released declared track keep their promised drain", async () => {
	const p = await pair("video");
	const group = p.producer.appendGroup();
	const frame = group.writeFrame(ms(1), text("frame"));
	group.finish();
	group.abort(new Error("late"));
	const datagram = p.producer.insertDatagram(1, ms(2), text("datagram"));
	p.producer.finish();
	p.producer.close();
	p.producer.abort(new Error("late"));
	await Promise.all([frame, datagram]);

	expect(await p.net.closed).toBeNull();
	const received = await recvGroup(p.consumer);
	expect(await readText(received)).toBe("frame");
	expect(await received.readFrame()).toBeUndefined();
	expect(datagramText(await p.consumer.recvDatagram())).toBe("datagram");
});

test("a refused timestamp does not burn the frame", async () => {
	const p = await pair("video");
	await p.net.info();
	const group = p.producer.appendGroup();
	await expect(group.writeFrame(undefined, text("untimed"))).rejects.toThrow("timestamp");
	await group.writeFrame(ms(1), text("ok"));
	group.finish();
	expect(await readText(await recvGroup(p.consumer))).toBe("ok");
});

test("produce waits for an unaccepted track's timescale", async () => {
	const generation = credential().produce();
	const net = new NetTrack.Producer(await generation.name("audio"));
	const consumer = generation.credential.consume(generation.epoch).consume(subscribe(net), "audio");
	const producing = generation.produce(net, "audio");
	net.accept({ timescale: Time.Timescale.MILLI });
	const producer = await producing;

	// The timescale is known from the start, so mismatches spend neither a sequence nor a frame.
	await expect(producer.insertDatagram(0, undefined, text("untimed"))).rejects.toThrow("timestamp");
	await producer.insertDatagram(0, ms(1), text("datagram"));
	const group = producer.createGroup(1);
	await expect(group.writeFrame(undefined, text("untimed"))).rejects.toThrow("timestamp");
	await group.writeFrame(ms(2), text("frame"));
	group.finish();
	expect(datagramText(await consumer.recvDatagram())).toBe("datagram");
	expect(await readText(await recvGroup(consumer))).toBe("frame");

	// A track closed before it is accepted never produces, and leaves its name unclaimed.
	const video = await generation.name("video");
	const closed = new NetTrack.Producer(video);
	const refused = generation.produce(closed, "video");
	closed.abort(new Error("gone"));
	await expect(refused).rejects.toThrow("gone");
	await generation.produce(netTrack(video), "video");
	expect(await failure(generation.produce(netTrack(video), "video"))).toBe("reuse");
});

test("a timestamp out of range at the track's timescale spends nothing", async () => {
	const generation = credential().produce();
	const net = new Broadcast.Producer().createTrack(await generation.name("video"), {
		timescale: Time.Timescale.NANO,
	});
	const consumer = generation.credential.consume(generation.epoch).consume(subscribe(net), "video");
	const producer = await generation.produce(net, "video");
	const huge = new Time.Timestamp(Number.MAX_SAFE_INTEGER, Time.Timescale.MILLI);

	await expect(producer.insertDatagram(0, huge, text("x"))).rejects.toThrow(RangeError);
	await producer.insertDatagram(0, ms(1), text("datagram"));
	const group = producer.createGroup(1);
	await expect(group.writeFrame(huge, text("x"))).rejects.toThrow(RangeError);
	await group.writeFrame(ms(2), text("frame"));
	group.finish();
	expect(datagramText(await consumer.recvDatagram())).toBe("datagram");
	expect(await readText(await recvGroup(consumer))).toBe("frame");
});

test("a grouped failure skips opens queued behind it", async () => {
	const p = await pair("video");
	const bad = p.net.createGroup(0);
	bad.writeFrame({ timestamp: ms(1), payload: await sealed(p, GROUP, 0, 0, "nope", true) });
	const good = p.net.createGroup(1);
	good.writeFrame({ timestamp: ms(2), payload: await sealed(p, GROUP, 1, 0, "ok") });
	p.net.insertDatagram(0, ms(3), await sealed(p, DATAGRAM, 0, 0, "late"));
	const forged = await recvGroup(p.consumer);
	const held = await recvGroup(p.consumer);

	const hold = holdFirstOpen();
	const admit = admitted(3);
	try {
		const reads = [forged.readFrame(), held.readFrame(), p.consumer.recvDatagram()];
		await admit.done;
		hold.release();
		expect(await Promise.all(reads.map(failure))).toEqual(["authentication", "authentication", "authentication"]);
		expect(hold.calls).toBe(1);
	} finally {
		hold.restore();
		admit.restore();
	}
});

test("a failed net write after encryption aborts the group", async () => {
	const p = await pair("video");
	const group = p.producer.appendGroup();
	const writes: Promise<void>[] = [];
	for (let i = 0; i < NetGroup.MAX_GROUP_FRAMES; i++) writes.push(group.writeFrame(ms(i), text("x")));
	await Promise.all(writes);
	// The net group refuses the frame only after it was encrypted, so the identity is spent
	// and the group can no longer line its transport index up with the nonce.
	await expect(group.writeFrame(ms(0), text("over"))).rejects.toThrow("group too large");
	await expect(group.writeFrame(ms(0), text("after"))).rejects.toThrow("group too large");
});

test("a group read past the first frame opens at its transport index", async () => {
	const p = await pair("video");
	const group = p.producer.appendGroup();
	await group.writeFrame(ms(1), text("zero"));
	await group.writeFrame(ms(2), text("one"));
	group.finish();

	// Skip frame 0 on the net cursor before wrapping, as a resumed subscription would.
	const net = await subscribe(p.net).recvGroup();
	if (!net) throw new Error("expected a net group");
	expect(net.tryReadFrame()).toBeDefined();
	const key = trackKey(p.consuming.credential, p.consuming.epoch, p.semantic, GROUP);
	const wrapped = Group.Consumer.make(net, { pump: new Pump(), key, terminal: new Terminal(subscribe(p.net)) });
	expect(await readText(wrapped)).toBe("one");
});

test("credentials mint fresh epochs and consumers bind them", async () => {
	const cred = credential();
	const first = cred.produce();
	const second = cred.produce();
	expect(first.epoch).not.toBe(second.epoch);
	expect(await first.name("video")).not.toBe(await second.name("video"));
	const bound = cred.consume(first.epoch);
	expect(bound.epoch).toBe(first.epoch);
	expect(await bound.name("video")).toBe(await first.name("video"));
	expect(bound.credential.kid).toBe(cred.kid);
	expect(thrown(() => cred.consume("not-an-epoch" as typeof first.epoch))).toBe("identity");
});

test("the package surface cannot mint a producer for a chosen epoch", () => {
	expect(Object.keys(E2EE)).not.toContain("Generation");
	const cred = credential();
	const chosen = cred.produce().epoch;
	// The classes stay reachable through an instance, so neither may accept a producing epoch.
	const producer = cred.produce().constructor as new (...args: unknown[]) => Generation.Producer;
	expect(new producer(cred, chosen).epoch).not.toBe(chosen);
	expect(Object.getOwnPropertyNames(producer)).not.toContain("make");
	const consumer = cred.consume(chosen).constructor as new (...args: unknown[]) => Generation.Consumer;
	expect(Object.getOwnPropertyNames(consumer)).not.toContain("make");
	expect(thrown(() => new consumer(cred, "not-an-epoch"))).toBe("identity");
});

test("credential bounds", async () => {
	expect(thrown(() => new Credential({ context: new Uint8Array(), kid: 0, secret: new Uint8Array(31) }))).toBe(
		"invalid_secret",
	);
	expect(thrown(() => new Credential({ context: new Uint8Array(), kid: -1, secret: SECRET }))).toBe("identity");
	expect(thrown(() => new Credential({ context: new Uint8Array(), kid: 0.5, secret: SECRET }))).toBe("identity");
	expect(thrown(() => new Credential({ context: new Uint8Array(65536), kid: 0, secret: SECRET }))).toBe("identity");

	const aes = await crypto.subtle.generateKey({ name: "AES-GCM", length: 128 }, false, ["encrypt"]);
	expect(thrown(() => new Credential({ context: new Uint8Array(), kid: 0, secret: aes }))).toBe("invalid_secret");
});

test("a nonextractable HKDF key derives the same names as its bytes", async () => {
	const key = await crypto.subtle.importKey("raw", SECRET, "HKDF", false, ["deriveBits"]);
	const fromKey = new Credential({ context: encoder.encode("example.com/meeting-123"), kid: 7, secret: key });
	expect(await fromKey.path("meeting.hang")).toBe(await credential().path("meeting.hang"));
	const generation = fromKey.produce();
	expect(await generation.name("video")).toBe(await credential().consume(generation.epoch).name("video"));
});

test("secrets stay out of serialization", async () => {
	const secret = SECRET.slice();
	const cred = new Credential({ context: encoder.encode("example.com/meeting-123"), kid: 7, secret });
	const generation = cred.produce();
	const net = netTrack(await generation.name("video"));
	const producer = await generation.produce(net, "video");
	const marker = "moq-e2ee-00 test secret";
	for (const value of [cred, generation, producer]) {
		expect(JSON.stringify(value) ?? "").not.toContain(marker);
		expect(Bun.inspect(value)).not.toContain(marker);
		expect(Bun.inspect(value)).not.toContain("6d6f712d");
	}
	// The caller's buffers are its own, and the context it reads back is a copy.
	secret.fill(0);
	cred.context.fill(0);
	expect(await cred.path("meeting.hang")).toBe(await credential().path("meeting.hang"));
});

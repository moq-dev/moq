import { afterAll, afterEach, expect, spyOn, test } from "bun:test";
import { type Dispose, Once } from "@moq/signals";
import { accept, connect } from "./connection/index.ts";
import type * as Group from "./group.ts";
import * as Ietf from "./ietf/index.ts";
import * as Lite from "./lite/index.ts";
import { createMockTransportPair } from "./mock.ts";
import { Producer as OriginProducer } from "./origin.ts";
import * as Path from "./path.ts";
import { Timestamp } from "./time.ts";
import { wireOf } from "./wire.ts";

// A subscription races its track's `closed` once per frame. These count the listeners left on
// every Once that is still pending, so a per-frame leak shows up as a slope in frames.

const url = new URL("https://localhost:4443/test");

// Listeners attached to each Once. A `then` on a pending Once holds its listener until it
// settles, so it is never released here.
const attached = new Map<Once<unknown>, Set<object>>();

function attach(once: Once<unknown>): Dispose {
	const token = {};
	let set = attached.get(once);
	if (!set) {
		set = new Set();
		attached.set(once, set);
	}
	set.add(token);
	const owner = set;
	return () => owner.delete(token);
}

function pendingListeners(): number {
	let count = 0;
	for (const [once, set] of attached) {
		if (once.peek() === undefined) count += set.size;
	}
	return count;
}

const proto = Once.prototype as Once<unknown>;
const { subscribe, changed, then } = proto;
const spies = [
	spyOn(proto, "subscribe").mockImplementation(function (this: Once<unknown>, fn) {
		const release = attach(this);
		const dispose = subscribe.call(this, fn);
		return () => {
			release();
			dispose();
		};
	}),
	spyOn(proto, "changed").mockImplementation(function (this: Once<unknown>, fn?: (value: unknown) => void) {
		if (!fn) {
			attach(this);
			return changed.call(this);
		}
		const release = attach(this);
		const dispose = changed.call(this, fn);
		return () => {
			release();
			dispose();
		};
	} as typeof proto.changed),
	spyOn(proto, "then").mockImplementation(function (this: Once<unknown>, onFulfilled, onRejected) {
		if (this.peek() === undefined) attach(this);
		return then.call(this, onFulfilled, onRejected);
	} as typeof proto.then),
];

afterEach(() => attached.clear());
afterAll(() => {
	for (const spy of spies) spy.mockRestore();
});

async function session(alpn: string) {
	const pair = createMockTransportPair(alpn);
	const origin = new OriginProducer();
	const [client, server] = await Promise.all([
		connect({ url, transport: pair.client }),
		accept({ transport: pair.server, url, publish: origin.consume() }),
	]);

	const broadcast = origin.createBroadcast(Path.from("test"));
	broadcast.announce();
	const producer = broadcast.createTrack("video");

	const remote = wireOf(client).consume(Path.from("test"));
	const track = remote.track("video").subscribe().ordered();

	const close = () => {
		track.close();
		remote.close();
		broadcast.close();
		client.close();
		server.close();
	};
	return { producer, track, close };
}

function frame(i: number): Group.Frame {
	return { payload: new Uint8Array([i & 0xff]), timestamp: Timestamp.fromMillis(i) };
}

async function readFrames(group: Group.Consumer, count: number): Promise<void> {
	for (let i = 0; i < count; i++) {
		if (!(await group.readFrame())) throw new Error(`group ended after ${i} frames`);
	}
}

for (const [name, alpn] of [
	["lite-03", Lite.ALPN_03],
	["lite-05", Lite.ALPN_05],
	["ietf-19", Ietf.ALPN.DRAFT_19],
]) {
	test(`${name}: the frames of one long group leave no listener behind`, async () => {
		const { producer, track, close } = await session(alpn);
		const group = producer.appendGroup();

		group.writeFrame(frame(0));
		const consumer = await track.nextGroup();
		if (!consumer) throw new Error("no group");
		await readFrames(consumer, 1);

		for (let i = 1; i < 50; i++) group.writeFrame(frame(i));
		await readFrames(consumer, 49);
		const before = pendingListeners();

		for (let i = 50; i < 1050; i++) group.writeFrame(frame(i));
		await readFrames(consumer, 1000);
		expect(pendingListeners() - before).toBeLessThan(10);

		close();
	});

	test(`${name}: single-frame groups leave no listener behind`, async () => {
		const { producer, track, close } = await session(alpn);

		const send = async (count: number, offset: number) => {
			for (let i = 0; i < count; i++) {
				const group = producer.appendGroup();
				group.writeFrame(frame(offset + i));
				group.close();
				const consumer = await track.nextGroup();
				if (!consumer) throw new Error("no group");
				await readFrames(consumer, 1);
			}
		};

		await send(50, 0);
		const before = pendingListeners();
		await send(200, 50);
		expect(pendingListeners() - before).toBeLessThan(10);

		close();
	});
}

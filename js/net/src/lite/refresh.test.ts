import { expect, spyOn, test } from "bun:test";
import { Manual, Nano } from "@moq/clock";
import { accept } from "../connection/accept.ts";
import { connect } from "../connection/connect.ts";
import { createMockTransportPair } from "../mock.ts";
import { Producer } from "../origin.ts";
import * as Path from "../path.ts";
import { SubscribeUpdate } from "./subscribe.ts";
import { ALPN_06, ALPN_07_WIP } from "./version.ts";

test.each([ALPN_06, ALPN_07_WIP])("refresh joins reach a browser publisher on %s", async (alpn) => {
	const clock = new Manual();
	const installed = clock.install();
	const source = new Producer();
	const b = new Producer();
	const broadcastPublication = source.createBroadcast(Path.from("room"));
	const broadcast = broadcastPublication.broadcast;
	broadcastPublication.announce();
	const producer = broadcast.createTrack("data", { refresh: 1 });
	const pair = createMockTransportPair(alpn);
	const url = new URL("https://localhost/refresh");
	const updates: number[] = [];
	const encode = SubscribeUpdate.prototype.encode;
	const spy = spyOn(SubscribeUpdate.prototype, "encode").mockImplementation(function (
		this: SubscribeUpdate,
		writer,
		version,
	) {
		if (this.wantedGroup !== undefined) updates.push(this.wantedGroup);
		return encode.call(this, writer, version);
	});
	const peers: { abort(): void }[] = [];
	try {
		peers.push(
			...(await Promise.all([
				connect({ url, transport: pair.client, consume: b }),
				accept({ url, transport: pair.server, publish: source.consume() }),
			])),
		);
		const announcements = b.announced();
		while ((await announcements.next())?.kind !== "start") {}
		const request = b.request(Path.from("room"));
		while (request.active.peek() === undefined) await request.active.changed();
		const active = request.active.peek();
		if (!active) throw new Error("broadcast missing");
		const remote = active.track("data");
		expect((await remote.info()).refresh).toBe(alpn === ALPN_07_WIP ? 1 : undefined);
		const baseline = remote.subscribe();
		const group = producer.appendGroup();
		group.writeFrame({ payload: new Uint8Array(3) });
		group.writeFrame({ payload: new Uint8Array(3) });
		const received = await baseline.recvGroup();
		if (!received) throw new Error("group missing");
		expect((await received.readFrame())?.payload.length).toBe(3);
		expect((await received.readFrame())?.payload.length).toBe(3);
		const joins = [remote.subscribe()];
		await clock.advance(Nano(0));
		expect(producer.demand().wanted.peek()).toBe(0);
		updates.length = 0;
		group.writeFrame({ payload: new Uint8Array(1) });
		expect((await received.readFrame())?.payload.length).toBe(1);
		for (let i = 0; i < 8; i++) joins.push(remote.subscribe());
		await clock.advance(Nano(0));
		if (alpn === ALPN_07_WIP) {
			while (producer.demand().wanted.peek() !== 1) await producer.demand().wanted.changed();
		}
		expect(producer.demand().wanted.peek()).toBe(alpn === ALPN_07_WIP ? 1 : 0);
		// Concurrent local joins coalesce into one upstream update.
		expect(updates).toEqual(alpn === ALPN_07_WIP ? [1] : []);
		// A parameter change restates the wanted group rather than clearing it.
		joins.push(remote.subscribe({ priority: 9 }));
		await clock.advance(Nano(0));
		expect(updates).toEqual(alpn === ALPN_07_WIP ? [1, 1] : []);
		for (const join of joins) join.close();
		received.close();
		baseline.close();
		group.finish();
	} finally {
		spy.mockRestore();
		for (const origin of [b, source]) origin.close();
		for (const peer of peers) peer.abort();
		await clock.advance(Nano(0));
		installed.close();
	}
});
